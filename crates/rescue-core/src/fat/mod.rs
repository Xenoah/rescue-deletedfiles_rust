//! Read-only FAT12/16/32 and exFAT directory recovery.
mod geometry;
pub(crate) mod indexed;
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
const ESTIMATE_VALIDATION_BUDGET: u64 = 4 * 1024 * 1024 * 1024;
fn estimate_budget() -> u64 {
    ESTIMATE_VALIDATION_BUDGET
}
const DISAGREE: &str = "mirrored FAT copies disagree on a referenced cluster";
const MAX_CHAIN: u64 = 1_048_576;
const MAX_DIRECTORIES: usize = 16384;
const MAX_DIRECTORY_BYTES: u64 = 64 * 1024 * 1024;

/// Discovery checks independent geometry, reserved FAT entries and root bounds.
/// Normal scanning still validates complete chains, allocation and directory sets.
pub(crate) fn filesystem(image: &mut Image) -> Result<FileSystem> {
    Ok(Geometry::open(image)?.filesystem)
}

/// Identify classic FAT by cluster count, never by the cosmetic filesystem label.
/// Full scanning subsequently verifies geometry, mirrors and live ownership.
pub(crate) fn detect_boot(boot: &[u8], length: u64) -> Option<FileSystem> {
    if boot.len() < 512 || boot[510..512] != [0x55, 0xaa] {
        return None;
    }
    let sector = u64::from(u16_at(boot, 11).ok()?);
    let spc = u64::from(boot[13]);
    let reserved = u64::from(u16_at(boot, 14).ok()?);
    let copies = u64::from(boot[16]);
    if ![512, 1024, 2048, 4096].contains(&sector)
        || !spc.is_power_of_two()
        || spc > 128
        || reserved == 0
        || !(1..=2).contains(&copies)
    {
        return None;
    }
    let root = u64::from(u16_at(boot, 17).ok()?);
    let short_fat = u64::from(u16_at(boot, 22).ok()?);
    let fat = if short_fat == 0 {
        u64::from(u32_at(boot, 36).ok()?)
    } else {
        short_fat
    };
    let small = u64::from(u16_at(boot, 19).ok()?);
    let total = if small == 0 {
        u64::from(u32_at(boot, 32).ok()?)
    } else {
        small
    };
    let data = reserved + copies * fat + (root * 32).div_ceil(sector);
    if fat == 0 || data >= total || total * sector > length {
        return None;
    }
    match (total - data) / spc {
        1..4085 if short_fat != 0 && root != 0 => Some(FileSystem::Fat12),
        4085..65525 if short_fat != 0 && root != 0 => Some(FileSystem::Fat16),
        65525..=0x0fff_fff5 if short_fat == 0 && root == 0 => Some(FileSystem::Fat32),
        _ => None,
    }
}

pub(crate) fn probe(image: &mut Image) -> Result<Vec<String>> {
    let geometry = Geometry::open(image)?;
    let mut copies = Vec::new();
    for offset in &geometry.fat_offsets {
        // A damaged USB/SD card often has one unreadable or overwritten FAT copy.
        let Ok(bytes) = image.read(*offset, 8) else {
            continue;
        };
        let valid = match geometry.filesystem {
            FileSystem::Fat12 => {
                u16_at(&bytes, 0)? & 0xff8 == 0xff8 && u16_at(&bytes, 1)? >> 4 == 0xfff
            }
            FileSystem::Fat16 => {
                u16_at(&bytes, 0)? & 0xfff8 == 0xfff8 && u16_at(&bytes, 2)? & 0x3fff == 0x3fff
            }
            _ => {
                u32_at(&bytes, 0)? & 0x0fff_fff8 == 0x0fff_fff8
                    && u32_at(&bytes, 4)? & 0x0fff_fff8 == 0x0fff_fff8
            }
        };
        if valid {
            copies.push(bytes);
        }
    }
    if copies.is_empty() {
        return Err(invalid("no FAT copy has valid reserved entries"));
    }
    let mut evidence = Vec::new();
    if copies.len() < geometry.fat_offsets.len() || copies.windows(2).any(|p| p[0] != p[1]) {
        evidence.push(format!(
            "FAT copies are damaged or disagree: {} of {} copies have valid reserved entries; each chain is accepted only when exactly one copy gives it",
            copies.len(),
            geometry.fat_offsets.len()
        ));
    }
    let (root_at, root_size) = match geometry.fixed_root {
        Some(root) => root,
        None => (geometry.address(geometry.root)?, geometry.cluster),
    };
    let root = image.read(root_at, root_size.min(65536) as usize)?;
    if geometry.filesystem == FileSystem::Exfat {
        for kind in [0x81, 0x82] {
            let entry = root
                .as_chunks::<32>()
                .0
                .iter()
                .take_while(|e| e[0] != 0)
                .find(|e| e[0] == kind)
                .ok_or_else(|| {
                    invalid("exFAT allocation/upcase root entry not found within probe")
                })?;
            geometry.address(u32_at(entry, 20)?)?;
            let length = u64_at(entry, 24)?;
            if length == 0 || length > u64::from(geometry.clusters) * geometry.cluster {
                return Err(invalid("exFAT system stream bounds"));
            }
        }
    }
    evidence.splice(
        0..0,
        [
            "Validated boot geometry and configured backup".to_string(),
            "Checked reserved FAT entries, mirrored copies and root directory bounds".to_string(),
        ],
    );
    Ok(evidence)
}

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
    upcase_warning: Option<String>,
    system_clusters: Vec<u32>,
    visits: u64,
    /// Use only this FAT copy (set while resolving a mirrored-copy disagreement).
    copy: Option<usize>,
    /// Chains resolved from a single FAT copy because the mirrors disagreed.
    copy_choices: Vec<String>,
    estimate_validation_remaining: u64,
}

/// Allocation-only view for signature search. It does not infer deletion from
/// a free cluster and does not guess around invalid/mismatched FAT copies.
pub(crate) struct AllocationReader(Volume);
impl AllocationReader {
    pub(crate) fn open(image: &mut Image, cancel: &AtomicBool) -> Result<Self> {
        let geometry = Geometry::open(image)?;
        let mut volume = Volume {
            geometry,
            fat_cache: BTreeMap::new(),
            bitmap: None,
            upcase: Vec::new(),
            upcase_warning: None,
            system_clusters: Vec::new(),
            visits: 0,
            copy: None,
            copy_choices: Vec::new(),
            estimate_validation_remaining: ESTIMATE_VALIDATION_BUDGET,
        };
        if volume.geometry.filesystem == FileSystem::Exfat {
            volume.prepare_exfat(image, 1_048_576, false, cancel)?;
        } else if volume.geometry.fixed_root.is_none()
            && volume.next(image, volume.geometry.root)? == 0
        {
            return Err(invalid("FAT32 root directory is marked free"));
        }
        Ok(Self(volume))
    }
    pub(crate) fn status(
        &mut self,
        image: &mut Image,
        offset: u64,
        length: u64,
        cancel: &AtomicBool,
    ) -> Result<AllocationStatus> {
        let volume = &mut self.0;
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid("allocation extent overflow"))?;
        let heap_end =
            volume.geometry.heap + u64::from(volume.geometry.clusters) * volume.geometry.cluster;
        if offset < volume.geometry.heap || end > heap_end {
            return Ok(AllocationStatus::Conflict);
        }
        let first = (offset - volume.geometry.heap) / volume.geometry.cluster + 2;
        let last = (end - volume.geometry.heap).div_ceil(volume.geometry.cluster) + 2;
        for cluster in first..last {
            if cluster.is_multiple_of(4096) && cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let allocated = if volume.geometry.filesystem == FileSystem::Exfat {
                volume.allocated(image, cluster as u32)?
            } else {
                volume.next(image, cluster as u32)? != 0
            };
            if allocated {
                return Ok(AllocationStatus::Conflict);
            }
        }
        Ok(AllocationStatus::Unallocated)
    }
}

/// Merged cluster intervals keep large exFAT NoFatChain live files cheap to index.
#[derive(Default, serde::Serialize, serde::Deserialize)]
struct Owners(BTreeMap<u32, u32>);
impl Owners {
    fn contains(&self, cluster: &u32) -> bool {
        self.0
            .range(..=*cluster)
            .next_back()
            .is_some_and(|(_, end)| cluster <= end)
    }
    fn add(&mut self, mut start: u32, mut end: u32) {
        if let Some((&previous, &last)) = self.0.range(..=start).next_back()
            && u64::from(last) + 1 >= u64::from(start)
        {
            start = previous;
            end = end.max(last);
            self.0.remove(&previous);
        }
        loop {
            let next = self.0.range(start..).next().map(|(&a, &b)| (a, b));
            let Some((a, b)) = next else {
                break;
            };
            if u64::from(a) > u64::from(end) + 1 {
                break;
            }
            end = end.max(b);
            self.0.remove(&a);
        }
        self.0.insert(start, end);
    }
    fn extend(&mut self, clusters: &[u32]) {
        for cluster in clusters {
            self.add(*cluster, *cluster);
        }
    }
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
        let (within, count) = match self.geometry.filesystem {
            FileSystem::Fat12 => (u64::from(cluster) * 3 / 2, 2),
            FileSystem::Fat16 => (u64::from(cluster) * 2, 2),
            _ => (u64::from(cluster) * 4, 4),
        };
        let mut values = Vec::new();
        let mut unreadable = None;
        'copies: for (index, base) in self.geometry.fat_offsets.iter().enumerate() {
            if self.copy.is_some_and(|only| only != index) {
                continue;
            }
            let mut raw = [0u8; 4];
            for (n, byte) in raw.iter_mut().take(count).enumerate() {
                let at = within + n as u64;
                let offset = base + at / sector * sector;
                if !self.fat_cache.contains_key(&offset) {
                    if self.fat_cache.len() >= 128 {
                        self.fat_cache.pop_first();
                    }
                    match image.read(offset, sector as usize) {
                        Ok(bytes) => {
                            self.fat_cache.insert(offset, bytes);
                        }
                        Err(e @ (Error::MissingRanges(_) | Error::Io(_))) => {
                            unreadable = Some(e);
                            continue 'copies;
                        }
                        Err(e) => return Err(e),
                    }
                }
                *byte = self.fat_cache[&offset][(at % sector) as usize];
            }
            let value = u32::from_le_bytes(raw);
            values.push(match self.geometry.filesystem {
                FileSystem::Fat12 if cluster.is_multiple_of(2) => value & 0xfff,
                FileSystem::Fat12 => value >> 4,
                FileSystem::Fat32 => value & 0x0fff_ffff,
                _ => value,
            });
        }
        if values.is_empty() {
            return Err(unreadable.unwrap_or_else(|| invalid("no FAT copy")));
        }
        if values.iter().any(|v| *v != values[0]) {
            return Err(invalid(DISAGREE));
        }
        Ok(values[0])
    }
    fn is_end(&self, next: u32) -> bool {
        match self.geometry.filesystem {
            FileSystem::Fat12 => next >= 0xff8,
            FileSystem::Fat16 => next >= 0xfff8,
            FileSystem::Fat32 => next >= 0x0fff_fff8,
            _ => next == 0xffff_ffff,
        }
    }
    fn is_fixed_root(&self, folder: &Folder) -> bool {
        self.geometry.fixed_root.is_some()
            && folder.first == 0
            && folder.parent.is_none()
            && folder.path.is_empty()
            && folder.depth == 0
            && !folder.deleted
    }
    fn directory_size(&self, folder: &Folder, clusters: &[u32]) -> u64 {
        if self.is_fixed_root(folder) {
            self.geometry.fixed_root.unwrap().1
        } else {
            clusters.len() as u64 * self.geometry.cluster
        }
    }
    fn directory_data(&self, folder: &Folder, clusters: &[u32]) -> Result<Data> {
        let size = self.directory_size(folder, clusters);
        if self.is_fixed_root(folder) {
            let (start, _) = self.geometry.fixed_root.unwrap();
            Ok(Data::NonResident {
                runs: vec![Run {
                    logical: 0,
                    physical: Some(start),
                    length: size,
                }],
                size,
                initialized: size,
            })
        } else {
            self.data(clusters, size, size)
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
        self.chain_observed(image, first, expected, freed_first, cancel, &mut |_| {})
    }
    fn file_clusters(
        &mut self,
        image: &mut Image,
        entry: &Entry,
        deleted: bool,
        cancel: &AtomicBool,
    ) -> Result<Vec<u32>> {
        self.file_clusters_observed(image, entry, deleted, cancel, &mut |_| {})
    }
    fn chain_observed(
        &mut self,
        image: &mut Image,
        first: u32,
        expected: Option<u64>,
        freed_first: bool,
        cancel: &AtomicBool,
        observe: &mut impl FnMut(u32),
    ) -> Result<Vec<u32>> {
        match self.chain_once(image, first, expected, freed_first, cancel, observe) {
            Err(Error::Invalid(message))
                if message.ends_with(DISAGREE)
                    && self.copy.is_none()
                    && self.geometry.fat_offsets.len() > 1 =>
            {
                let mut accepted = Vec::new();
                for index in 0..self.geometry.fat_offsets.len() {
                    self.copy = Some(index);
                    let attempt =
                        self.chain_once(image, first, expected, freed_first, cancel, &mut |_| {});
                    self.copy = None;
                    match attempt {
                        Ok(clusters) => accepted.push((index, clusters)),
                        Err(e @ (Error::Cancelled | Error::Limit(_))) => return Err(e),
                        Err(_) => {}
                    }
                }
                if accepted.len() != 1 {
                    return Err(invalid(&format!(
                        "{DISAGREE}; {} copies give a valid chain{}, so none is chosen",
                        accepted.len(),
                        if expected.is_some() {
                            " of the recorded length"
                        } else {
                            ""
                        }
                    )));
                }
                let (index, clusters) = accepted.pop().unwrap();
                for cluster in &clusters {
                    observe(*cluster);
                }
                if self.copy_choices.len() < 1000 {
                    self.copy_choices.push(format!(
                        "cluster {first}: mirrored FAT copies disagree; only FAT copy {index} gives a loop-free chain{}",
                        if expected.is_some() { " of the recorded file length" } else { "" }
                    ));
                }
                Ok(clusters)
            }
            other => other,
        }
    }
    fn chain_once(
        &mut self,
        image: &mut Image,
        first: u32,
        expected: Option<u64>,
        freed_first: bool,
        cancel: &AtomicBool,
        observe: &mut impl FnMut(u32),
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
            observe(current);
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
    fn file_clusters_observed(
        &mut self,
        image: &mut Image,
        entry: &Entry,
        deleted: bool,
        cancel: &AtomicBool,
        observe: &mut impl FnMut(u32),
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
                observe(cluster as u32);
                clusters.push(cluster as u32);
            }
            Ok(clusters)
        } else {
            self.chain_observed(image, entry.first, Some(count), deleted, cancel, observe)
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
    fn prepare_exfat(
        &mut self,
        image: &mut Image,
        limit: u64,
        allow_damaged_upcase: bool,
        cancel: &AtomicBool,
    ) -> Result<()> {
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
        match names::upcase_table(&bytes, u32_at(&upcase, 4)?) {
            Ok(table) => self.upcase = table,
            Err(Error::Invalid(reason)) if allow_damaged_upcase => {
                self.upcase_warning = Some(format!(
                    "EXFAT_UPCASE_UNVERIFIED: {reason}; explicit damaged-upcase option. Entry-set checksum and allocation checks retained; name hash cannot be verified. Original path spelling is unverified."
                ));
            }
            Err(error) => return Err(error),
        }
        let mut metadata = BTreeSet::new();
        for cluster in root.iter().chain(&bitmap_chain).chain(&table_chain) {
            if !metadata.insert(*cluster) || !self.allocated(image, *cluster)? {
                return Err(invalid(
                    "exFAT metadata is overlapping or marked free in the allocation bitmap",
                ));
            }
        }
        self.system_clusters.extend(bitmap_chain);
        self.system_clusters.extend(table_chain);
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
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
#[derive(serde::Serialize, serde::Deserialize)]
pub(crate) struct Scan {
    pub filesystem: FileSystem,
    #[serde(skip)]
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

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Folder {
    first: u32,
    size: Option<u64>,
    contiguous: bool,
    path: String,
    verified: bool,
    deleted: bool,
    depth: usize,
    parent: Option<usize>,
}

fn directory_first(filesystem: FileSystem, raw: &[u8]) -> Result<u32> {
    let low = u32::from(u16_at(raw, 26)?);
    Ok(if filesystem == FileSystem::Fat32 {
        low | u32::from(u16_at(raw, 20)?) << 16
    } else {
        low
    })
}

fn candidate(
    filesystem: FileSystem,
    offset: u64,
    name: names::Name,
    prefix: &Folder,
    size: u64,
    raw: &[u8],
) -> Candidate {
    let label = match filesystem {
        FileSystem::Fat12 => "fat12",
        FileSystem::Fat16 => "fat16",
        FileSystem::Fat32 => "fat32",
        _ => "exfat",
    };
    let path = if prefix.path.is_empty() {
        name.text.clone()
    } else {
        format!("{}/{}", prefix.path, name.text)
    };
    Candidate {
        fragment_offset: None,
        id: format!("{label}-{offset}"),
        record: offset,
        sequence: 0,
        stream_name: None,
        name: name.text,
        original_path: path,
        path_verified: prefix.verified && name.verified,
        alternate_names: Vec::new(),
        size: Some(size),
        supported: false,
        warnings: name.warnings,
        record_sha256: format!("{:x}", Sha256::digest(raw)),
        extent_evidence: ExtentEvidence::Unknown,
        allocation_status: AllocationStatus::Unknown,
        created: None,
        modified: None,
    }
}

fn invalid_origin(
    mut origin: usize,
    parents: &[Option<usize>],
    conflicted: &BTreeSet<usize>,
) -> bool {
    loop {
        if conflicted.contains(&origin) {
            return true;
        }
        let Some(parent) = parents[origin] else {
            return false;
        };
        origin = parent;
    }
}

pub(crate) fn scan(
    image: &mut Image,
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<Scan> {
    scan_selected(image, options, None, cancel, progress)
}

/// Still traverse all live ownership and deleted directory ancestry, but retain
/// only requested files. A late selected file must not inherit the legacy
/// candidate limit, and early matches cannot bypass later allocation conflicts.
pub(crate) fn scan_selected(
    image: &mut Image,
    options: ScanOptions,
    selected: Option<&BTreeSet<String>>,
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
        parent: None,
    }]);
    let mut deleted = VecDeque::new();
    let mut volume = Volume {
        geometry,
        fat_cache: BTreeMap::new(),
        bitmap: None,
        upcase: Vec::new(),
        upcase_warning: None,
        system_clusters: Vec::new(),
        visits: 0,
        copy: None,
        copy_choices: Vec::new(),
        estimate_validation_remaining: ESTIMATE_VALIDATION_BUDGET,
    };
    if scan.filesystem == FileSystem::Exfat {
        match volume.prepare_exfat(
            image,
            options.max_records,
            options.exfat_allow_damaged_upcase,
            cancel,
        ) {
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
    scan.warnings.extend(volume.upcase_warning.iter().cloned());
    let mut entries = Vec::new();
    let mut selected_bytes = 0u64;
    let mut directory_clusters: BTreeMap<u32, usize> = BTreeMap::new();
    let mut parents = Vec::new();
    let mut conflicted = BTreeSet::new();
    let mut live_clusters = Owners::default();
    live_clusters.extend(&volume.system_clusters);
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
        let origin = parents.len();
        parents.push(folder.parent);
        folders_seen += 1;
        if folder
            .parent
            .is_some_and(|id| invalid_origin(id, &parents, &conflicted))
        {
            if !folder.deleted {
                live_complete = false;
            }
            continue;
        }
        let clusters = if volume.is_fixed_root(&folder) {
            Ok(Vec::new())
        } else if folder.contiguous {
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
        let size = volume.directory_size(&folder, &clusters);
        if size == 0 || size > MAX_DIRECTORY_BYTES || folder.depth > 64 || folder.path.len() > 4096
        {
            scan.truncated = true;
            if !folder.deleted {
                live_complete = false;
            }
            continue;
        }
        let bitmap_conflict = if scan.filesystem == FileSystem::Exfat {
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
            .any(|c| directory_clusters.contains_key(c) || live_clusters.contains(c))
            || bitmap_conflict
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
        directory_clusters.extend(clusters.iter().map(|c| (*c, origin)));
        if !folder.deleted {
            live_clusters.extend(&clusters);
        }
        if folder.deleted && scan.filesystem.classic_fat() && volume.next(image, folder.first)? == 0
        {
            scan.truncated = true;
            if scan.warnings.len() < 100 {
                scan.warnings.push(format!("deleted directory {}: chain cleared; only its known first cluster was examined", folder.path));
            }
        }
        let mut directory = Directory::new(volume.directory_data(&folder, &clusters)?);
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
            let parsed = if scan.filesystem.classic_fat() {
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
                let name = names::fat32_name(&slot.raw, &lfn, options.fat_short_name_encoding);
                let size = u32_at(&slot.raw, 28)? as u64;
                let first = directory_first(scan.filesystem, &slot.raw)?;
                let mut raw = lfn.iter().flatten().copied().collect::<Vec<_>>();
                raw.extend(slot.raw);
                let mut entry = Entry {
                    candidate: candidate(scan.filesystem, slot.offset, name, &folder, size, &raw),
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
                    slot.raw[0] == 0xe5 || folder.deleted,
                ))
            } else if [5, 0x85].contains(&slot.raw[0]) {
                let count = usize::from(slot.raw[1]);
                if !(2..=18).contains(&count) {
                    if !folder.deleted && slot.raw[0] & 0x80 != 0 {
                        live_complete = false;
                    }
                    scan.problem("unsupported exFAT file-entry set size".into());
                    continue;
                }
                if scan.scanned_records + count as u64 > options.max_records {
                    scan.truncated = true;
                    if !folder.deleted {
                        live_complete = false;
                    }
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
                            verified: volume.upcase_warning.is_none(),
                            warnings: volume.upcase_warning.iter().cloned().collect(),
                        };
                        let raw = set.iter().flatten().copied().collect::<Vec<_>>();
                        let mut entry = Entry {
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
                        entry.candidate.created = parsed.created;
                        entry.candidate.modified = parsed.modified;
                        Some((entry, parsed.directory, parsed.deleted || folder.deleted))
                    }
                    Err(error) => {
                        if !folder.deleted && slot.raw[0] & 0x80 != 0 {
                            live_complete = false;
                        }
                        scan.problem(format!("entry {}: {error}", slot.offset));
                        None
                    }
                }
            } else {
                if slot.raw[0] & 0xe0 == 0x80 && ![0x81, 0x82, 0x83].contains(&slot.raw[0]) {
                    if !folder.deleted {
                        live_complete = false;
                    }
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
                        if !is_deleted {
                            live_complete = false;
                        }
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
                        parent: Some(origin),
                    };
                    if is_deleted {
                        deleted.push_back(child);
                    } else {
                        live.push_back(child);
                    }
                } else if is_deleted {
                    if selected.is_none_or(|ids| ids.contains(&entry.candidate.id)) {
                        if selected.is_some() {
                            selected_bytes += crate::serialized_size(&entry.candidate)?;
                            if selected_bytes > crate::CANDIDATE_BYTES {
                                return Err(Error::Limit(
                                    "selected FAT metadata exceeds 128 MiB".into(),
                                ));
                            }
                        }
                        entries.push((entry, origin));
                    }
                } else if scan.filesystem == FileSystem::Exfat && entry.contiguous && entry.size > 0
                {
                    let count = entry.size.div_ceil(volume.geometry.cluster);
                    let last = u64::from(entry.first) + count - 1;
                    if count > u64::from(volume.geometry.clusters)
                        || last > u64::from(u32::MAX)
                        || volume.geometry.address(entry.first).is_err()
                        || volume.geometry.address(last as u32).is_err()
                    {
                        live_complete = false;
                        if volume.geometry.address(entry.first).is_ok() {
                            live_clusters.add(entry.first, entry.first);
                            if let Some(id) = directory_clusters.get(&entry.first) {
                                conflicted.insert(*id);
                            }
                        }
                        scan.problem("invalid live exFAT contiguous allocation".into());
                    } else {
                        live_clusters.add(entry.first, last as u32);
                        conflicted.extend(
                            directory_clusters
                                .range(entry.first..=last as u32)
                                .map(|(_, id)| *id),
                        );
                    }
                } else {
                    // An active entry's known start remains owned even with a bad FAT.
                    if entry.first != 0 && volume.geometry.address(entry.first).is_ok() {
                        live_clusters.add(entry.first, entry.first);
                        if let Some(id) = directory_clusters.get(&entry.first) {
                            conflicted.insert(*id);
                        }
                    }
                    let mut protect = |cluster| {
                        live_clusters.add(cluster, cluster);
                        if let Some(id) = directory_clusters.get(&cluster) {
                            conflicted.insert(*id);
                        }
                    };
                    match volume.file_clusters_observed(image, &entry, false, cancel, &mut protect)
                    {
                        Ok(_) => {}
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
                    unit: crate::ProgressUnit::Records,
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
    if !conflicted.is_empty() {
        live_complete = false;
        scan.problem(format!("{} previously parsed directories overlap active file allocations; their descendants are not trusted", conflicted.len()));
    }
    let ownership = Ownership {
        parents: &parents,
        conflicted: &conflicted,
        live_clusters: &live_clusters,
        live_complete,
        estimate_contiguous: options.estimate_fat_contiguous,
    };
    for (entry, origin) in entries {
        let result = recover_entry(
            image,
            &mut volume,
            entry,
            origin,
            &ownership,
            &mut scan,
            cancel,
        );
        scan.entries.push(result);
    }
    if !scan.cancelled && !scan.truncated {
        scan.total_records = scan.scanned_records;
    }
    progress(Progress {
        scanned_records: scan.scanned_records,
        total_records: scan.total_records,
        candidates: scan.entries.len(),
        unit: crate::ProgressUnit::Records,
    });
    if !volume.copy_choices.is_empty() {
        scan.warnings.push(format!(
            "Damaged FAT mirror: {} chain(s) were taken from the single FAT copy that is consistent; see first: {}",
            volume.copy_choices.len(),
            volume.copy_choices[0]
        ));
    }
    Ok(scan)
}

struct Ownership<'a> {
    parents: &'a [Option<usize>],
    conflicted: &'a BTreeSet<usize>,
    live_clusters: &'a Owners,
    live_complete: bool,
    estimate_contiguous: bool,
}
fn recover_entry(
    image: &mut Image,
    volume: &mut Volume,
    mut entry: Entry,
    origin: usize,
    ownership: &Ownership<'_>,
    scan: &mut Scan,
    cancel: &AtomicBool,
) -> RecoveredEntry {
    if invalid_origin(origin, ownership.parents, ownership.conflicted) {
        entry.candidate.allocation_status = AllocationStatus::Conflict;
        entry.candidate.warnings.push(
            "Directory provenance overlaps an active file allocation; candidate is not trusted"
                .into(),
        );
        return RecoveredEntry {
            candidate: entry.candidate,
            data: None,
        };
    }
    let attempt = (|| -> Result<Data> {
        let (clusters, estimated) = match volume.file_clusters(image, &entry, true, cancel) {
            Ok(clusters) => (clusters, false),
            Err(Error::Unsupported(reason))
                if ownership.estimate_contiguous
                    && scan.filesystem.classic_fat()
                    && reason.starts_with("deleted cluster chain is cleared/cut") =>
            {
                if !ownership.live_complete {
                    return Err(Error::Unsupported(
                        "FAT estimate refused: live ownership traversal was incomplete".into(),
                    ));
                }
                let count = entry.size.div_ceil(volume.geometry.cluster);
                if count > MAX_CHAIN || entry.size > 64 * 1024 * 1024 {
                    return Err(Error::Limit(
                        "FAT estimate exceeds the cluster or 64 MiB file limit".into(),
                    ));
                }
                let last = u64::from(entry.first)
                    .checked_add(count)
                    .and_then(|n| n.checked_sub(1))
                    .filter(|n| *n <= u64::from(u32::MAX))
                    .ok_or_else(|| invalid("estimated FAT cluster overflow"))?;
                volume.geometry.address(entry.first)?;
                volume.geometry.address(last as u32)?;
                let mut clusters = Vec::with_capacity(count as usize);
                for cluster in u64::from(entry.first)..=last {
                    volume.visit(cancel)?;
                    let cluster = cluster as u32;
                    if ownership.live_clusters.contains(&cluster) {
                        entry.candidate.allocation_status = AllocationStatus::Conflict;
                        return Err(Error::Unsupported(
                            "FAT estimate overlaps a live file or directory".into(),
                        ));
                    }
                    // A guess requires every configured mirror to be readable
                    // and free. Residual-chain mirror fallback is not used here.
                    for copy in 0..volume.geometry.fat_offsets.len() {
                        volume.copy = Some(copy);
                        let value = volume.next(image, cluster);
                        volume.copy = None;
                        if value? != 0 {
                            entry.candidate.allocation_status = AllocationStatus::Conflict;
                            return Err(Error::Unsupported(
                                "FAT estimate includes a non-free cluster".into(),
                            ));
                        }
                    }
                    clusters.push(cluster);
                }
                entry.candidate.extent_evidence = ExtentEvidence::EstimatedContiguous;
                let checked = crate::carve::validate_estimated_extent(
                    image,
                    volume.geometry.address(entry.first)?,
                    entry.size,
                    &entry.candidate.name,
                    cancel,
                    &mut volume.estimate_validation_remaining,
                )?;
                entry.candidate.warnings.push(format!(
                    "ESTIMATED_CONTIGUOUS: the FAT chain was cleared; consecutive free clusters were assumed by explicit opt-in. {} Original content and fragmentation are unverified; compare with a known hash or inspect the output.",
                    if checked { "Format structure was validated." } else { "No structural validator is available for this format." }
                ));
                (clusters, true)
            }
            Err(error) => return Err(error),
        };
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
                if ownership.live_clusters.contains(cluster) || volume.allocated(image, *cluster)? {
                    entry.candidate.allocation_status = AllocationStatus::Conflict;
                    return Err(Error::Unsupported(
                            "deleted extents overlap a live file/directory or allocated exFAT bitmap bits".into(),
                        ));
                }
            }
            entry.candidate.allocation_status = AllocationStatus::Unallocated;
            if !ownership.live_complete {
                entry.candidate.allocation_status = AllocationStatus::Unknown;
                entry.candidate.warnings.push(
                    "Bitmap marks these extents free, but live ownership traversal was incomplete"
                        .into(),
                );
            }
        } else {
            let free = estimated
                || clusters.is_empty()
                || (clusters.len() == 1 && volume.next(image, entry.first)? == 0);
            entry.candidate.extent_evidence = if estimated {
                ExtentEvidence::EstimatedContiguous
            } else if clusters.is_empty() {
                ExtentEvidence::Metadata
            } else if free {
                ExtentEvidence::SingleCluster
            } else {
                ExtentEvidence::ResidualFatChain
            };
            if clusters.iter().any(|c| ownership.live_clusters.contains(c)) {
                entry.candidate.allocation_status = AllocationStatus::Conflict;
                return Err(Error::Unsupported(
                    "deleted cluster chain overlaps a live file/directory".into(),
                ));
            }
            if free && !ownership.live_complete && !clusters.is_empty() {
                entry.candidate.allocation_status = AllocationStatus::Unknown;
                return Err(Error::Unsupported("free FAT cluster cannot be trusted because live ownership traversal was incomplete".into()));
            }
            if free {
                entry.candidate.allocation_status = AllocationStatus::Unallocated;
            } else if ownership.live_complete {
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
    RecoveredEntry {
        candidate: entry.candidate,
        data,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::{Seek, SeekFrom, Write};

    #[test]
    fn ownership_intervals_match_a_cluster_set_after_overlapping_insertions() {
        let mut owners = Owners::default();
        let mut reference = BTreeSet::new();
        let mut seed = 0x7219_03abu32;
        for _ in 0..256 {
            seed = seed.wrapping_mul(1664525).wrapping_add(1013904223);
            let start = seed % 4096;
            let end = (start + ((seed >> 20) % 64)).min(4096);
            owners.add(start, end);
            reference.extend(start..=end);
        }
        for cluster in 0..=4097 {
            assert_eq!(owners.contains(&cluster), reference.contains(&cluster));
        }
        owners.add(u32::MAX - 2, u32::MAX);
        assert!(owners.contains(&u32::MAX));
        assert!(!owners.contains(&(u32::MAX - 3)));
    }

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
                fragments: false,
                estimate_fat_contiguous: false,
                fat_short_name_encoding: crate::FatShortNameEncoding::Ascii,
                exfat_allow_damaged_upcase: false,
                max_records: 1,
                max_candidates: 100,
            },
            ScanOptions {
                fragments: false,
                estimate_fat_contiguous: false,
                fat_short_name_encoding: crate::FatShortNameEncoding::Ascii,
                exfat_allow_damaged_upcase: false,
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
