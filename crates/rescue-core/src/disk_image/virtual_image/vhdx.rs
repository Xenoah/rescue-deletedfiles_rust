//! Read-only VHDX. Stored locator paths are never opened and active logs are rejected.
mod backing;
use super::{Block, Error, File, Result, invalid, le32, read_at};
use crate::crc32c::update;
pub(in crate::disk_image) use backing::import_vhdx_chain;
use std::{
    collections::HashSet,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

const MIB: u64 = 1 << 20;
const TABLE: usize = 64 << 10;
const MAX_BAT_ENTRIES: u64 = 1 << 20;
const BAT: u128 = 0x2dc27766_f623_4200_9d64_115e9bfd4a08;
const METADATA: u128 = 0x8b7ca206_4790_4b9a_b8fe_575f050f886e;
const PARAMETERS: u128 = 0xcaa16737_fa36_4d43_b3b6_33f0aa44e76b;
const SIZE: u128 = 0x2fa54224_cd1b_4876_b211_5dbed83bf4b8;
const DISK_ID: u128 = 0xbeca12ab_b2e6_4523_93ef_c309e000c746;
const LOGICAL: u128 = 0x8141bf1d_a96f_4709_ba47_f233a8faab5f;
const PHYSICAL: u128 = 0xcda348c7_445d_4471_9cc9_e9885251c556;
const PARENT: u128 = 0xa8d35f2d_b30b_454d_abf7_d3d84834ab0c;

fn le16(data: &[u8], pos: usize) -> u16 {
    u16::from_le_bytes(data[pos..pos + 2].try_into().unwrap())
}
fn le64(data: &[u8], pos: usize) -> u64 {
    u64::from_le_bytes(data[pos..pos + 8].try_into().unwrap())
}
fn guid(bytes: &[u8]) -> u128 {
    Uuid::from_bytes_le(bytes[..16].try_into().unwrap()).as_u128()
}

fn extent(start: u64, len: u64, limit: u64) -> Result<Range<u64>> {
    let end = start
        .checked_add(len)
        .filter(|&end| end <= limit && len > 0)
        .ok_or_else(|| invalid("VHDX region is empty, truncated or outside the file"))?;
    Ok(start..end)
}
fn disjoint(ranges: &mut [Range<u64>]) -> Result<()> {
    ranges.sort_unstable_by_key(|r| r.start);
    if ranges.windows(2).any(|r| r[0].end > r[1].start) {
        return Err(invalid("overlapping VHDX storage regions"));
    }
    Ok(())
}

fn read_copy(file: &mut File, offset: u64, size: usize, magic: &[u8]) -> Result<Option<Vec<u8>>> {
    let mut data = vec![0; size];
    read_at(file, offset, &mut data)?;
    let expected = le32(&data, 4);
    data[4..8].fill(0);
    Ok((data.starts_with(magic) && !update(u32::MAX, &data) == expected).then_some(data))
}
fn active_header(file: &mut File, length: u64) -> Result<(Range<u64>, u128)> {
    let a = read_copy(file, TABLE as u64, 4096, b"head")?;
    let b = read_copy(file, 2 * TABLE as u64, 4096, b"head")?;
    let header = match (a, b) {
        (Some(a), Some(b)) if le64(&a, 8) > le64(&b, 8) || a == b => a,
        (Some(a), Some(b)) if le64(&a, 8) < le64(&b, 8) => b,
        (Some(h), None) | (None, Some(h)) => h,
        _ => {
            return Err(invalid(
                "VHDX headers are both damaged or have conflicting sequence numbers",
            ));
        }
    };
    if le16(&header, 66) != 1 || le16(&header, 64) != 0 || header[48..64] != [0; 16] {
        return Err(Error::Unsupported(
            "VHDX requires a clean v1 header; active log replay is unsupported".into(),
        ));
    }
    let size = u64::from(le32(&header, 68));
    let start = le64(&header, 72);
    if !size.is_multiple_of(MIB) || !start.is_multiple_of(MIB) {
        return Err(invalid("unaligned VHDX log"));
    }
    Ok((extent(start, size, length)?, guid(&header[32..48])))
}

struct Regions {
    write_id: u128,
    bat: Range<u64>,
    metadata: Range<u64>,
    reserved: Vec<Range<u64>>,
}
impl Regions {
    fn open(file: &mut File, length: u64) -> Result<Self> {
        let (log, write_id) = active_header(file, length)?;
        let a = read_copy(file, 3 * TABLE as u64, TABLE, b"regi")?;
        let b = read_copy(file, 4 * TABLE as u64, TABLE, b"regi")?;
        let table = match (a, b) {
            (Some(a), Some(b)) if a == b => a,
            (Some(t), None) | (None, Some(t)) => t,
            _ => return Err(invalid("VHDX region table copies are damaged or disagree")),
        };
        let count = le32(&table, 8) as usize;
        if !(2..=2047).contains(&count) {
            return Err(invalid("VHDX region table count"));
        }
        let mut reserved = vec![0..MIB, log];
        let (mut bat, mut metadata) = (None, None);
        let mut seen = HashSet::new();
        for entry in table[16..16 + 32 * count].as_chunks::<32>().0 {
            let id = guid(entry);
            let at = le64(entry, 16);
            let size = u64::from(le32(entry, 24));
            let flags = le32(entry, 28);
            if !seen.insert(id) || !at.is_multiple_of(MIB) || !size.is_multiple_of(MIB) || flags > 1
            {
                return Err(invalid("VHDX duplicate, unaligned or invalid region"));
            }
            let range = extent(at, size, length)?;
            match id {
                BAT => bat = Some(range.clone()),
                METADATA => metadata = Some(range.clone()),
                _ if flags == 1 => {
                    return Err(Error::Unsupported("unknown required VHDX region".into()));
                }
                _ => (),
            }
            reserved.push(range);
        }
        disjoint(&mut reserved)?;
        Ok(Self {
            write_id,
            bat: bat.ok_or_else(|| invalid("missing VHDX BAT region"))?,
            metadata: metadata.ok_or_else(|| invalid("missing VHDX metadata region"))?,
            reserved,
        })
    }
}

struct Geometry {
    size: u64,
    block: u64,
    logical: u64,
    fixed: bool,
    page83: u128,
    parent: Option<backing::Parent>,
}
impl Geometry {
    fn read(file: &mut File, region: &Range<u64>) -> Result<Self> {
        let mut table = vec![0; TABLE];
        read_at(file, region.start, &mut table)?;
        let count = usize::from(le16(&table, 10));
        if &table[..8] != b"metadata" || !(5..=2047).contains(&count) {
            return Err(invalid("VHDX metadata signature or entry count"));
        }
        let mut ranges = Vec::with_capacity(count + 1);
        ranges.push(0..TABLE as u64);
        let mut seen = HashSet::new();
        let mut page83 = None;
        let mut parent = None;
        let (mut params, mut size, mut logical, mut physical) = (None, None, None, None);
        for entry in table[32..32 + count * 32].as_chunks::<32>().0 {
            let id = guid(entry);
            let offset = u64::from(le32(entry, 16));
            let len = u64::from(le32(entry, 20));
            let flags = le32(entry, 24);
            if !seen.insert(id) || flags & !7 != 0 {
                return Err(invalid("VHDX repeated metadata item or reserved flags"));
            }
            if len != 0 {
                ranges.push(extent(offset, len, region.end - region.start)?);
            }
            let (wanted, expected_flags) = match id {
                PARAMETERS => (8, 4),
                SIZE => (8, 6),
                LOGICAL | PHYSICAL => (4, 6),
                DISK_ID => (16, 6),
                PARENT => {
                    if flags != 4 || !(20..=65536).contains(&len) {
                        return Err(invalid("VHDX parent locator size or metadata flags"));
                    }
                    let mut data = vec![0; len as usize];
                    read_at(file, region.start + offset, &mut data)?;
                    parent = Some(backing::Parent::parse(&data)?);
                    continue;
                }
                _ if flags & 4 != 0 => {
                    return Err(Error::Unsupported(
                        "unknown required VHDX metadata item".into(),
                    ));
                }
                _ => continue,
            };
            if len != wanted || flags != expected_flags {
                return Err(invalid("VHDX system metadata length or flags"));
            }
            let mut bytes = [0; 16];
            read_at(file, region.start + offset, &mut bytes[..len as usize])?;
            match id {
                PARAMETERS => params = Some((u64::from(le32(&bytes, 0)), le32(&bytes, 4))),
                SIZE => size = Some(le64(&bytes, 0)),
                LOGICAL => logical = Some(u64::from(le32(&bytes, 0))),
                DISK_ID => page83 = Some(guid(&bytes)),
                PHYSICAL => physical = Some(le32(&bytes, 0)),
                _ => (),
            }
        }
        disjoint(&mut ranges)?;
        let (Some((block, flags)), Some(size), Some(logical), Some(physical)) =
            (params, size, logical, physical)
        else {
            return Err(invalid("missing VHDX geometry"));
        };
        if flags > 2
            || (flags == 2) != parent.is_some()
            || !(MIB..=256 * MIB).contains(&block)
            || !block.is_power_of_two()
            || !matches!(logical, 512 | 4096)
            || !matches!(physical, 512 | 4096)
            || size == 0
            || size > 64u64 << 40
            || !size.is_multiple_of(logical)
        {
            return Err(invalid("VHDX block, sector or virtual disk geometry"));
        }
        Ok(Self {
            size,
            block,
            logical,
            fixed: flags == 1,
            page83: page83.ok_or_else(|| invalid("missing VHDX disk identifier"))?,
            parent,
        })
    }
}

pub(super) struct Vhdx {
    pub size: u64,
    block_size: u64,
    logical: u64,
    write_id: u128,
    page83: u128,
    parent: Option<backing::Parent>,
    payload: Vec<u64>,
    bitmaps: Vec<u64>,
    cached_at: u64,
    bitmap: Vec<u8>,
}

impl Vhdx {
    pub fn open(file: &mut File, length: u64, cancel: &AtomicBool) -> Result<Self> {
        Self::open_member(file, length, cancel, false)
    }

    fn open_member(file: &mut File, length: u64, cancel: &AtomicBool, chain: bool) -> Result<Self> {
        let mut magic = [0; 8];
        read_at(file, 0, &mut magic)?;
        if &magic != b"vhdxfile" {
            return Err(invalid("VHDX file signature"));
        }
        let regions = Regions::open(file, length)?;
        let disk = Geometry::read(file, &regions.metadata)?;
        let child = disk.parent.is_some();
        if child && !chain {
            return Err(Error::Unsupported(
                "select all parent files for the differencing VHDX".into(),
            ));
        }
        let count = disk.size.div_ceil(disk.block);
        // Each bitmap covers 2^23 logical sectors. In a differencing disk its slot
        // follows a *full* chunk of BAT entries, even in the final partial chunk.
        let chunk = (1u64 << 23) * disk.logical / disk.block;
        let chunks = count.div_ceil(chunk);
        let entries = if child {
            chunks * (chunk + 1)
        } else {
            count + (count - 1) / chunk
        };
        if entries > MAX_BAT_ENTRIES || entries * 8 > regions.bat.end - regions.bat.start {
            return Err(invalid("VHDX BAT length or one-million-entry limit"));
        }
        let mut data = vec![0; entries as usize * 8];
        read_at(file, regions.bat.start, &mut data)?;
        let mut ranges = regions.reserved;
        let mut payload = Vec::with_capacity(count as usize);
        let mut bitmaps = Vec::with_capacity(chunks as usize);
        let mut needs_bitmap = false;
        for (index, entry) in data.as_chunks::<8>().0.iter().enumerate() {
            if index % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let value = u64::from_le_bytes(*entry);
            let state = value & 7;
            let at = value & !(MIB - 1);
            if value & 0x000f_fff8 != 0 {
                return Err(invalid("VHDX reserved BAT bits"));
            }
            if index as u64 % (chunk + 1) == chunk {
                match (state, at) {
                    (0, 0) if !needs_bitmap => {}
                    (6, at) if child && at != 0 => ranges.push(extent(at, MIB, length)?),
                    _ => return Err(invalid("missing or inconsistent VHDX sector bitmap")),
                }
                bitmaps.push(at);
                needs_bitmap = false;
                continue;
            }
            if payload.len() == count as usize {
                if value != 0 {
                    return Err(invalid("VHDX allocated BAT padding beyond virtual size"));
                }
                continue;
            }
            match state {
                0 if at == 0 && !disk.fixed => {}
                2 => {
                    if at != 0 {
                        ranges.push(extent(at, disk.block, length)?);
                    }
                }
                6 | 7 if at != 0 && (state == 6 || child) => {
                    ranges.push(extent(at, disk.block, length)?);
                    needs_bitmap |= state == 7;
                }
                _ => {
                    return Err(Error::Unsupported(
                        "undefined, discarded or invalid VHDX payload block".into(),
                    ));
                }
            }
            payload.push(value);
        }
        disjoint(&mut ranges)?;
        Ok(Self {
            size: disk.size,
            block_size: disk.block,
            logical: disk.logical,
            write_id: regions.write_id,
            page83: disk.page83,
            parent: disk.parent,
            payload,
            bitmaps,
            cached_at: 0,
            bitmap: Vec::new(),
        })
    }

    // Standalone opening only admits fully present or zero-filled blocks.
    pub fn block(&self, offset: u64) -> (Block, u64) {
        let relative = offset % self.block_size;
        let entry = self.payload[(offset / self.block_size) as usize];
        let block = if entry & 7 == 6 {
            Block::Data((entry & !(MIB - 1)) + relative)
        } else {
            Block::Zero
        };
        (block, self.block_size - relative)
    }

    fn layer_span(
        &mut self,
        file: &mut File,
        offset: u64,
        limit: &mut usize,
    ) -> Result<Option<Block>> {
        let relative = offset % self.block_size;
        *limit = (*limit as u64).min(self.block_size - relative) as usize;
        let entry = self.payload[(offset / self.block_size) as usize];
        let at = (entry & !(MIB - 1)) + relative;
        match entry & 7 {
            0 if self.parent.is_some() => Ok(None),
            0 | 2 => Ok(Some(Block::Zero)),
            6 => Ok(Some(Block::Data(at))),
            7 => {
                let sector = offset / self.logical;
                let bitmap_at = self.bitmaps[(sector >> 23) as usize];
                if self.cached_at != bitmap_at {
                    self.bitmap.resize(MIB as usize, 0);
                    read_at(file, bitmap_at, &mut self.bitmap)?;
                    self.cached_at = bitmap_at;
                }
                let bit = (sector % (1 << 23)) as usize;
                let present = |n: usize| self.bitmap[n / 8] & (1 << (n % 8)) != 0;
                let local = present(bit);
                let mut span = self.logical - offset % self.logical;
                let mut next = bit + 1;
                while span < *limit as u64 && present(next) == local {
                    span += self.logical;
                    next += 1;
                }
                *limit = (*limit as u64).min(span) as usize;
                Ok(local.then_some(Block::Data(at)))
            }
            _ => Err(invalid("invalid VHDX payload state")),
        }
    }
}
