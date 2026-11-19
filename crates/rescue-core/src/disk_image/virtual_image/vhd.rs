//! Standalone classic Microsoft VHD, with 512-byte, MSB-first sector allocation maps.
use super::{Block, Error, File, Result, invalid, read_at};
use std::sync::atomic::{AtomicBool, Ordering};

const SECTOR: u64 = 512;

fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}

fn be64(bytes: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(bytes[at..at + 8].try_into().unwrap())
}

fn checksum(bytes: &[u8], at: usize) -> bool {
    let sum: u32 = bytes[..at]
        .iter()
        .chain(&bytes[at + 4..])
        .map(|&v| u32::from(v))
        .sum();
    be32(bytes, at) == !sum
}

fn footer_valid(bytes: &[u8; 512]) -> bool {
    &bytes[..8] == b"conectix"
        && checksum(bytes, 64)
        && matches!(be32(bytes, 8), 2 | 3)
        && be32(bytes, 12) == 0x10000
}

fn checked_region(at: u64, length: u64, limit: u64) -> Result<std::ops::Range<u64>> {
    let end = at
        .checked_add(length)
        .filter(|&v| v <= limit)
        .ok_or_else(|| invalid("VHD extent exceeds its container"))?;
    if !at.is_multiple_of(SECTOR) {
        return Err(invalid("unaligned VHD extent"));
    }
    Ok(at..end)
}

pub(super) struct Vhd {
    pub size: u64,
    allocation: Option<Allocation>,
}

struct Allocation {
    block_size: u64,
    table: Vec<u8>,
    bitmap: Vec<u8>,
    cached: Option<u64>,
}

impl Vhd {
    pub(super) fn open(source: &mut File, length: u64, cancel: &AtomicBool) -> Result<Self> {
        if length < SECTOR || !length.is_multiple_of(SECTOR) {
            return Err(invalid("truncated VHD footer"));
        }
        let end = length - SECTOR;
        let mut footer = [0; 512];
        read_at(source, end, &mut footer)?;
        if !footer_valid(&footer) {
            return Err(invalid(
                "VHD footer signature, version, features or checksum",
            ));
        }
        let size = be64(&footer, 48);
        if size == 0 || size > 2u64 << 40 || !size.is_multiple_of(SECTOR) {
            return Err(invalid("VHD logical size"));
        }
        let header_at = be64(&footer, 16);
        let allocation = match be32(&footer, 60) {
            2 if header_at == u64::MAX && size == end => None,
            2 => return Err(invalid("fixed VHD size or header offset")),
            3 => {
                let mut backup = [0; 512];
                read_at(source, 0, &mut backup)?;
                if !footer_valid(&backup)
                    || backup[16..24] != footer[16..24]
                    || backup[48..56] != footer[48..56]
                    || backup[60..64] != footer[60..64]
                    || backup[68..84] != footer[68..84]
                {
                    return Err(invalid("dynamic VHD footer copies disagree"));
                }
                Some(Allocation::open(source, header_at, end, size, cancel)?)
            }
            _ => {
                return Err(Error::Unsupported(
                    "VHD requires a standalone fixed or dynamic image".into(),
                ));
            }
        };
        Ok(Self { size, allocation })
    }

    pub(super) fn block(&mut self, source: &mut File, offset: u64) -> Result<(Block, u64)> {
        match &mut self.allocation {
            Some(map) => map.block(source, offset),
            None => Ok((Block::Data(offset), self.size - offset)),
        }
    }
}

impl Allocation {
    fn open(
        source: &mut File,
        header_at: u64,
        end: u64,
        size: u64,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let header_region = checked_region(header_at, 1024, end)?;
        let mut header = [0; 1024];
        read_at(source, header_at, &mut header)?;
        if &header[..8] != b"cxsparse"
            || be64(&header, 8) != u64::MAX
            || be32(&header, 24) != 0x10000
            || !checksum(&header, 36)
        {
            return Err(invalid("dynamic VHD header signature, version or checksum"));
        }
        let count = u64::from(be32(&header, 28));
        let block_size = u64::from(be32(&header, 32));
        if !(SECTOR..=64 * 1024 * 1024).contains(&block_size)
            || !block_size.is_power_of_two()
            || count < size.div_ceil(block_size)
            || count > 16 * 1024 * 1024
        {
            return Err(invalid("dynamic VHD allocation table or block size"));
        }
        let table_region =
            checked_region(be64(&header, 16), (count * 4).next_multiple_of(SECTOR), end)?;
        let bitmap_size = (block_size / SECTOR).div_ceil(8).next_multiple_of(SECTOR);
        let mut table = vec![0; (count * 4) as usize];
        read_at(source, table_region.start, &mut table)?;
        let mut regions = vec![0..SECTOR, header_region, table_region];
        for (index, &entry) in table.as_chunks::<4>().0.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let slot = u32::from_be_bytes(entry);
            if slot == u32::MAX {
                continue;
            }
            if index as u64 >= size.div_ceil(block_size) {
                return Err(invalid("VHD allocation beyond guest capacity"));
            }
            regions.push(checked_region(
                u64::from(slot) * SECTOR,
                bitmap_size + block_size,
                end,
            )?);
        }
        regions.sort_unstable_by_key(|r| r.start);
        if regions.windows(2).any(|pair| pair[1].start < pair[0].end) {
            return Err(invalid("overlapping VHD metadata or data blocks"));
        }
        Ok(Self {
            block_size,
            table,
            bitmap: vec![0; bitmap_size as usize],
            cached: None,
        })
    }

    fn sector_present(&self, sector: u64) -> bool {
        self.bitmap[(sector / 8) as usize] & (0x80 >> (sector % 8)) != 0
    }

    fn block(&mut self, source: &mut File, offset: u64) -> Result<(Block, u64)> {
        let index = offset / self.block_size;
        let within = offset % self.block_size;
        let slot = be32(&self.table, index as usize * 4);
        if slot == u32::MAX {
            return Ok((Block::Zero, self.block_size - within));
        }
        let at = u64::from(slot) * SECTOR;
        if self.cached != Some(index) {
            read_at(source, at, &mut self.bitmap)?;
            self.cached = Some(index);
        }
        let sector = within / SECTOR;
        let present = self.sector_present(sector);
        let mut next = sector + 1;
        while next < self.block_size / SECTOR && self.sector_present(next) == present {
            next += 1;
        }
        let block = if present {
            Block::Data(at + self.bitmap.len() as u64 + within)
        } else {
            Block::Zero
        };
        Ok((block, (next - sector) * SECTOR))
    }
}
