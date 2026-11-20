//! Logical sectors from Parallels expanding HDS files and explicit single-storage snapshot descriptors.
mod descriptor;
use super::{
    chain::{self, Layer},
    invalid, le32, read_at,
};
use crate::{
    Error, Result,
    disk_image::{ImportedFile, RawExpectation},
};
use descriptor::{MAX_DESCRIPTOR, MAX_LAYERS};
use std::{
    collections::{HashMap, HashSet},
    ffi::OsString,
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const LEGACY: &[u8; 16] = b"WithoutFreeSpace";
const EXTENDED: &[u8; 16] = b"WithouFreSpacExt";
const MAX_ENTRIES: usize = 4 * 1024 * 1024;
const SECTOR: u64 = 512;

fn geometry(sectors: u64, cluster: u64) -> bool {
    (1..=(16 << 40) / SECTOR).contains(&sectors) && cluster.is_power_of_two() && cluster <= 65536
}

struct Hds {
    sectors: u64,
    cluster: u64,
    multiplier: u64,
    entries: Vec<u32>,
    has_holes: bool,
    stale_empty_flag: bool,
}

impl Hds {
    fn open(file: &mut File, length: u64, budget: &mut usize, cancel: &AtomicBool) -> Result<Self> {
        let mut h = [0u8; 64];
        read_at(file, 0, &mut h)?;
        let legacy = &h[..16] == LEGACY;
        if (!legacy && &h[..16] != EXTENDED) || le32(&h, 16) != 2 {
            return Err(invalid("invalid Parallels signature or header version"));
        }
        let cluster_sectors = u64::from(le32(&h, 28));
        let entries = le32(&h, 32) as usize;
        let sectors = u64::from_le_bytes(h[36..44].try_into().unwrap());
        if !geometry(sectors, cluster_sectors) || (legacy && sectors > u64::from(u32::MAX)) {
            return Err(invalid("unsupported Parallels disk or cluster size"));
        }
        let flags = le32(&h, 52);
        if le32(&h, 44) != 0 || flags & !1 != 0 || h[56..64] != [0; 8] {
            return Err(Error::Unsupported(
                "Parallels image is in use, under conversion, or has format extensions".into(),
            ));
        }
        let needed = sectors.div_ceil(cluster_sectors);
        if (entries as u64) < needed {
            return Err(invalid(
                "Parallels allocation table does not cover the logical disk",
            ));
        }
        *budget = budget
            .checked_sub(entries)
            .ok_or_else(|| Error::Limit("Parallels tables exceed four million entries".into()))?;
        let table_bytes = entries * 4;
        let min_sector = (64 + table_bytes as u64).div_ceil(SECTOR);
        let first = u64::from(le32(&h, 48));
        let first = if legacy && first == 0 {
            min_sector
        } else {
            first
        };
        if !length.is_multiple_of(SECTOR)
            || first < min_sector
            || first * SECTOR > length
            || (!legacy && !first.is_multiple_of(cluster_sectors))
        {
            return Err(invalid("Parallels data offset or source length"));
        }
        let cluster = cluster_sectors * SECTOR;
        let multiplier = if legacy { SECTOR } else { cluster };
        let mut table = vec![0u8; table_bytes];
        read_at(file, 64, &mut table)?;
        let mut mapping = Vec::with_capacity(needed as usize);
        let mut allocated = Vec::new();
        let mut has_holes = false;
        for (i, word) in table.as_chunks::<4>().0.iter().enumerate() {
            if i.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let value = u32::from_le_bytes(*word);
            if i as u64 >= needed {
                if value != 0 {
                    return Err(invalid("Parallels allocation beyond the guest"));
                }
                continue;
            }
            if value == 0 {
                has_holes = true;
            } else {
                let at = u64::from(value) * multiplier;
                if at < first * SECTOR || at.checked_add(cluster).is_none_or(|end| end > length) {
                    return Err(invalid(
                        "Parallels cluster overlaps metadata or exceeds source length",
                    ));
                }
                allocated.push(at);
            }
            mapping.push(value);
        }
        allocated.sort_unstable();
        if allocated.windows(2).any(|w| w[0] + cluster > w[1]) {
            return Err(invalid("overlapping Parallels data clusters"));
        }
        Ok(Self {
            sectors,
            cluster,
            multiplier,
            entries: mapping,
            has_holes,
            stale_empty_flag: flags == 1 && !allocated.is_empty(),
        })
    }
}

fn read_member(
    member: &mut Layer<Option<Hds>>,
    offset: u64,
    buf: &mut [u8],
    n: &mut usize,
) -> Result<bool> {
    let pos = if let Some(hds) = &member.layout {
        let within = offset % hds.cluster;
        *n = (*n).min((hds.cluster - within) as usize);
        let entry = hds.entries[(offset / hds.cluster) as usize];
        if entry == 0 {
            return Ok(false);
        }
        u64::from(entry) * hds.multiplier + within
    } else {
        offset
    };
    read_at(&mut member.file, pos, &mut buf[..*n])?;
    Ok(true)
}

fn check_empty_flag(hds: &Hds, expected: RawExpectation) -> Result<()> {
    if hds.stale_empty_flag && expected.sha256.is_none() {
        return Err(Error::Request("Parallels empty flag contradicts its allocated clusters. Supply an independent logical RAW SHA-256 to verify the result".into()));
    }
    Ok(())
}

fn descriptor_layers(
    paths: &[OsString],
    descriptor: &descriptor::Descriptor,
    expected: RawExpectation,
    cancel: &AtomicBool,
) -> Result<Vec<Layer<Option<Hds>>>> {
    if paths.len() != descriptor.layers.len() {
        return Err(Error::Request(
            "select every Parallels image named by the descriptor as a separate segment".into(),
        ));
    }
    let mut by_name = HashMap::new();
    for path in paths {
        let path = Path::new(path);
        let name = path
            .file_name()
            .and_then(|n| n.to_str())
            .ok_or_else(|| invalid("invalid Parallels member name"))?;
        if by_name.insert(name, path).is_some() {
            return Err(invalid("duplicate Parallels member name"));
        }
    }
    let mut members = Vec::new();
    let mut handles = HashSet::new();
    let mut budget = MAX_ENTRIES;
    for layer in &descriptor.layers {
        let path = by_name
            .remove(layer.name.as_str())
            .ok_or_else(|| invalid("selected Parallels file does not match its descriptor name"))?;
        let mut file = File::open(path)?;
        if !handles.insert(same_file::Handle::from_file(file.try_clone()?)?) {
            return Err(invalid("Parallels members alias one another"));
        }
        let before = file.metadata()?;
        let layout = if layer.plain {
            if before.len() != descriptor.sectors * SECTOR {
                return Err(invalid(
                    "Parallels plain base length differs from descriptor",
                ));
            }
            None
        } else {
            let hds = Hds::open(&mut file, before.len(), &mut budget, cancel)?;
            if hds.sectors != descriptor.sectors
                || hds.cluster != descriptor.cluster_sectors * SECTOR
            {
                return Err(invalid("Parallels member geometry differs from descriptor"));
            }
            check_empty_flag(&hds, expected)?;
            Some(hds)
        };
        members.push(Layer {
            file,
            before,
            layout,
        });
    }
    Ok(members)
}

fn descriptor_name_or_signature(path: &Path, file: &mut File, length: u64) -> Result<bool> {
    if path
        .file_name()
        .is_some_and(|n| n.eq_ignore_ascii_case("DiskDescriptor.xml"))
    {
        return Ok(true);
    }
    if !path
        .extension()
        .is_some_and(|n| n.eq_ignore_ascii_case("xml"))
    {
        return Ok(false);
    }
    use quick_xml::{Reader, events::Event};
    let mut prefix = vec![0; length.min(4096) as usize];
    read_at(file, 0, &mut prefix)?;
    let mut reader = Reader::from_reader(prefix.as_slice());
    loop {
        match reader.read_event() {
            Ok(Event::Start(e) | Event::Empty(e)) => {
                return Ok(e.name().as_ref() == b"Parallels_disk_image");
            }
            Ok(Event::Eof) | Err(_) => return Ok(false),
            _ => {}
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
    if kind.is_some_and(|v| v != "parallels") {
        return Ok(None);
    }
    let path = Path::new(&paths[0]);
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    let mut head = [0; 16];
    let hds = before.len() >= 16 && {
        read_at(&mut file, 0, &mut head)?;
        &head == LEGACY || &head == EXTENDED
    };
    if !hds && kind.is_none() && !descriptor_name_or_signature(path, &mut file, before.len())? {
        if path
            .extension()
            .is_some_and(|ext| ext.eq_ignore_ascii_case("hds"))
        {
            return Err(Error::Request("unrecognized HDS header; select DiskDescriptor.xml with its members, or explicitly import a known flat base as RAW".into()));
        }
        return Ok(None);
    }
    let output = crate::canonical_output_path(output)?;
    for input in paths {
        crate::disk_image::extents::guard_file_output(Path::new(input), &output)?;
    }
    if hds {
        if paths.len() != 1 {
            return Err(Error::Request(
                "select DiskDescriptor.xml as the source to resolve additional Parallels layers"
                    .into(),
            ));
        }
        let mut budget = MAX_ENTRIES;
        let image = Hds::open(&mut file, before.len(), &mut budget, cancel)?;
        if image.has_holes && expected.sha256.is_none() {
            return Err(Error::Request("a sparse HDS has no parent identity; select DiskDescriptor.xml and its members or supply the independent logical RAW SHA-256".into()));
        }
        check_empty_flag(&image, expected)?;
        let size = image.sectors * SECTOR;
        return chain::flatten(
            &mut [Layer {
                file,
                before,
                layout: Some(image),
            }],
            size,
            &output,
            expected,
            cancel,
            read_member,
        )
        .map(Some);
    }
    if paths.len() > MAX_LAYERS + 1 || before.len() > MAX_DESCRIPTOR {
        return Err(Error::Limit(
            "Parallels descriptor exceeds 1 MiB or eight layers".into(),
        ));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    (&mut file)
        .take(MAX_DESCRIPTOR + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != before.len() {
        return Err(Error::SourceChanged);
    }
    let parsed = descriptor::parse(&bytes)?;
    let mut members = descriptor_layers(&paths[1..], &parsed, expected, cancel)?;
    // Check the descriptor's handle along with all data handles before publishing.
    chain::flatten_checked(
        &mut members,
        parsed.sectors * SECTOR,
        &output,
        expected,
        cancel,
        read_member,
        Some((&file, &before)),
    )
    .map(Some)
}
