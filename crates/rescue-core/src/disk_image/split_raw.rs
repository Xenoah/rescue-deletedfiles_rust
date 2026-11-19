//! Stream single or split RAW files in logical order; retain per-segment evidence.
use super::{ImageInput, ImportedFile, fingerprint::strong, publish_output};
use crate::{Error, Result, source::regular_file_identity};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, HashSet},
    ffi::OsString,
    fs::{self, File},
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
    time::SystemTime,
};

#[derive(Default, Clone, Copy)]
pub struct RawExpectation<'a> {
    pub bytes: Option<u64>,
    pub sha256: Option<&'a str>,
}

#[derive(Debug, Serialize)]
pub struct RawSegment {
    pub path: PathBuf,
    pub offset: u64,
    pub bytes: u64,
    pub sha256: String,
}

#[derive(Debug, Serialize)]
pub struct RawVerification {
    pub segments: Vec<RawSegment>,
    pub expected_length_verified: bool,
    pub expected_sha256_verified: bool,
}

// Use the starting suffix width as a minimum, including .999 -> .1000.
fn number(path: &Path) -> Option<(usize, usize)> {
    let ext = path.extension()?.to_str()?;
    if !(3..=9).contains(&ext.len()) || !ext.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    Some((ext.parse().ok()?, ext.len()))
}

pub(super) fn numbered_start(path: &Path) -> bool {
    matches!(number(path), Some((0 | 1, _)))
}

fn member_number(path: &Path, first: &Path, width: usize) -> Option<usize> {
    if path.file_stem() != first.file_stem() {
        return None;
    }
    let (n, actual) = number(path)?;
    let digits = n.checked_ilog10().unwrap_or(0) as usize + 1;
    (actual == width.max(digits)).then_some(n)
}

fn discover(first: &Path, cancel: &AtomicBool) -> Result<Vec<PathBuf>> {
    let width = first.extension().expect("numbered RAW").len();
    let mut ordered = BTreeMap::new();
    for entry in fs::read_dir(
        first
            .parent()
            .ok_or_else(|| Error::Request("missing segment parent".into()))?,
    )? {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let entry = entry?;
        let path = entry.path();
        if path.file_stem() != first.file_stem() {
            continue;
        }
        if let Some(n) = member_number(&path, first, width) {
            if !entry.file_type()?.is_file() {
                return Err(Error::Request(format!(
                    "numbered RAW segment must be a regular file: {}",
                    path.display()
                )));
            }
            if ordered.len() >= 65_536 {
                return Err(Error::Limit(
                    "automatic RAW discovery exceeds 65536 segments".into(),
                ));
            }
            ordered.insert(n, path);
        }
    }
    if ordered.first_key_value().map(|(_, p)| p.as_path()) != Some(first) {
        return Err(Error::Request(
            "select the first numbered RAW segment".into(),
        ));
    }
    Ok(ordered.into_values().collect())
}

fn validate_sequence(paths: &[PathBuf]) -> Result<()> {
    let first = &paths[0];
    let Some((base, width)) = number(first) else {
        return Ok(());
    };
    for (i, path) in paths.iter().enumerate().skip(1) {
        // Arbitrarily named explicit segments have no intrinsic order or checksums.
        if member_number(path, first, width).is_some_and(|n| n != base + i) {
            return Err(Error::Invalid(format!(
                "numbered RAW sequence has a gap or is out of order at {}",
                path.display()
            )));
        }
    }
    Ok(())
}

#[derive(PartialEq, Eq)]
struct Stamp {
    identity: String,
    bytes: u64,
    modified: Option<SystemTime>,
}

fn stamp(file: &File) -> Result<Stamp> {
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() == 0 {
        return Err(Error::Invalid(
            "RAW segments must be nonempty regular files".into(),
        ));
    }
    Ok(Stamp {
        identity: regular_file_identity(file)?,
        bytes: metadata.len(),
        modified: metadata.modified().ok(),
    })
}

pub(super) fn import(
    paths: &[OsString],
    output: &Path,
    expected: RawExpectation<'_>,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    let digest = expected.sha256.map(strong).transpose()?;
    let first = Path::new(&paths[0]);
    let members: Vec<PathBuf> = if paths.len() == 1 && numbered_start(first) {
        let found = discover(first, cancel)?;
        // Every implicitly selected sibling must pass the normal source, map and device checks.
        let input = ImageInput {
            source: found[0].clone(),
            segment: found[1..].to_vec(),
            image_type: Some("raw".into()),
            sector_size: None,
        };
        input
            .arguments(Some(output))?
            .1
            .into_iter()
            .map(PathBuf::from)
            .collect()
    } else {
        paths.iter().map(PathBuf::from).collect()
    };
    validate_sequence(&members)?;
    let output = crate::canonical_output_path(output)?;
    if output.parent() == first.parent()
        && number(first).is_some_and(|(_, width)| member_number(&output, first, width).is_some())
    {
        return Err(Error::Request(
            "output cannot be a member of the input numbered RAW set".into(),
        ));
    }
    let mut seen = HashSet::new();
    let mut snapshots = Vec::with_capacity(members.len());
    let mut total = 0u64;
    for path in &members {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        crate::source_scope::check(path)?;
        super::extents::guard_file_output(path, &output)?;
        let snapshot = stamp(&File::open(path)?)?;
        if !seen.insert(snapshot.identity.clone()) {
            return Err(Error::Request("duplicate RAW segment or file alias".into()));
        }
        total = total
            .checked_add(snapshot.bytes)
            .ok_or_else(|| Error::Limit("RAW length overflow".into()))?;
        snapshots.push(snapshot);
    }
    if expected.bytes.is_some_and(|n| n != total) {
        return Err(Error::Invalid(
            "RAW length differs from the expected byte count".into(),
        ));
    }
    let mut segments = Vec::with_capacity(members.len());
    let mut result = publish_output(&output, cancel, |out| {
        let mut logical = Sha256::new();
        let mut buf = vec![0; 256 << 10];
        let mut offset = 0;
        for (path, original) in members.iter().zip(snapshots) {
            let mut input = File::open(path)?;
            if stamp(&input)? != original {
                return Err(Error::Invalid("RAW segment changed before import".into()));
            }
            let mut part = Sha256::new();
            let mut left = original.bytes;
            while left > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let n = left.min(buf.len() as u64) as usize;
                input.read_exact(&mut buf[..n])?;
                out.write_all(&buf[..n])?;
                logical.update(&buf[..n]);
                part.update(&buf[..n]);
                left -= n as u64;
            }
            if stamp(&input)? != original {
                return Err(Error::Invalid("RAW segment changed during import".into()));
            }
            segments.push(RawSegment {
                path: path.clone(),
                offset,
                bytes: original.bytes,
                sha256: format!("{:x}", part.finalize()),
            });
            offset += original.bytes;
        }
        if digest.is_some_and(|want| <[u8; 32]>::from(logical.clone().finalize()) != want) {
            return Err(Error::Invalid(
                "RAW SHA-256 differs from the expected digest".into(),
            ));
        }
        Ok((total, logical))
    })?;
    result.raw = Some(RawVerification {
        segments,
        expected_length_verified: expected.bytes.is_some(),
        expected_sha256_verified: digest.is_some(),
    });
    Ok(result)
}
