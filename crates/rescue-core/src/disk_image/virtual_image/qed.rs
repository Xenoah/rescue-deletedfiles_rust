//! QEMU Enhanced Disk. Parent names are checked against files selected by the caller.
use super::{
    ImportedFile,
    chain::{self, Layer},
    invalid, le32, read_at,
};
use crate::{Error, Result, disk_image::RawExpectation};
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::File,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const MAGIC: &[u8; 4] = b"QED\0";

struct Qed {
    bytes: u64,
    cluster: u64,
    // Sorted logical cluster numbers. Physical offset 1 explicitly overrides any parent with zeroes.
    entries: Vec<(u64, u64)>,
    backing: Option<(String, bool)>,
}
struct Budget {
    tables: u64,
    mappings: usize,
}
impl Budget {
    fn table(&mut self, bytes: u64) -> Result<()> {
        self.tables = self
            .tables
            .checked_sub(bytes)
            .ok_or_else(|| Error::Limit("QED allocation tables exceed 128 MiB".into()))?;
        Ok(())
    }
}

fn word(bytes: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn backing(
    file: &mut File,
    header: &[u8],
    header_bytes: u64,
    flags: u64,
) -> Result<Option<(String, bool)>> {
    if flags & 1 == 0 {
        return if flags & 4 == 0 {
            Ok(None)
        } else {
            Err(invalid("QED raw-parent flag without a parent"))
        };
    }
    let pos = u64::from(le32(header, 56));
    let size = le32(header, 60);
    if pos < 64 || !(1..=1023).contains(&size) || pos + u64::from(size) > header_bytes {
        return Err(invalid("QED parent name outside header"));
    }
    let mut name = vec![0; size as usize];
    read_at(file, pos, &mut name)?;
    let name = String::from_utf8(name).map_err(|_| invalid("invalid QED parent name"))?;
    if matches!(name.as_str(), "." | "..")
        || name.contains(['/', '\\', ':'])
        || name.chars().any(char::is_control)
    {
        return Err(Error::Unsupported(
            "QED parent must have a local filename without directories".into(),
        ));
    }
    Ok(Some((name, flags & 4 != 0)))
}

impl Qed {
    fn open(
        file: &mut File,
        length: u64,
        budget: &mut Budget,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let mut h = [0; 64];
        read_at(file, 0, &mut h)?;
        if &h[..4] != MAGIC {
            return Err(invalid(
                "expected a QED disk or an explicitly identified RAW parent",
            ));
        }
        let cluster = u64::from(le32(&h, 4));
        let n = u64::from(le32(&h, 8));
        let headers = u64::from(le32(&h, 12));
        let flags = word(&h, 16);
        if flags & !5 != 0 || word(&h, 32) != 0 {
            return Err(Error::Unsupported(
                "QED needs consistency checking or has unsupported features".into(),
            ));
        }
        if !(4096..=1 << 26).contains(&cluster)
            || !cluster.is_power_of_two()
            || !(1..=16).contains(&n)
            || !n.is_power_of_two()
            || headers == 0
        {
            return Err(invalid("QED cluster, table or header size"));
        }
        let header_bytes = headers * cluster;
        let table_bytes = n * cluster;
        let slots = table_bytes / 8;
        let bytes = word(&h, 48);
        let logical = bytes.div_ceil(cluster);
        if header_bytes > length
            || bytes == 0
            || !bytes.is_multiple_of(512)
            || logical > slots * slots
        {
            return Err(invalid("QED logical capacity or truncated header"));
        }
        let backing = backing(file, &h, header_bytes, flags)?;
        let mut occupied = Vec::new();
        let mut claim = |pos: u64, size: u64| -> Result<()> {
            let end = pos
                .checked_add(size)
                .filter(|&end| end <= length)
                .ok_or_else(|| invalid("QED allocation outside source"))?;
            if pos < header_bytes || !pos.is_multiple_of(cluster) {
                return Err(invalid("QED allocation overlaps header or is unaligned"));
            }
            occupied.push((pos, end));
            Ok(())
        };
        let l1_at = word(&h, 40);
        claim(l1_at, table_bytes)?;
        budget.table(table_bytes)?;
        let mut l1 = vec![0; table_bytes as usize];
        read_at(file, l1_at, &mut l1)?;
        let mut l2 = Vec::new();
        let mut entries = Vec::new();
        for (i, slot) in l1.as_chunks::<8>().0.iter().enumerate() {
            if i.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let at = u64::from_le_bytes(*slot);
            if at == 0 {
                continue;
            }
            let base = i as u64 * slots;
            if base >= logical {
                return Err(invalid("QED L1 entry beyond logical disk"));
            }
            claim(at, table_bytes)?;
            budget.table(table_bytes)?;
            l2.resize(table_bytes as usize, 0);
            read_at(file, at, &mut l2)?;
            for (j, slot) in l2.as_chunks::<8>().0.iter().enumerate() {
                if j.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let physical = u64::from_le_bytes(*slot);
                if physical == 0 {
                    continue;
                }
                let index = base + j as u64;
                if index >= logical {
                    return Err(invalid("QED data entry beyond logical disk"));
                }
                budget.mappings = budget
                    .mappings
                    .checked_sub(1)
                    .ok_or_else(|| Error::Limit("QED exceeds 2,097,152 mapped clusters".into()))?;
                if physical != 1 {
                    claim(physical, cluster)?;
                }
                entries.push((index, physical));
            }
        }
        occupied.sort_unstable();
        if occupied.windows(2).any(|w| w[0].1 > w[1].0) {
            return Err(invalid("overlapping QED table or data allocations"));
        }
        Ok(Self {
            bytes,
            cluster,
            entries,
            backing,
        })
    }
}

fn read_layer(
    layer: &mut Layer<Option<Qed>>,
    guest: u64,
    buf: &mut [u8],
    limit: &mut usize,
) -> Result<bool> {
    let size = layer
        .layout
        .as_ref()
        .map_or(layer.before.len(), |q| q.bytes);
    if guest >= size {
        buf[..*limit].fill(0);
        return Ok(true);
    }
    *limit = (*limit as u64).min(size - guest) as usize;
    let Some(q) = &layer.layout else {
        read_at(&mut layer.file, guest, &mut buf[..*limit])?;
        return Ok(true);
    };
    match q
        .entries
        .binary_search_by_key(&(guest / q.cluster), |&(i, _)| i)
    {
        Ok(i) => {
            let physical = q.entries[i].1;
            let within = guest % q.cluster;
            *limit = (*limit as u64).min(q.cluster - within) as usize;
            if physical == 1 {
                buf[..*limit].fill(0);
            } else {
                read_at(&mut layer.file, physical + within, &mut buf[..*limit])?;
            }
            Ok(true)
        }
        Err(i) => {
            if let Some((next, _)) = q.entries.get(i) {
                *limit = (*limit as u64).min(next * q.cluster - guest) as usize;
            }
            Ok(false)
        }
    }
}

pub(in crate::disk_image) fn import(
    paths: &[OsString],
    kind: Option<&str>,
    output: &Path,
    expected: RawExpectation,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if kind.is_some_and(|s| s != "qed") {
        return Ok(None);
    }
    let mut first = File::open(&paths[0])?;
    let mut sig = [0; 4];
    if first.metadata()?.len() >= 4 {
        read_at(&mut first, 0, &mut sig)?;
    }
    if &sig != MAGIC {
        return if kind.is_none() {
            Ok(None)
        } else {
            Err(invalid("signature differs from requested QED disk"))
        };
    }
    if paths.len() > 8 {
        return Err(Error::Limit("QED chain exceeds eight files".into()));
    }
    let mut members = Vec::new();
    let mut handles = HashSet::new();
    let mut budget = Budget {
        tables: 128 << 20,
        mappings: 2_097_152,
    };
    let mut source = Some(first);
    let mut raw = false;
    for (i, path) in paths.iter().enumerate() {
        let mut file = match source.take() {
            Some(f) => f,
            None => File::open(path)?,
        };
        if !handles.insert(same_file::Handle::from_file(file.try_clone()?)?) {
            return Err(invalid("repeated QED chain member"));
        }
        let before = file.metadata()?;
        let layout = if raw {
            if !before.len().is_multiple_of(512) {
                return Err(invalid("QED RAW parent is not sector aligned"));
            }
            None
        } else {
            Some(Qed::open(&mut file, before.len(), &mut budget, cancel)?)
        };
        match (
            layout.as_ref().and_then(|q| q.backing.as_ref()),
            paths.get(i + 1),
        ) {
            (Some((name, is_raw)), Some(next))
                if Path::new(next).file_name() == Some(OsStr::new(name)) =>
            {
                raw = *is_raw
            }
            (Some(_), Some(_)) => {
                return Err(invalid("QED parent name differs from selected file"));
            }
            (Some(_), None) => {
                return Err(Error::Request(
                    "select all QED backing files from nearest parent to base".into(),
                ));
            }
            (None, Some(_)) => {
                return Err(invalid("additional file after standalone QED or RAW base"));
            }
            (None, None) => {}
        }
        members.push(Layer {
            file,
            before,
            layout,
        });
    }
    let bytes = members[0].layout.as_ref().unwrap().bytes;
    chain::flatten(&mut members, bytes, output, expected, cancel, read_layer).map(Some)
}
