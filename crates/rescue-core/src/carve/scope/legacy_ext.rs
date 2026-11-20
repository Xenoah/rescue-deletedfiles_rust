//! Clean ext2/ext3 allocation maps verified against group counts and allocated inode trees.
mod owners;
use crate::{AllocationStatus, Error, Result, source::Image};
use std::sync::atomic::{AtomicBool, Ordering};

const MAX_GROUPS: usize = 16384;
const MAX_BITMAP: usize = 32 << 20;
fn bad(reason: &str) -> Error {
    Error::Invalid(format!("ext2/ext3 allocation: {reason}"))
}
fn word(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn dword(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}
pub(super) fn recognized(image: &mut Image) -> bool {
    image.read(1024, 104).is_ok_and(|s| {
        word(&s, 56) == 0xef53 && dword(&s, 96) & 0x40 == 0 && dword(&s, 100) & 0x400 == 0
    })
}

struct Geometry {
    block: usize,
    first: u32,
    blocks: u32,
    per_group: u32,
    inodes_per_group: u32,
    inode_size: usize,
    first_inode: u32,
    journal: u32,
    free_blocks: u32,
    free_inodes: u32,
    sparse_super: bool,
    reserved_gdt: u16,
    uuid: [u8; 16],
}
impl Geometry {
    fn parse(s: &[u8], length: u64) -> Result<Self> {
        let revision = dword(s, 76);
        let compatible = dword(s, 92);
        let incompatible = dword(s, 96);
        let readonly = dword(s, 100);
        let shift = dword(s, 24);
        if word(s, 56) != 0xef53
            || revision > 1
            || word(s, 58) != 1
            || dword(s, 72) != 0
            || shift > 2
            || dword(s, 28) != shift
            || dword(s, 36) != dword(s, 32)
            || compatible & !0x3c != 0
            || incompatible & !2 != 0
            || readonly & !3 != 0
            || dword(s, 232) != 0
            || dword(s, 228) != 0
            || word(s, 90) != 0
            || (revision == 0 && (compatible | incompatible | readonly) != 0)
        {
            return Err(bad(
                "requires a clean Linux ext2/ext3 superblock with classic group descriptors",
            ));
        }
        let block = 1024usize << shift;
        let first = dword(s, 20);
        let blocks = dword(s, 4);
        let per_group = dword(s, 32);
        let inodes_per_group = dword(s, 40);
        let inode_size = if revision == 0 {
            128
        } else {
            usize::from(word(s, 88))
        };
        let first_inode = if revision == 0 { 11 } else { dword(s, 84) };
        let journal = dword(s, 224);
        let reserved_gdt = word(s, 206);
        if first != u32::from(block == 1024)
            || blocks <= first
            || u64::from(blocks) * block as u64 > length
            || per_group == 0
            || !per_group.is_multiple_of(8)
            || per_group > (block * 8) as u32
            || inodes_per_group == 0
            || !inodes_per_group.is_multiple_of(8)
            || inodes_per_group > (block * 8) as u32
            || !(128..=block).contains(&inode_size)
            || !inode_size.is_power_of_two()
            || first_inode != 11
            || !(inodes_per_group as usize * inode_size).is_multiple_of(block)
            || compatible & 0x10 == 0 && reserved_gdt != 0
            || compatible & 4 != 0 && journal != 8
            || compatible & 4 == 0 && journal != 0
            || s[208..224].iter().any(|&v| v != 0)
        {
            return Err(bad(
                "unsupported block, inode, reserved-descriptor or journal geometry",
            ));
        }
        let groups = (blocks - first).div_ceil(per_group);
        if groups as usize > MAX_GROUPS
            || groups.checked_mul(inodes_per_group) != Some(dword(s, 0))
            || (groups as usize)
                .checked_mul(per_group as usize / 8)
                .is_none_or(|v| v > MAX_BITMAP)
        {
            return Err(bad(
                "group count or allocation bitmap exceeds supported bounds",
            ));
        }
        Ok(Self {
            block,
            first,
            blocks,
            per_group,
            inodes_per_group,
            inode_size,
            first_inode,
            journal,
            free_blocks: dword(s, 12),
            free_inodes: dword(s, 16),
            sparse_super: readonly & 1 != 0,
            reserved_gdt,
            uuid: s[104..120].try_into().unwrap(),
        })
    }
    fn groups(&self) -> u32 {
        (self.blocks - self.first).div_ceil(self.per_group)
    }
    fn address(&self, block: u32) -> u64 {
        u64::from(block) * self.block as u64
    }
}

pub(super) struct Map {
    geometry: Geometry,
    allocated: Vec<u8>,
}
fn has_super(group: u32, sparse: bool) -> bool {
    !sparse
        || group <= 1
        || [3, 5, 7].into_iter().any(|n| {
            let mut remaining = group;
            while remaining.is_multiple_of(n) {
                remaining /= n;
            }
            remaining == 1
        })
}
fn bit(map: &[u8], index: usize) -> bool {
    map[index / 8] & (1 << (index % 8)) != 0
}
fn verified_bitmap(bytes: &[u8], valid: usize, free: u16) -> Result<()> {
    // e2fsprogs and the kernel reserve all padding bits, including the short final group.
    let zeros: usize = bytes.iter().map(|n| n.count_zeros() as usize).sum();
    let pad_byte = valid / 8;
    if usize::from(free) != zeros
        || bytes.get(pad_byte).is_some_and(|&n| !n >> (valid % 8) != 0)
        || bytes[valid.div_ceil(8)..].iter().any(|&b| b != 0xff)
    {
        return Err(bad("group free count or bitmap padding differs"));
    }
    Ok(())
}
impl Map {
    pub(super) fn open(image: &mut Image, cancel: &AtomicBool) -> Result<Self> {
        let geometry = Geometry::parse(&image.read(1024, 1024)?, image.length)?;
        let groups = geometry.groups() as usize;
        let descriptors = image.read(geometry.address(geometry.first + 1), groups * 32)?;
        let bitmap_len = groups * geometry.per_group as usize / 8;
        let mut allocated = Vec::with_capacity(bitmap_len);
        let mut audit = owners::Audit::new(bitmap_len);
        let mut free_blocks = 0u64;
        let mut free_inodes = 0u64;
        for (group, desc) in descriptors.as_chunks::<32>().0.iter().enumerate() {
            check(cancel)?;
            let start = geometry.first + group as u32 * geometry.per_group;
            let blocks = (geometry.blocks - start).min(geometry.per_group);
            let bitmap = dword(desc, 0);
            let inode_map = dword(desc, 4);
            let table = dword(desc, 8);
            let table_blocks =
                (geometry.inodes_per_group as usize * geometry.inode_size / geometry.block) as u32;
            let prefix = if has_super(group as u32, geometry.sparse_super) {
                1 + (descriptors.len().div_ceil(geometry.block) as u32)
                    + u32::from(geometry.reserved_gdt)
            } else {
                0
            };
            if prefix > blocks
                || word(desc, 18) & !4 != 0
                || [bitmap, inode_map, table]
                    .iter()
                    .any(|&p| p < start + prefix || p >= start + blocks)
                || bitmap == inode_map
                || table
                    .checked_add(table_blocks)
                    .is_none_or(|end| end > start + blocks)
                || (table..table + table_blocks).contains(&bitmap)
                || (table..table + table_blocks).contains(&inode_map)
            {
                return Err(bad("overlapping, uninitialized, or out-of-group metadata"));
            }
            let group_map = image.read(geometry.address(bitmap), geometry.block)?;
            verified_bitmap(&group_map, blocks as usize, word(desc, 12))?;
            allocated.extend_from_slice(&group_map[..geometry.per_group as usize / 8]);
            free_blocks += u64::from(word(desc, 12));
            let inodes = image.read(geometry.address(inode_map), geometry.block)?;
            verified_bitmap(&inodes, geometry.inodes_per_group as usize, word(desc, 14))?;
            free_inodes += u64::from(word(desc, 14));
            for p in (start..start + prefix)
                .chain([bitmap, inode_map])
                .chain(table..table + table_blocks)
            {
                audit.mark(&geometry, p)?;
            }
            audit.group_inodes(image, &geometry, group as u32, desc, &inodes, cancel)?;
        }
        if free_blocks != u64::from(geometry.free_blocks)
            || free_inodes != u64::from(geometry.free_inodes)
        {
            return Err(bad("superblock and group free counts disagree"));
        }
        audit.finish(&geometry, &allocated)?;
        Ok(Self {
            geometry,
            allocated,
        })
    }
    pub(super) fn status(&self, at: u64, length: u64) -> AllocationStatus {
        let g = &self.geometry;
        let end = at.saturating_add(length);
        let block = at / g.block as u64;
        let stop = end.div_ceil(g.block as u64);
        if length == 0 {
            return AllocationStatus::Unknown;
        }
        if block < u64::from(g.first) {
            return AllocationStatus::Conflict;
        }
        let valid_stop = stop.min(u64::from(g.blocks));
        if block < valid_stop {
            let first = (block - u64::from(g.first)) as usize;
            let last = (valid_stop - u64::from(g.first)) as usize;
            if (first..last).any(|i| bit(&self.allocated, i)) {
                return AllocationStatus::Conflict;
            }
        }
        if stop > u64::from(g.blocks) {
            AllocationStatus::Unknown
        } else {
            AllocationStatus::Unallocated
        }
    }
}
