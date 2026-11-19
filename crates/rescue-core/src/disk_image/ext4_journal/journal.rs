//! Validate complete JBD2 v3 transactions retained in a clean internal journal.
use super::{bad, be32};
use crate::{Error, Result, crc32c::update};
use std::collections::{BTreeMap, BTreeSet};
use std::sync::atomic::{AtomicBool, Ordering};

const MAGIC: u32 = 0xc03b3998;
const DESCRIPTOR: u32 = 1;
const COMMIT: u32 = 2;
const REVOKE: u32 = 5;

pub(super) struct Transaction {
    pub sequence: u32,
    pub blocks: BTreeMap<u64, usize>,
    unavailable: BTreeSet<u64>,
}

fn checked_field(block: &[u8], at: usize, seed: u32) -> bool {
    let crc = update(
        update(update(seed, &block[..at]), &[0; 4]),
        &block[at + 4..],
    );
    crc == be32(block, at)
}

pub(super) fn read(
    bytes: &[u8],
    size: usize,
    fs_uuid: &[u8],
    fs_blocks: u64,
    cancel: &AtomicBool,
) -> Result<Vec<Transaction>> {
    let sb = bytes
        .get(..size)
        .ok_or_else(|| bad("missing JBD2 superblock"))?;
    if be32(sb, 0) != MAGIC
        || be32(sb, 4) != 4
        || be32(sb, 12) as usize != size
        || be32(sb, 36) != 0
        || be32(sb, 40) & !1 != 0x12
        || be32(sb, 44) != 0
        || sb[80] != 4
        || &sb[48..64] != fs_uuid
        || be32(sb, 64) != 1
    {
        return Err(Error::Unsupported(
            "expected a single-user internal JBD2 journal with 64-bit checksum-v3 tags".into(),
        ));
    }
    if !checked_field(&sb[..1024], 252, u32::MAX) || be32(sb, 28) != 0 {
        return Err(bad("JBD2 superblock checksum mismatch or recovery pending"));
    }
    let count = be32(sb, 16) as usize;
    let first = be32(sb, 20) as usize;
    if count != bytes.len() / size
        || !bytes.len().is_multiple_of(size)
        || first == 0
        || first >= count
    {
        return Err(bad("invalid JBD2 ring geometry"));
    }
    let next_sequence = be32(sb, 24);
    let seed = update(u32::MAX, fs_uuid);
    let block = |i: usize| &bytes[i * size..(i + 1) * size];
    let advance = |i| if i + 1 == count { first } else { i + 1 };
    let mut ordered = BTreeMap::new();
    let mut consumed = BTreeSet::new();
    // Start at the first descriptor of each coherent run, including wrapped transactions.
    for start in first..count {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let head = block(start);
        if be32(head, 0) != MAGIC || be32(head, 4) != DESCRIPTOR || consumed.contains(&start) {
            continue;
        }
        let sequence = be32(head, 8);
        let age = next_sequence.wrapping_sub(sequence);
        if age == 0 || age as usize > count {
            continue;
        }
        let mut pos = start;
        let mut seen = BTreeSet::new();
        let mut targets = BTreeMap::new();
        let mut revoked = BTreeSet::new();
        let mut complete = false;
        while seen.insert(pos) && seen.len() < count {
            let header = block(pos);
            if be32(header, 0) != MAGIC || be32(header, 8) != sequence {
                break;
            }
            let kind = be32(header, 4);
            if kind == COMMIT {
                if !checked_field(header, 16, seed) {
                    return Err(bad("invalid journal commit checksum"));
                }
                complete = true;
                break;
            }
            if !matches!(kind, DESCRIPTOR | REVOKE) {
                break;
            }
            if !checked_field(header, size - 4, seed) {
                return Err(bad("invalid journal metadata checksum"));
            }
            pos = advance(pos);
            if kind == REVOKE {
                let end = be32(header, 12) as usize;
                if !(16..=size - 4).contains(&end) || !(end - 16).is_multiple_of(8) {
                    return Err(bad("invalid journal revoke length"));
                }
                for value in header[16..end].as_chunks::<8>().0 {
                    revoked.insert(u64::from_be_bytes(*value));
                }
                continue;
            }
            let mut at = 12;
            loop {
                let tag = header
                    .get(at..at + 16)
                    .filter(|_| at + 16 <= size - 4)
                    .ok_or_else(|| bad("journal descriptor lacks final tag"))?;
                let flags = be32(tag, 4);
                let target = u64::from(be32(tag, 0)) | (u64::from(be32(tag, 8)) << 32);
                if flags & !0xb != 0 || target >= fs_blocks || !seen.insert(pos) {
                    return Err(bad("invalid journal block tag"));
                }
                at += 16;
                if flags & 2 == 0 {
                    let id = header
                        .get(at..at + 16)
                        .filter(|_| at + 16 <= size - 4)
                        .ok_or_else(|| bad("short journal tag UUID"))?;
                    if id != [0; 16] && id != fs_uuid {
                        return Err(bad("journal tag names a different filesystem"));
                    }
                    at += 16;
                }
                if update(update(seed, &sequence.to_be_bytes()), block(pos)) != be32(tag, 12) {
                    return Err(bad("journal payload checksum mismatch"));
                }
                // Escaped payloads start with JBD2 magic; inodes and extent roots cannot be decoded as such.
                if flags & 1 != 0 {
                    revoked.insert(target);
                } else if targets.insert(target, pos).is_some() {
                    return Err(bad("duplicate target in journal transaction"));
                }
                pos = advance(pos);
                if flags & 8 != 0 {
                    break;
                }
            }
        }
        if complete {
            // A wrapped transaction may be discovered first at a later descriptor. Keep the complete superset.
            for id in &revoked {
                targets.remove(id);
            }
            if let Some(existing) = ordered.get(&age) {
                let old: &Transaction = existing;
                if !old.blocks.iter().all(|(k, v)| targets.get(k) == Some(v)) {
                    return Err(bad("conflicting journal sequences"));
                }
            }
            consumed.extend(seen);
            ordered.insert(
                age,
                Transaction {
                    sequence,
                    blocks: targets,
                    unavailable: revoked,
                },
            );
        }
    }
    Ok(ordered.into_values().rev().collect())
}

/// Resolve an extent block at a transaction boundary. Uncommitted gaps, revocation
/// records and newer copies never supply data for an older inode.
pub(super) struct Versions {
    blocks: BTreeMap<u64, Vec<(usize, Option<usize>)>>,
    earliest: Vec<usize>,
}
impl Versions {
    pub fn new(history: &[Transaction]) -> Self {
        let mut blocks: BTreeMap<_, Vec<_>> = BTreeMap::new();
        let mut earliest = Vec::new();
        let mut begin = 0;
        for (idx, tx) in history.iter().enumerate() {
            if idx > 0 && history[idx - 1].sequence.wrapping_add(1) != tx.sequence {
                begin = idx;
            }
            earliest.push(begin);
            for (&block, &slot) in &tx.blocks {
                blocks.entry(block).or_default().push((idx, Some(slot)));
            }
            for &block in &tx.unavailable {
                blocks.entry(block).or_default().push((idx, None));
            }
        }
        Self { blocks, earliest }
    }
    pub fn slot(&self, block: u64, at: usize) -> Option<usize> {
        let versions = self.blocks.get(&block)?;
        let end = versions.partition_point(|(i, _)| *i <= at);
        let &(idx, slot) = versions[..end].last()?;
        if idx < self.earliest[at] { None } else { slot }
    }
}
