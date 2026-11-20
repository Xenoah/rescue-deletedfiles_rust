//! Reconstruct occupied blocks from every allocated inode plus reserved filesystem structures.
use super::{Geometry, bad, bit, check, dword, word};
use crate::{Result, source::Image};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};

pub(super) struct Audit {
    owned: Vec<u8>,
    indirect: BTreeSet<u32>,
    allocated_inodes: usize,
    pointers: usize,
    references: u64,
    clean_journal: bool,
}
impl Audit {
    pub(super) fn new(bytes: usize) -> Self {
        Self {
            owned: vec![0; bytes],
            indirect: BTreeSet::new(),
            allocated_inodes: 0,
            pointers: 0,
            references: 0,
            clean_journal: false,
        }
    }
    pub(super) fn mark(&mut self, g: &Geometry, p: u32) -> Result<()> {
        if p < g.first || p >= g.blocks {
            return Err(bad("inode or metadata points outside the volume"));
        }
        let bit = (p - g.first) as usize;
        self.owned[bit / 8] |= 1 << (bit % 8);
        Ok(())
    }
    fn tree(
        &mut self,
        image: &mut Image,
        g: &Geometry,
        p: u32,
        depth: usize,
        cancel: &AtomicBool,
    ) -> Result<()> {
        if p == 0 {
            return Ok(());
        }
        self.mark(g, p)?;
        self.references += 1;
        if depth > 0 {
            check(cancel)?;
            if self.indirect.len() >= 65536 || !self.indirect.insert(p) {
                return Err(bad(
                    "duplicate indirect block or indirect-block analysis limit exceeded",
                ));
            }
            let children = image.read(g.address(p), g.block)?;
            self.pointers += g.block / 4;
            if self.pointers > 16 * 1024 * 1024 {
                return Err(bad("inode block-reference analysis limit exceeded"));
            }
            for ptr in children.as_chunks::<4>().0 {
                self.tree(image, g, u32::from_le_bytes(*ptr), depth - 1, cancel)?;
            }
        }
        Ok(())
    }
    pub(super) fn group_inodes(
        &mut self,
        image: &mut Image,
        g: &Geometry,
        group: u32,
        descriptor: &[u8; 32],
        bitmap: &[u8],
        cancel: &AtomicBool,
    ) -> Result<()> {
        let table = dword(descriptor, 8);
        let mut table_block = None;
        let mut bytes = Vec::new();
        let mut directories = 0u32;
        for (byte_index, &used) in bitmap[..g.inodes_per_group as usize / 8].iter().enumerate() {
            if used == 0 {
                continue;
            }
            for b in 0..8 {
                if used & (1 << b) == 0 {
                    continue;
                }
                check(cancel)?;
                self.allocated_inodes += 1;
                if self.allocated_inodes > 262144 {
                    return Err(bad("allocated inode analysis limit exceeded"));
                }
                let number = byte_index * 8 + b;
                let pos = number * g.inode_size;
                let index = pos / g.block;
                if table_block != Some(index) {
                    bytes = image.read(g.address(table + index as u32), g.block)?;
                    table_block = Some(index);
                }
                let offset = pos % g.block;
                let inode = &bytes[offset..offset + g.inode_size];
                let ino = group * g.inodes_per_group + number as u32 + 1;
                let mode = word(inode, 0) >> 12;
                if mode == 0 && ino < g.first_inode {
                    // Reserved inode 1 may hold a bad-block list even without a file type.
                    if ino != 1 {
                        continue;
                    }
                } else if !matches!(mode, 1 | 2 | 4 | 6 | 8 | 10 | 12)
                    || word(inode, 26) == 0
                    || dword(inode, 20) != 0
                {
                    return Err(bad(
                        "allocated inode has an invalid type, link count or deletion time",
                    ));
                }
                if mode == 4 {
                    directories += 1;
                }
                if dword(inode, 32) & (0x4 | 0x800 | 0x80000 | 0x10000000) != 0 {
                    return Err(bad("unsupported inode data mapping"));
                }
                let initial_references = self.references;
                let attribute = dword(inode, 104);
                if attribute != 0 {
                    self.mark(g, attribute)?;
                    self.references += 1;
                }
                if ino == g.journal && ino != 0 {
                    self.clean_journal = journal(image, g, inode)?;
                }
                let inline_link = mode == 10
                    && dword(inode, 4) <= 60
                    && dword(inode, 28)
                        == if attribute != 0 {
                            (g.block / 512) as u32
                        } else {
                            0
                        };
                if !inline_link && (matches!(mode, 4 | 8 | 10) || ino == 1) {
                    for slot in 0usize..15 {
                        self.tree(
                            image,
                            g,
                            dword(inode, 40 + slot * 4),
                            slot.saturating_sub(11),
                            cancel,
                        )?;
                    }
                }
                if (self.references - initial_references) * (g.block / 512) as u64
                    != u64::from(dword(inode, 28))
                {
                    return Err(bad("sector count differs from inode block references"));
                }
            }
        }
        if directories != u32::from(word(descriptor, 16)) {
            return Err(bad("directory count differs from allocated inodes"));
        }
        Ok(())
    }
    pub(super) fn finish(&self, g: &Geometry, allocated: &[u8]) -> Result<()> {
        let n = (g.blocks - g.first) as usize;
        if (g.journal != 0 && !self.clean_journal)
            || self.owned[..n / 8] != allocated[..n / 8]
            || (n / 8 * 8..n).any(|i| bit(&self.owned, i) != bit(allocated, i))
        {
            return Err(bad(
                "allocated blocks disagree with inode ownership or journal is unavailable",
            ));
        }
        Ok(())
    }
}
fn journal(image: &mut Image, g: &Geometry, inode: &[u8]) -> Result<bool> {
    let first = dword(inode, 40);
    if word(inode, 0) >> 12 != 8 || first < g.first || first >= g.blocks {
        return Err(bad("invalid internal journal inode"));
    }
    let j = image.read(g.address(first), g.block)?;
    let value = |i: usize| u32::from_be_bytes(j[i..i + 4].try_into().unwrap());
    if value(0) != 0xc03b3998
        || value(4) != 4
        || value(12) != g.block as u32
        || u64::from(value(16)) * g.block as u64 != u64::from(dword(inode, 4))
        || value(20) == 0
        || value(20) >= value(16)
        || value(28) != 0
        || value(32) != 0
        || j[48..64] != g.uuid
        || value(64) != 1
        || j[36..48].iter().any(|&b| b != 0)
    {
        return Err(bad(
            "internal ext3 journal geometry, features or replay state unsupported",
        ));
    }
    Ok(true)
}
