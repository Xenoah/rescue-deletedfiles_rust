//! Bounded contiguous image/document/media carving. Deletion and allocation remain unknown.
mod container;
mod indexed;
pub(crate) use indexed::{resume as resume_indexed, scan as scan_indexed};
mod jpeg;
mod movie;
mod pdf;
mod png;
mod zip;
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
const PROVENANCE: &str = "Contiguous carving: original name, path, deletion and allocation status are unknown. Live files and embedded images may also be found.";

fn charge(budget: &mut u64, amount: usize) -> Result<()> {
    *budget = budget
        .checked_sub(amount as u64)
        .ok_or_else(|| Error::Limit("carving scan validation budget exhausted".into()))?;
    Ok(())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Format {
    Png,
    Jpeg,
    Pdf,
    Zip,
    Docx,
    Xlsx,
    Pptx,
    Mp4,
    Mov,
}
impl Format {
    fn prefix(self) -> &'static str {
        match self {
            Self::Png => "png",
            Self::Jpeg => "jpeg",
            Self::Pdf => "pdf",
            Self::Zip => "zip",
            Self::Docx => "docx",
            Self::Xlsx => "xlsx",
            Self::Pptx => "pptx",
            Self::Mp4 => "mp4",
            Self::Mov => "mov",
        }
    }
    fn extension(self) -> &'static str {
        match self {
            Self::Jpeg => "jpg",
            other => other.prefix(),
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
            Self::Pdf => ExtentEvidence::PdfStructure,
            Self::Zip | Self::Docx | Self::Xlsx | Self::Pptx => ExtentEvidence::ZipCrcValidated,
            Self::Mp4 | Self::Mov => ExtentEvidence::MediaStructure,
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
                        other => ext == other.extension(),
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
    ) -> Result<(Format, Vec<u8>, Vec<usize>)> {
        match self {
            Self::Png => {
                png::validate(image, at, cancel, budget, boundary).map(|b| (self, b, Vec::new()))
            }
            Self::Jpeg => {
                jpeg::validate(image, at, cancel, budget, boundary).map(|b| (self, b, Vec::new()))
            }
            Self::Pdf => {
                pdf::validate(image, at, cancel, budget, boundary).map(|b| (self, b, Vec::new()))
            }
            Self::Zip | Self::Docx | Self::Xlsx | Self::Pptx => {
                zip::validate(image, at, cancel, budget, boundary)
            }
            Self::Mp4 | Self::Mov => movie::validate(image, at, cancel, budget, boundary)
                .map(|(format, b)| (format, b, Vec::new())),
        }
    }
}

fn format_warnings(format: Format) -> &'static [&'static str] {
    match format {
        Format::Pdf => &[
            "Classic-xref PDF: references, page tree and declared stream extents validated. Encrypted/xref-stream PDFs are unsupported. Stream codecs, scripts and page rendering are not executed. EOF-based extent is an estimate; trailing padding cannot be established.",
        ],
        Format::Zip | Format::Docx | Format::Xlsx | Format::Pptx => &[
            "ZIP central/local records, decoded lengths and every entry CRC validated. Office classification identifies a container, not document rendering or macro safety. Stored/DEFLATE only, no encryption/multi-volume; 4096 entries, 128 MiB decoded total, 64 MiB per entry.",
        ],
        Format::Mp4 | Format::Mov => &[
            "Non-fragmented MP4/QuickTime container, complete sample tables and mdat ranges validated. Encoded video/audio frames are not decoded; original content is unknown. ftyp required, no zero-sized boxes, 1048576 samples, 256 tracks.",
        ],
        _ => &[],
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
        raid: None,
        schema_version: 6, filesystem: FileSystem::Raw, id: Uuid::new_v4().to_string(),
        source: image.path.clone(), fingerprint, candidates: Vec::new(), scanned_records: 0,
        total_records: boundary-start, cancelled: false, truncated: false, invalid_records: 0,
        search: None,
        region: None, volumes: Vec::new(),
        warnings: vec![PROVENANCE.into(), "PNG/JPEG/PDF/ZIP/Office/MP4/MOV contiguous carving; <=64 MiB encoded / pixel reservation, <=128 MiB PNG scanlines or JPEG coefficients, <=65536 markers/chunks; JPEG <=256 scans. Shared 4 GiB validation budget. Progress uses bytes. Metadata contents, deletion status and original bytes are not established by structure or pixel decoding.".into()],
    };
    let png_allowed = Format::Png.allowed(extensions);
    let jpeg_allowed = Format::Jpeg.allowed(extensions);
    let pdf_allowed = Format::Pdf.allowed(extensions);
    let zip_allowed = [Format::Zip, Format::Docx, Format::Xlsx, Format::Pptx]
        .iter()
        .any(|f| f.allowed(extensions));
    let movie_allowed = Format::Mp4.allowed(extensions) || Format::Mov.allowed(extensions);
    let mut zip_records = std::collections::BTreeSet::new();
    let mut attempts = 0u64;
    let mut offset = start;
    let mut budget = VALIDATION_BUDGET;
    let mut missing = false;
    'scan: while offset < boundary {
        if cancel.load(Ordering::Relaxed) {
            session.cancelled = true;
            break;
        }
        let count = WINDOW.min(boundary - offset);
        let (buffer, gaps) =
            image.read_available(offset, (count + 21).min(boundary - offset) as usize)?;
        missing |= !gaps.is_empty();
        for index in 0..count as usize {
            let rest = &buffer[index..];
            let format = if png_allowed && rest.starts_with(SIGNATURE) {
                Format::Png
            } else if jpeg_allowed && rest.starts_with(&[0xff, 0xd8, 0xff]) {
                Format::Jpeg
            } else if pdf_allowed && rest.starts_with(b"%PDF-") {
                Format::Pdf
            } else if zip_allowed
                && (rest.starts_with(b"PK\x03\x04")
                    || (rest.len() >= 22
                        && rest.starts_with(b"PK\x05\x06")
                        && rest[4..20] == [0; 16]))
            {
                Format::Zip
            } else if movie_allowed && rest.len() >= 8 && &rest[4..8] == b"ftyp" {
                Format::Mp4
            } else {
                continue;
            };
            if format == Format::Zip && zip_records.remove(&(offset + index as u64)) {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                session.cancelled = true;
                break 'scan;
            }
            session.scanned_records = offset + index as u64 - start;
            if session.candidates.len() >= options.max_candidates || attempts >= options.max_records
            {
                session.truncated = true;
                break 'scan;
            }
            attempts += 1;
            let at = offset + index as u64;
            let parsed = format.validate(&mut image, at, cancel, &mut budget, boundary);
            let mut candidate = Candidate {
                fragment_offset: None,
                id: format.id(at),
                record: at,
                sequence: 0,
                stream_name: None,
                name: format.name(at),
                original_path: format.name(at),
                path_verified: false,
                alternate_names: Vec::new(),
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
                Ok((validated_format, bytes, records)) => {
                    candidate.id = validated_format.id(at);
                    candidate.name = validated_format.name(at);
                    candidate.original_path = candidate.name.clone();
                    candidate.warnings.extend(
                        format_warnings(validated_format)
                            .iter()
                            .map(|s| s.to_string()),
                    );
                    for record in records.into_iter().filter(|r| *r > 0) {
                        if zip_records.len() >= 1_048_576 {
                            session.truncated = true;
                            session.warnings.push(
                                "ZIP local-record index limit; remaining source was not scanned"
                                    .into(),
                            );
                            break;
                        }
                        zip_records.insert(at + record as u64);
                    }
                    candidate.size = Some(bytes.len() as u64);
                    candidate.supported = true;
                    candidate.record_sha256 = format!("{:x}", Sha256::digest(&bytes));
                    candidate.extent_evidence = validated_format.evidence();
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
            if !matches!(format, Format::Png | Format::Jpeg) {
                session.schema_version = 7;
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
    if missing {
        session.truncated = true;
        session.warnings.push("MISSING_RANGES: acquired image has unread/failed bytes; signatures there were not searched. Parsers reject payloads crossing a missing range.".into());
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
    image.annotate_coverage(&mut session);
    Ok(session)
}

pub(crate) fn recovery_data(
    image: &mut Image,
    candidate: &Candidate,
    cancel: &AtomicBool,
) -> Result<RecoveryData> {
    let format = [
        Format::Png,
        Format::Jpeg,
        Format::Pdf,
        Format::Zip,
        Format::Docx,
        Format::Xlsx,
        Format::Pptx,
        Format::Mp4,
        Format::Mov,
    ]
    .into_iter()
    .find(|f| candidate.id == f.id(candidate.record))
    .ok_or(Error::SourceChanged)?;
    if candidate.id != format.id(candidate.record)
        || candidate.sequence != 0
        || candidate.stream_name.is_some()
    {
        return Err(Error::SourceChanged);
    }
    let mut budget = VALIDATION_BUDGET;
    let boundary = image.length;
    let (actual, bytes, _) =
        format.validate(image, candidate.record, cancel, &mut budget, boundary)?;
    if actual != format {
        return Err(Error::SourceChanged);
    }
    if candidate.record_sha256 != format!("{:x}", Sha256::digest(&bytes)) {
        return Err(Error::SourceChanged);
    }
    Ok(RecoveryData {
        data: Data::Resident(bytes),
        name: format.name(candidate.record),
        warnings: std::iter::once(PROVENANCE)
            .chain(format_warnings(format).iter().copied())
            .map(str::to_owned)
            .collect(),
        extent_evidence: format.evidence(),
        allocation_status: AllocationStatus::Unknown,
    })
}

pub(crate) fn revalidate_candidate(
    image: &mut Image,
    candidate: &Candidate,
    cancel: &AtomicBool,
) -> Result<Candidate> {
    let data = recovery_data(image, candidate, cancel)?;
    Ok(Candidate {
        id: candidate.id.clone(),
        record: candidate.record,
        sequence: 0,
        fragment_offset: None,
        stream_name: None,
        original_path: data.name.clone(),
        name: data.name,
        path_verified: false,
        alternate_names: Vec::new(),
        size: Some(data.data.size()),
        supported: true,
        warnings: data.warnings,
        record_sha256: candidate.record_sha256.clone(),
        extent_evidence: data.extent_evidence,
        allocation_status: data.allocation_status,
        created: None,
        modified: None,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn document_mutations_remain_bounded() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("mutated.raw");
        let cancel = AtomicBool::new(false);
        let samples: &[(Format, &[u8])] = &[
            (
                Format::Pdf,
                include_bytes!("../../../tests/corpus/files/sample.pdf"),
            ),
            (
                Format::Zip,
                include_bytes!("../../../tests/corpus/files/sample.zip"),
            ),
            (
                Format::Docx,
                include_bytes!("../../../tests/corpus/files/sample.docx"),
            ),
            (
                Format::Mp4,
                include_bytes!("../../../tests/corpus/files/sample.mp4"),
            ),
        ];
        let mut random = 0xb10c_5eed_u64;
        for (format, original) in samples {
            for case in 0..256 {
                let mut bytes = original.to_vec();
                for _ in 0..4 {
                    random = random.wrapping_mul(6364136223846793005).wrapping_add(1);
                    let at = random as usize % bytes.len();
                    bytes[at] = (random >> 32) as u8;
                }
                if case % 3 == 0 {
                    bytes.truncate(random as usize % bytes.len());
                }
                std::fs::write(&path, &bytes).unwrap();
                let mut image = Image::open(&path).unwrap();
                // Result may be unsupported or even another valid structure; panic is always wrong.
                let _ = format.validate(
                    &mut image,
                    0,
                    &cancel,
                    &mut (4 * 1024 * 1024),
                    bytes.len() as u64,
                );
            }
        }
    }
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
                fragments: false,
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
