//! Explicit, uncertain old-MFT evidence. Never invent a chain or an original path.
pub(crate) mod indexed;
use crate::*;
const CHUNK: u64 = 4 * 1024 * 1024;
const MAX_PROBE_BYTES: u64 = 256 * 1024 * 1024;
const CAUTION: &str = "Detached NTFS record: may belong to an older filesystem or a stored copy. Runlist addresses use the selected boot's cluster geometry; the old geometry and deletion state are unverified. Original paths are not established.";

fn overlaps_mft(ntfs: Option<&Ntfs>, offset: u64, length: u64) -> bool {
    ntfs.is_some_and(|n| match &n.mft {
        ntfs::Data::NonResident { runs, .. } => runs.iter().any(|r| {
            let current_length = r.length.min(n.mft.size().saturating_sub(r.logical));
            current_length != 0
                && r.physical
                    .is_some_and(|start| start < offset + length && offset < start + current_length)
        }),
        _ => true,
    })
}
fn record(
    image: &mut Image,
    geometry: &ntfs::Geometry,
    current: Option<&Ntfs>,
    offset: u64,
) -> Result<(u64, ntfs::Record)> {
    if !offset.is_multiple_of(geometry.sector as u64)
        || offset
            .checked_add(geometry.record as u64)
            .is_none_or(|end| end > geometry.volume_length)
        || overlaps_mft(current, offset, geometry.record as u64)
    {
        return Err(Error::SourceChanged);
    }
    let raw = image.read(offset, geometry.record)?;
    let number = u64::from(u32::from_le_bytes(raw[44..48].try_into().unwrap()));
    let mut r = ntfs::parse_record(raw, geometry)?;
    if number < 16 || r.base != 0 || r.directory || r.name.is_none() || r.sequence == 0 {
        return Err(Error::Unsupported(
            "not a self-contained user-file MFT remnant".into(),
        ));
    }
    if r.has_attribute_list() {
        r.unsupported.push(
            "Detached attribute-list references cannot be resolved through the current MFT".into(),
        );
    }
    Ok((number, r))
}
fn id(offset: u64, number: u64, sequence: u16, stream: Option<&str>) -> String {
    format!(
        "remnant-{offset:x}-{}",
        ntfs::candidate_id(number, sequence, stream)
    )
}
pub(crate) fn scan(
    image: &mut Image,
    session: &mut Session,
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(Progress),
) -> Result<()> {
    let geometry = ntfs::Geometry::parse(image)?;
    let current = Ntfs::open(image).ok();
    session.warnings.push(CAUTION.into());
    if current.is_none() {
        session.warnings.push("Current MFT ownership and allocation could not be validated; remnant provenance remains unknown.".into());
    }
    let mut cursor = 0;
    let mut probe_bytes = 0u64;
    'scan: while cursor < geometry.volume_length {
        if cancel.load(Ordering::Relaxed) {
            session.cancelled = true;
            break;
        }
        let count = (geometry.volume_length - cursor).min(CHUNK) as usize;
        let (data, gaps) = image.read_available(cursor, count)?;
        if !gaps.is_empty() {
            session.truncated = true;
        }
        for (index, sector) in data.as_chunks::<512>().0.iter().enumerate() {
            if &sector[..4] != b"FILE" {
                continue;
            }
            if cancel.load(Ordering::Relaxed) {
                session.cancelled = true;
                break 'scan;
            }
            let offset = cursor + index as u64 * 512;
            if overlaps_mft(current.as_ref(), offset, geometry.record as u64) {
                continue;
            }
            if session.candidates.len() >= options.max_candidates
                || session.scanned_records >= options.max_records
                || probe_bytes >= MAX_PROBE_BYTES
            {
                session.truncated = true;
                session
                    .warnings
                    .push("RESOURCE_LIMIT: remaining MFT fragment scan budget exhausted".into());
                break 'scan;
            }
            probe_bytes += geometry.record as u64;
            session.scanned_records += 1;
            let (number, mut r) = match record(image, &geometry, current.as_ref(), offset) {
                Ok(r) => r,
                Err(Error::MissingRanges(_)) => {
                    session.truncated = true;
                    continue;
                }
                Err(_) => continue,
            };
            if r.streams.is_empty() {
                r.streams.push(ntfs::Stream {
                    name: None,
                    data: None,
                    unsupported: Vec::new(),
                });
            }
            for stream in &r.streams {
                if session.candidates.len() >= options.max_candidates {
                    session.truncated = true;
                    break 'scan;
                }
                session.candidates.push(make_candidate(
                    image,
                    current.as_ref(),
                    offset,
                    number,
                    &r,
                    stream,
                ));
            }
        }
        cursor += count as u64;
        progress(Progress {
            scanned_records: cursor,
            total_records: geometry.volume_length,
            candidates: session.candidates.len(),
            unit: ProgressUnit::Bytes,
        });
    }
    session.total_records = session.total_records.max(session.scanned_records);
    Ok(())
}

fn make_candidate(
    image: &mut Image,
    current: Option<&Ntfs>,
    offset: u64,
    number: u64,
    r: &ntfs::Record,
    stream: &ntfs::Stream,
) -> Candidate {
    let name = r.name.as_ref().unwrap();
    let mut warnings = r.unsupported.clone();
    warnings.extend(stream.unsupported.clone());
    let supported = warnings.is_empty() && stream.data.is_some();
    warnings.push(CAUTION.into());
    if r.in_use {
        warnings.push(
            "The surviving record has its in-use flag set; it is not a current-MFT record.".into(),
        );
    }
    let (allocation_status, allocation_warnings) = match (current, &stream.data) {
        (Some(n), Some(d)) => n.allocation(image, d),
        _ => (AllocationStatus::Unknown, Vec::new()),
    };
    warnings.extend(allocation_warnings);
    let display = stream
        .name
        .as_ref()
        .map_or_else(|| name.clone(), |s| format!("{name}:{s}"));
    let alternate_names = r
        .names
        .iter()
        .filter(|n| n.name != *name)
        .map(|n| {
            let name = stream
                .name
                .as_ref()
                .map_or_else(|| n.name.clone(), |s| format!("{}:{s}", n.name));
            CandidateName {
                name: name.clone(),
                original_path: name,
                path_verified: false,
                namespace: n.namespace,
            }
        })
        .collect();
    Candidate {
        id: id(offset, number, r.sequence, stream.name.as_deref()),
        record: number,
        fragment_offset: Some(offset),
        sequence: r.sequence,
        stream_name: stream.name.clone(),
        name: display.clone(),
        original_path: display,
        path_verified: false,
        alternate_names,
        size: stream.data.as_ref().map(|d| d.size()),
        supported,
        warnings,
        record_sha256: r.hash.clone(),
        extent_evidence: ExtentEvidence::MetadataRemnant,
        allocation_status,
        created: r.created.clone(),
        modified: r.modified.clone(),
    }
}

pub(crate) fn recovery_data(
    image: &mut Image,
    current: Option<&Ntfs>,
    candidate: &Candidate,
) -> Result<RecoveryData> {
    let geometry = ntfs::Geometry::parse(image)?;
    let offset = candidate.fragment_offset.ok_or(Error::SourceChanged)?;
    let (number, r) = record(image, &geometry, current, offset)?;
    if r.hash != candidate.record_sha256
        || r.sequence != candidate.sequence
        || number != candidate.record
        || candidate.id != id(offset, number, r.sequence, candidate.stream_name.as_deref())
    {
        return Err(Error::SourceChanged);
    }
    if !r.unsupported.is_empty() {
        return Err(Error::Unsupported(r.unsupported.join("; ")));
    }
    let stream = r
        .streams
        .into_iter()
        .find(|s| s.name == candidate.stream_name)
        .ok_or(Error::SourceChanged)?;
    if !stream.unsupported.is_empty() {
        return Err(Error::Unsupported(stream.unsupported.join("; ")));
    }
    let data = stream
        .data
        .ok_or_else(|| Error::Unsupported("remnant DATA is unavailable".into()))?;
    let name = r.name.ok_or(Error::SourceChanged)?;
    let name = stream.name.as_ref().map_or_else(
        || name.clone(),
        |s| format!("{}__ADS__{}", safe_name(&name), safe_name(s)),
    );
    let (allocation_status, mut warnings) = current
        .map_or((AllocationStatus::Unknown, Vec::new()), |n| {
            n.allocation(image, &data)
        });
    warnings.push(CAUTION.into());
    Ok(RecoveryData {
        data,
        name,
        warnings,
        extent_evidence: ExtentEvidence::MetadataRemnant,
        allocation_status,
    })
}
