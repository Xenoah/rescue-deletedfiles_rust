//! Record/stream restart points for explicitly uncertain old-MFT evidence.
use super::*;
use crate::indexed_scan::Checkpoint;
#[derive(Serialize, Deserialize)]
pub(crate) struct State {
    next: u64,
    stream: usize,
    record_started: bool,
    probe_bytes: u64,
    attempts: u64,
    base_records: u64,
    pub(crate) complete: bool,
    pub(crate) limited: bool,
}
impl State {
    pub(crate) fn new(header: &mut Session, current_available: bool) -> Self {
        header.warnings.push(CAUTION.into());
        if !current_available {
            header.warnings.push("Current MFT ownership and allocation could not be validated; remnant provenance remains unknown.".into());
        }
        Self {
            next: 0,
            stream: 0,
            record_started: false,
            probe_bytes: 0,
            attempts: 0,
            base_records: header.scanned_records,
            complete: false,
            limited: false,
        }
    }
}
pub(crate) fn run_volume(
    writer: &mut IndexWriter,
    header: &mut Session,
    checkpoint: &mut Checkpoint,
    source: &Image,
    image: &mut Image,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(IndexedProgress),
) -> Result<()> {
    let geometry = ntfs::Geometry::parse(image)?;
    let current = Ntfs::open(image).ok();
    if current.is_some() != checkpoint.plans[checkpoint.volume].current_mft {
        return Err(Error::SourceChanged);
    }
    let mut state = checkpoint
        .remnants
        .take()
        .unwrap_or_else(|| State::new(header, current.is_some()));
    if state.next > geometry.volume_length
        || !state.next.is_multiple_of(512)
        || state.probe_bytes > MAX_PROBE_BYTES
        || state.attempts > checkpoint.options.max_records
        || state.base_records > checkpoint.options.max_records
        || state.base_records.checked_add(state.attempts) != Some(header.scanned_records)
        || !checkpoint.options.fragments
        || state.stream > 65536
        || (state.stream > 0 && !state.record_started)
    {
        return Err(Error::Request("invalid MFT remnant checkpoint".into()));
    }
    let settings = header.search.clone().unwrap_or_default();
    let mut committed = writer.index().status()?.candidates;
    let mut batch = vec![];
    let mut batch_bytes = 0u64;
    'scan: while state.next < geometry.volume_length && !state.complete && !state.limited {
        if cancel.load(Ordering::Relaxed) {
            header.cancelled = true;
            break;
        }
        let start = state.next;
        let count = (geometry.volume_length - start).min(CHUNK) as usize;
        let (data, gaps) = image.read_available(start, count)?;
        header.truncated |= !gaps.is_empty();
        for (index, sector) in data.as_chunks::<512>().0.iter().enumerate() {
            let offset = start + index as u64 * 512;
            if cancel.load(Ordering::Relaxed) {
                header.cancelled = true;
                state.next = offset;
                break 'scan;
            }
            if &sector[..4] != b"FILE"
                || overlaps_mft(current.as_ref(), offset, geometry.record as u64)
            {
                if state.stream != 0 || state.record_started {
                    return Err(Error::SourceChanged);
                }
                continue;
            }
            state.next = offset;
            if !state.record_started {
                if committed + batch.len() as u64 >= checkpoint.options.max_candidates as u64
                    || header.scanned_records >= checkpoint.options.max_records
                    || geometry.record as u64 > MAX_PROBE_BYTES - state.probe_bytes
                {
                    state.limited = true;
                    break 'scan;
                }
                state.record_started = true;
                state.probe_bytes += geometry.record as u64;
                state.attempts += 1;
                header.scanned_records = state.base_records + state.attempts;
            }
            let (number, mut record) =
                match super::record(image, &geometry, current.as_ref(), offset) {
                    Ok(r) => r,
                    Err(error) => {
                        if state.stream != 0 {
                            return Err(Error::SourceChanged);
                        }
                        if matches!(error, Error::MissingRanges(_)) {
                            header.truncated = true;
                        }
                        state.next = offset + 512;
                        state.record_started = false;
                        continue;
                    }
                };
            if record.streams.is_empty() {
                record.streams.push(ntfs::Stream {
                    name: None,
                    data: None,
                    unsupported: vec![],
                });
            }
            if state.stream > record.streams.len() {
                return Err(Error::SourceChanged);
            }
            for i in state.stream..record.streams.len() {
                if cancel.load(Ordering::Relaxed) {
                    header.cancelled = true;
                    break 'scan;
                }
                if committed + batch.len() as u64 >= checkpoint.options.max_candidates as u64 {
                    state.limited = true;
                    break 'scan;
                }
                // Flush before processing this stream so the restart counter
                // and the committed candidate batch always describe one point.
                if batch.len() >= 128 || batch_bytes >= 8 * 1024 * 1024 {
                    checkpoint.remnants = Some(state);
                    commit(writer, header, checkpoint, source, &batch, "running")?;
                    committed += batch.len() as u64;
                    batch.clear();
                    batch_bytes = 0;
                    notify(writer, header, checkpoint, committed, progress);
                    state = checkpoint.remnants.take().unwrap();
                }
                let mut candidate = make_candidate(
                    image,
                    current.as_ref(),
                    offset,
                    number,
                    &record,
                    &record.streams[i],
                );
                let keep = settings.matches(&candidate);
                if header.filesystem == FileSystem::Disk {
                    let part = checkpoint.plans[checkpoint.volume]
                        .region
                        .as_ref()
                        .ok_or_else(|| Error::Request("remnant volume region".into()))?
                        .index;
                    candidate.id = format!("p{part}:{}", candidate.id);
                    candidate.original_path = format!("P{part}/{}", candidate.original_path);
                    for alias in &mut candidate.alternate_names {
                        alias.original_path = format!("P{part}/{}", alias.original_path);
                    }
                }
                if keep {
                    batch_bytes += serialized_size(&candidate)?;
                    batch.push(candidate);
                }
                state.stream = i + 1;
            }
            state.stream = 0;
            state.record_started = false;
            state.next = offset + 512;
        }
        state.next = start + count as u64;
        checkpoint.remnants = Some(state);
        commit(writer, header, checkpoint, source, &batch, "running")?;
        committed += batch.len() as u64;
        batch.clear();
        batch_bytes = 0;
        notify(writer, header, checkpoint, committed, progress);
        state = checkpoint.remnants.take().unwrap();
    }
    state.complete = !header.cancelled && !state.limited && state.next == geometry.volume_length;
    if state.limited {
        header.truncated = true;
        header
            .warnings
            .push("RESOURCE_LIMIT: remaining MFT fragment scan budget exhausted".into());
    }
    checkpoint.remnants = Some(state);
    commit(
        writer,
        header,
        checkpoint,
        source,
        &batch,
        if header.cancelled {
            "cancelled"
        } else if checkpoint.remnants.as_ref().unwrap().limited {
            "partial"
        } else {
            "running"
        },
    )?;
    committed += batch.len() as u64;
    notify(writer, header, checkpoint, committed, progress);
    Ok(())
}
fn commit(
    writer: &mut IndexWriter,
    header: &Session,
    checkpoint: &Checkpoint,
    source: &Image,
    batch: &[Candidate],
    status: &str,
) -> Result<()> {
    if source.acquisition_identity()? != checkpoint.identity {
        return Err(Error::SourceChanged);
    }
    writer.commit(header, batch, status, status != "partial", checkpoint)
}
fn notify(
    writer: &IndexWriter,
    header: &Session,
    checkpoint: &Checkpoint,
    committed: u64,
    progress: &mut impl FnMut(IndexedProgress),
) {
    progress(IndexedProgress {
        index: writer.index().path().to_owned(),
        session_id: header.id.clone(),
        progress: Progress {
            scanned_records: checkpoint.remnants.as_ref().unwrap().next,
            total_records: checkpoint.plans[checkpoint.volume].fingerprint.length,
            candidates: committed as usize,
            unit: ProgressUnit::Bytes,
        },
        committed_candidates: committed,
    });
}
