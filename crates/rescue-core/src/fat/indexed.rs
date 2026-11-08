//! Durable FAT traversal. Unvalidated entries are disk-staged; immutable public
//! candidates are emitted only after the live ownership traversal is complete.
use super::*;
use crate::{IndexWriter, IndexedProgress, Session, index::StagedRow, indexed_scan::Checkpoint};
use serde::{Deserialize, Serialize};

#[derive(Serialize, Deserialize, PartialEq, Eq)]
enum Phase {
    Traverse,
    Validate,
    Complete,
}
#[derive(Serialize, Deserialize)]
struct Active {
    folder: Folder,
    origin: usize,
    clusters: Vec<u32>,
    at: u64,
    long: Vec<[u8; 32]>,
}
#[derive(Serialize, Deserialize)]
pub(crate) struct State {
    phase: Phase,
    scan: Scan,
    live: VecDeque<Folder>,
    deleted: VecDeque<Folder>,
    active: Option<Active>,
    directory_clusters: BTreeMap<u32, usize>,
    parents: Vec<Option<usize>>,
    conflicted: BTreeSet<usize>,
    live_clusters: Owners,
    live_complete: bool,
    folders_seen: usize,
    visits: u64,
    #[serde(default = "estimate_budget")]
    estimate_validation_remaining: u64,
    #[serde(default)]
    copy_choices: Vec<String>,
    staged: u64,
    next_entry: u64,
    base_candidates: usize,
    base_invalid: u64,
    base_warnings: usize,
}
impl State {
    fn new(volume: &Volume, header: &Session, base_candidates: usize) -> Self {
        let mut live_clusters = Owners::default();
        live_clusters.extend(&volume.system_clusters);
        Self {
            phase: Phase::Traverse,
            scan: Scan {
                filesystem: volume.geometry.filesystem,
                entries: vec![],
                scanned_records: 0,
                total_records: 0,
                invalid_records: 0,
                warnings: volume.geometry.warnings.clone(),
                cancelled: false,
                truncated: false,
            },
            live: VecDeque::from([Folder {
                first: volume.geometry.root,
                size: None,
                contiguous: false,
                path: String::new(),
                verified: true,
                deleted: false,
                depth: 0,
                parent: None,
            }]),
            deleted: VecDeque::new(),
            active: None,
            directory_clusters: BTreeMap::new(),
            parents: vec![],
            conflicted: BTreeSet::new(),
            live_clusters,
            live_complete: true,
            folders_seen: 0,
            visits: volume.visits,
            estimate_validation_remaining: volume.estimate_validation_remaining,
            copy_choices: volume.copy_choices.clone(),
            staged: 0,
            next_entry: 0,
            base_candidates,
            base_invalid: header.invalid_records,
            base_warnings: header.warnings.len(),
        }
    }
    fn validate(&self, volume: &Volume, options: ScanOptions, header: &Session) -> Result<()> {
        if self.scan.filesystem != volume.geometry.filesystem
            || self.scan.scanned_records > options.max_records
            || self.staged > options.max_candidates as u64
            || self.next_entry > self.staged
            || self.copy_choices.len() > 1000
            || self.copy_choices.iter().any(|choice| choice.len() > 1024)
            || self.visits > MAX_CLUSTER_VISITS
            || self.estimate_validation_remaining > ESTIMATE_VALIDATION_BUDGET
            || self.parents.len() > MAX_DIRECTORIES
            || self.folders_seen != self.parents.len()
            || self.live.len() + self.deleted.len() + self.folders_seen > MAX_DIRECTORIES
            || self.base_warnings > header.warnings.len()
            || self.base_invalid > header.invalid_records
            || self
                .parents
                .iter()
                .enumerate()
                .any(|(i, p)| p.is_some_and(|p| p >= i))
            || self.directory_clusters.len() > MAX_CLUSTER_VISITS as usize
            || self
                .directory_clusters
                .iter()
                .any(|(c, o)| *o >= self.parents.len() || volume.geometry.address(*c).is_err())
            || self.conflicted.iter().any(|o| *o >= self.parents.len())
            || self.live.iter().chain(self.deleted.iter()).any(|f| {
                f.depth > 65
                    || f.path.len() > 8192
                    || f.parent.is_some_and(|p| p >= self.parents.len())
            })
        {
            return Err(Error::Request("invalid FAT traversal checkpoint".into()));
        }
        if let Some(a) = &self.active {
            let size = volume.directory_size(&a.folder, &a.clusters);
            if a.origin >= self.parents.len()
                || (a.clusters.is_empty() && !volume.is_fixed_root(&a.folder))
                || size > MAX_DIRECTORY_BYTES
                || a.at > size
                || !a.at.is_multiple_of(32)
                || a.long.len() > 21
                || a.clusters
                    .iter()
                    .any(|c| volume.geometry.address(*c).is_err())
            {
                return Err(Error::Request("invalid FAT directory restart point".into()));
            }
        }
        Ok(())
    }
}
fn begin_folder(
    image: &mut Image,
    volume: &mut Volume,
    state: &mut State,
    cancel: &AtomicBool,
) -> Result<()> {
    let Some(folder) = state.live.pop_front().or_else(|| state.deleted.pop_front()) else {
        state.phase = Phase::Validate;
        if !state.conflicted.is_empty() {
            state.live_complete = false;
            state.scan.problem(format!("{} previously parsed directories overlap active file allocations; their descendants are not trusted", state.conflicted.len()));
        }
        return Ok(());
    };
    if state.folders_seen >= MAX_DIRECTORIES {
        state.scan.truncated = true;
        state.live_complete = false;
        state.phase = Phase::Validate;
        return Ok(());
    }
    let origin = state.parents.len();
    state.parents.push(folder.parent);
    state.folders_seen += 1;
    if folder
        .parent
        .is_some_and(|id| invalid_origin(id, &state.parents, &state.conflicted))
    {
        if !folder.deleted {
            state.live_complete = false;
        }
        return Ok(());
    }
    let clusters = if volume.is_fixed_root(&folder) {
        Ok(Vec::new())
    } else if folder.contiguous {
        let pseudo = Entry {
            candidate: candidate(
                state.scan.filesystem,
                0,
                names::Name {
                    text: String::new(),
                    verified: false,
                    warnings: Vec::new(),
                },
                &folder,
                0,
                &[],
            ),
            first: folder.first,
            size: folder.size.unwrap_or(0),
            initialized: 0,
            contiguous: true,
        };
        volume.file_clusters(image, &pseudo, folder.deleted, cancel)
    } else {
        volume.chain(
            image,
            folder.first,
            folder.size.map(|s| s.div_ceil(volume.geometry.cluster)),
            folder.deleted,
            cancel,
        )
    };
    let clusters = match clusters {
        Ok(c) => c,
        Err(Error::Cancelled) => {
            return Err(Error::Cancelled);
        }
        Err(error) => {
            if matches!(error, Error::Limit(_)) {
                state.scan.truncated = true;
            }
            state
                .scan
                .problem(format!("directory {}: {error}", folder.path));
            if !folder.deleted {
                state.live_complete = false;
            }
            return Ok(());
        }
    };
    let size = volume.directory_size(&folder, &clusters);
    if size == 0 || size > MAX_DIRECTORY_BYTES || folder.depth > 64 || folder.path.len() > 4096 {
        state.scan.truncated = true;
        if !folder.deleted {
            state.live_complete = false;
        }
        return Ok(());
    }
    let bitmap_conflict = if state.scan.filesystem == FileSystem::Exfat {
        clusters.iter().try_fold(false, |found, cluster| {
            volume
                .allocated(image, *cluster)
                .map(|allocated| found || allocated == folder.deleted)
        })?
    } else {
        false
    };
    if clusters
        .iter()
        .any(|c| state.directory_clusters.contains_key(c) || state.live_clusters.contains(c))
        || bitmap_conflict
    {
        state.scan.problem(format!(
            "directory {}: allocation overlaps a live/previous directory or file",
            folder.path
        ));
        if !folder.deleted {
            state.live_complete = false;
        }
        return Ok(());
    }
    state
        .directory_clusters
        .extend(clusters.iter().map(|c| (*c, origin)));
    if !folder.deleted {
        state.live_clusters.extend(&clusters);
    }
    if folder.deleted
        && state.scan.filesystem.classic_fat()
        && volume.next(image, folder.first)? == 0
    {
        state.scan.truncated = true;
        if state.scan.warnings.len() < 100 {
            state.scan.warnings.push(format!(
                "deleted directory {}: chain cleared; only its known first cluster was examined",
                folder.path
            ));
        }
    }
    state.active = Some(Active {
        folder,
        origin,
        clusters,
        at: 0,
        long: vec![],
    });
    Ok(())
}
fn traverse(
    image: &mut Image,
    volume: &mut Volume,
    state: &mut State,
    options: ScanOptions,
    cancel: &AtomicBool,
) -> Result<Vec<(Entry, usize)>> {
    if state.active.is_none() {
        begin_folder(image, volume, state, cancel)?;
    }
    let Some(mut active) = state.active.take() else {
        return Ok(vec![]);
    };
    let mut directory = Directory::new(volume.directory_data(&active.folder, &active.clusters)?);
    directory.at = active.at;
    let mut staged = vec![];
    let started = state.scan.scanned_records;
    let mut stage_bytes = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if state.scan.scanned_records >= options.max_records
            || state.staged >= options.max_candidates as u64
        {
            state.scan.truncated = true;
            state.live_complete = false;
            state.phase = Phase::Validate;
            break;
        }
        if state.scan.scanned_records - started >= 256
            || staged.len() >= 128
            || stage_bytes >= 8 * 1024 * 1024
        {
            active.at = directory.at;
            state.active = Some(active);
            break;
        }
        let Some(slot) = directory.next(image)? else {
            break;
        };
        state.scan.scanned_records += 1;
        if slot.raw[0] == 0 {
            break;
        }
        let previous = staged.len();
        let parsed = if state.scan.filesystem.classic_fat() {
            if slot.raw[11] == 0x0f {
                if active.long.len() < 21 {
                    active.long.push(slot.raw);
                }
                continue;
            }
            let lfn = std::mem::take(&mut active.long);
            if slot.raw[11] & 8 != 0 || slot.raw[0] == b'.' {
                continue;
            }
            if slot.raw[11] & !0x3f != 0 {
                state
                    .scan
                    .problem("reserved FAT32 directory attributes".into());
                state.live_complete = false;
                continue;
            }
            let name = names::fat32_name(&slot.raw, &lfn, options.fat_short_name_encoding);
            let size = u32_at(&slot.raw, 28)? as u64;
            let first = directory_first(state.scan.filesystem, &slot.raw)?;
            let mut raw = lfn.iter().flatten().copied().collect::<Vec<_>>();
            raw.extend(slot.raw);
            let mut entry = Entry {
                candidate: candidate(
                    state.scan.filesystem,
                    slot.offset,
                    name,
                    &active.folder,
                    size,
                    &raw,
                ),
                first,
                size,
                initialized: size,
                contiguous: false,
            };
            entry.candidate.created = crate::timestamps::dos(
                u16_at(&slot.raw, 16)?,
                u16_at(&slot.raw, 14)?,
                slot.raw[13],
                None,
            );
            entry.candidate.modified =
                crate::timestamps::dos(u16_at(&slot.raw, 24)?, u16_at(&slot.raw, 22)?, 0, None);
            Some((
                entry,
                slot.raw[11] & 16 != 0,
                slot.raw[0] == 0xe5 || active.folder.deleted,
            ))
        } else if [5, 0x85].contains(&slot.raw[0]) {
            let count = usize::from(slot.raw[1]);
            if !(2..=18).contains(&count) {
                if !active.folder.deleted && slot.raw[0] & 0x80 != 0 {
                    state.live_complete = false;
                }
                state
                    .scan
                    .problem("unsupported exFAT file-entry set size".into());
                continue;
            }
            if state.scan.scanned_records + count as u64 > options.max_records {
                state.scan.truncated = true;
                state.live_complete = false;
                state.phase = Phase::Validate;
                return Ok(staged);
            }
            let mut set = vec![slot.raw];
            for _ in 0..count {
                if let Some(secondary) = directory.next(image)? {
                    set.push(secondary.raw);
                    state.scan.scanned_records += 1;
                } else {
                    break;
                }
            }
            match names::exfat_entry(&set, &volume.upcase) {
                Ok(parsed) => {
                    let name = names::Name {
                        text: parsed.name,
                        verified: true,
                        warnings: Vec::new(),
                    };
                    let raw = set.iter().flatten().copied().collect::<Vec<_>>();
                    let mut entry = Entry {
                        candidate: candidate(
                            state.scan.filesystem,
                            slot.offset,
                            name,
                            &active.folder,
                            parsed.size,
                            &raw,
                        ),
                        first: parsed.first,
                        size: parsed.size,
                        initialized: parsed.initialized,
                        contiguous: parsed.contiguous,
                    };
                    entry.candidate.created = parsed.created;
                    entry.candidate.modified = parsed.modified;
                    Some((
                        entry,
                        parsed.directory,
                        parsed.deleted || active.folder.deleted,
                    ))
                }
                Err(error) => {
                    if !active.folder.deleted && slot.raw[0] & 0x80 != 0 {
                        state.live_complete = false;
                    }
                    state
                        .scan
                        .problem(format!("entry {}: {error}", slot.offset));
                    None
                }
            }
        } else {
            if slot.raw[0] & 0xe0 == 0x80 && ![0x81, 0x82, 0x83].contains(&slot.raw[0]) {
                if !active.folder.deleted {
                    state.live_complete = false;
                }
                state.scan.problem(format!(
                    "unknown critical exFAT directory entry: {:#x}",
                    slot.raw[0]
                ));
            }
            None
        };
        if let Some((entry, is_directory, is_deleted)) = parsed {
            if is_directory {
                if state.scan.filesystem == FileSystem::Exfat
                    && (entry.size == 0
                        || entry.initialized != entry.size
                        || !entry.size.is_multiple_of(volume.geometry.cluster))
                {
                    if !is_deleted {
                        state.live_complete = false;
                    }
                    state.scan.problem("invalid exFAT directory length".into());
                    continue;
                }
                if state.live.len() + state.deleted.len() + state.folders_seen >= MAX_DIRECTORIES {
                    state.scan.truncated = true;
                    if !is_deleted {
                        state.live_complete = false;
                    }
                    continue;
                }
                let child = Folder {
                    first: entry.first,
                    size: if state.scan.filesystem == FileSystem::Exfat {
                        Some(entry.size)
                    } else {
                        None
                    },
                    contiguous: entry.contiguous,
                    path: entry.candidate.original_path,
                    verified: entry.candidate.path_verified,
                    deleted: is_deleted,
                    depth: active.folder.depth + 1,
                    parent: Some(active.origin),
                };
                if is_deleted {
                    state.deleted.push_back(child);
                } else {
                    state.live.push_back(child);
                }
            } else if is_deleted {
                staged.push((entry, active.origin));
                state.staged += 1;
            } else if state.scan.filesystem == FileSystem::Exfat
                && entry.contiguous
                && entry.size > 0
            {
                let count = entry.size.div_ceil(volume.geometry.cluster);
                let last = u64::from(entry.first) + count - 1;
                if count > u64::from(volume.geometry.clusters)
                    || last > u64::from(u32::MAX)
                    || volume.geometry.address(entry.first).is_err()
                    || volume.geometry.address(last as u32).is_err()
                {
                    state.live_complete = false;
                    if volume.geometry.address(entry.first).is_ok() {
                        state.live_clusters.add(entry.first, entry.first);
                        if let Some(id) = state.directory_clusters.get(&entry.first) {
                            state.conflicted.insert(*id);
                        }
                    }
                    state
                        .scan
                        .problem("invalid live exFAT contiguous allocation".into());
                } else {
                    state.live_clusters.add(entry.first, last as u32);
                    state.conflicted.extend(
                        state
                            .directory_clusters
                            .range(entry.first..=last as u32)
                            .map(|(_, id)| *id),
                    );
                }
            } else {
                // An active entry's known start remains owned even with a bad FAT.
                if entry.first != 0 && volume.geometry.address(entry.first).is_ok() {
                    state.live_clusters.add(entry.first, entry.first);
                    if let Some(id) = state.directory_clusters.get(&entry.first) {
                        state.conflicted.insert(*id);
                    }
                }
                let mut protect = |cluster| {
                    state.live_clusters.add(cluster, cluster);
                    if let Some(id) = state.directory_clusters.get(&cluster) {
                        state.conflicted.insert(*id);
                    }
                };
                match volume.file_clusters_observed(image, &entry, false, cancel, &mut protect) {
                    Ok(_) => {}
                    Err(Error::Cancelled) => {
                        return Err(Error::Cancelled);
                    }
                    Err(error) => {
                        if matches!(error, Error::Limit(_)) {
                            state.scan.truncated = true;
                        }
                        state.live_complete = false;
                        state.scan.problem(format!(
                            "live file {}: {error}",
                            entry.candidate.original_path
                        ));
                    }
                }
            }
        }
        if staged.len() > previous {
            stage_bytes += crate::serialized_size(&staged[staged.len() - 1])?;
        }
    }
    Ok(staged)
}

pub(crate) fn run_volume(
    writer: &mut IndexWriter,
    header: &mut Session,
    checkpoint: &mut Checkpoint,
    source: &Image,
    volume_image: &mut Image,
    cancel: &AtomicBool,
    progress: &mut impl FnMut(IndexedProgress),
) -> Result<bool> {
    let geometry = Geometry::open(volume_image)?;
    let mut volume = Volume {
        geometry,
        fat_cache: BTreeMap::new(),
        bitmap: None,
        upcase: vec![],
        system_clusters: vec![],
        visits: 0,
        copy: None,
        copy_choices: Vec::new(),
        estimate_validation_remaining: ESTIMATE_VALIDATION_BUDGET,
    };
    if volume.geometry.filesystem == FileSystem::Exfat {
        volume.prepare_exfat(volume_image, checkpoint.options.max_records, cancel)?;
    }
    let committed = writer.index().status()?.candidates as usize;
    let mut state = checkpoint
        .fat
        .take()
        .unwrap_or_else(|| State::new(&volume, header, committed));
    let options = ScanOptions {
        max_records: checkpoint
            .options
            .max_records
            .checked_sub(checkpoint.previous_records)
            .ok_or_else(|| Error::Request("FAT record budget".into()))?,
        max_candidates: checkpoint
            .options
            .max_candidates
            .checked_sub(state.base_candidates)
            .ok_or_else(|| Error::Request("FAT candidate budget".into()))?,
        fragments: false,
        estimate_fat_contiguous: checkpoint.options.estimate_fat_contiguous,
        fat_short_name_encoding: checkpoint.options.fat_short_name_encoding,
    };
    if state.base_candidates > committed {
        return Err(Error::Request("FAT candidate baseline".into()));
    }
    state.validate(&volume, options, header)?;
    volume.visits = state.visits;
    volume.copy_choices = state.copy_choices.clone();
    volume.estimate_validation_remaining = state.estimate_validation_remaining;
    let settings = header.search.clone().unwrap_or_default();
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut batch = vec![];
        let mut staged = vec![];
        if state.phase == Phase::Traverse {
            let first = state.staged;
            let entries = traverse(volume_image, &mut volume, &mut state, options, cancel)?;
            for (i, entry) in entries.into_iter().enumerate() {
                staged.push(StagedRow {
                    namespace: checkpoint.volume as u64,
                    sequence: first + i as u64,
                    payload: serde_json::to_string(&entry)?,
                });
            }
        } else if state.phase == Phase::Validate {
            let ownership = Ownership {
                parents: &state.parents,
                conflicted: &state.conflicted,
                live_clusters: &state.live_clusters,
                live_complete: state.live_complete,
                estimate_contiguous: options.estimate_fat_contiguous,
            };
            let mut bytes = 0u64;
            for _ in 0..128 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                if state.next_entry == state.staged {
                    state.phase = Phase::Complete;
                    break;
                }
                let (entry, origin): (Entry, usize) = writer
                    .index()
                    .staged(checkpoint.volume as u64, state.next_entry)?;
                if origin >= state.parents.len() {
                    return Err(Error::Request("invalid staged FAT origin".into()));
                }
                let mut recovered = recover_entry(
                    volume_image,
                    &mut volume,
                    entry,
                    origin,
                    &ownership,
                    &mut state.scan,
                    cancel,
                );
                if state.scan.cancelled {
                    return Err(Error::Cancelled);
                }
                let keep = settings.matches(&recovered.candidate);
                if header.filesystem == FileSystem::Disk {
                    let part = checkpoint.plans[checkpoint.volume]
                        .region
                        .as_ref()
                        .ok_or_else(|| Error::Request("missing FAT volume region".into()))?
                        .index;
                    recovered.candidate.id = format!("p{part}:{}", recovered.candidate.id);
                    recovered.candidate.original_path =
                        format!("P{part}/{}", recovered.candidate.original_path);
                }
                if keep {
                    bytes += crate::serialized_size(&recovered.candidate)?;
                    batch.push(recovered.candidate);
                }
                state.next_entry += 1;
                if bytes >= 8 * 1024 * 1024 {
                    break;
                }
            }
        }
        state.visits = volume.visits;
        state.copy_choices = volume.copy_choices.clone();
        state.estimate_validation_remaining = volume.estimate_validation_remaining;
        header.scanned_records = checkpoint.previous_records + state.scan.scanned_records;
        header.invalid_records = state.base_invalid + state.scan.invalid_records;
        header.truncated |= state.scan.truncated;
        header.warnings.truncate(state.base_warnings);
        if !state.copy_choices.is_empty() && header.warnings.len() < 100 {
            header.warnings.push(format!("Damaged FAT mirror: {} chain(s) were taken from the single FAT copy that is consistent; see first: {}", state.copy_choices.len(), state.copy_choices[0]));
        }
        header.warnings.extend(
            state
                .scan
                .warnings
                .iter()
                .take(100usize.saturating_sub(header.warnings.len()))
                .cloned(),
        );
        let complete = state.phase == Phase::Complete;
        checkpoint.fat = Some(state);
        if source.acquisition_identity()? != checkpoint.identity {
            return Err(Error::SourceChanged);
        }
        writer.commit_staged(header, &batch, "running", true, checkpoint, &staged)?;
        let status = writer.index().status()?;
        progress(IndexedProgress {
            index: writer.index().path().to_owned(),
            session_id: header.id.clone(),
            progress: Progress {
                scanned_records: header.scanned_records,
                total_records: header.total_records,
                candidates: status.candidates as usize,
                unit: crate::ProgressUnit::Records,
            },
            committed_candidates: status.candidates,
        });
        if complete {
            return Ok(true);
        }
        state = checkpoint.fat.take().unwrap();
    }
}
