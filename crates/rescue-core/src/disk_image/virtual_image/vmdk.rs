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
const ZLIB: u32 = 1 << 16;
const MARKERS: u32 = 1 << 17;

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

// Stream writers can place the active directory at EOF and mark its initial
// location with all ones. The final three sectors hold a footer marker, a
// header with the resolved directory and an end-of-stream marker.
fn select_directory(
    file: &mut File,
    h: &[u8],
    length: u64,
    compressed: bool,
) -> Result<(u64, Option<u64>)> {
    let primary = u64le(h, 56);
    if !compressed || primary < u64le(h, 64) {
        return Ok((sectors(primary)?, None));
    }
    let at = length
        .checked_sub(3 * SECTOR)
        .filter(|at| at.is_multiple_of(SECTOR))
        .ok_or_else(|| invalid("VMDK footer position"))?;
    let mut tail = [0; 1536];
    read_at(file, at, &mut tail)?;
    let last = &tail[512..1024];
    if u64le(&tail, 0) != 1
        || le32(&tail, 8) != 0
        || le32(&tail, 12) != 3
        || tail[1024..1040] != [0; 16]
        || h[..48] != last[..48]
        || h[64..79] != last[64..79]
        || h[48..56] != last[48..56]
    {
        return Err(invalid("VMDK footer marker, repeated header or end marker"));
    }
    let directory = u64le(last, 56);
    if primary != u64::MAX && primary != directory {
        return Err(invalid("conflicting VMDK grain directory pointers"));
    }
    Ok((sectors(directory)?, Some(at)))
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
    compressed: bool,
    footer: Option<u64>,
}
impl Header {
    fn read(file: &mut File, length: u64) -> Result<Self> {
        let mut h = [0; 512];
        read_at(file, 0, &mut h)?;
        let flags = le32(&h, 8);
        let compressed = flags & ZLIB != 0;
        if &h[..4] != b"KDMV"
            || !(1..=3).contains(&le32(&h, 4))
            || flags & !(NL | REDUNDANT | ZERO_GRAIN | ZLIB | MARKERS) != 0
            || h[72] != 0
            || (flags & MARKERS != 0) != compressed
            || h[77..79] != [u8::from(compressed), 0]
        {
            return Err(Error::Unsupported(
                "requires a clean standalone hosted sparse or zlib streamOptimized VMDK".into(),
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
        let (directory, footer) = select_directory(file, &h, length, compressed)?;
        if footer.is_some_and(|f| f < overhead || directory < overhead) {
            return Err(invalid("VMDK streamed metadata placement"));
        }
        Ok(Self {
            capacity,
            grain_size,
            entries,
            flags,
            descriptor,
            directory,
            compressed,
            footer,
            backup: if flags & REDUNDANT != 0 {
                sectors(u64le(&h, 48))?
            } else {
                0
            },
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
        || kind
            != Some(if h.compressed {
                "\"streamOptimized\""
            } else {
                "\"monolithicSparse\""
            })
        || extent.is_none()
    {
        return Err(Error::Unsupported(
            "VMDK descriptor conflicts with standalone extent type".into(),
        ));
    }
    Ok(())
}

#[derive(Clone, Copy, Default)]
struct Grain {
    sector: u32,
    encoded: u32,
}

struct Maps {
    regions: Vec<Range<u64>>,
    length: u64,
}
impl Maps {
    fn table(
        &mut self,
        file: &mut File,
        at: u64,
        count: usize,
        h: &Header,
        marker_kind: u32,
    ) -> Result<Vec<u8>> {
        let len = count as u64 * 4;
        let mut area = region(
            at,
            len.next_multiple_of(SECTOR),
            h.footer.unwrap_or(h.overhead),
        )?;
        if at < SECTOR {
            return Err(invalid("VMDK table overlaps header"));
        }
        if h.footer.is_some() {
            if at < h.overhead + SECTOR {
                return Err(invalid("VMDK stream metadata overlaps its header"));
            }
            area.start -= SECTOR;
            let mut marker = [0; 16];
            read_at(file, area.start, &mut marker)?;
            if u64le(&marker, 0) != len.div_ceil(SECTOR)
                || le32(&marker, 8) != 0
                || le32(&marker, 12) != marker_kind
            {
                return Err(invalid("VMDK metadata marker type or length"));
            }
        }
        self.regions.push(area);
        let mut bytes = vec![0; len as usize];
        read_at(file, at, &mut bytes)?;
        Ok(bytes)
    }
    fn grain(&mut self, file: &mut File, sector: u32, index: usize, h: &Header) -> Result<Grain> {
        if sector == 0 || sector == 1 && h.flags & ZERO_GRAIN != 0 {
            return Ok(Grain::default());
        }
        let at = u64::from(sector) * SECTOR;
        if at < h.overhead {
            return Err(invalid("VMDK grain overlaps metadata"));
        }
        if !h.compressed {
            self.regions.push(region(at, h.grain_size, self.length)?);
            return Ok(Grain { sector, encoded: 0 });
        }
        let mut marker = [0; 12];
        read_at(file, at, &mut marker)?;
        let encoded = le32(&marker, 8);
        if u64le(&marker, 0) != index as u64 * (h.grain_size / SECTOR)
            || encoded == 0
            || u64::from(encoded) > 2 * h.grain_size + 1024
        {
            return Err(invalid("VMDK compressed grain address or length"));
        }
        self.regions.push(region(
            at,
            (12 + u64::from(encoded)).next_multiple_of(SECTOR),
            h.footer.unwrap_or(self.length),
        )?);
        Ok(Grain { sector, encoded })
    }
    fn read_grains(
        &mut self,
        file: &mut File,
        h: &Header,
        cancel: &AtomicBool,
    ) -> Result<Vec<Grain>> {
        let count = h.capacity.div_ceil(h.grain_size) as usize;
        let table_count = count.div_ceil(h.entries);
        let main = self.table(file, h.directory, table_count, h, 2)?;
        let backup = if h.flags & REDUNDANT != 0 {
            Some(self.table(file, h.backup, table_count, h, 2)?)
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
                result.resize(result.len() + used, Grain::default());
                continue;
            }
            let table = self.table(file, at, h.entries, h, 1)?;
            if let Some(b) = other
                && self.table(file, b, h.entries, h, 1)? != table
            {
                return Err(invalid("VMDK grain table copies disagree"));
            }
            for entry in table[..used * 4].as_chunks::<4>().0 {
                let grain = self.grain(file, u32::from_le_bytes(*entry), result.len(), h)?;
                result.push(grain);
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
    grains: Vec<Grain>,
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
        let grain = self.grains[(offset / self.grain_size) as usize];
        let within = offset % self.grain_size;
        let at = u64::from(grain.sector) * SECTOR;
        let block = if grain.sector == 0 {
            Block::Zero
        } else if grain.encoded != 0 {
            Block::Compressed {
                at: at + 12,
                bytes: grain.encoded as usize,
                codec: super::Compression::VmdkZlib,
            }
        } else {
            Block::Data(at + within)
        };
        (block, self.grain_size - within)
    }
}
