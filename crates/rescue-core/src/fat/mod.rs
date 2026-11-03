//! Read-only FAT32/exFAT directory recovery. No guessed multi-cluster extents.
mod geometry;
mod names;

use crate::{
    AllocationStatus, Candidate, Error, ExtentEvidence, FileSystem, Progress, Result, ScanOptions,
    ntfs::{Data, Run},
    source::Image,
};
use geometry::Geometry;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet, VecDeque},
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_CLUSTER_VISITS: u64 = 2_000_000;
const MAX_CHAIN: u64 = 1_048_576;
const MAX_DIRECTORIES: usize = 16384;
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

fn invalid(message: &str) -> Error {
    Error::Invalid(format!("FAT: {message}"))
}
fn u16_at(bytes: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(
        bytes
            .get(at..at + 2)
            .ok_or_else(|| invalid("short field"))?
            .try_into()
            .unwrap(),
    ))
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or_else(|| invalid("short field"))?
            .try_into()
            .unwrap(),
    ))
}
fn u64_at(bytes: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        bytes
            .get(at..at + 8)
            .ok_or_else(|| invalid("short field"))?
            .try_into()
            .unwrap(),
    ))
}

struct Bitmap {
    data: Data,
    cache: Vec<u8>,
    start: u64,
}
struct Volume {
    geometry: Geometry,
    fat_cache: BTreeMap<u64, Vec<u8>>,
    bitmap: Option<Bitmap>,
    upcase: Vec<u16>,
    visits: u64,
}

impl Volume {
    fn visit(&mut self, cancel: &AtomicBool) -> Result<()> {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        if self.visits >= MAX_CLUSTER_VISITS {
            return Err(Error::Limit("cluster validation budget exceeded".into()));
        }
        self.visits += 1;
        Ok(())
    }
    fn next(&mut self, image: &mut Image, cluster: u32) -> Result<u32> {
        self.geometry.address(cluster)?;
        let sector = self.geometry.sector;
        let within = u64::from(cluster) * 4;
        let mut values = Vec::new();
        for base in &self.geometry.fat_offsets {
            let offset = base + within / sector * sector;
            if !self.fat_cache.contains_key(&offset) {
                if self.fat_cache.len() >= 128 {
                    self.fat_cache.pop_first();
                }
                self.fat_cache
                    .insert(offset, image.read(offset, sector as usize)?);
            }
            let mut value = u32_at(&self.fat_cache[&offset], (within % sector) as usize)?;
            if self.geometry.filesystem == FileSystem::Fat32 {
                value &= 0x0fff_ffff;
            }
            values.push(value);
        }
        if values.iter().any(|v| *v != values[0]) {
            return Err(invalid(
                "mirrored FAT copies disagree on a referenced cluster",
            ));
        }
        Ok(values[0])
    }
    fn is_end(&self, next: u32) -> bool {
        if self.geometry.filesystem == FileSystem::Fat32 {
            next >= 0x0fff_fff8
        } else {
            next == 0xffff_ffff
        }
    }
    fn chain(
        &mut self,
        image: &mut Image,
        first: u32,
        expected: Option<u64>,
        freed_first: bool,
        cancel: &AtomicBool,
    ) -> Result<Vec<u32>> {
        if expected.is_some_and(|n| n == 0 || n > MAX_CHAIN) {
            return Err(Error::Limit("cluster chain size limit".into()));
        }
        let mut clusters = Vec::new();
        let mut seen = BTreeSet::new();
        let mut current = first;
        loop {
            self.visit(cancel)?;
            self.geometry.address(current)?;
            if !seen.insert(current) {
                return Err(invalid("cluster-chain loop"));
            }
            clusters.push(current);
            let next = self.next(image, current)?;
            if next == 0 && clusters.len() == 1 && freed_first && expected.is_none_or(|n| n == 1) {
                break;
            }
            if self.is_end(next) {
                if expected.is_some_and(|n| n != clusters.len() as u64) {
                    return Err(invalid("cluster chain shorter than data length"));
                }
                break;
            }
            if next == 0 {
                return Err(Error::Unsupported(
                    "deleted cluster chain is cleared/cut; multi-cluster positions are not guessed"
                        .into(),
                ));
            }
            if expected == Some(clusters.len() as u64) {
                return Err(invalid("cluster chain longer than data length or cyclic"));
            }
            if clusters.len() as u64
                >= expected
                    .unwrap_or(MAX_DIRECTORY_BYTES / self.geometry.cluster)
                    .min(MAX_CHAIN)
            {
                return Err(Error::Limit("cluster chain size limit".into()));
            }
            current = next;
        }
        Ok(clusters)
    }
    fn file_clusters(
        &mut self,
        image: &mut Image,
        entry: &Entry,
        deleted: bool,
        cancel: &AtomicBool,
    ) -> Result<Vec<u32>> {
        if entry.size == 0 {
            if entry.first != 0 {
                return Err(invalid("zero-length file has an allocation"));
            }
            return Ok(Vec::new());
        }
        let count = entry.size.div_ceil(self.geometry.cluster);
        if count > u64::from(self.geometry.clusters) {
            return Err(invalid("file larger than volume"));
        }
        if entry.contiguous {
            self.geometry.address(entry.first)?;
            let last = u64::from(entry.first) + count - 1;
            if last > u64::from(u32::MAX) {
                return Err(invalid("contiguous cluster range overflow"));
            }
            self.geometry.address(last as u32)?;
            if count > MAX_CHAIN {
                return Err(Error::Limit("contiguous cluster validation limit".into()));
            }
            let mut clusters = Vec::with_capacity(count as usize);
            for cluster in u64::from(entry.first)..=last {
                self.visit(cancel)?;
                clusters.push(cluster as u32);
            }
            Ok(clusters)
        } else {
            self.chain(image, entry.first, Some(count), deleted, cancel)
        }
    }
    fn data(&self, clusters: &[u32], size: u64, initialized: u64) -> Result<Data> {
        if size == 0 {
            return Ok(Data::Resident(Vec::new()));
        }
        let mut runs: Vec<Run> = Vec::new();
        for (index, cluster) in clusters.iter().enumerate() {
            let address = self.geometry.address(*cluster)?;
            if let Some(previous) = runs.last_mut()
                && previous
                    .physical
                    .is_some_and(|p| p + previous.length == address)
            {
                previous.length += self.geometry.cluster;
                continue;
            }
            runs.push(Run {
                logical: index as u64 * self.geometry.cluster,
                physical: Some(address),
                length: self.geometry.cluster,
            });
        }
        if size > clusters.len() as u64 * self.geometry.cluster || initialized > size {
            return Err(invalid("stream length outside allocation"));
        }
        Ok(Data::NonResident {
            runs,
            size,
            initialized,
        })
    }
    fn allocated(&mut self, image: &mut Image, cluster: u32) -> Result<bool> {
        self.geometry.address(cluster)?;
        let bitmap = self
            .bitmap
            .as_mut()
            .ok_or_else(|| invalid("missing allocation bitmap"))?;
        let bit = u64::from(cluster - 2);
        let offset = bit / 8;
        if offset < bitmap.start || offset >= bitmap.start + bitmap.cache.len() as u64 {
            let count = bitmap
                .data
                .size()
                .checked_sub(offset)
                .ok_or_else(|| invalid("short allocation bitmap"))?
                .min(65536) as usize;
            bitmap.cache = bitmap.data.read(image, offset, count)?;
            bitmap.start = offset;
        }
        Ok(bitmap
            .cache
            .get((offset - bitmap.start) as usize)
            .ok_or_else(|| invalid("short allocation bitmap"))?
            & (1 << (bit % 8))
            != 0)
    }
    fn prepare_exfat(&mut self, image: &mut Image, limit: u64, cancel: &AtomicBool) -> Result<()> {
        let root = self.chain(image, self.geometry.root, None, false, cancel)?;
        let size = root.len() as u64 * self.geometry.cluster;
        if size > MAX_DIRECTORY_BYTES {
            return Err(Error::Limit("root directory exceeds alpha limit".into()));
        }
        let mut directory = Directory::new(self.data(&root, size, size)?);
        let mut bitmap = None;
        let mut upcase = None;
        for _ in 0..limit {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let Some(slot) = directory.next(image)? else {
                break;
            };
            if slot.raw[0] == 0 {
                break;
            }
            if slot.raw[0] == 0x81 {
                if bitmap.is_some() || slot.raw[1] != 0 {
                    return Err(invalid("multiple/invalid active allocation bitmaps"));
                }
                bitmap = Some(slot.raw);
            }
            if slot.raw[0] == 0x82 {
                if upcase.is_some() {
                    return Err(invalid("duplicate upcase table"));
                }
                upcase = Some(slot.raw);
            }
        }
        let bitmap =
            bitmap.ok_or_else(|| invalid("allocation bitmap not found within root scan limit"))?;
        let upcase =
            upcase.ok_or_else(|| invalid("upcase table not found within root scan limit"))?;
        let bitmap_size = u64_at(&bitmap, 24)?;
        if bitmap_size < u64::from(self.geometry.clusters).div_ceil(8)
            || bitmap_size > u64::from(self.geometry.clusters).div_ceil(8) + self.geometry.cluster
        {
            return Err(invalid("allocation bitmap length"));
        }
        let bitmap_chain = self.chain(
            image,
            u32_at(&bitmap, 20)?,
            Some(bitmap_size.div_ceil(self.geometry.cluster)),
            false,
            cancel,
        )?;
        self.bitmap = Some(Bitmap {
            data: self.data(&bitmap_chain, bitmap_size, bitmap_size)?,
            cache: Vec::new(),
            start: 0,
        });
        let table_size = u64_at(&upcase, 24)?;
        if table_size == 0 || table_size > 131072 {
            return Err(invalid("upcase table size"));
        }
        let table_chain = self.chain(
            image,
            u32_at(&upcase, 20)?,
            Some(table_size.div_ceil(self.geometry.cluster)),
            false,
            cancel,
        )?;
        let bytes =
            self.data(&table_chain, table_size, table_size)?
                .read(image, 0, table_size as usize)?;
        self.upcase = names::upcase_table(&bytes, u32_at(&upcase, 4)?)?;
        let mut metadata = BTreeSet::new();
        for cluster in root.iter().chain(&bitmap_chain).chain(&table_chain) {
            if !metadata.insert(*cluster) || !self.allocated(image, *cluster)? {
                return Err(invalid(
                    "exFAT metadata is overlapping or marked free in the allocation bitmap",
                ));
            }
        }
        Ok(())
    }
}

struct Slot {
    offset: u64,
    raw: [u8; 32],
}
struct Directory {
    data: Data,
    at: u64,
    cache_start: u64,
    cache: Vec<u8>,
}
impl Directory {
    fn new(data: Data) -> Self {
        Self {
            data,
            at: 0,
            cache_start: 0,
            cache: Vec::new(),
        }
    }
    fn next(&mut self, image: &mut Image) -> Result<Option<Slot>> {
        if self.at >= self.data.size() {
            return Ok(None);
        }
        if self.at >= self.cache_start + self.cache.len() as u64 {
            self.cache = self.data.read(
                image,
                self.at,
                (self.data.size() - self.at).min(65536) as usize,
            )?;
            self.cache_start = self.at;
        }
        let Data::NonResident { runs, .. } = &self.data else {
            return Err(invalid("resident directory"));
        };
        let index = runs
            .partition_point(|r| r.logical <= self.at)
            .checked_sub(1)
            .ok_or_else(|| invalid("unmapped directory"))?;
        let run = &runs[index];
        if self.at - run.logical >= run.length {
            return Err(invalid("directory mapping gap"));
        }
        let offset =
            run.physical.ok_or_else(|| invalid("sparse directory"))? + (self.at - run.logical);
        let within = (self.at - self.cache_start) as usize;
        let raw = self
            .cache
            .get(within..within + 32)
            .ok_or_else(|| invalid("short directory slot"))?
            .try_into()
            .unwrap();
        self.at += 32;
        Ok(Some(Slot { offset, raw }))
    }
}

struct Entry {
    candidate: Candidate,
    first: u32,
    size: u64,
    initialized: u64,
    contiguous: bool,
}
pub(crate) struct RecoveredEntry {
    pub candidate: Candidate,
    pub data: Option<Data>,
}
pub(crate) struct Scan {
    pub filesystem: FileSystem,
    pub entries: Vec<RecoveredEntry>,
    pub scanned_records: u64,
    pub total_records: u64,
    pub invalid_records: u64,
    pub warnings: Vec<String>,
    pub cancelled: bool,
    pub truncated: bool,
}
impl Scan {
    fn problem(&mut self, message: String) {
        self.invalid_records += 1;
        if self.warnings.len() < 100 {
            self.warnings.push(message);
        }
    }
}

struct Folder {
    first: u32,
    size: Option<u64>,
    contiguous: bool,
    path: String,
    verified: bool,
    deleted: bool,
    depth: usize,
}

fn candidate(
    filesystem: FileSystem,
    offset: u64,
    name: names::Name,
    prefix: &Folder,
    size: u64,
    raw: &[u8],
) -> Candidate {
    let label = if filesystem == FileSystem::Fat32 {
        "fat32"
    } else {
        "exfat"
    };
    let path = if prefix.path.is_empty() {
        name.text.clone()
    } else {
        format!("{}/{}", prefix.path, name.text)
    };
    Candidate {
        id: format!("{label}-{offset}"),
        record: offset,
        sequence: 0,
        stream_name: None,
        name: name.text,
        original_path: path,
        path_verified: prefix.verified && name.verified,
        size: Some(size),
        supported: false,
        warnings: name.warnings,
        record_sha256: format!("{:x}", Sha256::digest(raw)),
        extent_evidence: ExtentEvidence::Unknown,
        allocation_status: AllocationStatus::Unknown,
    }
}

pub(crate) fn scan(
    image: &mut Image,
    options: ScanOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Scan> {
    let geometry = Geometry::open(image)?;
    let mut scan = Scan {
        filesystem: geometry.filesystem,
        entries: Vec::new(),
        scanned_records: 0,
        total_records: 0,
        invalid_records: 0,
        warnings: geometry.warnings.clone(),
        cancelled: false,
        truncated: false,
    };
    let mut live = VecDeque::from([Folder {
        first: geometry.root,
        size: None,
        contiguous: false,
        path: String::new(),
        verified: true,
        deleted: false,
        depth: 0,
    }]);
    let mut deleted = VecDeque::new();
    let mut volume = Volume {
        geometry,
        fat_cache: BTreeMap::new(),
        bitmap: None,
        upcase: Vec::new(),
        visits: 0,
    };
    if scan.filesystem == FileSystem::Exfat {
        match volume.prepare_exfat(image, options.max_records, cancel) {
            Ok(()) => {}
            Err(Error::Cancelled) => {
                scan.cancelled = true;
                return Ok(scan);
            }
            Err(Error::Limit(message)) => {
                scan.truncated = true;
                scan.warnings.push(format!("RESOURCE_LIMIT: {message}"));
                return Ok(scan);
            }
            Err(error) => return Err(error),
        }
    }
    let mut entries = Vec::new();
    let mut directory_clusters: BTreeSet<u32> = BTreeSet::new();
    let mut live_clusters: BTreeSet<u32> = BTreeSet::new();
    let mut live_complete = true;
    let mut folders_seen = 0;
    while let Some(folder) = live.pop_front().or_else(|| deleted.pop_front()) {
        if cancel.load(Ordering::Relaxed) {
            scan.cancelled = true;
            break;
        }
        if folders_seen >= MAX_DIRECTORIES
            || entries.len() >= options.max_candidates
            || scan.scanned_records >= options.max_records
        {
            scan.truncated = true;
            live_complete = false;
            break;
        }
        folders_seen += 1;
        let clusters = if folder.contiguous {
            let pseudo = Entry {
                candidate: candidate(
                    scan.filesystem,
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
                scan.cancelled = true;
                break;
            }
            Err(error) => {
                if matches!(error, Error::Limit(_)) {
                    scan.truncated = true;
                }
                scan.problem(format!("directory {}: {error}", folder.path));
                if !folder.deleted {
                    live_complete = false;
                }
                continue;
            }
        };
        let size = clusters.len() as u64 * volume.geometry.cluster;
        if size == 0 || size > MAX_DIRECTORY_BYTES || folder.depth > 64 || folder.path.len() > 4096
        {
            scan.truncated = true;
            if !folder.deleted {
                live_complete = false;
            }
            continue;
        }
        let bitmap_conflict = if folder.deleted && scan.filesystem == FileSystem::Exfat {
            clusters.iter().try_fold(false, |found, cluster| {
                volume
                    .allocated(image, *cluster)
                    .map(|allocated| found || allocated)
            })?
        } else {
            false
        };
        if clusters.iter().any(|c| {
            directory_clusters.contains(c) || (folder.deleted && live_clusters.contains(c))
        }) || bitmap_conflict
        {
            scan.problem(format!(
                "directory {}: allocation overlaps a live/previous directory or file",
                folder.path
            ));
            if !folder.deleted {
                live_complete = false;
            }
            continue;
        }
        directory_clusters.extend(&clusters);
        if !folder.deleted {
            live_clusters.extend(&clusters);
        }
        if folder.deleted
            && scan.filesystem == FileSystem::Fat32
            && volume.next(image, folder.first)? == 0
        {
            scan.truncated = true;
            if scan.warnings.len() < 100 {
                scan.warnings.push(format!("deleted directory {}: chain cleared; only its known first cluster was examined", folder.path));
            }
        }
        let mut directory = Directory::new(volume.data(&clusters, size, size)?);
        let mut long = Vec::new();
        loop {
            if cancel.load(Ordering::Relaxed) {
                scan.cancelled = true;
                break;
            }
            if scan.scanned_records >= options.max_records
                || entries.len() >= options.max_candidates
            {
                scan.truncated = true;
                if !folder.deleted {
                    live_complete = false;
                }
                break;
            }
            let Some(slot) = directory.next(image)? else {
                break;
            };
            scan.scanned_records += 1;
            if slot.raw[0] == 0 {
                break;
            }
            let parsed = if scan.filesystem == FileSystem::Fat32 {
                if slot.raw[11] == 0x0f {
                    if long.len() < 21 {
                        long.push(slot.raw);
                    }
                    continue;
                }
                let lfn = std::mem::take(&mut long);
                if slot.raw[11] & 8 != 0 || slot.raw[0] == b'.' {
                    continue;
                }
                if slot.raw[11] & !0x3f != 0 {
                    scan.problem("reserved FAT32 directory attributes".into());
                    live_complete = false;
                    continue;
                }
                let name = names::fat32_name(&slot.raw, &lfn);
                let size = u32_at(&slot.raw, 28)? as u64;
                let first =
                    (u32::from(u16_at(&slot.raw, 20)?) << 16) | u32::from(u16_at(&slot.raw, 26)?);
                let mut raw = lfn.iter().flatten().copied().collect::<Vec<_>>();
                raw.extend(slot.raw);
                let entry = Entry {
                    candidate: candidate(scan.filesystem, slot.offset, name, &folder, size, &raw),
                    first,
                    size,
                    initialized: size,
                    contiguous: false,
                };
                Some((
                    entry,
                    slot.raw[11] & 16 != 0,
                    slot.raw[0] == 0xe5 || folder.deleted,
                ))
            } else if [5, 0x85].contains(&slot.raw[0]) {
                let count = usize::from(slot.raw[1]);
                if !(2..=18).contains(&count) {
                    scan.problem("unsupported exFAT file-entry set size".into());
                    continue;
                }
                if scan.scanned_records + count as u64 > options.max_records {
                    scan.truncated = true;
                    break;
                }
                let mut set = vec![slot.raw];
                for _ in 0..count {
                    if let Some(secondary) = directory.next(image)? {
                        set.push(secondary.raw);
                        scan.scanned_records += 1;
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
                        let entry = Entry {
                            candidate: candidate(
                                scan.filesystem,
                                slot.offset,
                                name,
                                &folder,
                                parsed.size,
                                &raw,
                            ),
                            first: parsed.first,
                            size: parsed.size,
                            initialized: parsed.initialized,
                            contiguous: parsed.contiguous,
                        };
                        Some((entry, parsed.directory, parsed.deleted || folder.deleted))
                    }
                    Err(error) => {
                        scan.problem(format!("entry {}: {error}", slot.offset));
                        None
                    }
                }
            } else {
                if slot.raw[0] & 0xe0 == 0x80 && ![0x81, 0x82, 0x83].contains(&slot.raw[0]) {
                    scan.problem(format!(
                        "unknown critical exFAT directory entry: {:#x}",
                        slot.raw[0]
                    ));
                }
                None
            };
            if let Some((entry, is_directory, is_deleted)) = parsed {
                if is_directory {
                    if scan.filesystem == FileSystem::Exfat
                        && (entry.size == 0
                            || entry.initialized != entry.size
                            || !entry.size.is_multiple_of(volume.geometry.cluster))
                    {
                        scan.problem("invalid exFAT directory length".into());
                        continue;
                    }
                    if live.len() + deleted.len() + folders_seen >= MAX_DIRECTORIES {
                        scan.truncated = true;
                        if !is_deleted {
                            live_complete = false;
                        }
                        continue;
                    }
                    let child = Folder {
                        first: entry.first,
                        size: if scan.filesystem == FileSystem::Exfat {
                            Some(entry.size)
                        } else {
                            None
                        },
                        contiguous: entry.contiguous,
                        path: entry.candidate.original_path,
                        verified: entry.candidate.path_verified,
                        deleted: is_deleted,
                        depth: folder.depth + 1,
                    };
                    if is_deleted {
                        deleted.push_back(child);
                    } else {
                        live.push_back(child);
                    }
                } else if is_deleted {
                    entries.push(entry);
                } else if scan.filesystem == FileSystem::Fat32 {
                    match volume.file_clusters(image, &entry, false, cancel) {
                        Ok(clusters) => live_clusters.extend(clusters),
                        Err(Error::Cancelled) => {
                            scan.cancelled = true;
                            break;
                        }
                        Err(error) => {
                            if matches!(error, Error::Limit(_)) {
                                scan.truncated = true;
                            }
                            live_complete = false;
                            scan.problem(format!(
                                "live file {}: {error}",
                                entry.candidate.original_path
                            ));
                        }
                    }
                }
            }
            if scan.scanned_records.is_multiple_of(256) {
                progress(Progress {
                    scanned_records: scan.scanned_records,
                    total_records: 0,
                    candidates: entries.len(),
                });
            }
        }
        if scan.cancelled
            || scan.scanned_records >= options.max_records
            || entries.len() >= options.max_candidates
        {
            break;
        }
    }
    if !live.is_empty() || !deleted.is_empty() {
        scan.truncated = !scan.cancelled;
        live_complete = false;
    }
    for mut entry in entries {
        let attempt = (|| -> Result<Data> {
            let clusters = volume.file_clusters(image, &entry, true, cancel)?;
            if scan.filesystem == FileSystem::Exfat {
                entry.candidate.extent_evidence = if clusters.is_empty() {
                    ExtentEvidence::Metadata
                } else if entry.contiguous {
                    ExtentEvidence::NoFatChain
                } else if clusters.len() == 1 && volume.next(image, entry.first)? == 0 {
                    ExtentEvidence::SingleCluster
                } else {
                    ExtentEvidence::ResidualFatChain
                };
                for cluster in &clusters {
                    if volume.allocated(image, *cluster)? {
                        entry.candidate.allocation_status = AllocationStatus::Conflict;
                        return Err(Error::Unsupported(
                            "deleted extents are allocated in the exFAT bitmap".into(),
                        ));
                    }
                }
                entry.candidate.allocation_status = AllocationStatus::Unallocated;
            } else {
                let free = clusters.is_empty()
                    || (clusters.len() == 1 && volume.next(image, entry.first)? == 0);
                entry.candidate.extent_evidence = if clusters.is_empty() {
                    ExtentEvidence::Metadata
                } else if free {
                    ExtentEvidence::SingleCluster
                } else {
                    ExtentEvidence::ResidualFatChain
                };
                if clusters.iter().any(|c| live_clusters.contains(c)) {
                    entry.candidate.allocation_status = AllocationStatus::Conflict;
                    return Err(Error::Unsupported(
                        "deleted cluster chain overlaps a live file/directory".into(),
                    ));
                }
                if free {
                    entry.candidate.allocation_status = AllocationStatus::Unallocated;
                } else if live_complete {
                    entry.candidate.allocation_status = AllocationStatus::Unreferenced;
                    entry.candidate.warnings.push("Residual FAT allocation has no live directory reference in this traversal; original content is not proven".into());
                } else {
                    return Err(Error::Unsupported("residual FAT allocation cannot be checked because live directory traversal was incomplete".into()));
                }
            }
            volume.data(&clusters, entry.size, entry.initialized)
        })();
        let data = match attempt {
            Ok(data) => {
                entry.candidate.supported = true;
                Some(data)
            }
            Err(error) => {
                if matches!(error, Error::Cancelled) {
                    scan.cancelled = true;
                }
                if matches!(error, Error::Limit(_)) {
                    scan.truncated = true;
                }
                entry
                    .candidate
                    .warnings
                    .push(format!("{}: {error}", error.code()));
                None
            }
        };
        scan.entries.push(RecoveredEntry {
            candidate: entry.candidate,
            data,
        });
    }
    if !scan.cancelled && !scan.truncated {
        scan.total_records = scan.scanned_records;
    }
    progress(Progress {
        scanned_records: scan.scanned_records,
        total_records: scan.total_records,
        candidates: scan.entries.len(),
    });
    Ok(scan)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn scan_limits_and_preexisting_cancel_return_explicit_partial_sessions() {
        let mut temporary = tempfile::NamedTempFile::new().unwrap();
        let heap = (32 + 513) * 512u64;
        temporary.as_file().set_len(heap + 65536 * 512).unwrap();
        let mut boot = [0u8; 512];
        boot[11..13].copy_from_slice(&512u16.to_le_bytes());
        boot[13] = 1;
        boot[14..16].copy_from_slice(&32u16.to_le_bytes());
        boot[16] = 1;
        boot[32..36].copy_from_slice(&66081u32.to_le_bytes());
        boot[36..40].copy_from_slice(&513u32.to_le_bytes());
        boot[44..48].copy_from_slice(&2u32.to_le_bytes());
        boot[50..52].copy_from_slice(&6u16.to_le_bytes());
        boot[510..512].copy_from_slice(&[0x55, 0xaa]);
        temporary.write_all(&boot).unwrap();
        temporary.seek(SeekFrom::Start(6 * 512)).unwrap();
        temporary.write_all(&boot).unwrap();
        temporary.seek(SeekFrom::Start(32 * 512 + 8)).unwrap();
        temporary.write_all(&0x0fff_ffffu32.to_le_bytes()).unwrap();
        let mut slot = [0u8; 32];
        slot[..11].copy_from_slice(b"\xe5AMPLE  TXT");
        slot[11] = 0x20;
        temporary.seek(SeekFrom::Start(heap)).unwrap();
        temporary.write_all(&slot).unwrap();
        temporary.write_all(&slot).unwrap();
        temporary.flush().unwrap();
        for options in [
            ScanOptions {
                max_records: 1,
                max_candidates: 100,
            },
            ScanOptions {
                max_records: 100,
                max_candidates: 1,
            },
        ] {
            let session =
                crate::scan_image(temporary.path(), options, &AtomicBool::new(false), |_| {})
                    .unwrap();
            assert!(session.truncated && !session.cancelled);
            assert_eq!(session.candidates.len(), 1);
            assert_eq!(session.scanned_records, 1);
            assert_eq!(session.total_records, 0);
        }
        let session = crate::scan_image(
            temporary.path(),
            ScanOptions::default(),
            &AtomicBool::new(true),
            |_| {},
        )
        .unwrap();
        assert!(session.cancelled);
        assert!(session.candidates.is_empty());
    }
}
