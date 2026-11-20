//! Read allocation status from a clean HFS Plus or HFSX volume's allocation file.
use crate::{AllocationStatus, Error, Result, source::Image};
use std::ops::Range;
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_BITMAP: usize = 32 * 1024 * 1024;
const HEADER_SIZE: usize = 512;
const UNMOUNTED: u32 = 1 << 8;
const INCONSISTENT: u32 = (1 << 11) | (1 << 14);
const JOURNALED: u32 = 1 << 13;

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("HFS allocation map: {reason}"))
}
fn be32(s: &[u8], pos: usize) -> u32 {
    u32::from_be_bytes(s[pos..pos + 4].try_into().unwrap())
}
fn signature(s: &[u8]) -> bool {
    matches!(s, b"H+\0\x04" | b"HX\0\x05")
}
pub(super) fn recognized(image: &mut Image) -> bool {
    image.length >= 4096 && image.read(1024, 4).is_ok_and(|s| signature(&s))
}

struct Fork {
    size: u64,
    runs: Vec<Range<u32>>,
}
impl Fork {
    fn open(
        header: &[u8],
        position: usize,
        total: u32,
        block_size: u64,
        complete: bool,
    ) -> Result<Self> {
        let fork = &header[position..position + 80];
        let size = u64::from_be_bytes(fork[..8].try_into().unwrap());
        let blocks = be32(fork, 12);
        if blocks > total || size > u64::from(blocks) * block_size {
            return Err(invalid("special file geometry"));
        }
        let mut allocated = 0u32;
        let mut runs: Vec<Range<u32>> = Vec::new();
        let mut finished = false;
        for pos in (16..80).step_by(8) {
            let start = be32(fork, pos);
            let n = be32(fork, pos + 4);
            if n == 0 {
                if start != 0 {
                    return Err(invalid("empty extent has an offset"));
                }
                finished = true;
                continue;
            }
            let end = start
                .checked_add(n)
                .filter(|&v| v <= total)
                .ok_or_else(|| invalid("extent outside volume"))?;
            if finished || runs.iter().any(|r| r.start < end && start < r.end) {
                return Err(invalid("ambiguous special-file extents"));
            }
            allocated = allocated
                .checked_add(n)
                .ok_or_else(|| invalid("extent count overflow"))?;
            runs.push(start..end);
        }
        if allocated > blocks {
            return Err(invalid("extent counts exceed fork allocation"));
        }
        if complete && allocated != blocks {
            return Err(Error::Unsupported(
                "HFS allocation file needs the extents overflow B-tree".into(),
            ));
        }
        Ok(Self { size, runs })
    }
}

// JournalInfoBlock uses big-endian fields. The journal header records its writer's
// byte order and checksums the first 44 on-disk bytes with its checksum field zeroed.
fn journal_ranges(
    image: &mut Image,
    info_block: u32,
    block: u64,
    volume_bytes: u64,
) -> Result<[Range<u32>; 2]> {
    let info_at = u64::from(info_block) * block;
    if info_block == 0 || info_at >= volume_bytes {
        return Err(invalid("journal info block outside volume"));
    }
    let info = image.read(info_at, 52)?;
    if be32(&info, 0) != 1 {
        return Err(Error::Unsupported(
            "HFS journal is external, uninitialized or has unknown flags".into(),
        ));
    }
    let at = u64::from_be_bytes(info[36..44].try_into().unwrap());
    let size = u64::from_be_bytes(info[44..52].try_into().unwrap());
    let end = at
        .checked_add(size)
        .filter(|&n| n <= volume_bytes)
        .ok_or_else(|| invalid("journal outside volume"))?;
    if !(256 * 1024..=1 << 30).contains(&size) || !at.is_multiple_of(block) {
        return Err(invalid("journal extent geometry"));
    }

    let mut header = image.read(at, 44)?;
    let little = match &header[..8] {
        b"xLNJ\x78\x56\x34\x12" => true,
        b"JNLx\x12\x34\x56\x78" => false,
        _ => return Err(invalid("journal signature or byte order")),
    };
    let read32 = |bytes: &[u8]| {
        let bytes = bytes.try_into().unwrap();
        if little {
            u32::from_le_bytes(bytes)
        } else {
            u32::from_be_bytes(bytes)
        }
    };
    let read64 = |bytes: &[u8]| {
        let bytes = bytes.try_into().unwrap();
        if little {
            u64::from_le_bytes(bytes)
        } else {
            u64::from_be_bytes(bytes)
        }
    };
    let stored = read32(&header[36..40]);
    header[36..40].fill(0);
    let checksum = !header.iter().fold(0u32, |sum, &byte| {
        (sum << 8) ^ sum.wrapping_add(u32::from(byte))
    });
    if checksum != stored {
        return Err(invalid("journal header checksum"));
    }
    let start = read64(&header[8..16]);
    if start != read64(&header[16..24]) {
        return Err(Error::Unsupported("HFS journal requires replay".into()));
    }
    let sector = u64::from(read32(&header[40..44]));
    let list = u64::from(read32(&header[32..36]));
    if read64(&header[24..32]) != size
        || !(512..=65536).contains(&sector)
        || !sector.is_power_of_two()
        || !size.is_multiple_of(sector)
        || !(sector..=size).contains(&start)
        || !start.is_multiple_of(sector)
        || !(sector..size).contains(&list)
        || !list.is_multiple_of(sector)
    {
        return Err(invalid("journal header geometry"));
    }
    Ok([
        info_block..info_block + 1,
        (at / block) as u32..end.div_ceil(block) as u32,
    ])
}

pub(super) struct Map {
    bits: Vec<u8>,
    block_size: u64,
    volume_bytes: u64,
}
impl Map {
    pub fn open(image: &mut Image, cancel: &AtomicBool) -> Result<Self> {
        let header = image.read(1024, HEADER_SIZE)?;
        if !signature(&header[..4]) {
            return Err(invalid("volume signature or version"));
        }
        let attrs = be32(&header, 4);
        if attrs & UNMOUNTED == 0 || attrs & INCONSISTENT != 0 {
            return Err(Error::Unsupported(
                "HFS volume is dirty or inconsistent".into(),
            ));
        }
        let block_size = u64::from(be32(&header, 40));
        let total = be32(&header, 44);
        let free = be32(&header, 48);
        if !(512..=1 << 30).contains(&block_size)
            || !block_size.is_power_of_two()
            || total < 4
            || free > total
        {
            return Err(invalid("volume geometry"));
        }
        let volume_bytes = u64::from(total) * block_size;
        if volume_bytes > image.length
            || image.length - volume_bytes >= block_size
            || volume_bytes < 4096
        {
            return Err(invalid("partition length disagrees with volume size"));
        }
        let alt = image.read(image.length - 1024, HEADER_SIZE)?;
        // The alternate's counters may be older than the primary's. Its layout and allocation fork must agree.
        if alt[..4] != header[..4]
            || (be32(&alt, 4) ^ attrs) & JOURNALED != 0
            || alt[12..16] != header[12..16]
            || alt[40..48] != header[40..48]
            || alt[112..192] != header[112..192]
        {
            return Err(invalid(
                "primary and alternate allocation file or geometry disagree",
            ));
        }
        let allocation = Fork::open(&header, 112, total, block_size, true)?;
        let count = total.div_ceil(8) as usize;
        if count > MAX_BITMAP {
            return Err(Error::Limit("HFS allocation bitmap exceeds 32 MiB".into()));
        }
        if allocation.size < count as u64 {
            return Err(invalid("allocation file is too short"));
        }
        let mut bits = Vec::with_capacity(count);
        for r in &allocation.runs {
            let mut offset = u64::from(r.start) * block_size;
            let mut length =
                (u64::from(r.end - r.start) * block_size).min((count - bits.len()) as u64);
            while length != 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let n = length.min(65536) as usize;
                bits.extend_from_slice(&image.read(offset, n)?);
                offset += n as u64;
                length -= n as u64;
            }
        }
        if bits.len() != count {
            return Err(invalid("allocation extents do not cover bitmap"));
        }
        let map = Self {
            bits,
            block_size,
            volume_bytes,
        };
        let allocated: u64 = map.bits[..count - 1]
            .chunks(65536)
            .try_fold(0u64, |sum, chunk| {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                Ok(sum + chunk.iter().map(|v| u64::from(v.count_ones())).sum::<u64>())
            })?;
        let unused = (8 - total % 8) % 8;
        let last = map.bits[count - 1] & (0xffu8 << unused);
        if allocated + u64::from(last.count_ones()) != u64::from(total - free) {
            return Err(invalid("free-block count disagrees with bitmap"));
        }
        // Keep the allocation file, metadata forks and both headers out of verified free space.
        let reserved_end = 1536u64.div_ceil(block_size) as u32;
        let alt_block = ((image.length - 1024) / block_size) as u32;
        let mut ranges = allocation.runs;
        ranges.push(0..reserved_end);
        if alt_block < total {
            ranges.push(alt_block..total);
        }
        for pos in [192, 272, 352, 432] {
            let fork = Fork::open(&header, pos, total, block_size, false)?;
            if matches!(pos, 192 | 272) && (fork.size == 0 || fork.runs.is_empty()) {
                return Err(invalid("missing extents or catalog B-tree"));
            }
            ranges.extend(fork.runs);
        }
        if attrs & JOURNALED != 0 {
            ranges.extend(journal_ranges(
                image,
                be32(&header, 12),
                block_size,
                volume_bytes,
            )?);
        }
        ranges.sort_unstable_by_key(|r| r.start);
        if ranges.windows(2).any(|w| w[0].end > w[1].start) {
            return Err(invalid("overlapping metadata files"));
        }
        for r in ranges {
            map.require_allocated(r)?;
        }
        Ok(map)
    }
    fn require_allocated(&self, r: Range<u32>) -> Result<()> {
        if self.range_has(r, false) {
            return Err(invalid("metadata block marked free"));
        }
        Ok(())
    }
    // MSB first within a byte. Inspect whole interior bytes without iterating over individual blocks.
    fn range_has(&self, r: Range<u32>, want_set: bool) -> bool {
        if r.is_empty() {
            return false;
        }
        let first = (r.start / 8) as usize;
        let last = ((r.end - 1) / 8) as usize;
        let left = 0xffu8 >> (r.start % 8);
        let right = 0xffu8 << (7 - (r.end - 1) % 8);
        let check = |bits: u8, mask: u8| (if want_set { bits } else { !bits }) & mask != 0;
        if first == last {
            return check(self.bits[first], left & right);
        }
        check(self.bits[first], left)
            || check(self.bits[last], right)
            || self.bits[first + 1..last].iter().any(|&v| check(v, 0xff))
    }
    pub fn status(&self, at: u64, length: u64) -> AllocationStatus {
        let Some(end) = at.checked_add(length) else {
            return AllocationStatus::Unknown;
        };
        let start = at.min(self.volume_bytes) / self.block_size;
        let stop = end.min(self.volume_bytes).div_ceil(self.block_size);
        if self.range_has(start as u32..stop as u32, true) {
            AllocationStatus::Conflict
        } else if end > self.volume_bytes || length == 0 {
            AllocationStatus::Unknown
        } else {
            AllocationStatus::Unallocated
        }
    }
}
