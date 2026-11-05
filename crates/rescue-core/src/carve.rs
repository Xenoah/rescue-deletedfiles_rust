//! Bounded contiguous PNG/JPEG carving. Deletion and allocation remain unknown.
mod jpeg;
mod png;
use crate::{
    AllocationStatus, Candidate, Error, ExtentEvidence, FileSystem, Progress, ProgressUnit,
    RecoveryData, Result, ScanOptions, Session, ntfs::Data, source::Image,
};
use sha2::{Digest, Sha256};
use std::{
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;

const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const WINDOW: u64 = 1024 * 1024;
const VALIDATION_BUDGET: u64 = 4 * 1024 * 1024 * 1024;
const PROVENANCE: &str = "Contiguous PNG/JPEG carving: original name, path, deletion and allocation status are unknown. Live files and embedded images may also be found.";

fn charge(budget: &mut u64, amount: usize) -> Result<()> {
    *budget = budget
        .checked_sub(amount as u64)
        .ok_or_else(|| Error::Limit("carving scan validation budget exhausted".into()))?;
    Ok(())
}

#[derive(Clone, Copy)]
enum Format {
    Png,
    Jpeg,
}
impl Format {
    fn prefix(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpg",
        }
    }
    fn id(self, offset: u64) -> String {
        format!("{}-{offset:016x}", self.prefix())
    }
    fn name(self, offset: u64) -> String {
        format!("{}_{offset:016x}.{}", self.prefix(), self.extension())
    }
    fn evidence(self) -> ExtentEvidence {
        match self {
            Self::Png => ExtentEvidence::PngValidated,
            Self::Jpeg => ExtentEvidence::JpegValidated,
        }
    }
    fn allowed(self, extensions: &[String]) -> bool {
        extensions.is_empty()
            || extensions.iter().any(|ext| {
                let ext = ext
                    .trim()
                    .trim_start_matches("*.")
                    .trim_start_matches('.')
                    .to_ascii_lowercase();
                ext == "*"
                    || match self {
                        Self::Png => ext == "png",
                        Self::Jpeg => matches!(ext.as_str(), "jpg" | "jpeg" | "jpe"),
                    }
            })
    }
    fn validate(
        self,
        image: &mut Image,
        at: u64,
        cancel: &AtomicBool,
        budget: &mut u64,
        boundary: u64,
    ) -> Result<Vec<u8>> {
        match self {
            Self::Png => png::validate(image, at, cancel, budget, boundary),
            Self::Jpeg => jpeg::validate(image, at, cancel, budget, boundary),
        }
    }
}

/// Scans every byte of a regular image. `max_records` bounds signature attempts;
/// progress counters count source bytes, including the final signature overlap.
pub fn scan_carving(
    path: &Path,
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<Session> {
    scan_range(path, options, 0, None, &[], cancel, progress)
}

pub(crate) fn scan_range(
    path: &Path,
    options: ScanOptions,
    start: u64,
    end: Option<u64>,
    extensions: &[String],
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Session> {
    let mut image = Image::open(path)?;
    let fingerprint = image.fingerprint()?;
    let boundary = end.unwrap_or(image.length);
    if start > boundary || boundary > image.length {
        return Err(Error::Request("scan range outside image".into()));
    }
    let mut session = Session {
        schema_version: 6, filesystem: FileSystem::Raw, id: Uuid::new_v4().to_string(),
        source: image.path.clone(), fingerprint, candidates: Vec::new(), scanned_records: 0,
        total_records: boundary-start, cancelled: false, truncated: false, invalid_records: 0,
        search: None,
        region: None, volumes: Vec::new(),
        warnings: vec![PROVENANCE.into(), "PNG and 8-bit Huffman JPEG; <=64 MiB encoded / pixel reservation, <=128 MiB PNG scanlines or JPEG coefficients, <=65536 markers/chunks; JPEG <=256 scans. Shared 4 GiB validation budget. Progress uses bytes. Metadata contents, deletion status and original bytes are not established by structure or pixel decoding.".into()],
    };
    let png_allowed = Format::Png.allowed(extensions);
    let jpeg_allowed = Format::Jpeg.allowed(extensions);
    let mut offset = start;
    let mut budget = VALIDATION_BUDGET;
    'scan: while offset < boundary {
        if cancel.load(Ordering::Relaxed) {
            session.cancelled = true;
            break;
        }
        let count = WINDOW.min(boundary - offset);
        let buffer = image.read(offset, (count + 7).min(boundary - offset) as usize)?;
        for index in 0..count as usize {
            let rest = &buffer[index..];
            let format = if png_allowed && rest.starts_with(SIGNATURE) {
                Format::Png
            } else if jpeg_allowed && rest.starts_with(&[0xff, 0xd8, 0xff]) {
                Format::Jpeg
            } else {
                continue;
            };
            if cancel.load(Ordering::Relaxed) {
                session.cancelled = true;
                break 'scan;
            }
            session.scanned_records = offset + index as u64 - start;
            if session.candidates.len() >= options.max_candidates
                || session.candidates.len() as u64 >= options.max_records
            {
                session.truncated = true;
                break 'scan;
            }
            let at = offset + index as u64;
            let parsed = format.validate(&mut image, at, cancel, &mut budget, boundary);
            let mut candidate = Candidate {
                id: format.id(at),
                record: at,
                sequence: 0,
                stream_name: None,
                name: format.name(at),
                original_path: format.name(at),
                path_verified: false,
                size: None,
                supported: false,
                warnings: vec![PROVENANCE.into()],
                record_sha256: String::new(),
                extent_evidence: ExtentEvidence::Unknown,
                allocation_status: AllocationStatus::Unknown,
                created: None,
                modified: None,
            };
            match parsed {
                Ok(bytes) => {
                    candidate.size = Some(bytes.len() as u64);
                    candidate.supported = true;
                    candidate.record_sha256 = format!("{:x}", Sha256::digest(&bytes));
                    candidate.extent_evidence = format.evidence();
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
        session.scanned_records = offset - start;
        progress(Progress {
            scanned_records: offset - start,
            total_records: boundary - start,
            candidates: session.candidates.len(),
            unit: ProgressUnit::Bytes,
        });
    }
    if session.truncated {
        session
            .warnings
            .push("Scan stopped at a resource limit; remaining bytes were not searched.".into());
    }
    session.bound_candidates()?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    progress(Progress {
        scanned_records: session.scanned_records,
        total_records: boundary - start,
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
    let format = if candidate.id == Format::Png.id(candidate.record) {
        Format::Png
    } else if candidate.id == Format::Jpeg.id(candidate.record) {
        Format::Jpeg
    } else {
        return Err(Error::SourceChanged);
    };
    if candidate.id != format.id(candidate.record)
        || candidate.sequence != 0
        || candidate.stream_name.is_some()
    {
        return Err(Error::SourceChanged);
    }
    let mut budget = VALIDATION_BUDGET;
    let boundary = image.length;
    let bytes = format.validate(image, candidate.record, cancel, &mut budget, boundary)?;
    if candidate.record_sha256 != format!("{:x}", Sha256::digest(&bytes)) {
        return Err(Error::SourceChanged);
    }
    Ok(RecoveryData {
        data: Data::Resident(bytes),
        name: format.name(candidate.record),
        warnings: vec![PROVENANCE.into()],
        extent_evidence: format.evidence(),
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
