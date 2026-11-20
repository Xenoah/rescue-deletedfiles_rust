//! Free data extents in a clean, single-device XFS v5 filesystem.
mod log;
mod tree;

use crate::{AllocationStatus, Error, Image, Result, crc32c::update};
use std::{
    collections::BTreeSet,
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};
use tree::{Group, MAX_RECORDS};

fn invalid(why: &str) -> Error {
    Error::Invalid(format!("XFS allocation map: {why}"))
}
fn be16(b: &[u8], i: usize) -> u16 {
    u16::from_be_bytes(b[i..i + 2].try_into().unwrap())
}
fn be32(b: &[u8], i: usize) -> u32 {
    u32::from_be_bytes(b[i..i + 4].try_into().unwrap())
}
fn be64(b: &[u8], i: usize) -> u64 {
    u64::from_be_bytes(b[i..i + 8].try_into().unwrap())
}
fn checksum(data: &[u8], pos: usize) -> u32 {
    update(
        update(update(u32::MAX, &data[..pos]), &[0; 4]),
        &data[pos + 4..],
    )
}
fn checked_crc(data: &[u8], pos: usize) -> bool {
    !checksum(data, pos) == u32::from_le_bytes(data[pos..pos + 4].try_into().unwrap())
}
pub(super) fn recognized(image: &mut Image) -> bool {
    image.read(0, 4).is_ok_and(|s| s == b"XFSB")
}

struct Geometry {
    block: u64,
    blocks: u64,
    sector: usize,
    ag_blocks: u32,
    groups: u32,
    uuid: [u8; 16],
    log: Range<u64>,
    free: u64,
}

impl Geometry {
    fn open(sb: &[u8], length: u64) -> Result<Self> {
        let block = u64::from(be32(sb, 4));
        let blocks = be64(sb, 8);
        let sector = usize::from(be16(sb, 102));
        let ag_blocks = be32(sb, 84);
        let groups = be32(sb, 88);
        if &sb[..4] != b"XFSB"
            || be16(sb, 100) & 0x40f != 0x405
            || sb[126] != 0
            || be64(sb, 16) != 0
            || be64(sb, 24) != 0
            || be32(sb, 208) != 0
            || be32(sb, 212) & !15 != 0
            || be32(sb, 216) & !0x2b != 0
            || be32(sb, 220) != 0
        {
            return Err(Error::Unsupported(
                "XFS requires v5, an internal v2 log, and supported single-device features".into(),
            ));
        }
        if !(512..=65536).contains(&block)
            || !block.is_power_of_two()
            || !(512..=32768).contains(&sector)
            || !sector.is_power_of_two()
            || sector as u64 > block
            || sector != sb.len()
            || sb[120] != block.ilog2() as u8
            || sb[121] != sector.ilog2() as u8
            || !checked_crc(sb, 224)
            || blocks == 0
            || blocks > length / block
            || ag_blocks < 8
            || u32::BITS - (ag_blocks - 1).leading_zeros() != u32::from(sb[124])
            || !(1..=32768).contains(&groups)
            || blocks.div_ceil(u64::from(ag_blocks)) != u64::from(groups)
        {
            return Err(invalid("superblock geometry or checksum"));
        }
        let log_fsb = be64(sb, 48);
        let mask = (1u64 << sb[124]) - 1;
        let ag = log_fsb >> sb[124];
        let local = log_fsb & mask;
        let log_blocks = u64::from(be32(sb, 96));
        if log_fsb == 0
            || ag >= u64::from(groups)
            || log_blocks == 0
            || local + log_blocks > u64::from(ag_blocks)
        {
            return Err(invalid("external or invalid log extent"));
        }
        let log = ag * u64::from(ag_blocks) + local;
        if log + log_blocks > blocks {
            return Err(invalid("journal outside volume"));
        }
        Ok(Self {
            block,
            blocks,
            sector,
            ag_blocks,
            groups,
            uuid: sb[32..48].try_into().unwrap(),
            log: log..log + log_blocks,
            free: be64(sb, 144),
        })
    }
}

pub(super) struct Map {
    free: Vec<Range<u64>>,
    block: u64,
    bytes: u64,
}

impl Map {
    pub fn open(image: &mut Image, cancel: &AtomicBool) -> Result<Self> {
        let prefix = image.read(0, 512)?;
        let sector = usize::from(be16(&prefix, 102));
        if !(512..=32768).contains(&sector) || !sector.is_power_of_two() {
            return Err(invalid("sector size"));
        }
        let sb = image.read(0, sector)?;
        let g = Geometry::open(&sb, image.length)?;
        let lsn = log::clean_lsn(
            image,
            g.log.start * g.block,
            (g.log.end - g.log.start) * g.block,
            &g.uuid,
            cancel,
        )?;
        if be64(&sb, 240) > lsn {
            return Err(invalid("superblock is newer than clean journal head"));
        }
        let mut budget = 256 << 20;
        let mut free = Vec::new();
        let mut count = 0u64;
        for number in 0..g.groups {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let first = u64::from(number) * u64::from(g.ag_blocks);
            let group = Group {
                first,
                blocks: (g.blocks - first).min(u64::from(g.ag_blocks)) as u32,
                number,
                block_bytes: g.block,
                uuid: &g.uuid,
                lsn,
            };
            let at = first * g.block;
            let headers = image.read(at, 4 * g.sector)?;
            let alt = &headers[..g.sector];
            if !checked_crc(alt, 224)
                || alt[..56] != sb[..56]
                || alt[84..100] != sb[84..100]
                // ATTR and QUOTA can be enabled after mkfs on the primary.
                || (be16(alt, 100) ^ be16(&sb, 100)) & !0x50 != 0
                || alt[102..108] != sb[102..108]
                || alt[120..125] != sb[120..125]
                || alt[208..224] != sb[208..224]
            {
                return Err(invalid(
                    "allocation group superblock disagrees with geometry",
                ));
            }
            let agf = &headers[g.sector..2 * g.sector];
            let agi = &headers[2 * g.sector..3 * g.sector];
            if &agf[..4] != b"XAGF"
                || &agi[..4] != b"XAGI"
                || agf[4..16] != agi[4..16]
                || be32(agf, 4) != 1
                || be32(agf, 8) != number
                || be32(agf, 12) != group.blocks
                || agf[64..80] != g.uuid
                || agi[296..312] != g.uuid
                || be64(agf, 208) > lsn
                || be64(agi, 320) > lsn
                || !checked_crc(agf, 216)
                || !checked_crc(agi, 312)
            {
                return Err(invalid("allocation group headers, UUID or checksums"));
            }
            let mut reserved: BTreeSet<u32> =
                (0..(4 * g.sector as u64).div_ceil(g.block) as u32).collect();
            for root in [be32(agf, 24), be32(agf, 88), be32(agi, 20), be32(agi, 328)] {
                if root != 0 && (root >= group.blocks || !reserved.insert(root)) {
                    return Err(invalid("auxiliary tree roots"));
                }
            }
            let fl = &headers[3 * g.sector..];
            let max = ((fl.len() - 36) / 4) as u32;
            let (first_fl, last_fl, n) = (be32(agf, 40), be32(agf, 44), be32(agf, 48));
            if &fl[..4] != b"XAFL"
                || be32(fl, 4) != number
                || fl[8..24] != g.uuid
                || be64(fl, 24) > lsn
                || !checked_crc(fl, 32)
                || first_fl >= max
                || last_fl >= max
                || n > max
                || (first_fl + n) % max != (last_fl + 1) % max
            {
                return Err(invalid("freelist checksum or circular indices"));
            }
            for i in 0..n {
                let block = be32(fl, 36 + 4 * ((first_fl + i) % max) as usize);
                if block >= group.blocks || !reserved.insert(block) {
                    return Err(invalid("freelist block reused or outside group"));
                }
            }
            let mut read_tree = |pos, level, by_size| {
                tree::records(
                    image,
                    &group,
                    (be32(agf, pos), be32(agf, level)),
                    by_size,
                    &mut reserved,
                    &mut budget,
                    cancel,
                )
            };
            let bno = read_tree(16, 28, false)?;
            let mut cnt = read_tree(20, 32, true)?;
            cnt.sort_unstable();
            if bno != cnt
                || bno.iter().map(|&(_, n)| u64::from(n)).sum::<u64>() != u64::from(be32(agf, 52))
                || bno.iter().map(|&(_, n)| n).max().unwrap_or(0) != be32(agf, 56)
                || free.len() + bno.len() > MAX_RECORDS
            {
                return Err(invalid("free-space indexes or summary counts disagree"));
            }
            count += u64::from(n) + u64::from(be32(agf, 52)) + u64::from(be32(agf, 60));
            let mut previous = None;
            for (start, len) in bno {
                let end = start + len;
                let extent = first + u64::from(start)..first + u64::from(end);
                if previous.is_some_and(|end| start <= end)
                    || reserved.range(start..end).next().is_some()
                    || (extent.start < g.log.end && extent.end > g.log.start)
                {
                    return Err(invalid("overlapping free extents or metadata marked free"));
                }
                free.push(extent);
                previous = Some(end);
            }
        }
        if count != g.free {
            return Err(invalid(
                "superblock free total disagrees with allocation groups",
            ));
        }
        Ok(Self {
            free,
            block: g.block,
            bytes: g.blocks * g.block,
        })
    }

    pub fn status(&self, at: u64, size: u64) -> AllocationStatus {
        let Some(end) = at.checked_add(size) else {
            return AllocationStatus::Unknown;
        };
        if size == 0 || at >= self.bytes {
            return AllocationStatus::Unknown;
        }
        let first = at / self.block;
        let limit = end.min(self.bytes).div_ceil(self.block);
        let i = self.free.partition_point(|r| r.end <= first);
        if !self
            .free
            .get(i)
            .is_some_and(|r| r.start <= first && r.end >= limit)
        {
            AllocationStatus::Conflict
        } else if end > self.bytes {
            AllocationStatus::Unknown
        } else {
            AllocationStatus::Unallocated
        }
    }
}
