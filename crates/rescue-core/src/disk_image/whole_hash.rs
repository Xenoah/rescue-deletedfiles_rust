//! Find byte-exact copies when only the original size and whole-file digest survive.
mod stride;
use super::{
    ImportedFile,
    extents::{Extent, Layout, guard_image_output, restore_layout},
    fingerprint::strong,
};
use crate::{Error, Image, Result, canonical_output_path};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
pub use stride::{MatchedStride, StrideSearch, StrideSpacing};

pub const DEFAULT_HASH_WORK: u64 = 512 << 20;
const MAX_CANDIDATES: u64 = 1_000_000;

pub struct HashSearch<'a> {
    pub sha256: &'a str,
    pub file_bytes: u64,
    /// Position grid relative to start; also the split unit relative to first_offset.
    pub block_size: u32,
    pub start: u64,
    pub end: Option<u64>,
    pub first_offset: Option<u64>,
    /// Maximum logical bytes requested during search (final verified copy is separate).
    pub max_work: u64,
}

#[derive(Serialize)]
pub struct HashMatch {
    pub file: ImportedFile,
    pub extents: Vec<HashExtent>,
    pub original_sha256_verified: bool,
    pub candidates_tried: u64,
    pub search_bytes: u64,
    pub reads_with_gaps: u64,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub matched_stride: Option<MatchedStride>,
}

#[derive(Clone, Copy, Serialize)]
pub struct HashExtent {
    pub offset: u64,
    pub length: u64,
}

struct Search<'a> {
    image: &'a mut Image,
    cancel: &'a AtomicBool,
    target: [u8; 32],
    limit: u64,
    bytes: u64,
    candidates: u64,
    gaps: u64,
}

impl Search<'_> {
    // Charge even an unsuccessful read, and never pass substituted bytes to SHA-256.
    fn append(&mut self, digest: &mut Sha256, span: HashExtent) -> Result<bool> {
        let mut at = span.offset;
        let end = at + span.length;
        while at < end {
            if self.cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let take = (end - at).min(256 << 10);
            if take > self.limit - self.bytes {
                return Err(Error::Limit(format!(
                    "SHA-256 search byte budget exhausted after {} requested bytes and {} candidates",
                    self.bytes, self.candidates
                )));
            }
            self.bytes += take;
            match self.image.read(at, take as usize) {
                Ok(bytes) => digest.update(bytes),
                Err(Error::MissingRanges(_)) => {
                    self.gaps += 1;
                    return Ok(false);
                }
                Err(error) => return Err(error),
            }
            at += take;
        }
        Ok(true)
    }

    fn start_candidate(&mut self) -> Result<()> {
        if self.candidates >= MAX_CANDIDATES {
            return Err(Error::Limit(format!(
                "SHA-256 search candidate limit ({MAX_CANDIDATES}) exhausted"
            )));
        }
        self.candidates += 1;
        Ok(())
    }

    fn matches(&mut self, prefix: &Sha256, tail: HashExtent) -> Result<bool> {
        self.start_candidate()?;
        let mut digest = prefix.clone();
        Ok(self.append(&mut digest, tail)? && <[u8; 32]>::from(digest.finalize()) == self.target)
    }

    fn find(&mut self, req: &HashSearch<'_>, end: u64) -> Result<Vec<HashExtent>> {
        let step = u64::from(req.block_size);
        let size = req.file_bytes;
        let empty = Sha256::new();
        if size <= end - req.start {
            let last = end - size;
            let first = req.first_offset.unwrap_or(req.start);
            let stop = if req.first_offset.is_some() {
                first.min(last)
            } else {
                last
            };
            for offset in (first..=stop).step_by(step as usize) {
                let span = HashExtent {
                    offset,
                    length: size,
                };
                if self.matches(&empty, span)? {
                    return Ok(vec![span]);
                }
            }
        }
        if let Some(first) = req.first_offset {
            // Extend the same SHA state by one allocation unit per split.
            let mut prefix = empty;
            let mut split = step;
            while split < size && split <= end - first {
                let piece = HashExtent {
                    offset: first + (split - step),
                    length: step,
                };
                if !self.append(&mut prefix, piece)? {
                    break; // A longer first run would include exactly the same gap.
                }
                let tail_len = size - split;
                let head = HashExtent {
                    offset: first,
                    length: split,
                };
                // A tail may precede the first run, or start strictly after its end.
                // Enumerate just those two intervals on the caller's search grid.
                let before = first.checked_sub(tail_len).map(|last| (req.start, last));
                let after = (first + split)
                    .checked_add(1)
                    .and_then(|p| (p - req.start).div_ceil(step).checked_mul(step))
                    .and_then(|n| req.start.checked_add(n))
                    .zip(end.checked_sub(tail_len));
                for (begin, last) in [before, after].into_iter().flatten() {
                    for offset in (begin..=last).step_by(step as usize) {
                        let tail = HashExtent {
                            offset,
                            length: tail_len,
                        };
                        if self.matches(&prefix, tail)? {
                            return Ok(vec![head, tail]);
                        }
                    }
                }
                split += step;
            }
        }
        Err(self.no_match())
    }

    fn no_match(&self) -> Error {
        Error::Invalid(format!(
            "no whole-file SHA-256 match in the selected layouts: {} candidates tried, {} reads intersected missing ranges",
            self.candidates, self.gaps
        ))
    }
}

pub fn recover_by_hash(
    source: &Path,
    output: &Path,
    req: &HashSearch<'_>,
    cancel: &AtomicBool,
) -> Result<HashMatch> {
    search_and_restore(source, output, req, None, cancel)
}

/// Search only the requested regularly spaced layouts; every output is independently re-read and hashed.
pub fn recover_by_stride(
    source: &Path,
    output: &Path,
    req: &HashSearch<'_>,
    stride: StrideSearch,
    cancel: &AtomicBool,
) -> Result<HashMatch> {
    search_and_restore(source, output, req, Some(stride), cancel)
}

fn search_and_restore(
    source: &Path,
    output: &Path,
    req: &HashSearch<'_>,
    stride: Option<StrideSearch>,
    cancel: &AtomicBool,
) -> Result<HashMatch> {
    let target = strong(req.sha256)?;
    if req.file_bytes == 0 || req.file_bytes > 64 << 20 {
        return Err(Error::Request(
            "whole-file search requires a file length from 1 byte through 64 MiB".into(),
        ));
    }
    if !req.block_size.is_power_of_two() || req.block_size > 1 << 20 {
        return Err(Error::Request(
            "block size must be a power of two from 1 through 1048576".into(),
        ));
    }
    if req.max_work == 0 || req.max_work > 32 << 30 {
        return Err(Error::Request(
            "search byte budget must be from 1 byte through 32 GiB".into(),
        ));
    }
    let mut image = Image::open(source)?;
    let end = req.end.unwrap_or(image.length);
    if end > image.length
        || req.start >= end
        || req.first_offset.is_some_and(|p| p < req.start || p >= end)
    {
        return Err(Error::Request(
            "invalid search range or first offset outside the range".into(),
        ));
    }
    let output = canonical_output_path(output)?;
    guard_image_output(&image, &output)?;
    if output.try_exists()? {
        return Err(Error::Request(format!(
            "output exists: {}",
            output.display()
        )));
    }
    if req.file_bytes > end - req.start {
        return Err(Error::Invalid(
            "selected range is shorter than the requested file".into(),
        ));
    }
    let mut search = Search {
        image: &mut image,
        cancel,
        target,
        limit: req.max_work,
        bytes: 0,
        candidates: 0,
        gaps: 0,
    };
    let (extents, matched_stride) = if let Some(pattern) = stride {
        let (found, matched) = search.find_strided(req, end, pattern)?;
        (found, Some(matched))
    } else {
        (search.find(req, end)?, None)
    };
    let layout = Layout {
        schema_version: 1,
        partition: None,
        sha256: req.sha256.to_owned(),
        extents: extents
            .iter()
            .map(|s| Extent::Source {
                offset: s.offset,
                length: s.length,
            })
            .collect(),
    };
    let file = restore_layout(search.image, layout, &output, cancel)?;
    Ok(HashMatch {
        file,
        extents,
        original_sha256_verified: true,
        candidates_tried: search.candidates,
        search_bytes: search.bytes,
        reads_with_gaps: search.gaps,
        matched_stride,
    })
}
