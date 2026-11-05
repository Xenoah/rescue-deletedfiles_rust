//! Durable signature enumeration. Parsers and their bounds are shared with the
//! in-memory path; only candidate storage and traversal checkpoints differ.
use super::*;
use crate::{AcquisitionIdentity, IndexWriter, IndexedProgress, SearchSettings};
use serde::{Deserialize, Serialize};
use std::{collections::BTreeSet, path::PathBuf};

#[derive(Serialize, Deserialize)]
struct Checkpoint {
    version: u32,
    identity: AcquisitionIdentity,
    options: ScanOptions,
    start: u64,
    end: u64,
    next: u64,
    attempts: u64,
    validation_budget: u64,
    zip_records: BTreeSet<u64>,
    missing: bool,
}
pub(crate) fn scan(
    path: &Path,
    work: &Path,
    options: ScanOptions,
    settings: &SearchSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(IndexedProgress),
) -> Result<PathBuf> {
    crate::guard_output_path(path, work)?;
    let mut image = Image::open(path)?;
    let end = settings.range_end.unwrap_or(image.length);
    if settings.range_start > end || end > image.length {
        return Err(Error::Request("scan range outside image".into()));
    }
    let mut header=Session {
        raid:None,schema_version:7,filesystem:FileSystem::Raw,id:Uuid::new_v4().to_string(),source:image.path.clone(),fingerprint:image.fingerprint()?,candidates:vec![],scanned_records:0,total_records:end-settings.range_start,cancelled:false,truncated:false,invalid_records:0,warnings:vec![PROVENANCE.into(),"Bounded contiguous carving with a shared 4 GiB validation budget. Progress counts source bytes. Original names, dates and deletion status are unknown.".into()],search:Some(settings.clone()),region:None,volumes:vec![],
    };
    image.annotate_coverage(&mut header);
    let checkpoint = Checkpoint {
        version: 1,
        identity: image.acquisition_identity()?,
        options,
        start: settings.range_start,
        end,
        next: settings.range_start,
        attempts: 0,
        validation_budget: VALIDATION_BUDGET,
        zip_records: BTreeSet::new(),
        missing: false,
    };
    let writer = IndexWriter::create(work, &header, "carve_signatures_v1", &checkpoint)?;
    resume(writer, cancel, progress)
}
pub(crate) fn resume(
    mut writer: IndexWriter,
    cancel: &AtomicBool,
    mut progress: impl FnMut(IndexedProgress),
) -> Result<PathBuf> {
    let path = writer.index().path().to_owned();
    let mut header = writer.index().session()?;
    let mut saved: Checkpoint = writer.index().checkpoint()?;
    let settings = header
        .search
        .clone()
        .ok_or_else(|| Error::Request("missing carving settings".into()))?;
    settings.validate()?;
    let mut image = Image::open(&header.source)?;
    if saved.version != 1
        || saved.start > saved.next
        || saved.next > saved.end
        || saved.end > image.length
        || saved.validation_budget > VALIDATION_BUDGET
        || saved.zip_records.len() > 1_048_576
        || saved
            .zip_records
            .iter()
            .any(|n| *n < saved.next || *n >= saved.end)
        || settings.method != crate::ScanMethod::Carve
        || settings.range_start != saved.start
        || settings.range_end.unwrap_or(image.length) != saved.end
        || header.scanned_records != saved.next - saved.start
    {
        return Err(Error::Request(
            "invalid carving checkpoint or selected range".into(),
        ));
    }
    if image.acquisition_identity()? != saved.identity || image.fingerprint()? != header.fingerprint
    {
        return Err(Error::SourceChanged);
    }
    image.validate_raid(&header)?;
    header.cancelled = false;
    let mut committed = writer.index().status()?.candidates;
    let mut batch = Vec::new();
    let mut limited = false;
    notify(&path, &header, committed, &mut progress);
    'scan: while saved.next < saved.end {
        if cancel.load(Ordering::Relaxed) {
            header.cancelled = true;
            break;
        }
        let start = saved.next;
        let count = WINDOW.min(saved.end - start);
        let (buffer, gaps) =
            image.read_available(start, (count + 21).min(saved.end - start) as usize)?;
        saved.missing |= !gaps.is_empty();
        for index in 0..count as usize {
            let at = start + index as u64;
            if index.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
                header.cancelled = true;
                saved.next = at;
                break 'scan;
            }
            let rest = &buffer[index..];
            let Some(format) = detect(rest, &settings.extensions) else {
                continue;
            };
            if format == Format::Zip && saved.zip_records.remove(&at) {
                continue;
            }
            saved.next = at;
            if committed + batch.len() as u64 >= saved.options.max_candidates as u64
                || saved.attempts >= saved.options.max_records
            {
                limited = true;
                break 'scan;
            }
            saved.attempts += 1;
            let mut candidate = Candidate {
                fragment_offset: None,
                id: format.id(at),
                record: at,
                sequence: 0,
                stream_name: None,
                name: format.name(at),
                original_path: format.name(at),
                path_verified: false,
                alternate_names: vec![],
                size: None,
                supported: false,
                warnings: vec![PROVENANCE.into()],
                record_sha256: String::new(),
                extent_evidence: ExtentEvidence::Unknown,
                allocation_status: AllocationStatus::Unknown,
                created: None,
                modified: None,
            };
            match format.validate(
                &mut image,
                at,
                cancel,
                &mut saved.validation_budget,
                saved.end,
            ) {
                Ok((found, bytes, records)) => {
                    candidate.id = found.id(at);
                    candidate.name = found.name(at);
                    candidate.original_path = candidate.name.clone();
                    candidate
                        .warnings
                        .extend(format_warnings(found).iter().map(|s| s.to_string()));
                    candidate.size = Some(bytes.len() as u64);
                    candidate.supported = true;
                    candidate.record_sha256 = format!("{:x}", Sha256::digest(&bytes));
                    candidate.extent_evidence = found.evidence();
                    for record in records.into_iter().filter(|r| *r > 0) {
                        if saved.zip_records.len() >= 1_048_576 {
                            limited = true;
                            header.warnings.push(
                                "ZIP local-record index limit; remaining source was not scanned"
                                    .into(),
                            );
                            break;
                        }
                        let address = at
                            .checked_add(record as u64)
                            .filter(|r| *r < saved.end)
                            .ok_or_else(|| {
                                Error::Invalid("ZIP record outside selected range".into())
                            })?;
                        saved.zip_records.insert(address);
                    }
                }
                Err(Error::Cancelled) => {
                    header.cancelled = true;
                    break 'scan;
                }
                Err(Error::Io(e)) => return Err(e.into()),
                Err(error) => {
                    header.invalid_records += 1;
                    candidate
                        .warnings
                        .push(format!("{}: {error}", error.code()));
                    if matches!(&error,Error::Limit(reason) if reason.contains("budget")) {
                        limited = true;
                        header.warnings.push(error.to_string());
                    }
                }
            }
            if settings.matches(&candidate) {
                batch.push(candidate);
            }
            saved.next = at + 1;
            if batch.len() >= 128 {
                commit(&mut writer, &image, &mut header, &batch, "running", &saved)?;
                committed += batch.len() as u64;
                batch.clear();
                notify(&path, &header, committed, &mut progress);
            }
            if limited {
                break 'scan;
            }
        }
        saved.next = start + count;
        commit(&mut writer, &image, &mut header, &batch, "running", &saved)?;
        committed += batch.len() as u64;
        batch.clear();
        notify(&path, &header, committed, &mut progress);
    }
    if limited {
        header.truncated = true;
        header
            .warnings
            .push("Scan stopped at a resource limit; remaining bytes were not searched.".into());
    }
    if saved.missing {
        header.truncated = true;
        header.warnings.push("MISSING_RANGES: failed/unread signatures were not searched; payloads crossing gaps are unavailable.".into());
    }
    if image.fingerprint()? != header.fingerprint {
        return Err(Error::SourceChanged);
    }
    let state = if header.cancelled {
        "cancelled"
    } else if header.truncated || header.invalid_records > 0 {
        "partial"
    } else {
        "completed"
    };
    commit(&mut writer, &image, &mut header, &batch, state, &saved)?;
    committed += batch.len() as u64;
    notify(&path, &header, committed, &mut progress);
    Ok(path)
}
fn detect(bytes: &[u8], extensions: &[String]) -> Option<Format> {
    if bytes.starts_with(SIGNATURE) && Format::Png.allowed(extensions) {
        Some(Format::Png)
    } else if bytes.starts_with(&[0xff, 0xd8, 0xff]) && Format::Jpeg.allowed(extensions) {
        Some(Format::Jpeg)
    } else if bytes.starts_with(b"%PDF-") && Format::Pdf.allowed(extensions) {
        Some(Format::Pdf)
    } else if (bytes.starts_with(b"PK\x03\x04")
        || (bytes.len() >= 22 && bytes.starts_with(b"PK\x05\x06") && bytes[4..20] == [0; 16]))
        && [Format::Zip, Format::Docx, Format::Xlsx, Format::Pptx]
            .iter()
            .any(|f| f.allowed(extensions))
    {
        Some(Format::Zip)
    } else if bytes.len() >= 8
        && &bytes[4..8] == b"ftyp"
        && (Format::Mp4.allowed(extensions) || Format::Mov.allowed(extensions))
    {
        Some(Format::Mp4)
    } else {
        None
    }
}
fn commit(
    writer: &mut IndexWriter,
    image: &Image,
    header: &mut Session,
    batch: &[Candidate],
    state: &str,
    saved: &Checkpoint,
) -> Result<()> {
    if image.acquisition_identity()? != saved.identity {
        return Err(Error::SourceChanged);
    }
    header.scanned_records = saved.next - saved.start;
    writer.commit(
        header,
        batch,
        state,
        matches!(state, "running" | "cancelled"),
        saved,
    )
}
fn notify(path: &Path, header: &Session, count: u64, progress: &mut impl FnMut(IndexedProgress)) {
    progress(IndexedProgress {
        index: path.to_owned(),
        session_id: header.id.clone(),
        progress: Progress {
            scanned_records: header.scanned_records,
            total_records: header.total_records,
            candidates: count as usize,
            unit: ProgressUnit::Bytes,
        },
        committed_candidates: count,
    });
}
