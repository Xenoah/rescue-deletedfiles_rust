//! Bind historical file names to a checksummed directory and inode from the same transaction.
use super::{Historical, Volume, bad, journal::Transaction, le16, le32, volume::runs};
use crate::{Error, Result, crc32c::update};
use serde_json::json;
use std::{
    collections::{BTreeMap, BTreeSet},
    sync::atomic::{AtomicBool, Ordering},
};

fn journal_inode<'a>(
    vol: &Volume,
    journal: &'a [u8],
    tx: &Transaction,
    ino: u32,
) -> Option<&'a [u8]> {
    let (block, offset) = vol.inode_address(ino)?;
    let slot = tx.blocks.get(&block)?;
    let start = slot * vol.block_size + offset;
    journal.get(start..start + vol.inode_size)
}

pub(super) fn attach(
    vol: &Volume,
    journal: &[u8],
    history: &[Transaction],
    files: &mut BTreeMap<u32, Historical>,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut recorded = BTreeSet::new();
    let mut total = 0;
    for tx in history.iter().rev() {
        for (&block, &slot) in &tx.blocks {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let data = &journal[slot * vol.block_size..(slot + 1) * vol.block_size];
            if data[4..9] != [12, 0, 1, 2, b'.'] || data[18..22] != [2, 2, b'.', b'.'] {
                continue;
            }
            let parent = le32(data, 0);
            let Some(dir) = journal_inode(vol, journal, tx, parent) else {
                continue;
            };
            if le16(dir, 0) & 0xf000 != 0x4000
                || !vol.valid_inode(parent, dir)
                || le32(dir, 32) & 0x1000 != 0
            {
                continue;
            }
            let Ok(layout) = runs(dir, vol.blocks) else {
                continue;
            };
            if !layout
                .first()
                .is_some_and(|r| r.start == 0 && !r.unwritten && r.physical == block)
            {
                continue;
            }
            let end = data.len() - 12;
            if data[end..end + 8] != [0, 0, 0, 0, 12, 0, 0, 0xde]
                || update(vol.inode_seed(parent, dir), &data[..end]) != le32(data, end + 8)
            {
                return Err(bad("directory checksum mismatch"));
            }
            let mut at = 0;
            while at < end {
                if end - at < 8 {
                    return Err(bad("short directory entry"));
                }
                let inode = le32(data, at);
                let len = usize::from(le16(data, at + 4));
                let n = usize::from(data[at + 6]);
                if len < 8 + n || len > end - at || !len.is_multiple_of(4) {
                    return Err(bad("invalid directory record length"));
                }
                if data[at + 7] == 1
                    && let Some(file) = files.get_mut(&inode)
                    && let Some(historic) = journal_inode(vol, journal, tx, inode)
                    && file.generation == le32(historic, 100)
                    && le16(historic, 0) & 0xf000 == 0x8000
                    && le16(historic, 26) > 0
                    && le32(historic, 20) == 0
                    && vol.valid_inode(inode, historic)
                {
                    let name = &data[at + 8..at + 8 + n];
                    if name.is_empty()
                        || name.contains(&0)
                        || name.contains(&b'/')
                        || name == b"."
                        || name == b".."
                    {
                        return Err(bad("invalid journal file name"));
                    }
                    if recorded.insert((inode, parent, name.to_vec())) {
                        total += name.len();
                        if total > 8 << 20 || recorded.len() > 65536 {
                            return Err(Error::Limit("too many historical ext4 names".into()));
                        }
                        file.names.push(
                            json!({"directory_inode":parent,"directory_generation":le32(dir,100),
                            "journal_sequence":tx.sequence,"name":String::from_utf8_lossy(name),
                            "name_hex":name.iter().map(|b|format!("{b:02x}")).collect::<String>()}),
                        );
                    }
                }
                at += len;
            }
        }
    }
    Ok(())
}
