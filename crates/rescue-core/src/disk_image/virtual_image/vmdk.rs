//! Self-contained hosted sparse VMDK. Descriptor strings never select a host path.
use super::{Block, Error, File, Result, invalid, le32, read_at};
use std::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

const SECTOR: u64 = 512;
const MAX_GRAINS: u64 = 1 << 22;
const NL: u32 = 1;
const REDUNDANT: u32 = 2;
const ZERO_GRAIN: u32 = 4;

fn u64le(data: &[u8], at: usize) -> u64 {
    u64::from_le_bytes(data[at..at + 8].try_into().unwrap())
}
fn sectors(n: u64) -> Result<u64> {
    n.checked_mul(SECTOR)
        .ok_or_else(|| invalid("VMDK sector offset overflow"))
}
fn region(start: u64, len: u64, end: u64) -> Result<Range<u64>> {
    let stop = start
        .checked_add(len)
        .filter(|&x| x <= end && len != 0)
        .ok_or_else(|| invalid("VMDK region outside container"))?;
    Ok(start..stop)
}

struct Header {
    capacity: u64,
    grain_size: u64,
    entries: usize,
    flags: u32,
    descriptor: Range<u64>,
    directory: u64,
    backup: u64,
    overhead: u64,
}
impl Header {
    fn read(file: &mut File, length: u64) -> Result<Self> {
        let mut h = [0; 512];
        read_at(file, 0, &mut h)?;
        let flags = le32(&h, 8);
        if &h[..4] != b"KDMV"
            || !(1..=3).contains(&le32(&h, 4))
            || flags & !(NL | REDUNDANT | ZERO_GRAIN) != 0
            || h[72] != 0
            || h[77..79] != [0, 0]
        {
            return Err(Error::Unsupported(
                "requires a clean standalone uncompressed hosted sparse VMDK".into(),
            ));
        }
        if flags & NL != 0 && h[73..77] != [10, 32, 13, 10] {
            return Err(invalid("VMDK newline-detection bytes disagree"));
        }
        let capacity = sectors(u64le(&h, 12))?;
        let grain_size = sectors(u64le(&h, 20))?;
        let entries = le32(&h, 44) as usize;
        let overhead = sectors(u64le(&h, 64))?;
        if capacity == 0
            || capacity > 64u64 << 40
            || !(SECTOR..=16 << 20).contains(&grain_size)
            || !grain_size.is_power_of_two()
            || !(1..=512).contains(&entries)
            || !entries.is_power_of_two()
            || overhead < SECTOR
            || overhead > length
            || capacity.div_ceil(grain_size) > MAX_GRAINS
        {
            return Err(invalid("VMDK geometry, overhead or grain count"));
        }
        let ds = sectors(u64le(&h, 28))?;
        let dl = sectors(u64le(&h, 36))?;
        if ds < SECTOR || dl > 1 << 20 {
            return Err(invalid("missing or excessive VMDK descriptor"));
        }
        let descriptor = region(ds, dl, overhead)?;
        Ok(Self {
            capacity,
            grain_size,
            entries,
            flags,
            descriptor,
            directory: sectors(u64le(&h, 56))?,
            backup: sectors(u64le(&h, 48))?,
            overhead,
        })
    }
}

fn descriptor(file: &mut File, h: &Header) -> Result<()> {
    let mut bytes = vec![0; (h.descriptor.end - h.descriptor.start) as usize];
    read_at(file, h.descriptor.start, &mut bytes)?;
    let end = bytes.iter().position(|&c| c == 0).unwrap_or(bytes.len());
    if bytes[end..].iter().any(|&c| c != 0) {
        return Err(invalid("VMDK descriptor has data after NUL"));
    }
    let text =
        std::str::from_utf8(&bytes[..end]).map_err(|_| invalid("non-UTF-8 VMDK descriptor"))?;
    let (mut version, mut cid, mut parent, mut kind, mut extent) = (None, None, None, None, None);
    for line in text
        .lines()
        .map(str::trim)
        .filter(|s| !s.is_empty() && !s.starts_with('#'))
    {
        if let Some((key, value)) = line
            .split_once('=')
            .filter(|(k, _)| !k.trim().contains(char::is_whitespace))
        {
            let value = value.trim();
            let slot = match key.trim() {
                "version" => &mut version,
                "CID" => &mut cid,
                "parentCID" => &mut parent,
                "createType" => &mut kind,
                "parentFileNameHint" => {
                    return Err(Error::Unsupported("VMDK parent reference".into()));
                }
                _ => continue,
            };
            if slot.replace(value).is_some() {
                return Err(invalid("duplicate VMDK descriptor field"));
            }
        } else {
            let (prefix, rest) = line
                .split_once('"')
                .ok_or_else(|| invalid("VMDK extent descriptor"))?;
            let name = rest
                .strip_suffix('"')
                .ok_or_else(|| invalid("VMDK extent name"))?;
            let mut fields = prefix.split_ascii_whitespace();
            let access = fields.next();
            let count = fields.next().and_then(|v| v.parse::<u64>().ok());
            if !matches!(access, Some("RW" | "RDONLY"))
                || count != Some(h.capacity / SECTOR)
                || fields.next() != Some("SPARSE")
                || fields.next().is_some()
                || name.is_empty()
                || name.contains(['"', '/', '\\', ':'])
                || matches!(name, "." | "..")
                || extent.replace(()).is_some()
            {
                return Err(Error::Unsupported(
                    "VMDK requires one standalone sparse extent".into(),
                ));
            }
        }
    }
    if version != Some("1")
        || !cid.is_some_and(|s| s.len() == 8 && s.bytes().all(|c| c.is_ascii_hexdigit()))
        || !parent.is_some_and(|s| s.eq_ignore_ascii_case("ffffffff"))
        || kind != Some("\"monolithicSparse\"")
        || extent.is_none()
    {
        return Err(Error::Unsupported(
            "VMDK descriptor is incomplete, dependent or not monolithicSparse".into(),
        ));
    }
    Ok(())
}

struct Maps {
    regions: Vec<Range<u64>>,
    length: u64,
}
impl Maps {
    fn table(&mut self, file: &mut File, at: u64, count: usize, limit: u64) -> Result<Vec<u8>> {
        let len = count as u64 * 4;
        let area = region(at, len.next_multiple_of(SECTOR), limit)?;
        if at < SECTOR {
            return Err(invalid("VMDK table overlaps header"));
        }
        self.regions.push(area);
        let mut bytes = vec![0; len as usize];
        read_at(file, at, &mut bytes)?;
        Ok(bytes)
    }
    fn read_grains(
        &mut self,
        file: &mut File,
        h: &Header,
        cancel: &AtomicBool,
    ) -> Result<Vec<u32>> {
        let count = h.capacity.div_ceil(h.grain_size) as usize;
        let table_count = count.div_ceil(h.entries);
        let main = self.table(file, h.directory, table_count, h.overhead)?;
        let backup = if h.flags & REDUNDANT != 0 {
            Some(self.table(file, h.backup, table_count, h.overhead)?)
        } else {
            None
        };
        let mut result = Vec::with_capacity(count);
        for i in 0..table_count {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let at = u64::from(le32(&main, i * 4)) * SECTOR;
            let other = backup.as_ref().map(|v| u64::from(le32(v, i * 4)) * SECTOR);
            if other.is_some_and(|b| (b == 0) != (at == 0)) {
                return Err(invalid("VMDK grain directory copies disagree"));
            }
            let used = h.entries.min(count - result.len());
            if at == 0 {
                result.resize(result.len() + used, 0);
                continue;
            }
            let table = self.table(file, at, h.entries, h.overhead)?;
            if let Some(b) = other
                && self.table(file, b, h.entries, h.overhead)? != table
            {
                return Err(invalid("VMDK grain table copies disagree"));
            }
            for entry in table[..used * 4].as_chunks::<4>().0 {
                let sector = u32::from_le_bytes(*entry);
                if sector == 0 || sector == 1 && h.flags & ZERO_GRAIN != 0 {
                    result.push(0);
                } else {
                    let start = u64::from(sector) * SECTOR;
                    if start < h.overhead {
                        return Err(invalid("VMDK grain overlaps metadata"));
                    }
                    self.regions.push(region(start, h.grain_size, self.length)?);
                    result.push(sector);
                }
            }
        }
        self.regions.sort_unstable_by_key(|r| r.start);
        if self.regions.windows(2).any(|w| w[0].end > w[1].start) {
            return Err(invalid("overlapping VMDK grains or tables"));
        }
        Ok(result)
    }
}
pub(super) struct Vmdk {
    pub size: u64,
    grain_size: u64,
    grains: Vec<u32>,
}
impl Vmdk {
    pub fn open(file: &mut File, length: u64, cancel: &AtomicBool) -> Result<Self> {
        let h = Header::read(file, length)?;
        descriptor(file, &h)?;
        let mut maps = Maps {
            length,
            regions: vec![0..SECTOR, h.descriptor.clone()],
        };
        Ok(Self {
            size: h.capacity,
            grain_size: h.grain_size,
            grains: maps.read_grains(file, &h, cancel)?,
        })
    }
    pub fn block(&self, offset: u64) -> (Block, u64) {
        let at = self.grains[(offset / self.grain_size) as usize];
        let within = offset % self.grain_size;
        (
            if at == 0 {
                Block::Zero
            } else {
                Block::Data(u64::from(at) * SECTOR + within)
            },
            self.grain_size - within,
        )
    }
}
