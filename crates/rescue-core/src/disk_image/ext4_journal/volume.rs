//! Read ext4 metadata checksums, allocation maps and extent records.
use super::{bad, le16, le32};
use crate::{Error, Image, Result, crc32c::update};
use std::collections::BTreeMap;

#[derive(Clone, Copy)]
struct Group {
    table: u64,
    block_map: u64,
    inode_map: u64,
    block_sum: u32,
    inode_sum: u32,
    flags: u16,
}

pub(super) struct Volume {
    pub image: Image,
    pub block_size: usize,
    pub blocks: u64,
    pub uuid: [u8; 16],
    pub journal_ino: u32,
    pub inode_size: usize,
    pub first_inode: u32,
    seed: u32,
    first: u64,
    per_group: u32,
    inodes_per: u32,
    inodes: u32,
    groups: Vec<Group>,
    tables: BTreeMap<u64, usize>,
    maps: BTreeMap<(usize, bool), Vec<u8>>,
}

impl Volume {
    pub fn open(mut image: Image) -> Result<Self> {
        let sb = image.read(1024, 1024)?;
        let incompat = le32(&sb, 96);
        let readonly = le32(&sb, 100);
        if le16(&sb, 56) != 0xef53
            || le32(&sb, 92) & 4 == 0
            || incompat & 0xc0 != 0xc0
            || incompat & !0x22c2 != 0
            || readonly & 0x400 == 0
            || readonly & !0x47b != 0
            || sb[373] != 1
            || le16(&sb, 254) != 64
            || le32(&sb, 72) != 0
            || le16(&sb, 58) & 1 == 0
        {
            return Err(Error::Unsupported("expected clean ext4 with extents, 64-bit groups, internal journal and metadata checksums".into()));
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
            };
            if !(first..blocks).contains(&g.block_map)
                || !(first..blocks).contains(&g.inode_map)
                || g.table < first
                || g.table.checked_add(table_blocks).is_none_or(|n| n > blocks)
                || tables.insert(g.table, i).is_some()
            {
                return Err(bad("invalid group metadata address"));
            }
            groups.push(g);
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
    let flags = le32(inode, 32);
    if flags & 0x80000 == 0
        || flags & (0x4 | 0x800 | 0x100000 | 0x10000000) != 0
        || le16(inode, 40) != 0xf30a
        || le16(inode, 46) != 0
        || le16(inode, 44) != 4
        || le16(inode, 42) > 4
    {
        return Err(Error::Unsupported(
            "ext4 inode needs a resident extent root with up to four extents".into(),
        ));
    }
    let mut runs = Vec::new();
    let mut end = 0;
    for i in 0..usize::from(le16(inode, 42)) {
        let at = 52 + i * 12;
        let len = le16(inode, at + 4);
        let unwritten = len > 0x8000;
        let count = u64::from(if unwritten { len - 0x8000 } else { len });
        let start = u64::from(le32(inode, at));
        let physical = u64::from(le32(inode, at + 8)) | (u64::from(le16(inode, at + 6)) << 32);
        if count == 0
            || physical == 0
            || physical.checked_add(count).is_none_or(|n| n > blocks)
            || start < end
        {
            return Err(bad("invalid historical extent range"));
        }
        end = start + count;
        runs.push(Run {
            start,
            physical,
            count,
            unwritten,
        });
    }
    Ok(runs)
}

pub(super) fn file_size(inode: &[u8]) -> u64 {
    u64::from(le32(inode, 4)) | (u64::from(le32(inode, 108)) << 32)
}
