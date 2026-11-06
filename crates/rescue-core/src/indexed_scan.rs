//! Bounded NTFS enumeration with durable record/stream checkpoints.
use crate::*;
use std::collections::{BTreeMap, BTreeSet};

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IndexedProgress {
    pub index: PathBuf,
    pub session_id: String,
    pub progress: Progress,
    pub committed_candidates: u64,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Plan {
    pub(crate) region: Option<SourceRegion>,
    pub(crate) fingerprint: Fingerprint,
    pub(crate) records: u64,
    #[serde(default)]
    pub(crate) filesystem: FileSystem,
    #[serde(default = "default_true")]
    pub(crate) current_mft: bool,
    #[serde(default)]
    warnings: Vec<String>,
}
fn default_true() -> bool {
    true
}
#[derive(Serialize, Deserialize)]
pub(crate) struct Checkpoint {
    pub(crate) version: u32,
    pub(crate) identity: AcquisitionIdentity,
    #[serde(default)]
    pub(crate) source_fingerprint: Option<Fingerprint>,
    pub(crate) options: ScanOptions,
    pub(crate) plans: Vec<Plan>,
    pub(crate) volume: usize,
    pub(crate) record: u64,
    pub(crate) stream: usize,
    pub(crate) previous_records: u64,
    #[serde(default)]
    pub(crate) fat: Option<crate::fat::indexed::State>,
    #[serde(default)]
    pub(crate) remnants: Option<crate::ntfs_remnants::indexed::State>,
}
pub fn scan_indexed(
    source: &Path,
    work: &Path,
    mut options: ScanOptions,
    settings: &SearchSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(IndexedProgress),
) -> Result<PathBuf> {
    settings.validate()?;
    options.fragments = settings.fragments;
    if options.max_candidates == 0
        || options.max_candidates > 10_000_000
        || options.max_records == 0
        || options.max_records > 100_000_000
    {
        return Err(Error::Request(
            "indexed limits: 1..10M candidates, 1..100M records".into(),
        ));
    }
    if settings.method == ScanMethod::Carve {
        return crate::carve::scan_indexed(source, work, options, settings, cancel, progress);
    }
    guard_output_path(source, work)?;
    let mut image = Image::open(source)?;
    let fingerprint = image.fingerprint()?;
    let identity = image.acquisition_identity()?;
    let layout = if let Some(region) = &settings.recovered_volume {
        crate::discovery::validate_region(source, region)?;
        SourceLayout {
            source_info: image.source_info()?,
            path: image.path.clone(),
            bytes: image.length,
            sector_size: image.sector_size(),
            device: is_device(source),
            scheme: "recovered".into(),
            regions: vec![region.clone()],
            warnings: vec![],
        }
    } else {
        inspect_source(source)?
    };
    let disk = !matches!(layout.scheme.as_str(), "volume" | "recovered");
    let regions: Vec<Option<SourceRegion>> = if layout.scheme == "volume" {
        vec![None]
    } else {
        layout
            .regions
            .into_iter()
            .filter(|r| r.filesystem.is_some())
            .map(Some)
            .collect()
    };
    if regions.is_empty() {
        return Err(Error::Unsupported(
            "no recognized volume for indexed scanning".into(),
        ));
    }
    let mut plans = Vec::new();
    for region in regions {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut volume = Image::open_region(source, region.as_ref())?;
        let (filesystem, records, current_mft, warnings) =
            if &volume.boot_read(512)?[3..11] == b"NTFS    " {
                match Ntfs::open(&mut volume) {
                    Ok(n) => (FileSystem::Ntfs, n.records, true, n.warnings.clone()),
                    Err(error) if options.fragments => {
                        ntfs::Geometry::parse(&mut volume)?;
                        (
                            FileSystem::Ntfs,
                            0,
                            false,
                            vec![format!("Current MFT is unavailable: {error}")],
                        )
                    }
                    Err(error) => return Err(error),
                }
            } else {
                (crate::fat::filesystem(&mut volume)?, 0, false, vec![])
            };
        plans.push(Plan {
            region,
            fingerprint: volume.fingerprint()?,
            records,
            filesystem,
            current_mft,
            warnings,
        });
    }
    let mut header = Session {
        source_info: None,
        raid: None,
        schema_version: if settings.fragments {
            8
        } else if disk || settings.recovered_volume.is_some() {
            5
        } else {
            3
        },
        filesystem: if disk {
            FileSystem::Disk
        } else {
            plans[0].filesystem
        },
        id: Uuid::new_v4().to_string(),
        source: image.path.clone(),
        fingerprint: if disk {
            fingerprint.clone()
        } else {
            plans[0].fingerprint.clone()
        },
        candidates: vec![],
        scanned_records: 0,
        total_records: if plans.iter().any(|p| p.filesystem != FileSystem::Ntfs) {
            0
        } else {
            plans
                .iter()
                .try_fold(0u64, |n, p| n.checked_add(p.records))
                .ok_or_else(|| Error::Limit("total record count".into()))?
        },
        cancelled: false,
        truncated: false,
        invalid_records: 0,
        warnings: layout.warnings,
        search: Some(settings.clone()),
        region: if disk { None } else { plans[0].region.clone() },
        volumes: if disk {
            plans
                .iter()
                .map(|p| ScannedVolume {
                    region: p.region.clone().unwrap(),
                    fingerprint: p.fingerprint.clone(),
                })
                .collect()
        } else {
            vec![]
        },
    };
    for plan in &plans {
        header.warnings.extend(
            plan.warnings
                .iter()
                .take(100usize.saturating_sub(header.warnings.len()))
                .cloned(),
        );
    }
    // A discovered volume uses a bounded view while the resume checkpoint
    // separately fingerprints the whole source. Record the session's view,
    // not the full-disk window, in its source descriptor.
    if header.region.is_some() {
        Image::open_region(source, header.region.as_ref())?.annotate_coverage(&mut header)?;
    } else {
        image.annotate_coverage(&mut header)?;
    }
    let checkpoint = Checkpoint {
        version: 1,
        identity,
        source_fingerprint: Some(fingerprint),
        options,
        plans,
        volume: 0,
        record: 0,
        stream: 0,
        previous_records: 0,
        fat: None,
        remnants: None,
    };
    let writer = IndexWriter::create(work, &header, "filesystem_records_v1", &checkpoint)?;
    run(writer, cancel, progress)
}
pub fn resume_indexed_scan(
    index: &Path,
    cancel: &AtomicBool,
    progress: impl FnMut(IndexedProgress),
) -> Result<PathBuf> {
    let writer = IndexWriter::resume(index)?;
    let status = writer.index().status()?;
    if ![
        "ntfs_records_v1",
        "filesystem_records_v1",
        "carve_signatures_v1",
    ]
    .contains(&status.engine.as_str())
        || !status.resumable
        || matches!(status.state.as_str(), "completed" | "failed")
    {
        return Err(Error::Request(
            "this index has no resumable scan checkpoint".into(),
        ));
    }
    if status.engine == "carve_signatures_v1" {
        return crate::carve::resume_indexed(writer, cancel, progress);
    }
    run(writer, cancel, progress)
}
fn run(
    mut writer: IndexWriter,
    cancel: &AtomicBool,
    mut progress: impl FnMut(IndexedProgress),
) -> Result<PathBuf> {
    let path = writer.index().path().to_owned();
    let mut header = writer.index().session()?;
    let mut checkpoint: Checkpoint = writer.index().checkpoint()?;
    if checkpoint.version != 1
        || checkpoint.plans.is_empty()
        || checkpoint.plans.len() > 256
        || checkpoint.volume > checkpoint.plans.len()
        || checkpoint.options.max_records == 0
        || checkpoint.options.max_records > 100_000_000
        || checkpoint.options.max_candidates == 0
        || checkpoint.options.max_candidates > 10_000_000
        || checkpoint.previous_records > checkpoint.options.max_records
        || checkpoint.plans.iter().any(|p| {
            !matches!(
                p.filesystem,
                FileSystem::Ntfs | FileSystem::Fat32 | FileSystem::Exfat
            )
        })
    {
        return Err(Error::Request("invalid scan checkpoint".into()));
    }
    let mut image = Image::open(&header.source)?;
    if image.acquisition_identity()? != checkpoint.identity
        || image.fingerprint()?
            != *checkpoint
                .source_fingerprint
                .as_ref()
                .unwrap_or(&header.fingerprint)
    {
        return Err(Error::SourceChanged);
    }
    image.validate_raid(&header)?;
    if header.filesystem != FileSystem::Disk
        && (checkpoint.plans.len() != 1
            || checkpoint.plans[0].region != header.region
            || checkpoint.plans[0].filesystem != header.filesystem
            || checkpoint.plans[0].fingerprint != header.fingerprint)
    {
        return Err(Error::Request(
            "selected volume identity differs from its scan plan; rescan the source".into(),
        ));
    }
    if header.filesystem == FileSystem::Disk {
        let layout = inspect_source(&header.source)?;
        if checkpoint.plans.iter().any(|p| {
            p.region
                .as_ref()
                .is_none_or(|r| !layout.regions.contains(r))
        }) {
            return Err(Error::SourceChanged);
        }
    } else {
        crate::disk_scan::validate_region(&header)?;
    }
    header.cancelled = false;
    let settings = header.search.clone().unwrap_or_default();
    settings.validate()?;
    if settings.method != ScanMethod::Quick || settings.fragments != checkpoint.options.fragments {
        return Err(Error::Request(
            "saved metadata scan settings changed".into(),
        ));
    }
    let mut committed = writer.index().status()?.candidates;
    let notify = |header: &Session, committed: u64, progress: &mut dyn FnMut(IndexedProgress)| {
        progress(IndexedProgress {
            index: path.clone(),
            session_id: header.id.clone(),
            progress: Progress {
                scanned_records: header.scanned_records,
                total_records: header.total_records,
                candidates: committed as usize,
                unit: ProgressUnit::Records,
            },
            committed_candidates: committed,
        })
    };
    notify(&header, committed, &mut progress);
    while checkpoint.volume < checkpoint.plans.len() {
        if checkpoint.previous_records >= checkpoint.options.max_records
            || committed >= checkpoint.options.max_candidates as u64
        {
            header.truncated = true;
            break;
        }
        let plan = checkpoint.plans[checkpoint.volume].clone();
        let mut volume = Image::open_region(&header.source, plan.region.as_ref())?;
        if volume.fingerprint()? != plan.fingerprint {
            return Err(Error::SourceChanged);
        }
        if checkpoint.options.fragments
            && plan.filesystem == FileSystem::Ntfs
            && (checkpoint.remnants.is_some() || !plan.current_mft)
        {
            crate::ntfs_remnants::indexed::run_volume(
                &mut writer,
                &mut header,
                &mut checkpoint,
                &image,
                &mut volume,
                cancel,
                &mut progress,
            )?;
            if volume.fingerprint()? != plan.fingerprint {
                return Err(Error::SourceChanged);
            }
            if header.cancelled || checkpoint.remnants.as_ref().is_some_and(|r| r.limited) {
                return Ok(path);
            }
            if !checkpoint.remnants.as_ref().is_some_and(|r| r.complete) {
                return Err(Error::Request("unfinished remnant phase".into()));
            }
            checkpoint.previous_records = header.scanned_records;
            checkpoint.volume += 1;
            checkpoint.record = 0;
            checkpoint.stream = 0;
            checkpoint.remnants = None;
            commit(&mut writer, &image, &header, &[], "running", &checkpoint)?;
            committed = writer.index().status()?.candidates;
            notify(&header, committed, &mut progress);
            continue;
        }
        if plan.filesystem != FileSystem::Ntfs {
            if crate::fat::filesystem(&mut volume)? != plan.filesystem {
                return Err(Error::SourceChanged);
            }
            let result = crate::fat::indexed::run_volume(
                &mut writer,
                &mut header,
                &mut checkpoint,
                &image,
                &mut volume,
                cancel,
                &mut progress,
            );
            if matches!(result, Err(Error::Cancelled)) {
                // Discard the uncommitted directory/ownership step as a unit.
                header = writer.index().session()?;
                checkpoint = writer.index().checkpoint()?;
                header.cancelled = true;
                commit(&mut writer, &image, &header, &[], "cancelled", &checkpoint)?;
                notify(&header, writer.index().status()?.candidates, &mut progress);
                return Ok(path);
            }
            result?;
            if volume.fingerprint()? != plan.fingerprint {
                return Err(Error::SourceChanged);
            }
            checkpoint.previous_records = header.scanned_records;
            checkpoint.volume += 1;
            checkpoint.record = 0;
            checkpoint.stream = 0;
            checkpoint.fat = None;
            commit(&mut writer, &image, &header, &[], "running", &checkpoint)?;
            committed = writer.index().status()?.candidates;
            notify(&header, committed, &mut progress);
            continue;
        }
        if checkpoint.fat.is_some() {
            return Err(Error::Request("FAT state for an NTFS volume".into()));
        }
        let ntfs = Ntfs::open(&mut volume)?;
        if ntfs.records != plan.records || checkpoint.record > ntfs.records {
            return Err(Error::SourceChanged);
        }
        let mut limited = false;
        let mut parents = Parents::default();
        let mut batch = Vec::new();
        let mut batch_bytes = 0u64;
        let mut since_commit = 0u64;
        while checkpoint.record < ntfs.records {
            if cancel.load(Ordering::Relaxed) {
                header.cancelled = true;
                break;
            }
            if checkpoint.previous_records + checkpoint.record >= checkpoint.options.max_records
                || committed + batch.len() as u64 >= checkpoint.options.max_candidates as u64
            {
                header.truncated = true;
                limited = true;
                break;
            }
            let at = checkpoint.record;
            match ntfs.record(&mut volume, at) {
                Ok(Some(record))
                    if record.base == 0
                        && !record.in_use
                        && !record.directory
                        && record.name.is_some() =>
                {
                    let mut record = record;
                    if record.streams.is_empty() {
                        record.streams.push(ntfs::Stream {
                            name: None,
                            data: None,
                            unsupported: vec![],
                        });
                    }
                    if checkpoint.stream > record.streams.len() {
                        return Err(Error::SourceChanged);
                    }
                    for i in checkpoint.stream..record.streams.len() {
                        if cancel.load(Ordering::Relaxed) {
                            header.cancelled = true;
                            break;
                        }
                        if committed + batch.len() as u64
                            >= checkpoint.options.max_candidates as u64
                        {
                            header.truncated = true;
                            limited = true;
                            break;
                        }
                        let mut candidate = match make_candidate(
                            &ntfs,
                            &mut volume,
                            at,
                            &record,
                            i,
                            &mut parents,
                            cancel,
                        ) {
                            Ok(c) => c,
                            Err(Error::Cancelled) => {
                                header.cancelled = true;
                                break;
                            }
                            Err(e) => return Err(e),
                        };
                        let keep = settings.matches(&candidate);
                        if header.filesystem == FileSystem::Disk {
                            let part = plan.region.as_ref().unwrap().index;
                            candidate.id = format!("p{part}:{}", candidate.id);
                            candidate.original_path =
                                format!("P{part}/{}", candidate.original_path);
                            for name in &mut candidate.alternate_names {
                                name.original_path = format!("P{part}/{}", name.original_path);
                            }
                        }
                        let size = serialized_size(&candidate)?;
                        if size > 8 * 1024 * 1024 {
                            return Err(Error::Limit("candidate metadata exceeds 8 MiB".into()));
                        }
                        if !batch.is_empty()
                            && (batch.len() >= 128 || batch_bytes + size > 16 * 1024 * 1024)
                        {
                            commit(&mut writer, &image, &header, &batch, "running", &checkpoint)?;
                            committed += batch.len() as u64;
                            batch.clear();
                            batch_bytes = 0;
                            notify(&header, committed, &mut progress);
                        }
                        if keep {
                            batch_bytes += size;
                            batch.push(candidate);
                        }
                        checkpoint.stream = i + 1;
                    }
                    if header.cancelled || limited {
                        break;
                    }
                }
                Ok(_) => {
                    if checkpoint.stream != 0 {
                        return Err(Error::SourceChanged);
                    }
                }
                Err(error) => {
                    if checkpoint.stream != 0 {
                        return Err(Error::SourceChanged);
                    }
                    header.invalid_records += 1;
                    if header.warnings.len() < 100 {
                        header.warnings.push(format!("record {at}: {error}"));
                    }
                }
            }
            checkpoint.record += 1;
            checkpoint.stream = 0;
            header.scanned_records = checkpoint.previous_records + checkpoint.record;
            since_commit += 1;
            if since_commit >= 256 {
                commit(&mut writer, &image, &header, &batch, "running", &checkpoint)?;
                committed += batch.len() as u64;
                batch.clear();
                batch_bytes = 0;
                since_commit = 0;
                notify(&header, committed, &mut progress);
            }
        }
        if volume.fingerprint()? != plan.fingerprint {
            return Err(Error::SourceChanged);
        }
        if header.cancelled || limited {
            commit(
                &mut writer,
                &image,
                &header,
                &batch,
                if header.cancelled {
                    "cancelled"
                } else {
                    "partial"
                },
                &checkpoint,
            )?;
            committed += batch.len() as u64;
            notify(&header, committed, &mut progress);
            return Ok(path);
        }
        if checkpoint.options.fragments {
            checkpoint.remnants =
                Some(crate::ntfs_remnants::indexed::State::new(&mut header, true));
            commit(&mut writer, &image, &header, &batch, "running", &checkpoint)?;
            committed += batch.len() as u64;
            notify(&header, committed, &mut progress);
            continue;
        }
        checkpoint.previous_records += ntfs.records;
        checkpoint.volume += 1;
        checkpoint.record = 0;
        checkpoint.stream = 0;
        commit(&mut writer, &image, &header, &batch, "running", &checkpoint)?;
        committed += batch.len() as u64;
        notify(&header, committed, &mut progress);
    }
    if image.fingerprint()?
        != *checkpoint
            .source_fingerprint
            .as_ref()
            .unwrap_or(&header.fingerprint)
    {
        return Err(Error::SourceChanged);
    }
    if !header.truncated {
        header.total_records = header.scanned_records;
    }
    commit(
        &mut writer,
        &image,
        &header,
        &[],
        if header.truncated || header.invalid_records > 0 {
            "partial"
        } else {
            "completed"
        },
        &checkpoint,
    )?;
    notify(&header, committed, &mut progress);
    Ok(path)
}
fn commit(
    writer: &mut IndexWriter,
    image: &Image,
    header: &Session,
    batch: &[Candidate],
    state: &str,
    checkpoint: &Checkpoint,
) -> Result<()> {
    if image.acquisition_identity()? != checkpoint.identity {
        return Err(Error::SourceChanged);
    }
    writer.commit(
        header,
        batch,
        state,
        matches!(state, "running" | "cancelled" | "paused"),
        checkpoint,
    )
}
type Parent = (u16, u64, u16, String);
#[derive(Default)]
pub(crate) struct Parents {
    cache: BTreeMap<u64, Option<Parent>>,
}
impl Parents {
    fn path(
        &mut self,
        ntfs: &Ntfs,
        image: &mut Image,
        name: &str,
        mut parent: u64,
        mut sequence: u16,
        cancel: &AtomicBool,
    ) -> Result<(String, bool)> {
        let mut parts = vec![name.to_owned()];
        let mut visited = BTreeSet::new();
        let mut verified = false;
        for _ in 0..256 {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            if !visited.insert(parent) {
                break;
            }
            if !self.cache.contains_key(&parent) {
                let record = ntfs
                    .record(image, parent)
                    .ok()
                    .flatten()
                    .filter(|r| r.directory && r.base == 0);
                let value = record
                    .and_then(|r| r.name.map(|n| (r.sequence, r.parent, r.parent_sequence, n)));
                if self.cache.len() >= 4096 {
                    self.cache.clear();
                }
                self.cache.insert(parent, value);
            }
            let Some((found, next, next_sequence, name)) =
                self.cache.get(&parent).and_then(Option::as_ref)
            else {
                break;
            };
            if *found != sequence {
                break;
            }
            if parent == 5 {
                verified = true;
                break;
            }
            parts.push(name.clone());
            parent = *next;
            sequence = *next_sequence;
        }
        parts.reverse();
        Ok((parts.join("/"), verified))
    }
}
pub(crate) fn make_candidate(
    ntfs: &Ntfs,
    image: &mut Image,
    index: u64,
    record: &ntfs::Record,
    i: usize,
    parents: &mut Parents,
    cancel: &AtomicBool,
) -> Result<Candidate> {
    let stream = &record.streams[i];
    let name = record.name.as_ref().unwrap();
    let mut warnings = record.unsupported.clone();
    warnings.extend(stream.unsupported.clone());
    if stream.data.is_none() {
        warnings.push("DATA is unavailable".into());
    }
    let supported = warnings.is_empty() && stream.data.is_some();
    warnings.extend(record.provenance.clone());
    let (allocation_status, allocation_warnings) = stream
        .data
        .as_ref()
        .map_or((AllocationStatus::Unknown, vec![]), |data| {
            ntfs.allocation(image, data)
        });
    warnings.extend(allocation_warnings);
    let display = |name: &str| {
        stream
            .name
            .as_ref()
            .map_or_else(|| name.to_string(), |s| format!("{name}:{s}"))
    };
    let name = display(name);
    let (original_path, path_verified) = parents.path(
        ntfs,
        image,
        &name,
        record.parent,
        record.parent_sequence,
        cancel,
    )?;
    let mut alternate_names = Vec::new();
    for alias in &record.names {
        if alias.name == *record.name.as_ref().unwrap()
            && alias.parent == record.parent
            && alias.sequence == record.parent_sequence
        {
            continue;
        }
        let name = display(&alias.name);
        let (original_path, path_verified) =
            parents.path(ntfs, image, &name, alias.parent, alias.sequence, cancel)?;
        alternate_names.push(CandidateName {
            name,
            original_path,
            path_verified,
            namespace: alias.namespace,
        });
    }
    Ok(Candidate {
        id: ntfs::candidate_id(index, record.sequence, stream.name.as_deref()),
        record: index,
        fragment_offset: None,
        sequence: record.sequence,
        stream_name: stream.name.clone(),
        name,
        original_path,
        path_verified,
        alternate_names,
        size: stream.data.as_ref().map(|d| d.size()),
        supported,
        warnings,
        record_sha256: record.hash.clone(),
        extent_evidence: ExtentEvidence::Metadata,
        allocation_status,
        created: record.created.clone(),
        modified: record.modified.clone(),
    })
}
