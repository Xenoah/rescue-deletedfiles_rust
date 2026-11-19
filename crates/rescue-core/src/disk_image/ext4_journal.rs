//! Recover ext4 inode layouts from committed internal JBD2 transactions after unlink.
mod directory;
mod journal;
mod volume;
use super::extents::{Extent, guard_image_output, write_layout};
use super::publish_output;
use crate::{Error, Image, Result, canonical_output_path};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};
use volume::{Run, Volume, file_size, runs};

pub struct Ext4JournalRequest {
    pub source: PathBuf,
    pub inode: Option<u32>,
    pub output: Option<PathBuf>,
    pub sha256: Option<String>,
    pub allow_unverified_content: bool,
}
fn bad(reason: &str) -> Error {
    Error::Invalid(format!("ext4 journal: {reason}"))
}
fn le16(bytes: &[u8], at: usize) -> u16 {
    u16::from_le_bytes(bytes[at..at + 2].try_into().unwrap())
}
fn le32(bytes: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(bytes[at..at + 4].try_into().unwrap())
}
fn be32(bytes: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(bytes[at..at + 4].try_into().unwrap())
}
const MAX_JOURNAL: u64 = 256 << 20;

fn read_journal(vol: &mut Volume, cancel: &AtomicBool) -> Result<Vec<u8>> {
    let ino = vol.journal_ino;
    if ino != 8 {
        return Err(Error::Unsupported(
            "expected a standard internal journal inode".into(),
        ));
    }
    let inode = vol.current_inode(ino)?;
    if !vol.valid_inode(ino, &inode) || le16(&inode, 0) & 0xf000 != 0x8000 || vol.inode_free(ino)? {
        return Err(bad("invalid journal inode"));
    }
    let length = file_size(&inode);
    if !(1024..=MAX_JOURNAL).contains(&length) || !length.is_multiple_of(vol.block_size as u64) {
        return Err(Error::Unsupported(
            "expected a block-aligned ext4 journal of at most 256 MiB".into(),
        ));
    }
    let mut bytes = Vec::with_capacity(length as usize);
    let size = vol.block_size as u64;
    for run in runs(&inode, vol.blocks)? {
        if run.start * size != bytes.len() as u64
            || run.unwritten
            || (run.start + run.count) * size > length
        {
            return Err(bad("sparse or inconsistent journal inode"));
        }
        let mut at = 0;
        while at < run.count {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let count = (run.count - at).min((1 << 20) / size);
            bytes.extend(
                vol.image
                    .read((run.physical + at) * size, (count * size) as usize)?,
            );
            at += count;
        }
    }
    if bytes.len() as u64 != length {
        return Err(bad("incomplete journal inode"));
    }
    Ok(bytes)
}
struct Historical {
    sequence: u32,
    generation: u32,
    size: u64,
    runs: Vec<Run>,
    names: Vec<Value>,
}
fn candidates(
    vol: &mut Volume,
    bytes: &[u8],
    cancel: &AtomicBool,
) -> Result<BTreeMap<u32, Historical>> {
    let history = journal::read(bytes, vol.block_size, &vol.uuid, vol.blocks, cancel)?;
    let versions = journal::Versions::new(&history);
    let mut files = BTreeMap::new();
    // Inspect the newest inode-table copy first; an older layout cannot supersede an unsupported newer one.
    let mut seen = std::collections::BTreeSet::new();
    for (index, tx) in history.iter().enumerate().rev() {
        for (&target, &slot) in &tx.blocks {
            let Some(numbers) = vol.inode_numbers(target) else {
                continue;
            };
            let block = &bytes[slot * vol.block_size..(slot + 1) * vol.block_size];
            for (ino, inode) in numbers.zip(block.chunks_exact(vol.inode_size)) {
                if ino < vol.first_inode
                    || le16(inode, 0) & 0xf000 != 0x8000
                    || le16(inode, 26) == 0
                    || le32(inode, 20) != 0
                {
                    continue;
                }
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                if !seen.insert(ino) {
                    continue;
                }
                if seen.len() > 131072 {
                    return Err(Error::Limit("too many ext4 journal inodes".into()));
                }
                if !vol.valid_inode(ino, inode) {
                    return Err(bad("historical inode checksum mismatch"));
                }
                if !vol.inode_free(ino)? {
                    continue;
                }
                let live = vol.current_inode(ino)?;
                let generation = le32(inode, 100);
                if !vol.valid_inode(ino, &live)
                    || generation != le32(&live, 100)
                    || le32(&live, 20) == 0
                    || le16(&live, 26) != 0
                    || le16(&live, 0) & 0xf000 != 0x8000
                {
                    continue;
                }
                let extents = match volume::extent_tree(
                    inode,
                    vol.blocks,
                    vol.inode_seed(ino, inode),
                    |addr| {
                        let pos = versions.slot(addr,index).ok_or_else(|| {
                            Error::Unsupported(
                                "extent block is unavailable in contiguous committed journal history".into(),
                            )
                        })?;
                        Ok(&bytes[pos * vol.block_size..(pos + 1) * vol.block_size])
                    },
                ) {
                    Ok(runs) => runs,
                    Err(Error::Unsupported(_)) => continue,
                    Err(e) => return Err(e),
                };
                let size = file_size(inode);
                if size > 16 << 40 {
                    continue;
                }
                files.insert(
                    ino,
                    Historical {
                        sequence: tx.sequence,
                        generation,
                        size,
                        runs: extents,
                        names: Vec::new(),
                    },
                );
            }
        }
    }
    directory::attach(vol, bytes, &history, &mut files, cancel)?;
    Ok(files)
}

fn layout(vol: &mut Volume, file: &Historical) -> Result<Vec<Extent>> {
    let mut out = Vec::new();
    let mut end = 0;
    let size = vol.block_size as u64;
    for r in &file.runs {
        let start = r.start * size;
        if start >= file.size {
            break;
        }
        let length = (r.count * size).min(file.size - start);
        if start > end {
            out.push(Extent::Hole {
                zero_bytes: start - end,
            });
        }
        if r.unwritten {
            out.push(Extent::Hole { zero_bytes: length });
        } else {
            vol.blocks_free(r.physical, length.div_ceil(size))?;
            out.push(Extent::Source {
                offset: r.physical * size,
                length,
            });
        }
        end = start + length;
    }
    if end < file.size {
        out.push(Extent::Hole {
            zero_bytes: file.size - end,
        });
    }
    Ok(out)
}

pub fn ext4_journal(req: Ext4JournalRequest, cancel: &AtomicBool) -> Result<Value> {
    if req.output.is_some()
        && (req.inode.is_none() || req.sha256.is_some() == req.allow_unverified_content)
        || req.output.is_none() && (req.sha256.is_some() || req.allow_unverified_content)
        || req
            .sha256
            .as_ref()
            .is_some_and(|h| h.len() != 64 || !h.bytes().all(|c| c.is_ascii_hexdigit()))
    {
        return Err(Error::Request("export requires an inode, output, and either an original SHA-256 or explicit unverified-content selection".into()));
    }
    let mut image = Image::open(&req.source)?;
    let before = image.fingerprint()?;
    let mut vol = Volume::open(image)?;
    let journal = read_journal(&mut vol, cancel)?;
    let files = candidates(&mut vol, &journal, cancel)?;
    let mut rows = Vec::new();
    for (&ino, f) in &files {
        if req.inode.is_none_or(|n| n == ino) {
            rows.push(json!({"inode":ino,"inode_generation":f.generation,"journal_sequence":f.sequence,"size":f.size,"historical_names":f.names}));
        }
    }
    if req.inode.is_some() && rows.is_empty() {
        return Err(Error::Request(
            "inode has no supported committed pre-deletion layout".into(),
        ));
    }
    let mut report = json!({"filesystem":"ext4","journal":"JBD2 checksum v3","block_size":vol.block_size,"candidates":rows,
        "content_evidence":"ext4 metadata checksums do not cover ordinary file data"});
    if let Some(path) = req.output {
        let file = &files[&req.inode.unwrap()];
        let dest = canonical_output_path(&path)?;
        guard_image_output(&vol.image, &dest)?;
        let layout = layout(&mut vol, file)?;
        let saved = publish_output(&dest, cancel, |out| {
            let digest = write_layout(&mut vol.image, &layout, file.size, out, cancel)?;
            use sha2::Digest;
            if req.sha256.as_ref().is_some_and(|h| {
                !h.eq_ignore_ascii_case(&format!("{:x}", digest.clone().finalize()))
            }) {
                return Err(bad(
                    "recovered bytes differ from the supplied original SHA-256",
                ));
            }
            if vol.image.fingerprint()? != before {
                return Err(Error::SourceChanged);
            }
            Ok((file.size, digest))
        })?;
        report["saved"] = serde_json::to_value(saved)?;
        report["original_sha256_matched"] = json!(req.sha256.is_some());
    } else if vol.image.fingerprint()? != before {
        return Err(Error::SourceChanged);
    }
    Ok(report)
}
