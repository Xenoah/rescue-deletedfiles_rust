//! Reassemble a known original from its chunk fingerprints without prior disk offsets.
use super::{
    ImportedFile,
    extents::{Extent, Layout, guard_file_output, guard_image_output, restore_layout},
    publish_output,
};
use crate::{Error, Image, Result, canonical_output_path};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashMap, HashSet},
    fs::File,
    io::{Read, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const WINDOW: usize = 1024 * 1024;
const MAX_CHUNKS: usize = 65_536;
const MANIFEST_LIMIT: u64 = 12 * 1024 * 1024;
const BASE: u64 = 0x100000001b3;
type Hash = [u8; 32];

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Chunk {
    rolling64: String,
    sha256: String,
}

#[derive(Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    schema_version: u32,
    chunk_size: u32,
    file_bytes: u64,
    sha256: String,
    chunks: Vec<Chunk>,
}

#[derive(Serialize)]
pub struct FingerprintedFile {
    pub manifest: ImportedFile,
    pub file_bytes: u64,
    pub chunks: usize,
    pub sha256: String,
}

#[derive(Serialize)]
pub struct MatchedFile {
    pub file: ImportedFile,
    pub matched_chunks: usize,
    pub distinct_chunks: usize,
    pub original_sha256_verified: bool,
}

fn cancel_check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn chunk_size_ok(value: u32) -> Result<()> {
    if (512..=WINDOW as u32).contains(&value) && value.is_power_of_two() {
        Ok(())
    } else {
        Err(Error::Request(
            "chunk size must be a power of two from 512 to 1048576".into(),
        ))
    }
}

fn rolling(bytes: &[u8]) -> u64 {
    bytes.iter().fold(0u64, |hash, &b| {
        hash.wrapping_mul(BASE).wrapping_add(u64::from(b))
    })
}

fn strong(s: &str) -> Result<Hash> {
    if s.len() != 64 || !s.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(Error::Request("invalid chunk manifest SHA-256".into()));
    }
    let mut result = [0; 32];
    for (i, v) in result.iter_mut().enumerate() {
        *v = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).expect("validated ASCII hex");
    }
    Ok(result)
}

pub fn fingerprint_file(
    source: &Path,
    output: &Path,
    chunk_size: u32,
    cancel: &AtomicBool,
) -> Result<FingerprintedFile> {
    chunk_size_ok(chunk_size)?;
    let source = crate::canonical_source_path(source)?;
    let output = canonical_output_path(output)?;
    guard_file_output(&source, &output)?;
    if !std::fs::metadata(&source)?.is_file() {
        return Err(Error::Request(
            "fingerprint-file requires a regular original file".into(),
        ));
    }
    let mut file = File::open(&source)?;
    let before = file.metadata()?;
    let size = before.len();
    let count = size.div_ceil(u64::from(chunk_size));
    if count > MAX_CHUNKS as u64 {
        return Err(Error::Limit(
            "fingerprint exceeds 65536 chunks; choose a larger chunk size".into(),
        ));
    }
    let mut chunks = Vec::with_capacity(count as usize);
    let mut buffer = vec![0; chunk_size as usize];
    let mut whole = Sha256::new();
    let mut remaining = size;
    while remaining > 0 {
        cancel_check(cancel)?;
        let length = remaining.min(u64::from(chunk_size)) as usize;
        let part = &mut buffer[..length];
        file.read_exact(part)?;
        whole.update(&*part);
        chunks.push(Chunk {
            rolling64: format!("{:016x}", rolling(part)),
            sha256: format!("{:x}", Sha256::digest(&*part)),
        });
        remaining -= length as u64;
    }
    let after = file.metadata()?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(Error::SourceChanged);
    }
    let sha256 = format!("{:x}", whole.finalize());
    let manifest = Manifest {
        schema_version: 1,
        chunk_size,
        file_bytes: size,
        sha256: sha256.clone(),
        chunks,
    };
    let out = publish_output(&output, cancel, |f| {
        let bytes = serde_json::to_vec(&manifest)?;
        f.write_all(&bytes)?;
        Ok((bytes.len() as u64, Sha256::new_with_prefix(&bytes)))
    })?;
    Ok(FingerprintedFile {
        manifest: out,
        file_bytes: size,
        chunks: count as usize,
        sha256,
    })
}

struct Group {
    length: usize,
    power: u64,
    wanted: HashMap<u64, HashSet<Hash>>,
}

impl Group {
    fn new(length: usize) -> Self {
        Self {
            length,
            power: BASE.wrapping_pow((length - 1) as u32),
            wanted: HashMap::new(),
        }
    }

    fn search(
        &mut self,
        block: &SearchWindow,
        found: &mut HashMap<(usize, Hash), u64>,
        remaining_checks: &mut u64,
    ) -> Result<()> {
        let n = self.length;
        if self.wanted.is_empty() || block.bytes.len() < n {
            return Ok(());
        }
        let mut at = ((block.alignment as u64 - block.offset % block.alignment as u64)
            % block.alignment as u64) as usize;
        let stop = block.starts.min(block.bytes.len() - n + 1);
        if at >= stop {
            return Ok(());
        }
        let all_zero = block.bytes.iter().all(|&v| v == 0);
        let mut weak = rolling(&block.bytes[at..at + n]);
        while at < stop {
            if let Some(set) = self.wanted.get_mut(&weak) {
                let absolute = block.offset + at as u64;
                let hash: Hash = Sha256::digest(&block.bytes[at..at + n]).into();
                if set.remove(&hash) {
                    found.insert((n, hash), absolute);
                    if set.is_empty() {
                        self.wanted.remove(&weak);
                    }
                    if self.wanted.is_empty() {
                        break;
                    }
                } else {
                    *remaining_checks = remaining_checks.checked_sub(n as u64).ok_or_else(|| Error::Limit(
                        "too many rolling fingerprint collisions; select a smaller range or a larger alignment".into()))?;
                }
            }
            // Every same-length slice of this readable interval is identical.
            if all_zero {
                break;
            }
            let next = at + block.alignment;
            if next >= stop {
                break;
            }
            if block.alignment >= n {
                weak = rolling(&block.bytes[next..next + n]);
            } else {
                for i in at..next {
                    weak = weak
                        .wrapping_sub(u64::from(block.bytes[i]).wrapping_mul(self.power))
                        .wrapping_mul(BASE)
                        .wrapping_add(u64::from(block.bytes[i + n]));
                }
            }
            at = next;
            if at & 0x3fff == 0 {
                cancel_check(block.cancel)?;
            }
        }
        Ok(())
    }
}

struct SearchWindow<'a> {
    bytes: &'a [u8],
    offset: u64,
    starts: usize,
    alignment: usize,
    cancel: &'a AtomicBool,
}

fn read_manifest(path: &Path) -> Result<Manifest> {
    if !std::fs::metadata(path)?.is_file() {
        return Err(Error::Request(
            "choose a regular fingerprint manifest".into(),
        ));
    }
    let mut bytes = Vec::new();
    File::open(path)?
        .take(MANIFEST_LIMIT + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MANIFEST_LIMIT {
        return Err(Error::Limit("fingerprint manifest exceeds 12 MiB".into()));
    }
    let manifest: Manifest = serde_json::from_slice(&bytes)?;
    chunk_size_ok(manifest.chunk_size)?;
    if manifest.schema_version != 1
        || manifest.chunks.len() > MAX_CHUNKS
        || manifest.file_bytes.div_ceil(u64::from(manifest.chunk_size))
            != manifest.chunks.len() as u64
    {
        return Err(Error::Request(
            "inconsistent file size or unsupported fingerprint manifest version".into(),
        ));
    }
    strong(&manifest.sha256)?;
    Ok(manifest)
}

pub fn recover_fingerprinted(
    source: &Path,
    manifest_path: &Path,
    output: &Path,
    alignment: u32,
    start: u64,
    end: Option<u64>,
    cancel: &AtomicBool,
) -> Result<MatchedFile> {
    if alignment == 0 || !alignment.is_power_of_two() || alignment > WINDOW as u32 {
        return Err(Error::Request(
            "alignment must be a power of two from 1 to 1048576".into(),
        ));
    }
    let manifest = read_manifest(manifest_path)?;
    let mut image = Image::open(source)?;
    let end = end.unwrap_or(image.length);
    if start > end || end > image.length {
        return Err(Error::Request(
            "fingerprint search range outside source".into(),
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
    let mut groups: BTreeMap<usize, Group> = BTreeMap::new();
    let mut sequence = Vec::with_capacity(manifest.chunks.len());
    let mut remaining = manifest.file_bytes;
    for chunk in manifest.chunks {
        let n = remaining.min(u64::from(manifest.chunk_size)) as usize;
        let hash = strong(&chunk.sha256)?;
        if chunk.rolling64.len() != 16 || !chunk.rolling64.bytes().all(|v| v.is_ascii_hexdigit()) {
            return Err(Error::Request(
                "invalid 16-digit rolling64 fingerprint".into(),
            ));
        }
        let weak = u64::from_str_radix(&chunk.rolling64, 16).expect("validated hex");
        groups
            .entry(n)
            .or_insert_with(|| Group::new(n))
            .wanted
            .entry(weak)
            .or_default()
            .insert(hash);
        sequence.push((n, hash));
        remaining -= n as u64;
    }
    let wanted: usize = groups
        .values()
        .flat_map(|group| group.wanted.values())
        .map(HashSet::len)
        .sum();
    let max_len = groups.keys().next_back().copied().unwrap_or(1);
    let mut found = HashMap::new();
    // A supplied weak fingerprint can be made to collide intentionally. Bound
    // redundant strong checks without limiting successful matches.
    let mut remaining_checks = (end - start).saturating_mul(8).max(256 * 1024 * 1024);
    let mut offset = start;
    while offset < end && found.len() < wanted {
        cancel_check(cancel)?;
        let starts = (end - offset).min(WINDOW as u64) as usize;
        let fetch = (end - offset).min((starts + max_len - 1) as u64) as usize;
        let (bytes, mut gaps) = image.read_available(offset, fetch)?;
        let base = image.absolute_offset(offset);
        gaps.sort_unstable_by_key(|g| g.source_offset);
        // Strict recovery will re-read all selected chunks. During discovery,
        // never hash a window that overlaps an unreadable source interval.
        let mut first = 0;
        let limits = gaps
            .iter()
            .map(|g| {
                let left = g.source_offset.saturating_sub(base).min(fetch as u64) as usize;
                let right = g
                    .source_offset
                    .saturating_add(g.length)
                    .saturating_sub(base)
                    .min(fetch as u64) as usize;
                (left, right)
            })
            .chain(std::iter::once((fetch, fetch)));
        for (left, right) in limits {
            if first < left && first < starts {
                let block = SearchWindow {
                    bytes: &bytes[first..left],
                    offset: offset + first as u64,
                    starts: (starts - first).min(left - first),
                    alignment: alignment as usize,
                    cancel,
                };
                for group in groups.values_mut() {
                    group.search(&block, &mut found, &mut remaining_checks)?;
                }
            }
            first = first.max(right);
        }
        offset += starts as u64;
    }
    if found.len() != wanted {
        return Err(Error::Invalid(format!(
            "matched {} of {} distinct fingerprinted chunks in readable source data",
            found.len(),
            wanted
        )));
    }
    let matched = sequence.len();
    let extents = sequence
        .into_iter()
        .map(|key| Extent::Source {
            offset: found[&key],
            length: key.0 as u64,
        })
        .collect();
    let layout = Layout {
        schema_version: 1,
        sha256: manifest.sha256,
        extents,
    };
    let file = restore_layout(&mut image, layout, &output, cancel)?;
    Ok(MatchedFile {
        file,
        matched_chunks: matched,
        distinct_chunks: wanted,
        original_sha256_verified: true,
    })
}
