mod acquisition;
mod acquisition_reader;
mod acquisition_worker;
mod archive;
mod carve;
mod contract;
mod destination;
mod device;
mod discovery;
mod disk_scan;
mod fat;
mod index;
mod indexed_scan;
mod lznt1;
mod mapped_source;
mod naming;
mod ntfs;
mod ntfs_remnants;
mod partitions;
mod preview;
mod raid;
mod read_access;
mod read_map;
mod report;
mod restore;
mod search;
mod source;
mod source_info;
mod source_scope;
mod timestamps;

pub use acquisition::{
    AcquisitionOptions, AcquisitionProgress, AcquisitionResult, ReadMapPage, ReadMapRange,
    acquire_image, acquire_image_with_roots, inspect_read_map, read_map_page,
};
pub use acquisition_worker::acquisition_worker_entry;
pub use archive::{ArchiveRecovery, recover_zip, recover_zip_to};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
pub use contract::{JobKind, OperationError};
pub use destination::{OutputRoot, RecoveryDestination, canonical_output_path};
pub use device::{DiskSource, canonical_source_path, guard_output_path, is_device, list_disks};
pub use discovery::{
    BootRole, DiscoveredVolume, DiscoveryOptions, DiscoveryResult, RecoveryBoot, discover_volumes,
    scan_discovered_volume,
};
pub use disk_scan::scan_source;
pub use index::{
    CandidateIndex, IndexCursor, IndexGroup, IndexPage, IndexQuery, IndexSort, IndexStatus,
    IndexWriter,
};
pub use indexed_scan::{IndexedProgress, resume_indexed_scan, scan_indexed};
pub use naming::{NameBasis, NameChange, NameChangeReason, RecoveryNaming, file_extension};
use ntfs::Ntfs;
pub use partitions::{SourceLayout, SourceRegion, inspect_source};
pub use preview::{
    Preview, PreviewExport, PreviewInfo, preview_candidate, preview_worker_entry, save_preview,
};
pub use raid::{
    RAID_SUFFIX, RaidConfig, RaidEvidence, RaidInspection, RaidLayout, RaidLevel, RaidMember,
    create_raid_manifest, inspect_raid, is_raid_path,
};
pub use raid::{RaidAlternative, RaidSearchOptions, RaidSearchResult, search_raid};
pub use raid::{RaidReadRange, RaidReadTrace};
pub use read_access::{ReadAccess, read_broker_entry, request_read_access};
pub use read_map::{
    AcquisitionIdentity, AcquisitionStrategy, MissingRange, ReadMapSummary, ReadState,
};
pub use report::{
    ReportFormat, export_recovery_report, read_recovery_report, recovery_report,
    render_recovery_report,
};
pub use restore::{
    CollisionPolicy, MAX_RESTORE_MEMORY, RawExport, RestoreOptions, RestoreTarget, VolumeRoot,
    ZeroFilled, export_unrecovered, restore_files, restore_files_with_roots, restore_zip,
    restore_zip_with_roots,
};
pub use search::{CarveScope, ScanMethod, SearchSettings, scan_with_settings};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use source::Fingerprint;
use source::Image;
pub use source_info::{CandidateReference, ScopedCandidate, SourceInfo, SourceKind, SourceReadMap};
pub use source_scope::SourceScope;
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
pub use timestamps::FileTimestamp;
use uuid::Uuid;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_SESSION_BYTES: u64 = 128 * 1024 * 1024;
// Keep room for source identity, session fields, bounded warnings and separators.
const CANDIDATE_BYTES: u64 = MAX_SESSION_BYTES - 1024 * 1024;
pub use carve::scan_carving;
pub type Result<T> = std::result::Result<T, Error>;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileSystem {
    #[default]
    Ntfs,
    Fat12,
    Fat16,
    Fat32,
    Exfat,
    /// Signature scan of a regular image; no filesystem interpretation.
    Raw,
    /// A physical disk containing one or more partition volumes.
    Disk,
}
impl FileSystem {
    pub(crate) fn classic_fat(self) -> bool {
        matches!(self, Self::Fat12 | Self::Fat16 | Self::Fat32)
    }
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtentEvidence {
    #[default]
    Unknown,
    Metadata,
    MetadataRemnant,
    ResidualFatChain,
    /// Explicit opt-in: cleared FAT entries are assumed to have been contiguous.
    EstimatedContiguous,
    SingleCluster,
    NoFatChain,
    PngValidated,
    JpegValidated,
    PdfStructure,
    ZipCrcValidated,
    MediaStructure,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AllocationStatus {
    #[default]
    Unknown,
    Unallocated,
    Unreferenced,
    Conflict,
}

#[derive(Debug, thiserror::Error)]
pub enum Error {
    #[error("session index: {0}")]
    Index(#[from] rusqlite::Error),
    #[error("{0}")]
    Io(#[from] std::io::Error),
    #[error("{0}")]
    Json(#[from] serde_json::Error),
    #[error("invalid image: {0}")]
    Invalid(String),
    #[error("unsupported: {0}")]
    Unsupported(String),
    #[error("resource limit reached: {0}")]
    Limit(String),
    #[error("source identity or metadata changed; rescan the image")]
    SourceChanged,
    #[error("destination directory changed; select the output folder again")]
    DestinationChanged,
    #[error("unsafe destination: {0}")]
    UnsafeDestination(String),
    #[error("operation cancelled")]
    Cancelled,
    #[error("source contains unread or failed byte ranges")]
    MissingRanges(Vec<MissingRange>),
    #[error("invalid request: {0}")]
    Request(String),
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Index(_) => "INDEX_ERROR",
            Self::Io(_) => "IO_ERROR",
            Self::Json(_) => "INVALID_JSON",
            Self::Invalid(_) => "INVALID_IMAGE",
            Self::Unsupported(_) => "UNSUPPORTED",
            Self::Limit(_) => "RESOURCE_LIMIT",
            Self::SourceChanged => "SOURCE_CHANGED",
            Self::DestinationChanged => "DESTINATION_CHANGED",
            Self::Cancelled => "CANCELLED",
            Self::MissingRanges(_) => "MISSING_RANGES",
            Self::UnsafeDestination(_) => "UNSAFE_DESTINATION",
            Self::Request(_) => "INVALID_REQUEST",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct CandidateName {
    pub name: String,
    pub original_path: String,
    pub path_verified: bool,
    /// NTFS FILE_NAME namespace: 0 POSIX, 1 Win32, 2 DOS, 3 Win32/DOS.
    pub namespace: u8,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub record: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fragment_offset: Option<u64>,
    pub sequence: u16,
    #[serde(default)]
    pub stream_name: Option<String>,
    pub name: String,
    pub original_path: String,
    pub path_verified: bool,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub alternate_names: Vec<CandidateName>,
    pub size: Option<u64>,
    pub supported: bool,
    pub warnings: Vec<String>,
    pub record_sha256: String,
    #[serde(default)]
    pub extent_evidence: ExtentEvidence,
    #[serde(default)]
    pub allocation_status: AllocationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub created: Option<FileTimestamp>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub modified: Option<FileTimestamp>,
}

impl Candidate {
    pub fn matches_name(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.name.to_lowercase().contains(&needle)
            || self
                .alternate_names
                .iter()
                .any(|n| n.name.to_lowercase().contains(&needle))
    }
    pub fn matches_path_text(&self, needle: &str) -> bool {
        let needle = needle.to_lowercase();
        self.original_path.to_lowercase().contains(&needle)
            || self
                .alternate_names
                .iter()
                .any(|n| n.original_path.to_lowercase().contains(&needle))
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    /// Absent in legacy sessions. Recomputed from the opened source for new scans.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub source_info: Option<SourceInfo>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid: Option<RaidEvidence>,
    pub schema_version: u32,
    #[serde(default)]
    pub filesystem: FileSystem,
    pub id: String,
    pub source: PathBuf,
    pub fingerprint: Fingerprint,
    pub candidates: Vec<Candidate>,
    pub scanned_records: u64,
    pub total_records: u64,
    pub cancelled: bool,
    pub truncated: bool,
    pub invalid_records: u64,
    pub warnings: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub search: Option<SearchSettings>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub region: Option<SourceRegion>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub volumes: Vec<ScannedVolume>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ScannedVolume {
    pub region: SourceRegion,
    pub fingerprint: Fingerprint,
}
impl Session {
    pub(crate) fn supported_version(&self) -> bool {
        let cp932 = self
            .search
            .as_ref()
            .is_some_and(|s| s.fat_short_name_encoding == FatShortNameEncoding::Cp932);
        (1..=10).contains(&self.schema_version) && (self.schema_version == 10) == cp932
    }

    pub fn candidate_filesystem(&self, candidate: &Candidate) -> FileSystem {
        self.volumes
            .iter()
            .find(|v| candidate.id.starts_with(&format!("p{}:", v.region.index)))
            .and_then(|v| v.region.filesystem)
            .unwrap_or(self.filesystem)
    }
    pub fn save(&self, work_dir: &Path) -> Result<PathBuf> {
        self.guard_output(work_dir)?;
        if serialized_size(self)? + 1 > MAX_SESSION_BYTES {
            return Err(Error::Limit("serialized session exceeds 128 MiB".into()));
        }
        fs::create_dir_all(work_dir)?;
        let dir = Dir::open_ambient_dir(work_dir, ambient_authority())?;
        let name = format!("{}.rescue-session.json", self.id);
        let mut file = dir.open_with(&name, OpenOptions::new().write(true).create_new(true))?;
        serde_json::to_writer(&mut file, self)?;
        file.write_all(b"\n")?;
        file.sync_all()?;
        Ok(work_dir.canonicalize()?.join(name))
    }
    pub fn load(path: &Path) -> Result<Self> {
        let file = fs::File::open(path)?;
        if file.metadata()?.len() > MAX_SESSION_BYTES {
            return Err(Error::Request("session is too large".into()));
        }
        let mut data = Vec::new();
        file.take(MAX_SESSION_BYTES + 1).read_to_end(&mut data)?;
        if data.len() as u64 > MAX_SESSION_BYTES {
            return Err(Error::Request("session is too large".into()));
        }
        let session: Self = serde_json::from_slice(&data)?;
        if !session.supported_version() {
            return Err(Error::Unsupported("session schema".into()));
        }
        Uuid::parse_str(&session.id).map_err(|_| Error::Request("invalid session ID".into()))?;
        Ok(session)
    }
    pub fn guard_output(&self, path: &Path) -> Result<()> {
        if self.fingerprint.modified_ns.starts_with("device:") && !device::is_device(&self.source) {
            return Err(Error::SourceChanged);
        }
        guard_output_path(&self.source, path)
    }
}

#[derive(Default)]
struct ByteCounter(u64);
impl Write for ByteCounter {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 = self
            .0
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| std::io::Error::other("serialized size overflow"))?;
        Ok(bytes.len())
    }
    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}
fn serialized_size(value: &impl Serialize) -> Result<u64> {
    let mut count = ByteCounter::default();
    serde_json::to_writer(&mut count, value)?;
    Ok(count.0)
}
impl Session {
    fn bound_candidates(&mut self) -> Result<()> {
        let mut bytes = 0;
        let mut keep = 0;
        for candidate in &self.candidates {
            bytes += serialized_size(candidate)? + 1;
            if bytes > CANDIDATE_BYTES {
                break;
            }
            keep += 1;
        }
        if keep < self.candidates.len() {
            let omitted = self.candidates.len() - keep;
            self.candidates.truncate(keep);
            self.truncated = true;
            self.warnings.push(format!("RESOURCE_LIMIT: {omitted} candidates omitted to keep the saved session within 128 MiB"));
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub scanned_records: u64,
    pub total_records: u64,
    pub candidates: usize,
    #[serde(default)]
    pub unit: ProgressUnit,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProgressUnit {
    #[default]
    Records,
    Bytes,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FatShortNameEncoding {
    #[default]
    Ascii,
    Cp932,
}

#[derive(Clone, Copy, Debug, Serialize, Deserialize)]
pub struct ScanOptions {
    pub max_records: u64,
    pub max_candidates: usize,
    pub fragments: bool,
    #[serde(default)]
    pub estimate_fat_contiguous: bool,
    #[serde(default)]
    pub fat_short_name_encoding: FatShortNameEncoding,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_records: 2_000_000,
            max_candidates: 100_000,
            fragments: false,
            estimate_fat_contiguous: false,
            fat_short_name_encoding: FatShortNameEncoding::Ascii,
        }
    }
}

pub fn inspect_image(path: &Path) -> Result<Fingerprint> {
    Image::open(path)?.fingerprint()
}
/// Validate a read-only source handle and geometry without probing potentially damaged sectors.
pub fn validate_source(path: &Path) -> Result<()> {
    Image::open(path).map(|_| ())
}

pub fn scan_image(
    path: &Path,
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<Session> {
    scan_volume(path, None, options, cancel, progress)
}

fn scan_volume(
    path: &Path,
    region: Option<&SourceRegion>,
    options: ScanOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Session> {
    let mut image = Image::open_region(path, region)?;
    let fingerprint = image.fingerprint()?;
    let schema_version = if options.fat_short_name_encoding == FatShortNameEncoding::Cp932 {
        10
    } else if options.fragments || region.is_some_and(|r| r.recovery_boot.is_some()) {
        8
    } else if region.is_some() {
        5
    } else {
        3
    };
    let search = (options.fragments
        || options.estimate_fat_contiguous
        || options.fat_short_name_encoding != FatShortNameEncoding::Ascii)
        .then(|| SearchSettings {
            fragments: options.fragments,
            estimate_fat_contiguous: options.estimate_fat_contiguous,
            fat_short_name_encoding: options.fat_short_name_encoding,
            ..Default::default()
        });
    if &image.boot_read(512)?[3..11] != b"NTFS    " {
        let result = fat::scan(&mut image, options, cancel, &mut progress)?;
        if image.fingerprint()? != fingerprint {
            return Err(Error::SourceChanged);
        }
        let mut session = Session {
            source_info: None,
            raid: None,
            schema_version,
            filesystem: result.filesystem,
            id: Uuid::new_v4().to_string(),
            source: image.path.clone(),
            fingerprint,
            candidates: result.entries.into_iter().map(|e| e.candidate).collect(),
            scanned_records: result.scanned_records,
            total_records: result.total_records,
            cancelled: result.cancelled,
            truncated: result.truncated,
            invalid_records: result.invalid_records,
            warnings: result.warnings,
            search,
            region: region.cloned(),
            volumes: Vec::new(),
        };
        session.bound_candidates()?;
        image.annotate_coverage(&mut session)?;
        return Ok(session);
    }
    let opened = Ntfs::open(&mut image);
    let mut session = Session {
        source_info: None,
        raid: None,
        schema_version,
        filesystem: FileSystem::Ntfs,
        id: Uuid::new_v4().to_string(),
        source: image.path.clone(),
        fingerprint,
        candidates: Vec::new(),
        scanned_records: 0,
        total_records: opened.as_ref().map_or(0, |n| n.records),
        cancelled: false,
        truncated: false,
        invalid_records: 0,
        warnings: opened.as_ref().map_or_else(
            |e| vec![format!("Current MFT is unavailable: {e}")],
            |n| n.warnings.clone(),
        ),
        search,
        region: region.cloned(),
        volumes: Vec::new(),
    };
    let ntfs = match opened {
        Ok(ntfs) => ntfs,
        Err(_) if options.fragments => {
            ntfs_remnants::scan(&mut image, &mut session, options, cancel, &mut progress)?;
            if image.fingerprint()? != session.fingerprint {
                return Err(Error::SourceChanged);
            }
            session.bound_candidates()?;
            image.annotate_coverage(&mut session)?;
            return Ok(session);
        }
        Err(error) => return Err(error),
    };
    let mut directories = BTreeMap::new();
    let mut parents = Vec::new();
    let mut alias_parents = Vec::new();
    for index in 0..ntfs.records.min(options.max_records) {
        if cancel.load(Ordering::Relaxed) {
            session.cancelled = true;
            break;
        }
        if session.candidates.len() >= options.max_candidates {
            session.truncated = true;
            break;
        }
        session.scanned_records = index + 1;
        match ntfs.record(&mut image, index) {
            Ok(Some(mut record)) if record.base == 0 => {
                if record.directory {
                    if let Some(name) = record.name {
                        directories.insert(
                            index,
                            (record.sequence, record.parent, record.parent_sequence, name),
                        );
                    }
                } else if !record.in_use
                    && let Some(name) = record.name
                {
                    if record.streams.is_empty() {
                        record.streams.push(ntfs::Stream {
                            name: None,
                            data: None,
                            unsupported: Vec::new(),
                        });
                    }
                    for stream in record.streams {
                        if session.candidates.len() >= options.max_candidates {
                            session.truncated = true;
                            break;
                        }
                        let mut warnings = record.unsupported.clone();
                        warnings.extend(stream.unsupported);
                        if stream.data.is_none() {
                            warnings.push("DATA is unavailable".into());
                        }
                        let supported = warnings.is_empty() && stream.data.is_some();
                        warnings.extend(record.provenance.clone());
                        let (allocation_status, allocation_warnings) = stream
                            .data
                            .as_ref()
                            .map_or((AllocationStatus::Unknown, Vec::new()), |data| {
                                ntfs.allocation(&mut image, data)
                            });
                        warnings.extend(allocation_warnings);
                        let display = stream
                            .name
                            .as_ref()
                            .map_or_else(|| name.clone(), |s| format!("{name}:{s}"));
                        parents.push((record.parent, record.parent_sequence));
                        let aliases = record
                            .names
                            .iter()
                            .filter(|n| {
                                n.name != name
                                    || n.parent != record.parent
                                    || n.sequence != record.parent_sequence
                            })
                            .collect::<Vec<_>>();
                        alias_parents.push(
                            aliases
                                .iter()
                                .map(|n| (n.parent, n.sequence))
                                .collect::<Vec<_>>(),
                        );
                        let alternate_names = aliases
                            .iter()
                            .map(|n| {
                                let display = stream
                                    .name
                                    .as_ref()
                                    .map_or_else(|| n.name.clone(), |s| format!("{}:{s}", n.name));
                                CandidateName {
                                    name: display.clone(),
                                    original_path: display,
                                    path_verified: false,
                                    namespace: n.namespace,
                                }
                            })
                            .collect();
                        session.candidates.push(Candidate {
                            fragment_offset: None,
                            id: ntfs::candidate_id(index, record.sequence, stream.name.as_deref()),
                            record: index,
                            sequence: record.sequence,
                            stream_name: stream.name,
                            original_path: display.clone(),
                            name: display,
                            path_verified: false,
                            alternate_names,
                            size: stream.data.as_ref().map(|d| d.size()),
                            supported,
                            warnings,
                            record_sha256: record.hash.clone(),
                            extent_evidence: ExtentEvidence::Metadata,
                            allocation_status,
                            created: record.created.clone(),
                            modified: record.modified.clone(),
                        });
                    }
                }
            }
            Err(error) => {
                session.invalid_records += 1;
                if session.warnings.len() < 100 {
                    session.warnings.push(format!("record {index}: {error}"));
                }
            }
            _ => {}
        }
        if index % 256 == 0 {
            progress(Progress {
                scanned_records: session.scanned_records,
                total_records: ntfs.records,
                candidates: session.candidates.len(),
                unit: crate::ProgressUnit::Records,
            });
        }
    }
    if session.scanned_records < ntfs.records && !session.cancelled {
        session.truncated = true;
    }
    let candidate_count = session.candidates.len();
    let mut path_bytes = 0;
    let mut retained = 0;
    for ((candidate, (mut parent, mut sequence)), aliases) in session
        .candidates
        .iter_mut()
        .zip(parents)
        .zip(alias_parents)
    {
        let mut parts = vec![candidate.name.clone()];
        let mut visited = BTreeSet::new();
        for _ in 0..256 {
            let Some((found_sequence, next, next_sequence, name)) = directories.get(&parent) else {
                break;
            };
            if *found_sequence != sequence || !visited.insert(parent) {
                break;
            }
            if parent == 5 {
                candidate.path_verified = true;
                break;
            }
            parts.push(name.clone());
            parent = *next;
            sequence = *next_sequence;
        }
        parts.reverse();
        candidate.original_path = parts.join("/");
        for (alias, (mut parent, mut sequence)) in candidate.alternate_names.iter_mut().zip(aliases)
        {
            let mut parts = vec![alias.name.clone()];
            let mut visited = BTreeSet::new();
            for _ in 0..256 {
                let Some((found, next, next_sequence, name)) = directories.get(&parent) else {
                    break;
                };
                if *found != sequence || !visited.insert(parent) {
                    break;
                }
                if parent == 5 {
                    alias.path_verified = true;
                    break;
                }
                parts.push(name.clone());
                parent = *next;
                sequence = *next_sequence;
            }
            parts.reverse();
            alias.original_path = parts.join("/");
        }
        path_bytes += serialized_size(candidate)? + 1;
        if path_bytes > CANDIDATE_BYTES {
            break;
        }
        retained += 1;
    }
    if retained < candidate_count {
        session.candidates.truncate(retained);
        session.truncated = true;
        session.warnings.push(format!("RESOURCE_LIMIT: {} candidates omitted to keep paths and the saved session within 128 MiB", candidate_count - retained));
    }
    if options.fragments && !session.cancelled {
        ntfs_remnants::scan(&mut image, &mut session, options, cancel, &mut progress)?;
        session.bound_candidates()?;
    }
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    progress(Progress {
        scanned_records: session.scanned_records,
        total_records: session.total_records,
        candidates: session.candidates.len(),
        unit: crate::ProgressUnit::Records,
    });
    image.annotate_coverage(&mut session)?;
    Ok(session)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryResult {
    /// Absent in legacy reports and when metadata revalidation failed.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub naming: Option<RecoveryNaming>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub raid_reads: Option<RaidReadTrace>,
    pub candidate_id: String,
    pub status: String,
    pub path: Option<PathBuf>,
    pub bytes_written: u64,
    pub sha256: Option<String>,
    pub error: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error_details: Option<OperationError>,
    pub warnings: Vec<String>,
    pub extent_evidence: ExtentEvidence,
    pub allocation_status: AllocationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_entry: Option<String>,
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub missing_ranges: Vec<MissingRange>,
}

struct RecoveryData {
    data: ntfs::Data,
    name: String,
    warnings: Vec<String>,
    extent_evidence: ExtentEvidence,
    allocation_status: AllocationStatus,
}

fn ntfs_recovery_data(
    ntfs: &Ntfs,
    image: &mut Image,
    candidate: &Candidate,
) -> Result<RecoveryData> {
    if candidate.fragment_offset.is_some() {
        return ntfs_remnants::recovery_data(image, Some(ntfs), candidate);
    }
    let record = ntfs
        .record(image, candidate.record)?
        .ok_or(Error::SourceChanged)?;
    if record.hash != candidate.record_sha256
        || record.in_use
        || record.directory
        || record.base != 0
        || record.sequence != candidate.sequence
        || candidate.id
            != ntfs::candidate_id(
                candidate.record,
                record.sequence,
                candidate.stream_name.as_deref(),
            )
    {
        return Err(Error::SourceChanged);
    }
    if !record.unsupported.is_empty() {
        return Err(Error::Unsupported(record.unsupported.join("; ")));
    }
    let stream = record
        .streams
        .into_iter()
        .find(|s| s.name == candidate.stream_name)
        .ok_or(Error::SourceChanged)?;
    if !stream.unsupported.is_empty() {
        return Err(Error::Unsupported(stream.unsupported.join("; ")));
    }
    let data = stream
        .data
        .ok_or_else(|| Error::Unsupported("no DATA".into()))?;
    let name = record
        .name
        .ok_or_else(|| Error::Invalid("no filename".into()))?;
    let name = stream.name.map_or_else(
        || name.clone(),
        |s| format!("{}__ADS__{}", safe_name(&name), safe_name(&s)),
    );
    let (allocation_status, mut warnings) = ntfs.allocation(image, &data);
    warnings.extend(record.provenance);
    warnings.extend(ntfs.warnings.clone());
    Ok(RecoveryData {
        data,
        name,
        warnings,
        extent_evidence: ExtentEvidence::Metadata,
        allocation_status,
    })
}

enum RecoverySource {
    Ntfs(Box<Ntfs>),
    NtfsFragments,
    Fat(BTreeMap<String, fat::RecoveredEntry>),
    Carved,
}

fn candidate_data(
    source: &RecoverySource,
    image: &mut Image,
    candidate: &Candidate,
    cancel: &AtomicBool,
) -> Result<RecoveryData> {
    match source {
        RecoverySource::Ntfs(ntfs) => ntfs_recovery_data(ntfs, image, candidate),
        RecoverySource::NtfsFragments => ntfs_remnants::recovery_data(image, None, candidate),
        RecoverySource::Carved => carve::recovery_data(image, candidate, cancel),
        RecoverySource::Fat(entries) => {
            let entry = entries.get(&candidate.id).ok_or(Error::SourceChanged)?;
            if candidate.record != entry.candidate.record
                || candidate.record_sha256 != entry.candidate.record_sha256
                || candidate.sequence != 0
                || candidate.stream_name.is_some()
            {
                return Err(Error::SourceChanged);
            }
            Ok(RecoveryData {
                data: entry
                    .data
                    .clone()
                    .ok_or_else(|| Error::Unsupported(entry.candidate.warnings.join("; ")))?,
                name: entry.candidate.name.clone(),
                warnings: entry.candidate.warnings.clone(),
                extent_evidence: entry.candidate.extent_evidence,
                allocation_status: entry.candidate.allocation_status,
            })
        }
    }
}

fn safe_name(name: &str) -> String {
    let mut value = name
        .chars()
        .take(80)
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>();
    // Leave room for UUID and .partial on filesystems whose component limit is bytes.
    while value.len() > 180 {
        value.pop();
    }
    let value = value.trim_end_matches([' ', '.']);
    if value.is_empty() {
        "unnamed".into()
    } else {
        value.into()
    }
}

/// Re-reads source metadata. FAT entries are rediscovered by traversing the volume directories.
/// Serialized candidates never supply trusted byte offsets or payloads.
pub fn recover(
    session: &Session,
    ids: &[String],
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<RecoveryResult>> {
    session.guard_output(destination)?;
    let destination = RecoveryDestination::open(destination)?;
    recover_to(session, ids, &destination, cancel)
}

pub fn recover_to(
    session: &Session,
    ids: &[String],
    destination: &RecoveryDestination,
    cancel: &AtomicBool,
) -> Result<Vec<RecoveryResult>> {
    session.guard_output(&destination.path)?;
    if !session.supported_version() {
        return Err(Error::Unsupported("session schema".into()));
    }
    if session.filesystem == FileSystem::Disk {
        return disk_scan::recover_disk(session, ids, destination, cancel);
    }
    disk_scan::validate_region(session)?;
    if ids.is_empty() {
        return Err(Error::Request("select at least one candidate".into()));
    }
    let mut selected = Vec::new();
    let mut unique = BTreeSet::new();
    for id in ids {
        if !unique.insert(id) {
            continue;
        }
        let candidate = session
            .candidates
            .iter()
            .find(|c| c.id == *id)
            .ok_or_else(|| Error::Request(format!("unknown candidate: {id}")))?;
        selected.push(candidate);
    }
    let mut image = Image::open_region(&session.source, session.region.as_ref())?;
    image.validate_raid(session)?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    let source = if session.filesystem == FileSystem::Ntfs {
        match Ntfs::open(&mut image) {
            Ok(ntfs) => RecoverySource::Ntfs(Box::new(ntfs)),
            Err(_) if selected.iter().all(|c| c.fragment_offset.is_some()) => {
                RecoverySource::NtfsFragments
            }
            Err(error) => return Err(error),
        }
    } else if session.filesystem == FileSystem::Raw {
        RecoverySource::Carved
    } else {
        let wanted = selected.iter().map(|c| c.id.clone()).collect();
        let fresh = restore::validation::fat_selection(
            &mut image,
            &wanted,
            session
                .search
                .as_ref()
                .is_some_and(|s| s.estimate_fat_contiguous),
            session
                .search
                .as_ref()
                .map_or(crate::FatShortNameEncoding::Ascii, |s| {
                    s.fat_short_name_encoding
                }),
            cancel,
        )?;
        if fresh.cancelled {
            return Err(Error::Cancelled);
        }
        if fresh.filesystem != session.filesystem {
            return Err(Error::SourceChanged);
        }
        RecoverySource::Fat(
            fresh
                .entries
                .into_iter()
                .map(|entry| (entry.candidate.id.clone(), entry))
                .collect(),
        )
    };
    destination.verify()?;
    let dir = &destination.dir;
    let output_path = &destination.path;
    let mut results = Vec::new();
    let mut parents = indexed_scan::Parents::default();
    for candidate in selected {
        if cancel.load(Ordering::Relaxed) {
            results.push(RecoveryResult {
                naming: None,
                raid_reads: None,
                candidate_id: candidate.id.clone(),
                status: "cancelled".into(),
                path: None,
                bytes_written: 0,
                sha256: None,
                error: Some("CANCELLED: not started".into()),
                error_details: Some(Error::Cancelled.report(format!("candidate:{}", candidate.id))),
                warnings: Vec::new(),
                extent_evidence: ExtentEvidence::Unknown,
                allocation_status: AllocationStatus::Unknown,
                archive_entry: None,
                missing_ranges: Vec::new(),
            });
            continue;
        }
        let mut result = RecoveryResult {
            naming: None,
            raid_reads: None,
            candidate_id: candidate.id.clone(),
            status: "failed".into(),
            path: None,
            bytes_written: 0,
            sha256: None,
            error: None,
            error_details: None,
            warnings: Vec::new(),
            extent_evidence: ExtentEvidence::Unknown,
            allocation_status: AllocationStatus::Unknown,
            archive_entry: None,
            missing_ranges: Vec::new(),
        };
        image.take_raid_trace();
        let attempt = (|| -> Result<()> {
            // Media were checked before opening this pinned destination. Recheck
            // its file/volume identity for every file without rediscovering topology.
            destination.verify()?;
            let candidate = restore::validation::candidate(
                &source,
                &mut image,
                candidate,
                &mut parents,
                session,
                cancel,
            )?;
            let mut naming = RecoveryNaming::from_candidate(&candidate, session.filesystem);
            result.naming = Some(naming.clone());
            let fresh = match &source {
                RecoverySource::Ntfs(ntfs) => ntfs_recovery_data(ntfs, &mut image, &candidate)?,
                RecoverySource::NtfsFragments => {
                    ntfs_remnants::recovery_data(&mut image, None, &candidate)?
                }
                RecoverySource::Carved => carve::recovery_data(&mut image, &candidate, cancel)?,
                RecoverySource::Fat(entries) => {
                    let entry = entries.get(&candidate.id).ok_or(Error::SourceChanged)?;
                    if candidate.record != entry.candidate.record
                        || candidate.record_sha256 != entry.candidate.record_sha256
                        || candidate.sequence != 0
                        || candidate.stream_name.is_some()
                    {
                        return Err(Error::SourceChanged);
                    }
                    result.warnings = entry.candidate.warnings.clone();
                    result.extent_evidence = entry.candidate.extent_evidence;
                    result.allocation_status = entry.candidate.allocation_status;
                    let data = entry
                        .data
                        .clone()
                        .ok_or_else(|| Error::Unsupported(entry.candidate.warnings.join("; ")))?;
                    RecoveryData {
                        data,
                        name: entry.candidate.name.clone(),
                        warnings: entry.candidate.warnings.clone(),
                        extent_evidence: entry.candidate.extent_evidence,
                        allocation_status: entry.candidate.allocation_status,
                    }
                }
            };
            result.warnings = fresh.warnings;
            result.extent_evidence = fresh.extent_evidence;
            result.allocation_status = fresh.allocation_status;
            let data = fresh.data;
            let name = fresh.name;
            naming.changed(
                NameChangeReason::StreamingFlatOutput,
                candidate.original_path.clone(),
                candidate.name.clone(),
            );
            naming.changed(
                NameChangeReason::AlternateStream,
                candidate.name,
                name.clone(),
            );
            let portable = safe_name(&name);
            naming.changed(NameChangeReason::PortableName, name, portable.clone());
            let filename = format!("{}_{}", Uuid::new_v4(), portable);
            naming.changed(
                NameChangeReason::UniqueOutputName,
                portable,
                filename.clone(),
            );
            result.naming = Some(naming);
            let partial = format!("{filename}.partial");
            let mut output =
                dir.open_with(&partial, OpenOptions::new().write(true).create_new(true))?;
            result.path = Some(output_path.join(&partial));
            let mut hash = Sha256::new();
            let mut offset = 0;
            while offset < data.size() {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let count = (data.size() - offset).min(256 * 1024) as usize;
                let buffer = data.read(&mut image, offset, count)?;
                output.write_all(&buffer)?;
                hash.update(&buffer);
                offset += count as u64;
                result.bytes_written = offset;
            }
            result.raid_reads = image.take_raid_trace();
            output.sync_all()?;
            drop(output);
            if image.fingerprint()? != session.fingerprint {
                return Err(Error::SourceChanged);
            }
            destination.verify()?;
            // Atomic no-clobber publication. NTFS and ordinary Linux filesystems support this.
            // Filesystems without hard links fail safely, retaining a named .partial file.
            dir.hard_link(&partial, dir, &filename)?;
            result.path = Some(output_path.join(&filename));
            dir.remove_file(&partial)?;
            result.sha256 = Some(format!("{:x}", hash.finalize()));
            result.status = "completed".into();
            Ok(())
        })();
        if result.raid_reads.is_none() {
            result.raid_reads = image.take_raid_trace();
        }
        if let Err(error) = attempt {
            if let Some(path) = &result.path
                && path
                    .extension()
                    .is_some_and(|extension| extension == "partial")
                && let Some(naming) = &mut result.naming
                && let Some(leaf) = path.file_name().and_then(|name| name.to_str())
            {
                naming.changed(
                    NameChangeReason::PartialOutput,
                    leaf.trim_end_matches(".partial").into(),
                    leaf.into(),
                );
            }
            if let Error::MissingRanges(ranges) = &error {
                result.missing_ranges = ranges.clone();
            }
            result.status = if matches!(error, Error::Cancelled) {
                "cancelled"
            } else if result.path.is_some() || matches!(error, Error::MissingRanges(_)) {
                "incomplete"
            } else {
                "failed"
            }
            .into();
            result.error = Some(format!("{}: {error}", error.code()));
            result.error_details = Some(error.report(format!("candidate:{}", result.candidate_id)));
        }
        results.push(result);
    }
    Ok(results)
}

pub fn save_report(
    destination: &Path,
    session: &Session,
    results: &[RecoveryResult],
) -> Result<PathBuf> {
    session.guard_output(destination)?;
    let destination = RecoveryDestination::open(destination)?;
    save_report_to(&destination, session, results)
}

pub fn save_report_to(
    destination: &RecoveryDestination,
    session: &Session,
    results: &[RecoveryResult],
) -> Result<PathBuf> {
    destination.verify()?;
    session.guard_output(&destination.path)?;
    write_report(destination, session, results)
}
/// Write a report after buffered restoration, including on the source volume.
pub fn save_restore_report(
    destination: &Path,
    session: &Session,
    results: &[RecoveryResult],
) -> Result<PathBuf> {
    write_report(&RecoveryDestination::open(destination)?, session, results)
}
pub fn save_restore_report_with_roots(
    destination: &Path,
    session: &Session,
    results: &[RecoveryResult],
    roots: &[OutputRoot],
) -> Result<PathBuf> {
    write_report(
        &RecoveryDestination::within_create(destination, roots)?,
        session,
        results,
    )
}
fn write_report(
    destination: &RecoveryDestination,
    session: &Session,
    results: &[RecoveryResult],
) -> Result<PathBuf> {
    destination.verify()?;
    let dir = &destination.dir;
    let name = format!("rescue-report-{}.json", Uuid::new_v4());
    let mut file = dir.open_with(&name, OpenOptions::new().write(true).create_new(true))?;
    serde_json::to_writer_pretty(
        &mut file,
        &serde_json::json!({
            "schema_version":1, "session_id":session.id, "source":session.source,
            "source_info":session.source_info,
            "filesystem":session.filesystem,
            "source_fingerprint":session.fingerprint, "raid":session.raid, "results":results,
            "integrity_note":"Output hashes do not prove equality with the deleted originals."
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(destination.path.join(name))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn source_rejects_non_disk_types_directories_and_out_of_range_reads() {
        let dir = tempfile::tempdir().unwrap();
        assert!(Image::open(dir.path()).is_err());
        let path = dir.path().join("image");
        fs::write(&path, [0u8; 512]).unwrap();
        let mut image = Image::open(&path).unwrap();
        assert!(image.read(u64::MAX, 1).is_err());
        assert!(image.read(511, 2).is_err());
        assert_eq!(image.read(511, 1).unwrap(), vec![0]);
    }
    #[test]
    fn unsafe_names_are_flattened() {
        for name in ["../../outside", r"C:\outside:stream", "x\0y", "name. "] {
            let sanitized = safe_name(name);
            assert!(!sanitized.contains(['/', '\\', ':', '\0']));
            assert!(!sanitized.ends_with(['.', ' ']));
        }
        assert_eq!(safe_name(""), "unnamed");
        assert_eq!(safe_name("日本語.txt"), "日本語.txt");
    }
    #[test]
    fn invalid_session_does_not_get_accepted() {
        let file = tempfile::NamedTempFile::new().unwrap();
        fs::write(file.path(), b"{").unwrap();
        assert!(Session::load(file.path()).is_err());
    }
}
