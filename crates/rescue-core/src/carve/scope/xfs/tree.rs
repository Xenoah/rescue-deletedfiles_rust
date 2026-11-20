//! Cross-check the allocation trees sorted by location and by extent length.
use super::{be16, be32, be64, checked_crc, invalid};
use crate::{Error, Image, Result};
use std::{
    collections::BTreeSet,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) struct Group<'a> {
    pub first: u64,
    pub blocks: u32,
    pub number: u32,
    pub block_bytes: u64,
    pub uuid: &'a [u8],
    pub lsn: u64,
}
pub(super) type Record = (u32, u32);
pub(super) const MAX_RECORDS: usize = 1 << 20;

pub(super) fn records(
    image: &mut Image,
    group: &Group,
    (root, levels): (u32, u32),
    by_size: bool,
    reserved: &mut BTreeSet<u32>,
    budget: &mut usize,
    cancel: &AtomicBool,
) -> Result<Vec<Record>> {
    if !(1..=8).contains(&levels) {
        return Err(invalid("allocation tree height"));
    }
    let mut result = Vec::new();
    let mut nodes = vec![(root, None)];
    let order = |(start, len): Record| if by_size { (len, start) } else { (start, len) };
    let magic = if by_size { b"AB3C" } else { b"AB3B" };
    for level in (0..levels).rev() {
        let mut children = Vec::new();
        for (i, &(block, key)) in nodes.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if block == 0 || block >= group.blocks || !reserved.insert(block) {
                return Err(invalid("invalid or reused allocation tree node"));
            }
            let size = group.block_bytes as usize;
            *budget = budget
                .checked_sub(size)
                .ok_or_else(|| Error::Limit("XFS allocation trees exceed 256 MiB".into()))?;
            let at = (group.first + u64::from(block)) * group.block_bytes;
            let node = image.read(at, size)?;
            let count = usize::from(be16(&node, 6));
            let width = if level == 0 { 8 } else { 12 };
            let maximum = (size - 56) / width;
            let sibling = |n: Option<&(u32, Option<Record>)>| n.map_or(u32::MAX, |v| v.0);
            if node[..4] != *magic
                || u32::from(be16(&node, 4)) != level
                || count > maximum
                || (count == 0 && (level != 0 || levels != 1))
                || be32(&node, 8) != sibling(i.checked_sub(1).and_then(|n| nodes.get(n)))
                || be32(&node, 12) != sibling(nodes.get(i + 1))
                || be64(&node, 16) != at / 512
                || node[32..48] != *group.uuid
                || be32(&node, 48) != group.number
                || be64(&node, 24) > group.lsn
                || !checked_crc(&node, 52)
            {
                return Err(invalid("allocation tree header, siblings or checksum"));
            }
            let mut previous = None;
            for j in 0..count {
                let pos = 56 + j * 8;
                let record = (be32(&node, pos), be32(&node, pos + 4));
                if record.1 == 0
                    || record
                        .0
                        .checked_add(record.1)
                        .is_none_or(|end| end > group.blocks)
                    || (j == 0 && key.is_some_and(|k| k != record))
                    || previous.is_some_and(|r| order(r) >= order(record))
                {
                    return Err(invalid("allocation tree extent or separator order"));
                }
                previous = Some(record);
                if level == 0 {
                    if result.len() >= MAX_RECORDS
                        || result.last().is_some_and(|&r| order(r) >= order(record))
                    {
                        return Err(invalid("allocation record count or order"));
                    }
                    result.push(record);
                } else {
                    if children.len() >= 65536 {
                        return Err(Error::Limit("XFS tree width".into()));
                    }
                    children.push((be32(&node, 56 + maximum * 8 + j * 4), Some(record)));
                }
            }
        }
        nodes = children;
    }
    Ok(result)
}
