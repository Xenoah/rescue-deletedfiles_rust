mod archive;
mod carve;
mod destination;
mod fat;
mod lznt1;
mod ntfs;
mod search;
mod source;
mod timestamps;

pub use archive::{ArchiveRecovery, recover_zip, recover_zip_to};
use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
pub use destination::{OutputRoot, RecoveryDestination};
use ntfs::Ntfs;
pub use search::{ScanMethod, SearchSettings, scan_with_settings};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
pub use source::Fingerprint;
use source::Image;
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
    Fat32,
    Exfat,
    /// Signature scan of a regular image; no filesystem interpretation.
    Raw,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ExtentEvidence {
    #[default]
    Unknown,
    Metadata,
    ResidualFatChain,
    SingleCluster,
    NoFatChain,
    PngValidated,
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
    #[error("operation cancelled")]
    Cancelled,
    #[error("invalid request: {0}")]
    Request(String),
}
impl Error {
    pub fn code(&self) -> &'static str {
        match self {
            Self::Io(_) => "IO_ERROR",
            Self::Json(_) => "INVALID_JSON",
            Self::Invalid(_) => "INVALID_IMAGE",
            Self::Unsupported(_) => "UNSUPPORTED",
            Self::Limit(_) => "RESOURCE_LIMIT",
            Self::SourceChanged => "SOURCE_CHANGED",
            Self::DestinationChanged => "DESTINATION_CHANGED",
            Self::Cancelled => "CANCELLED",
            Self::Request(_) => "INVALID_REQUEST",
        }
    }
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Candidate {
    pub id: String,
    pub record: u64,
    pub sequence: u16,
    #[serde(default)]
    pub stream_name: Option<String>,
    pub name: String,
    pub original_path: String,
    pub path_verified: bool,
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

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
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
}
impl Session {
    pub fn save(&self, work_dir: &Path) -> Result<PathBuf> {
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
        if ![1, 2, 3, 4].contains(&session.schema_version) {
            return Err(Error::Unsupported("session schema".into()));
        }
        Uuid::parse_str(&session.id).map_err(|_| Error::Request("invalid session ID".into()))?;
        Ok(session)
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

#[derive(Clone, Copy)]
pub struct ScanOptions {
    pub max_records: u64,
    pub max_candidates: usize,
}
impl Default for ScanOptions {
    fn default() -> Self {
        Self {
            max_records: 2_000_000,
            max_candidates: 100_000,
        }
    }
}

pub fn inspect_image(path: &Path) -> Result<Fingerprint> {
    Image::open(path)?.fingerprint()
}

pub fn scan_image(
    path: &Path,
    options: ScanOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Session> {
    let mut image = Image::open(path)?;
    let fingerprint = image.fingerprint()?;
    if image.read(3, 8)? != b"NTFS    " {
        let result = fat::scan(&mut image, options, cancel, &mut progress)?;
        if image.fingerprint()? != fingerprint {
            return Err(Error::SourceChanged);
        }
        let mut session = Session {
            schema_version: 3,
            filesystem: result.filesystem,
            id: Uuid::new_v4().to_string(),
            source: image.path,
            fingerprint,
            candidates: result.entries.into_iter().map(|e| e.candidate).collect(),
            scanned_records: result.scanned_records,
            total_records: result.total_records,
            cancelled: result.cancelled,
            truncated: result.truncated,
            invalid_records: result.invalid_records,
            warnings: result.warnings,
            search: None,
        };
        session.bound_candidates()?;
        return Ok(session);
    }
    let ntfs = Ntfs::open(&mut image)?;
    let mut session = Session {
        schema_version: 3,
        filesystem: FileSystem::Ntfs,
        id: Uuid::new_v4().to_string(),
        source: image.path.clone(),
        fingerprint,
        candidates: Vec::new(),
        scanned_records: 0,
        total_records: ntfs.records,
        cancelled: false,
        truncated: false,
        invalid_records: 0,
        warnings: Vec::new(),
        search: None,
    };
    let mut directories = BTreeMap::new();
    let mut parents = Vec::new();
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
                        let display = stream
                            .name
                            .as_ref()
                            .map_or_else(|| name.clone(), |s| format!("{name}:{s}"));
                        parents.push((record.parent, record.parent_sequence));
                        session.candidates.push(Candidate {
                            id: ntfs::candidate_id(index, record.sequence, stream.name.as_deref()),
                            record: index,
                            sequence: record.sequence,
                            stream_name: stream.name,
                            original_path: display.clone(),
                            name: display,
                            path_verified: false,
                            size: stream.data.as_ref().map(|d| d.size()),
                            supported,
                            warnings,
                            record_sha256: record.hash.clone(),
                            extent_evidence: ExtentEvidence::Metadata,
                            allocation_status: AllocationStatus::Unknown,
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
    for (candidate, (mut parent, mut sequence)) in session.candidates.iter_mut().zip(parents) {
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
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    progress(Progress {
        scanned_records: session.scanned_records,
        total_records: ntfs.records,
        candidates: session.candidates.len(),
        unit: crate::ProgressUnit::Records,
    });
    Ok(session)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RecoveryResult {
    pub candidate_id: String,
    pub status: String,
    pub path: Option<PathBuf>,
    pub bytes_written: u64,
    pub sha256: Option<String>,
    pub error: Option<String>,
    pub warnings: Vec<String>,
    pub extent_evidence: ExtentEvidence,
    pub allocation_status: AllocationStatus,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub archive_entry: Option<String>,
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
    Ok(RecoveryData {
        data,
        name,
        warnings: Vec::new(),
        extent_evidence: ExtentEvidence::Metadata,
        allocation_status: AllocationStatus::Unknown,
    })
}

enum RecoverySource {
    Ntfs(Ntfs),
    Fat(BTreeMap<String, fat::RecoveredEntry>),
    Carved,
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
    let destination = RecoveryDestination::open(destination)?;
    recover_to(session, ids, &destination, cancel)
}

pub fn recover_to(
    session: &Session,
    ids: &[String],
    destination: &RecoveryDestination,
    cancel: &AtomicBool,
) -> Result<Vec<RecoveryResult>> {
    if ![1, 2, 3, 4].contains(&session.schema_version) {
        return Err(Error::Unsupported("session schema".into()));
    }
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
    let mut image = Image::open(&session.source)?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    let source = if session.filesystem == FileSystem::Ntfs {
        RecoverySource::Ntfs(Ntfs::open(&mut image)?)
    } else if session.filesystem == FileSystem::Raw {
        RecoverySource::Carved
    } else {
        let fresh = fat::scan(&mut image, ScanOptions::default(), cancel, |_| {})?;
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
    for candidate in selected {
        if cancel.load(Ordering::Relaxed) {
            results.push(RecoveryResult {
                candidate_id: candidate.id.clone(),
                status: "cancelled".into(),
                path: None,
                bytes_written: 0,
                sha256: None,
                error: Some("CANCELLED: not started".into()),
                warnings: Vec::new(),
                extent_evidence: ExtentEvidence::Unknown,
                allocation_status: AllocationStatus::Unknown,
                archive_entry: None,
            });
            continue;
        }
        let mut result = RecoveryResult {
            candidate_id: candidate.id.clone(),
            status: "failed".into(),
            path: None,
            bytes_written: 0,
            sha256: None,
            error: None,
            warnings: Vec::new(),
            extent_evidence: ExtentEvidence::Unknown,
            allocation_status: AllocationStatus::Unknown,
            archive_entry: None,
        };
        let attempt = (|| -> Result<()> {
            destination.verify()?;
            let fresh = match &source {
                RecoverySource::Ntfs(ntfs) => ntfs_recovery_data(ntfs, &mut image, candidate)?,
                RecoverySource::Carved => carve::recovery_data(&mut image, candidate, cancel)?,
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
            let filename = format!("{}_{}", Uuid::new_v4(), safe_name(&name));
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
        if let Err(error) = attempt {
            result.status = if matches!(error, Error::Cancelled) {
                "cancelled"
            } else if result.path.is_some() {
                "incomplete"
            } else {
                "failed"
            }
            .into();
            result.error = Some(format!("{}: {error}", error.code()));
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
    let destination = RecoveryDestination::open(destination)?;
    save_report_to(&destination, session, results)
}

pub fn save_report_to(
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
            "filesystem":session.filesystem,
            "source_fingerprint":session.fingerprint, "results":results,
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
    fn source_rejects_devices_directories_and_out_of_range_reads() {
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
