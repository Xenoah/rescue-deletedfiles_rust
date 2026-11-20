//! Read ext4 metadata checksums, allocation maps and extent records.
use super::{bad, le16, le32};
use crate::{AllocationStatus, Error, Image, Result, crc32c::update};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Group {
    table: u64,
    block_map: u64,
    inode_map: u64,
    block_sum: u32,
    inode_sum: u32,
    flags: u16,
    free_blocks: u32,
}

pub(crate) struct Volume {
    pub image: Image,
    pub block_size: usize,
    pub blocks: u64,
    pub uuid: [u8; 16],
    pub journal_ino: u32,
    pub inode_size: usize,
    pub first_inode: u32,
    seed: u32,
    verify_free_counts: bool,
    first: u64,
    per_group: u32,
    inodes_per: u32,
    inodes: u32,
    groups: Vec<Group>,
    tables: BTreeMap<u64, usize>,
    maps: BTreeMap<(usize, bool), Vec<u8>>,
}

impl Volume {
    pub fn open(image: Image) -> Result<Self> {
        Self::parse(image, true)
    }

    pub(crate) fn recognized(image: &mut Image) -> bool {
        image.length >= 2048 && image.read(1024 + 56, 2).is_ok_and(|b| b == [0x53, 0xef])
    }

    pub(crate) fn open_allocation(image: Image) -> Result<Self> {
        Self::parse(image, false)
    }

    fn parse(mut image: Image, require_journal: bool) -> Result<Self> {
        let sb = image.read(1024, 1024)?;
        let incompat = le32(&sb, 96);
        let readonly = le32(&sb, 100);
        if le16(&sb, 56) != 0xef53
            || incompat & 0xc0 != 0xc0
            || incompat & !0x22c2 != 0
            || readonly & 0x400 == 0
            || readonly & !0x47b != 0
            || sb[373] != 1
            || le16(&sb, 254) != 64
            || le32(&sb, 72) != 0
            || le16(&sb, 58) != 1
            || le32(&sb, 232) != 0
        {
            return Err(Error::Unsupported(
                "expected clean ext4 with extents, 64-bit groups and metadata checksums".into(),
            ));
        }
        if require_journal && le32(&sb, 92) & 4 == 0 {
            return Err(Error::Unsupported(
                "ext4 volume has no internal journal".into(),
            ));
        }
        if update(u32::MAX, &sb[..1020]) != le32(&sb, 1020) {
            return Err(bad("ext4 superblock checksum mismatch"));
        }
        let log = le32(&sb, 24);
        if log > 6 || log != le32(&sb, 28) {
            return Err(bad("unsupported ext4 block or cluster size"));
        }
        let block_size = 1024usize << log;
        let inodes = le32(&sb, 0);
        let blocks = u64::from(le32(&sb, 4)) | (u64::from(le32(&sb, 336)) << 32);
        let first = u64::from(le32(&sb, 20));
        let per_group = le32(&sb, 32);
        let inodes_per = le32(&sb, 40);
        let inode_size = usize::from(le16(&sb, 88));
        let first_inode = le32(&sb, 84);
        if first != u64::from(block_size == 1024)
            || blocks <= first
            || blocks > image.length / block_size as u64
            || per_group == 0
            || per_group as usize > block_size * 8
            || !per_group.is_multiple_of(8)
            || inodes_per == 0
            || inodes_per as usize > block_size * 8
            || !inodes_per.is_multiple_of(8)
            || !(256..=block_size).contains(&inode_size)
            || !inode_size.is_power_of_two()
            || !(11..inodes).contains(&first_inode)
        {
            return Err(bad("invalid ext4 geometry"));
        }
        let count = (blocks - first).div_ceil(u64::from(per_group));
        if count > 65536 || u64::from(inodes.div_ceil(inodes_per)) != count {
            return Err(bad("unsupported group count"));
        }
        let descriptors = image.read((first + 1) * block_size as u64, count as usize * 64)?;
        let uuid: [u8; 16] = sb[104..120].try_into().unwrap();
        let seed = if incompat & 0x2000 != 0 {
            le32(&sb, 624)
        } else {
            update(u32::MAX, &uuid)
        };
        let mut groups = Vec::new();
        let mut tables = BTreeMap::new();
        let table_blocks = (u64::from(inodes_per) * inode_size as u64).div_ceil(block_size as u64);
        for (i, group) in descriptors.as_chunks::<64>().0.iter().enumerate() {
            let addr = |lo, hi| u64::from(le32(group, lo)) | (u64::from(le32(group, hi)) << 32);
            let sum = |lo, hi| u32::from(le16(group, lo)) | (u32::from(le16(group, hi)) << 16);
            let mut check = *group;
            check[30..32].fill(0);
            if update(update(seed, &(i as u32).to_le_bytes()), &check) as u16 != le16(group, 30) {
                return Err(bad("group descriptor checksum mismatch"));
            }
            let g = Group {
                block_map: addr(0, 32),
                inode_map: addr(4, 36),
                table: addr(8, 40),
                block_sum: sum(24, 56),
                inode_sum: sum(26, 58),
                flags: le16(group, 18),
                free_blocks: sum(12, 44),
            };
            if !(first..blocks).contains(&g.block_map)
                || !(first..blocks).contains(&g.inode_map)
                || u64::from(g.free_blocks)
                    > (blocks - first - i as u64 * u64::from(per_group)).min(u64::from(per_group))
                || g.table < first
                || g.table.checked_add(table_blocks).is_none_or(|n| n > blocks)
                || tables.insert(g.table, i).is_some()
            {
                return Err(bad("invalid group metadata address"));
            }
            groups.push(g);
        }
        let free_blocks = u64::from(le32(&sb, 12)) | (u64::from(le32(&sb, 344)) << 32);
        if !require_journal
            && groups.iter().map(|g| u64::from(g.free_blocks)).sum::<u64>() != free_blocks
        {
            return Err(bad("ext4 group free counts disagree with superblock"));
        }
        let mut end = 0;
        for &start in tables.keys() {
            if start < end {
                return Err(bad("overlapping inode tables"));
            }
            end = start + table_blocks;
        }
        Ok(Self {
            image,
            block_size,
            blocks,
            uuid,
            journal_ino: le32(&sb, 224),
            inode_size,
            first_inode,
            seed,
            first,
            verify_free_counts: !require_journal,
            per_group,
            inodes_per,
            inodes,
            groups,
            tables,
            maps: BTreeMap::new(),
        })
    }

    pub fn inode_numbers(&self, block: u64) -> Option<std::ops::RangeInclusive<u32>> {
        let (&start, &group) = self.tables.range(..=block).next_back()?;
        let local = (block - start).checked_mul((self.block_size / self.inode_size) as u64)?;
        if local >= u64::from(self.inodes_per) {
            return None;
        }
        let begin = group as u64 * u64::from(self.inodes_per) + local + 1;
        let end = (begin + (self.block_size / self.inode_size) as u64 - 1)
            .min((group as u64 + 1) * u64::from(self.inodes_per))
            .min(self.inodes.into());
        Some(u32::try_from(begin).ok()?..=u32::try_from(end).ok()?)
    }

    pub fn inode_address(&self, ino: u32) -> Option<(u64, usize)> {
        if ino == 0 || ino > self.inodes {
            return None;
        }
        let local = ino - 1;
        let table = self.groups[(local / self.inodes_per) as usize].table;
        let bytes = u64::from(local % self.inodes_per) * self.inode_size as u64;
        Some((
            table + bytes / self.block_size as u64,
            (bytes % self.block_size as u64) as usize,
        ))
    }

    pub fn current_inode(&mut self, ino: u32) -> Result<Vec<u8>> {
        let (block, offset) = self
            .inode_address(ino)
            .ok_or_else(|| bad("inode outside filesystem"))?;
        self.image.read(
            block * self.block_size as u64 + offset as u64,
            self.inode_size,
        )
    }

    pub fn inode_seed(&self, ino: u32, inode: &[u8]) -> u32 {
        update(update(self.seed, &ino.to_le_bytes()), &inode[100..104])
    }

    pub fn valid_inode(&self, ino: u32, inode: &[u8]) -> bool {
        let extra = usize::from(le16(inode, 128));
        if extra < 4 || extra > self.inode_size - 128 {
            return false;
        }
        let seed = self.inode_seed(ino, inode);
        let crc = update(
            update(
                update(
                    update(update(seed, &inode[..124]), &[0; 2]),
                    &inode[126..130],
                ),
                &[0; 2],
            ),
            &inode[132..],
        );
        crc == u32::from(le16(inode, 124)) | (u32::from(le16(inode, 130)) << 16)
    }

    fn allocation_map(&mut self, group: usize, inode: bool) -> Result<&[u8]> {
        if !self.maps.contains_key(&(group, inode)) {
            let g = self.groups[group];
            let (pos, amount, expected, uninit) = if inode {
                (g.inode_map, self.inodes_per, g.inode_sum, 1)
            } else {
                (g.block_map, self.per_group, g.block_sum, 2)
            };
            if g.flags & uninit != 0 {
                return Err(bad("allocation bitmap is uninitialized"));
            }
            if self.maps.len() >= 1024 {
                return Err(Error::Limit("ext4 bitmap cache exceeds 1024 groups".into()));
            }
            let bytes = self
                .image
                .read(pos * self.block_size as u64, amount as usize / 8)?;
            if update(self.seed, &bytes) != expected {
                return Err(bad("allocation bitmap checksum mismatch"));
            }
            if self.verify_free_counts && !inode {
                let valid = (self.blocks - self.first - group as u64 * u64::from(self.per_group))
                    .min(u64::from(self.per_group)) as usize;
                let whole = valid / 8;
                let mut used: u32 = bytes[..whole].iter().map(|byte| byte.count_ones()).sum();
                if !valid.is_multiple_of(8) {
                    used += (bytes[whole] & ((1u8 << (valid % 8)) - 1)).count_ones();
                }
                if valid as u32 - used != g.free_blocks {
                    return Err(bad("ext4 free count disagrees with allocation bitmap"));
                }
            }
            self.maps.insert((group, inode), bytes);
        }
        Ok(&self.maps[&(group, inode)])
    }

    pub fn inode_free(&mut self, ino: u32) -> Result<bool> {
        let index = ino - 1;
        let group = (index / self.inodes_per) as usize;
        let bit = index % self.inodes_per;
        Ok(self.allocation_map(group, true)?[(bit / 8) as usize] & (1 << (bit % 8)) == 0)
    }

    /// Scan every known part, even after an unreadable group. One allocated
    /// block anywhere in the payload excludes it from unallocated-only carving.
    pub(crate) fn carve_status(&mut self, at: u64, length: u64) -> Result<AllocationStatus> {
        let size = self.block_size as u64;
        let end = at.checked_add(length).ok_or(Error::SourceChanged)?;
        let mut block = at / size;
        let stop = end.div_ceil(size).min(self.blocks);
        if length == 0 {
            return Ok(AllocationStatus::Unknown);
        }
        if block < self.first {
            return Ok(AllocationStatus::Conflict); // 1 KiB filesystems reserve block zero.
        }
        let per_group = u64::from(self.per_group);
        let mut failure = None;
        while block < stop {
            let index = block - self.first;
            let group = (index / per_group) as usize;
            let first_bit = (index % per_group) as usize;
            let count = (stop - block).min(per_group - first_bit as u64);
            match self.allocation_map(group, false) {
                Ok(map) => {
                    let end_bit = first_bit + count as usize;
                    for (i, &byte) in map[first_bit / 8..end_bit.div_ceil(8)].iter().enumerate() {
                        let base = (first_bit / 8 + i) * 8;
                        let low = first_bit.saturating_sub(base);
                        let high = (end_bit - base).min(8);
                        let mask = (0xffu8 << low) & (0xffu8 >> (8 - high));
                        if byte & mask != 0 {
                            return Ok(AllocationStatus::Conflict);
                        }
                    }
                }
                Err(error) => {
                    failure.get_or_insert(error);
                }
            }
            block += count;
        }
        if let Some(error) = failure {
            return Err(error);
        }
        if end > self.blocks * size {
            return Err(bad("range includes space outside ext4 allocation groups"));
        }
        Ok(AllocationStatus::Unallocated)
    }

    pub fn blocks_free(&mut self, start: u64, len: u64) -> Result<()> {
        if len == 0 || start < self.first || start.checked_add(len).is_none_or(|v| v > self.blocks)
        {
            return Err(bad("file extent outside the volume"));
        }
        for block in start..start + len {
            let index = block - self.first;
            let g = (index / u64::from(self.per_group)) as usize;
            let bit = (index % u64::from(self.per_group)) as usize;
            if self.allocation_map(g, false)?[bit / 8] & (1 << (bit % 8)) != 0 {
                return Err(bad("historical file extent is currently allocated"));
            }
        }
        Ok(())
    }
}

#[derive(Clone)]
pub(super) struct Run {
    pub start: u64,
    pub physical: u64,
    pub count: u64,
    pub unwritten: bool,
}
pub(super) fn runs(inode: &[u8], blocks: u64) -> Result<Vec<Run>> {
    extent_tree(inode, blocks, 0, |_| {
        Err(Error::Unsupported(
            "external blocks are unavailable for this inode".into(),
        ))
    })
}

/// Only accept external index and leaf blocks supplied by the selected committed transaction.
pub(super) fn extent_tree<'a>(
    inode: &'a [u8],
    blocks: u64,
    seed: u32,
    mut external: impl FnMut(u64) -> Result<&'a [u8]>,
) -> Result<Vec<Run>> {
    let flags = le32(inode, 32);
    let depth = le16(inode, 46);
    if flags & 0x80000 == 0 || flags & (0x4 | 0x800 | 0x100000 | 0x10000000) != 0 || depth > 5 {
        return Err(Error::Unsupported(
            "unsupported ext4 file layout or extent-tree depth".into(),
        ));
    }
    let mut output = Vec::new();
    let mut stack = vec![(&inode[40..100], depth, 0u64, 1u64 << 32, true)];
    let mut seen = std::collections::BTreeSet::new();
    while let Some((node, level, min, max, root)) = stack.pop() {
        let count = usize::from(le16(node, 2));
        let capacity = usize::from(le16(node, 4));
        if le16(node, 0) != 0xf30a
            || le16(node, 6) != level
            || capacity != (node.len() - 12) / 12
            || count > capacity
            || level != 0 && count == 0
        {
            return Err(bad("invalid ext4 extent tree header"));
        }
        if !root {
            let tail = 12 + capacity * 12;
            if node.len() != tail + 4 || update(seed, &node[..tail]) != le32(node, tail) {
                return Err(bad("extent tree checksum mismatch"));
            }
        }
        let mut children = Vec::new();
        let mut end = min;
        for i in 0..count {
            let at = 12 + i * 12;
            let start = u64::from(le32(node, at));
            if start < end || start >= max {
                return Err(bad("inconsistent extent logical range"));
            }
            if level > 0 {
                let child = u64::from(le32(node, at + 4)) | (u64::from(le16(node, at + 8)) << 32);
                if child == 0 || child >= blocks || !seen.insert(child) {
                    return Err(bad("invalid or duplicate extent tree child"));
                }
                if seen.len() > 4096 {
                    return Err(Error::Limit("ext4 extent tree exceeds 4096 blocks".into()));
                }
                let upper = if i + 1 < count {
                    u64::from(le32(node, at + 12))
                } else {
                    max
                };
                if upper <= start || upper > max {
                    return Err(bad("unordered extent tree index"));
                }
                children.push((external(child)?, level - 1, start, upper, false));
                end = start + 1;
            } else {
                let len = le16(node, at + 4);
                let unwritten = len > 0x8000;
                let count = u64::from(if unwritten { len - 0x8000 } else { len });
                let physical =
                    u64::from(le32(node, at + 8)) | (u64::from(le16(node, at + 6)) << 32);
                if count == 0
                    || physical == 0
                    || physical.checked_add(count).is_none_or(|n| n > blocks)
                    || start + count > max
                {
                    return Err(bad("invalid historical extent range"));
                }
                if output.len() >= 65536 {
                    return Err(Error::Limit("ext4 file exceeds 65536 extents".into()));
                }
                output.push(Run {
                    start,
                    physical,
                    count,
                    unwritten,
                });
                end = start + count;
            }
        }
        stack.extend(children.into_iter().rev());
    }
    Ok(output)
}

pub(super) fn file_size(inode: &[u8]) -> u64 {
    u64::from(le32(inode, 4)) | (u64::from(le32(inode, 108)) << 32)
}
