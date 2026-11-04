//! Bounded, contiguous PNG carving. Deletion and allocation status remain unknown.
use crate::{
    AllocationStatus, Candidate, Error, ExtentEvidence, FileSystem, Progress, ProgressUnit,
    RecoveryData, Result, ScanOptions, Session, ntfs::Data, source::Image,
};
use sha2::{Digest, Sha256};
use std::{
    io::Cursor,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const WINDOW: u64 = 1024 * 1024;
const MAX_PNG: usize = 64 * 1024 * 1024;
const MAX_CHUNKS: usize = 65_536;
const VALIDATION_BUDGET: u64 = 4 * 1024 * 1024 * 1024;
const PROVENANCE: &str = "Contiguous PNG carving: original name, path, deletion and allocation status are unknown. Live files and embedded images may also be found.";

fn invalid(message: &str) -> Error {
    Error::Invalid(format!("PNG: {message}"))
}
fn charge(budget: &mut u64, amount: usize) -> Result<()> {
    *budget = budget
        .checked_sub(amount as u64)
        .ok_or_else(|| Error::Limit("PNG scan validation budget exhausted".into()))?;
    Ok(())
}

/// Independently determines the extent, validates every chunk CRC, then decodes pixels.
/// Text/ICC payloads are retained verbatim; their compressed contents are not interpreted.
fn validate(
    image: &mut Image,
    offset: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
) -> Result<Vec<u8>> {
    let mut bytes = image.read(offset, 8)?;
    charge(budget, 8)?;
    if bytes != SIGNATURE {
        return Err(invalid("signature"));
    }
    let mut has_idat = false;
    let mut ended_idat = false;
    let mut has_palette = false;
    let mut finished = false;
    for index in 0..MAX_CHUNKS {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let start = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("offset overflow"))?;
        let header = image.read(start, 8)?;
        charge(budget, 8)?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let kind = &header[4..8];
        if length > MAX_PNG || bytes.len().saturating_add(length + 12) > MAX_PNG {
            return Err(Error::Limit("PNG encoded extent exceeds 64 MiB".into()));
        }
        if !kind.iter().all(u8::is_ascii_alphabetic) || !kind[2].is_ascii_uppercase() {
            return Err(invalid("chunk type"));
        }
        if (index == 0 && (kind != b"IHDR" || length != 13)) || (index != 0 && kind == b"IHDR") {
            return Err(invalid("IHDR order or length"));
        }
        match kind {
            b"acTL" | b"fcTL" | b"fdAT" => {
                return Err(Error::Unsupported("animated PNG carving".into()));
            }
            b"PLTE" => {
                if has_palette
                    || has_idat
                    || length == 0
                    || length > 768
                    || !length.is_multiple_of(3)
                {
                    return Err(invalid("PLTE order or length"));
                }
                has_palette = true;
            }
            b"IDAT" => {
                if ended_idat {
                    return Err(invalid("nonconsecutive IDAT"));
                }
                has_idat = true;
            }
            b"IEND" => {
                if length != 0 || !has_idat {
                    return Err(invalid("IEND without image data"));
                }
            }
            b"IHDR" => {}
            _ if kind[0].is_ascii_uppercase() => {
                return Err(Error::Unsupported("unknown critical PNG chunk".into()));
            }
            _ => {}
        }
        if has_idat && kind != b"IDAT" {
            ended_idat = true;
        }
        charge(budget, length + 4)?;
        let body = image.read(start + 8, length + 4)?;
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(&body[..length]);
        if crc.finalize() != u32::from_be_bytes(body[length..].try_into().unwrap()) {
            return Err(invalid("chunk CRC mismatch"));
        }
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&body);
        if kind == b"IEND" {
            finished = true;
            break;
        }
    }
    if !finished {
        return Err(Error::Limit("PNG chunk count exceeds 65536".into()));
    }
    let mut decoder =
        png::Decoder::new_with_limits(Cursor::new(&bytes), png::Limits { bytes: MAX_PNG });
    decoder.ignore_checksums(false);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().map_err(|e| invalid(&e.to_string()))?;
    let length = reader
        .output_buffer_size()
        .filter(|n| *n <= MAX_PNG)
        .ok_or_else(|| Error::Limit("PNG decoded pixels exceed 64 MiB".into()))?;
    charge(budget, length)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut pixels = vec![0; length];
    reader
        .next_frame(&mut pixels)
        .map_err(|e| invalid(&e.to_string()))?;
    reader.finish().map_err(|e| invalid(&e.to_string()))?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(bytes)
}

fn name(offset: u64) -> String {
    format!("png_{offset:016x}.png")
}
fn id(offset: u64) -> String {
    format!("png-{offset:016x}")
}

/// Scans every byte of a regular image. `max_records` bounds signature attempts;
/// progress counters count source bytes, including the final signature overlap.
pub fn scan_carving(
    path: &Path,
    options: ScanOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Session> {
    let mut image = Image::open(path)?;
    let fingerprint = image.fingerprint()?;
    let mut session = Session {
        schema_version: 4, filesystem: FileSystem::Raw, id: Uuid::new_v4().to_string(),
        source: image.path.clone(), fingerprint, candidates: Vec::new(), scanned_records: 0,
        total_records: image.length, cancelled: false, truncated: false, invalid_records: 0,
        warnings: vec![PROVENANCE.into(), "PNG only; <=64 MiB encoded and decoded, <=65536 chunks per file; 4 GiB validation budget. Progress counters use bytes. Ancillary text/ICC contents are not semantically validated.".into()],
    };
    let mut offset = 0;
    let mut budget = VALIDATION_BUDGET;
    'scan: while offset < image.length {
        if cancel.load(Ordering::Relaxed) {
            session.cancelled = true;
            break;
        }
        let count = WINDOW.min(image.length - offset);
        let buffer = image.read(offset, (count + 7).min(image.length - offset) as usize)?;
        for (index, window) in buffer.windows(8).enumerate().take(count as usize) {
            if window != SIGNATURE {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                session.cancelled = true;
                break 'scan;
            }
            session.scanned_records = offset + index as u64;
            if session.candidates.len() >= options.max_candidates
                || session.candidates.len() as u64 >= options.max_records
            {
                session.truncated = true;
                break 'scan;
            }
            let at = offset + index as u64;
            let parsed = validate(&mut image, at, cancel, &mut budget);
            let mut candidate = Candidate {
                id: id(at),
                record: at,
                sequence: 0,
                stream_name: None,
                name: name(at),
                original_path: name(at),
                path_verified: false,
                size: None,
                supported: false,
                warnings: vec![PROVENANCE.into()],
                record_sha256: String::new(),
                extent_evidence: ExtentEvidence::Unknown,
                allocation_status: AllocationStatus::Unknown,
            };
            match parsed {
                Ok(bytes) => {
                    candidate.size = Some(bytes.len() as u64);
                    candidate.supported = true;
                    candidate.record_sha256 = format!("{:x}", Sha256::digest(&bytes));
                    candidate.extent_evidence = ExtentEvidence::PngValidated;
                }
                Err(Error::Cancelled) => {
                    session.cancelled = true;
                    break 'scan;
                }
                Err(Error::Io(e)) => return Err(e.into()),
                Err(e) => {
                    session.invalid_records += 1;
                    candidate.warnings.push(format!("{}: {e}", e.code()));
                    if matches!(&e, Error::Limit(reason) if reason.contains("budget")) {
                        session.truncated = true;
                        session.warnings.push(e.to_string());
                    }
                }
            }
            session.candidates.push(candidate);
            if session.truncated {
                break 'scan;
            }
        }
        offset += count;
        session.scanned_records = offset;
        progress(Progress {
            scanned_records: offset,
            total_records: image.length,
            candidates: session.candidates.len(),
            unit: ProgressUnit::Bytes,
        });
    }
    if session.truncated {
        session
            .warnings
            .push("Scan stopped at a resource limit; remaining bytes were not searched.".into());
    }
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    progress(Progress {
        scanned_records: session.scanned_records,
        total_records: image.length,
        candidates: session.candidates.len(),
        unit: ProgressUnit::Bytes,
    });
    Ok(session)
}

pub(crate) fn recovery_data(
    image: &mut Image,
    candidate: &Candidate,
    cancel: &AtomicBool,
) -> Result<RecoveryData> {
    if candidate.id != id(candidate.record)
        || candidate.sequence != 0
        || candidate.stream_name.is_some()
    {
        return Err(Error::SourceChanged);
    }
    let mut budget = VALIDATION_BUDGET;
    let bytes = validate(image, candidate.record, cancel, &mut budget)?;
    if candidate.record_sha256 != format!("{:x}", Sha256::digest(&bytes)) {
        return Err(Error::SourceChanged);
    }
    Ok(RecoveryData {
        data: Data::Resident(bytes),
        name: name(candidate.record),
        warnings: vec![PROVENANCE.into()],
        extent_evidence: ExtentEvidence::PngValidated,
        allocation_status: AllocationStatus::Unknown,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn signature_boundaries_limits_and_cancellation() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.raw");
        let mut input = vec![0; WINDOW as usize + 32];
        input[WINDOW as usize - 3..WINDOW as usize + 5].copy_from_slice(SIGNATURE);
        std::fs::write(&path, input).unwrap();
        let stop = AtomicBool::new(false);
        let s = scan_carving(&path, ScanOptions::default(), &stop, |_| {}).unwrap();
        assert_eq!(s.candidates.len(), 1);
        assert_eq!(s.candidates[0].record, WINDOW - 3);
        assert!(!s.candidates[0].supported);
        let s = scan_carving(
            &path,
            ScanOptions {
                max_records: 0,
                max_candidates: 1,
            },
            &stop,
            |_| {},
        )
        .unwrap();
        assert!(s.truncated && s.candidates.is_empty());
        stop.store(true, Ordering::Relaxed);
        let s = scan_carving(&path, ScanOptions::default(), &stop, |_| {}).unwrap();
        assert!(s.cancelled && s.scanned_records == 0);
        assert!(charge(&mut 7, 8).is_err());
    }
}
