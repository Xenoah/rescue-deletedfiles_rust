mod lznt1;
mod ntfs;
mod source;

use cap_std::{
    ambient_authority,
    fs::{Dir, OpenOptions},
};
use ntfs::Ntfs;
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
use uuid::Uuid;

pub const VERSION: &str = env!("CARGO_PKG_VERSION");
pub const MAX_SESSION_BYTES: u64 = 128 * 1024 * 1024;
pub type Result<T> = std::result::Result<T, Error>;

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
    #[error("source identity or metadata changed; rescan the image")]
    SourceChanged,
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
            Self::SourceChanged => "SOURCE_CHANGED",
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
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct Session {
    pub schema_version: u32,
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
}
impl Session {
    pub fn save(&self, work_dir: &Path) -> Result<PathBuf> {
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
        if ![1, 2].contains(&session.schema_version) {
            return Err(Error::Unsupported("session schema".into()));
        }
        Uuid::parse_str(&session.id).map_err(|_| Error::Request("invalid session ID".into()))?;
        Ok(session)
    }
}

#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct Progress {
    pub scanned_records: u64,
    pub total_records: u64,
    pub candidates: usize,
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
    let ntfs = Ntfs::open(&mut image)?;
    let mut session = Session {
        schema_version: 2,
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
            });
        }
    }
    if session.scanned_records < ntfs.records && !session.cancelled {
        session.truncated = true;
    }
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
    }
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    progress(Progress {
        scanned_records: session.scanned_records,
        total_records: ntfs.records,
        candidates: session.candidates.len(),
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
}

fn safe_name(name: &str) -> String {
    let value = name
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
    let value = value.trim_end_matches([' ', '.']);
    if value.is_empty() {
        "unnamed".into()
    } else {
        value.into()
    }
}

/// Re-reads MFT records; serialized candidates never supply trusted byte offsets or payloads.
pub fn recover(
    session: &Session,
    ids: &[String],
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<Vec<RecoveryResult>> {
    if ![1, 2].contains(&session.schema_version) {
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
    let ntfs = Ntfs::open(&mut image)?;
    fs::create_dir_all(destination)?;
    let destination = destination.canonicalize()?;
    let dir = Dir::open_ambient_dir(&destination, ambient_authority())?;
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
        };
        let attempt = (|| -> Result<()> {
            let record = ntfs
                .record(&mut image, candidate.record)?
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
            let filename = format!("{}_{}", Uuid::new_v4(), safe_name(&name));
            let partial = format!("{filename}.partial");
            let mut output =
                dir.open_with(&partial, OpenOptions::new().write(true).create_new(true))?;
            result.path = Some(destination.join(&partial));
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
            // Atomic no-clobber publication. NTFS and ordinary Linux filesystems support this.
            // Filesystems without hard links fail safely, retaining a named .partial file.
            dir.hard_link(&partial, &dir, &filename)?;
            result.path = Some(destination.join(&filename));
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
    let dir = Dir::open_ambient_dir(destination, ambient_authority())?;
    let name = format!("rescue-report-{}.json", Uuid::new_v4());
    let mut file = dir.open_with(&name, OpenOptions::new().write(true).create_new(true))?;
    serde_json::to_writer_pretty(
        &mut file,
        &serde_json::json!({
            "schema_version":1, "session_id":session.id, "source":session.source,
            "source_fingerprint":session.fingerprint, "results":results,
            "integrity_note":"Output hashes do not prove equality with the deleted originals."
        }),
    )?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    Ok(destination.canonicalize()?.join(name))
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
