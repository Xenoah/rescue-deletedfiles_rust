//! Read the complete selected batch before creating anything on its source disk.
//! The legacy streaming API remains available for large, separate-disk jobs.
use crate::*;
use serde::{Deserialize, Serialize};
use std::io::ErrorKind;

pub const MAX_RESTORE_MEMORY: u64 = 512 * 1024 * 1024;

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CollisionPolicy {
    #[default]
    KeepBoth,
    Skip,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct VolumeRoot {
    /// Partition number for a whole-disk scan; None for a volume or image.
    pub partition: Option<u32>,
    pub path: PathBuf,
}
#[derive(Clone, Debug)]
pub enum RestoreTarget {
    Directory(PathBuf),
    /// Explicit mappings override automatic mounted-volume discovery.
    Original(Vec<VolumeRoot>),
}
#[derive(Clone, Debug)]
pub struct RestoreOptions {
    pub target: RestoreTarget,
    pub collision: CollisionPolicy,
}
struct PreparedFile {
    bytes: Vec<u8>,
    relative: PathBuf,
    partition: Option<u32>,
    region: Option<SourceRegion>,
    original_verified: bool,
    result: RecoveryResult,
}
struct Batch {
    files: Vec<PreparedFile>,
    failures: Vec<RecoveryResult>,
}
fn result_for(id: &str) -> RecoveryResult {
    RecoveryResult {
        candidate_id: id.into(),
        status: "failed".into(),
        path: None,
        bytes_written: 0,
        sha256: None,
        error: None,
        warnings: Vec::new(),
        extent_evidence: ExtentEvidence::Unknown,
        allocation_status: AllocationStatus::Unknown,
        archive_entry: None,
    }
}
fn component(value: &str) -> String {
    let mut value = value
        .chars()
        .map(|c| {
            if c.is_control() || r#"<>:"/\|?*"#.contains(c) {
                '_'
            } else {
                c
            }
        })
        .collect::<String>();
    if value.len() > 255 {
        let extension = Path::new(&value)
            .extension()
            .and_then(|e| e.to_str())
            .filter(|e| e.len() <= 128)
            .map(|e| format!(".{e}"));
        if let Some(extension) = extension {
            let mut stem = value[..value.len() - extension.len()].to_owned();
            while stem.len() + extension.len() > 255 {
                stem.pop();
            }
            value = format!("{stem}{extension}");
        } else {
            while value.len() > 255 {
                value.pop();
            }
        }
    }
    let value = value.trim_end_matches([' ', '.']).to_owned();
    let value = if value.is_empty() {
        "unnamed".into()
    } else {
        value
    };
    let stem = value.split('.').next().unwrap_or("").to_ascii_uppercase();
    let mut value = if matches!(stem.as_str(), "CON" | "PRN" | "AUX" | "NUL")
        || (stem.len() == 4
            && (stem.starts_with("COM") || stem.starts_with("LPT"))
            && stem.as_bytes()[3].is_ascii_digit())
    {
        format!("_{value}")
    } else {
        value
    };
    while value.len() > 255 {
        value.pop();
    }
    value
}
fn relative_path(path: &str, name: &str, verified: bool) -> Result<PathBuf> {
    if !verified {
        return Ok(PathBuf::from(component(name)));
    }
    let path = path.replace('\\', "/");
    let mut relative = PathBuf::new();
    for part in path.trim_start_matches('/').split('/') {
        if part.is_empty() {
            continue;
        }
        if part == "." || part == ".." || part.contains(':') || part.contains('\0') {
            return Err(Error::Request("invalid original path".into()));
        }
        relative.push(component(part));
    }
    if relative.as_os_str().is_empty() {
        relative.push(component(name));
    }
    Ok(relative)
}
fn prepare(session: &Session, ids: &[String], cancel: &AtomicBool) -> Result<Batch> {
    if ids.is_empty() {
        return Err(Error::Request("select at least one candidate".into()));
    }
    if ![1, 2, 3, 4, 5, 6].contains(&session.schema_version) {
        return Err(Error::Unsupported("session schema".into()));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if Image::open_region(&session.source, session.region.as_ref())?.fingerprint()?
        != session.fingerprint
    {
        return Err(Error::SourceChanged);
    }
    // Paths, sizes and extents come from a new source scan, never from JSON.
    let fresh = if let Some(region) = &session.region {
        disk_scan::validate_region(session)?;
        scan_volume(
            &session.source,
            Some(region),
            ScanOptions::default(),
            cancel,
            |_| {},
        )?
    } else {
        let settings = session.search.clone().unwrap_or_else(|| SearchSettings {
            method: if session.filesystem == FileSystem::Raw {
                ScanMethod::Carve
            } else {
                ScanMethod::Quick
            },
            ..Default::default()
        });
        scan_with_settings(
            &session.source,
            ScanOptions::default(),
            &settings,
            cancel,
            |_| {},
        )?
    };
    if fresh.cancelled {
        return Err(Error::Cancelled);
    }
    if fresh.fingerprint != session.fingerprint
        || fresh.filesystem != session.filesystem
        || fresh.region != session.region
        || fresh.volumes.len() != session.volumes.len()
        || fresh
            .volumes
            .iter()
            .zip(&session.volumes)
            .any(|(a, b)| a.region != b.region || a.fingerprint != b.fingerprint)
    {
        return Err(Error::SourceChanged);
    }
    let mut selected = BTreeMap::new();
    let mut batch = Batch {
        files: Vec::new(),
        failures: Vec::new(),
    };
    let mut unique = BTreeSet::new();
    for id in ids {
        if !unique.insert(id) {
            continue;
        }
        let old = session
            .candidates
            .iter()
            .find(|c| c.id == *id)
            .ok_or_else(|| Error::Request(format!("unknown candidate: {id}")))?;
        let Some(candidate) = fresh.candidates.iter().find(|c| c.id == *id) else {
            let mut result = result_for(id);
            result.error = Some("SOURCE_CHANGED: candidate is no longer present".into());
            batch.failures.push(result);
            continue;
        };
        if old.record_sha256 != candidate.record_sha256
            || old.record != candidate.record
            || old.sequence != candidate.sequence
            || old.stream_name != candidate.stream_name
        {
            let mut result = result_for(id);
            result.error = Some("SOURCE_CHANGED: candidate metadata changed".into());
            batch.failures.push(result);
            continue;
        }
        selected.insert(id.clone(), candidate.clone());
    }
    let mut used = 0u64;
    if fresh.filesystem == FileSystem::Disk {
        for volume in &fresh.volumes {
            let prefix = format!("p{}:", volume.region.index);
            let candidates: Vec<_> = selected
                .values()
                .filter(|c| c.id.starts_with(&prefix))
                .cloned()
                .collect();
            if candidates.is_empty() {
                continue;
            }
            let mut child = fresh.clone();
            child.filesystem = volume.region.filesystem.ok_or(Error::SourceChanged)?;
            child.region = Some(volume.region.clone());
            child.fingerprint = volume.fingerprint.clone();
            read_volume(
                &child,
                candidates,
                &prefix,
                Some(volume.region.index),
                cancel,
                &mut used,
                &mut batch,
            )?;
        }
        if Image::open(&session.source)?.fingerprint()? != session.fingerprint {
            return Err(Error::SourceChanged);
        }
    } else {
        read_volume(
            &fresh,
            selected.into_values().collect(),
            "",
            fresh.region.as_ref().map(|r| r.index),
            cancel,
            &mut used,
            &mut batch,
        )?;
    }
    // No source handle remains open when publication starts. Own filesystem
    // writes can now change source fingerprints without invalidating this batch.
    Ok(batch)
}
fn read_volume(
    session: &Session,
    candidates: Vec<Candidate>,
    prefix: &str,
    partition: Option<u32>,
    cancel: &AtomicBool,
    used: &mut u64,
    batch: &mut Batch,
) -> Result<()> {
    let mut image = Image::open_region(&session.source, session.region.as_ref())?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    let source = match session.filesystem {
        FileSystem::Ntfs => RecoverySource::Ntfs(Ntfs::open(&mut image)?),
        FileSystem::Raw => RecoverySource::Carved,
        _ => {
            let fresh = fat::scan(&mut image, ScanOptions::default(), cancel, |_| {})?;
            if fresh.cancelled {
                return Err(Error::Cancelled);
            }
            RecoverySource::Fat(
                fresh
                    .entries
                    .into_iter()
                    .map(|e| (e.candidate.id.clone(), e))
                    .collect(),
            )
        }
    };
    for mut candidate in candidates {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let mut result = result_for(&candidate.id);
        if !prefix.is_empty() {
            candidate.id = candidate
                .id
                .strip_prefix(prefix)
                .ok_or(Error::SourceChanged)?
                .into();
            candidate.original_path = candidate
                .original_path
                .strip_prefix(&format!("P{}/", partition.unwrap()))
                .ok_or(Error::SourceChanged)?
                .into();
        }
        let mut memory_limit = false;
        let attempt = (|| -> Result<PreparedFile> {
            let data = candidate_data(&source, &mut image, &candidate, cancel)?;
            result.warnings = data.warnings;
            result.extent_evidence = data.extent_evidence;
            result.allocation_status = data.allocation_status;
            let size = data.data.size();
            if size > MAX_RESTORE_MEMORY.saturating_sub(*used) {
                memory_limit = true;
                return Err(Error::Limit("selected payload exceeds the 512 MiB read-before-write limit; select fewer files or use separate-disk streaming recovery".into()));
            }
            let mut bytes = Vec::new();
            bytes.try_reserve_exact(size as usize).map_err(|_| {
                memory_limit = true;
                Error::Limit("cannot allocate selected payload".into())
            })?;
            let mut offset = 0;
            let mut hash = Sha256::new();
            while offset < size {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let chunk =
                    data.data
                        .read(&mut image, offset, (size - offset).min(256 * 1024) as usize)?;
                if chunk.is_empty() {
                    return Err(Error::Invalid("short recovery payload".into()));
                }
                offset += chunk.len() as u64;
                hash.update(&chunk);
                bytes.extend_from_slice(&chunk);
            }
            let path = if candidate.stream_name.is_some() {
                candidate.original_path.rsplit_once('/').map_or_else(
                    || data.name.clone(),
                    |(parent, _)| format!("{parent}/{}", data.name),
                )
            } else {
                candidate.original_path.clone()
            };
            let (relative, original_verified) =
                match relative_path(&path, &data.name, candidate.path_verified) {
                    Ok(path) => (path, candidate.path_verified),
                    Err(_) => (PathBuf::from(component(&data.name)), false),
                };
            result.sha256 = Some(format!("{:x}", hash.finalize()));
            *used += size;
            Ok(PreparedFile {
                bytes,
                relative,
                partition,
                region: session.region.clone(),
                original_verified,
                result: result.clone(),
            })
        })();
        match attempt {
            Ok(file) => batch.files.push(file),
            Err(e @ (Error::Cancelled | Error::SourceChanged)) => return Err(e),
            Err(e) if memory_limit => return Err(e),
            Err(e) => {
                result.error = Some(format!("{}: {e}", e.code()));
                batch.failures.push(result);
            }
        }
    }
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    Ok(())
}
fn renamed(name: &str, n: u32, directory: bool) -> String {
    let suffix = format!(" (復元 {n})");
    let fit = |stem: &str, extension: &str| {
        let mut stem = stem.to_owned();
        let mut extension = extension.to_owned();
        while stem.len() + suffix.len() + extension.len() > 255 {
            if !stem.is_empty() {
                stem.pop();
            } else if !extension.is_empty() {
                extension.pop();
            } else {
                break;
            }
        }
        format!("{stem}{suffix}{extension}")
    };
    if directory {
        return fit(name, "");
    }
    let path = Path::new(name);
    match (
        path.file_stem().and_then(|s| s.to_str()),
        path.extension().and_then(|s| s.to_str()),
    ) {
        (Some(stem), Some(ext)) => fit(stem, &format!(".{ext}")),
        _ => fit(name, ""),
    }
}
fn existing(dir: &Dir, name: &str) -> Result<Option<String>> {
    // Consistent collisions across case-sensitive Linux and Windows/macOS.
    for entry in dir.entries()? {
        let entry = entry?;
        if entry.file_name().to_string_lossy().to_lowercase() == name.to_lowercase() {
            return Ok(Some(entry.file_name().to_string_lossy().into_owned()));
        }
    }
    Ok(None)
}
fn reserve(
    dir: &Dir,
    name: &str,
    collision: CollisionPolicy,
) -> Result<Option<(String, cap_std::fs::File)>> {
    for n in 0..100_000 {
        let name = if n == 0 {
            name.into()
        } else {
            renamed(name, n, false)
        };
        if existing(dir, &name)?.is_some() {
            if collision == CollisionPolicy::Skip {
                return Ok(None);
            }
            continue;
        }
        match dir.open_with(
            &name,
            OpenOptions::new().write(true).read(true).create_new(true),
        ) {
            Ok(file) => return Ok(Some((name, file))),
            Err(e) if e.kind() == ErrorKind::AlreadyExists => {
                if collision == CollisionPolicy::Skip {
                    return Ok(None);
                }
            }
            Err(e) => return Err(e.into()),
        }
    }
    Err(Error::Limit("too many filename collisions".into()))
}
fn folder(
    root: &RecoveryDestination,
    relative: &Path,
    collision: CollisionPolicy,
    directories: &mut BTreeMap<PathBuf, String>,
) -> Result<Option<RecoveryDestination>> {
    let mut current = root.try_clone()?;
    let mut requested = root.path.clone();
    for part in relative.iter() {
        let name = part
            .to_str()
            .ok_or_else(|| Error::Request("invalid directory name".into()))?;
        requested.push(part);
        if let Some(actual) = directories.get(&requested) {
            current = current.child(actual)?;
            continue;
        }
        let mut chosen = None;
        for n in 0..100_000 {
            let proposed = if n == 0 {
                name.into()
            } else {
                renamed(name, n, true)
            };
            if let Some(found) = existing(&current.dir, &proposed)? {
                let metadata = current.dir.symlink_metadata(&found)?;
                if metadata.is_dir() && !metadata.file_type().is_symlink() {
                    chosen = Some(found);
                    break;
                }
                if collision == CollisionPolicy::Skip {
                    return Ok(None);
                }
                continue;
            }
            match current.dir.create_dir(&proposed) {
                Ok(()) => {
                    chosen = Some(proposed);
                    break;
                }
                Err(e) if e.kind() == ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
        }
        let chosen = chosen.ok_or_else(|| Error::Limit("directory collisions".into()))?;
        current = current.child(&chosen)?;
        directories.insert(requested.clone(), chosen);
    }
    Ok(Some(current))
}
fn open_output(path: &Path, roots: Option<&[OutputRoot]>) -> Result<RecoveryDestination> {
    match roots {
        Some(roots) => RecoveryDestination::within_create(path, roots),
        None => RecoveryDestination::open(path),
    }
}
pub fn restore_files(
    session: &Session,
    ids: &[String],
    options: &RestoreOptions,
    cancel: &AtomicBool,
) -> Result<Vec<RecoveryResult>> {
    restore_impl(session, ids, options, None, cancel)
}
pub fn restore_files_with_roots(
    session: &Session,
    ids: &[String],
    options: &RestoreOptions,
    roots: &[OutputRoot],
    cancel: &AtomicBool,
) -> Result<Vec<RecoveryResult>> {
    restore_impl(session, ids, options, Some(roots), cancel)
}
fn restore_impl(
    session: &Session,
    ids: &[String],
    options: &RestoreOptions,
    roots: Option<&[OutputRoot]>,
    cancel: &AtomicBool,
) -> Result<Vec<RecoveryResult>> {
    let batch = prepare(session, ids, cancel)?;
    if let Some(roots) = roots {
        for root in roots {
            root.verify()?;
        }
    }
    let mut results = batch.failures;
    let mut directories = BTreeMap::new();
    for mut file in batch.files {
        let attempt = (|| -> Result<()> {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let base = match &options.target {
                RestoreTarget::Directory(path) => {
                    if session.filesystem == FileSystem::Disk {
                        path.join(format!("P{}", file.partition.unwrap()))
                    } else {
                        path.clone()
                    }
                }
                RestoreTarget::Original(mappings) => {
                    if !file.original_verified {
                        return Err(Error::Request(
                            "original path is unknown; choose a destination folder".into(),
                        ));
                    }
                    mappings.iter().find(|m| m.partition == file.partition).map(|m| m.path.clone())
                        .or_else(|| crate::device::mounted_root(&session.source, file.region.as_ref()).ok().flatten())
                        .ok_or_else(|| Error::Request("original volume is not mounted or identified; provide a volume root or choose a folder".into()))?
                }
            };
            let base = crate::destination::canonical_output_path(&base)?;
            if base.join(&file.relative).to_string_lossy().len() > 3000
                || file.relative.components().count() > 64
            {
                if matches!(options.target, RestoreTarget::Original(_)) {
                    return Err(Error::Request(
                        "original path is too long/deep; choose a destination folder".into(),
                    ));
                }
                file.relative = PathBuf::from(
                    file.relative
                        .file_name()
                        .ok_or_else(|| Error::Request("missing filename".into()))?,
                );
                file.result.warnings.push("Original hierarchy exceeds the portable path limit; saved directly in the destination folder.".into());
            }
            let parent = file.relative.parent().unwrap_or(Path::new(""));
            // MCP may authorize a subfolder of the original volume, rather than
            // authorizing its entire drive. Resolve only that requested subtree.
            let full_parent = base.join(parent);
            let (root, relative) = if let Some(roots) = roots {
                let absolute = if full_parent.is_absolute() {
                    full_parent.clone()
                } else {
                    std::env::current_dir()?.join(&full_parent)
                };
                let approved = roots
                    .iter()
                    .find(|r| absolute.starts_with(&r.path))
                    .ok_or_else(|| {
                        Error::Request(
                            "original/destination path is outside allowed output roots".into(),
                        )
                    })?;
                let relative = absolute.strip_prefix(&approved.path).unwrap().to_owned();
                if relative.components().any(|c| {
                    !matches!(
                        c,
                        std::path::Component::Normal(_) | std::path::Component::CurDir
                    )
                }) {
                    return Err(Error::Request("invalid output path".into()));
                }
                (
                    RecoveryDestination::within(&approved.path, roots)?,
                    relative,
                )
            } else {
                (open_output(&base, None)?, parent.to_owned())
            };
            let Some(destination) = folder(&root, &relative, options.collision, &mut directories)?
            else {
                file.result.status = "skipped".into();
                file.result.sha256 = None;
                file.result
                    .warnings
                    .push("An existing file blocks an original folder; it was kept.".into());
                return Ok(());
            };
            destination.verify()?;
            let name = file
                .relative
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Request("missing filename".into()))?;
            let Some((name, mut output)) = reserve(&destination.dir, name, options.collision)?
            else {
                file.result.status = "skipped".into();
                file.result.sha256 = None;
                file.result
                    .warnings
                    .push("An existing filename was kept.".into());
                return Ok(());
            };
            file.result.path = Some(destination.path.join(name));
            for chunk in file.bytes.chunks(256 * 1024) {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                output.write_all(chunk)?;
                file.result.bytes_written += chunk.len() as u64;
            }
            output.sync_all()?;
            destination.verify()?;
            file.result.status = "completed".into();
            Ok(())
        })();
        if let Err(e) = attempt {
            file.result.status = if matches!(e, Error::Cancelled) {
                "cancelled"
            } else if file.result.path.is_some() {
                "incomplete"
            } else {
                "failed"
            }
            .into();
            file.result.error = Some(format!("{}: {e}", e.code()));
            if file.result.status != "completed" {
                file.result.sha256 = None;
            }
        }
        results.push(file.result);
    }
    Ok(results)
}

pub fn restore_zip(
    session: &Session,
    ids: &[String],
    archive: &Path,
    collision: CollisionPolicy,
    cancel: &AtomicBool,
) -> Result<ArchiveRecovery> {
    zip_impl(session, ids, archive, collision, None, cancel)
}
pub fn restore_zip_with_roots(
    session: &Session,
    ids: &[String],
    archive: &Path,
    collision: CollisionPolicy,
    roots: &[OutputRoot],
    cancel: &AtomicBool,
) -> Result<ArchiveRecovery> {
    zip_impl(session, ids, archive, collision, Some(roots), cancel)
}
fn zip_impl(
    session: &Session,
    ids: &[String],
    archive: &Path,
    collision: CollisionPolicy,
    roots: Option<&[OutputRoot]>,
    cancel: &AtomicBool,
) -> Result<ArchiveRecovery> {
    let batch = prepare(session, ids, cancel)?;
    let parent = archive
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Request("invalid ZIP filename".into()))?;
    if component(name) != name || !name.to_lowercase().ends_with(".zip") {
        return Err(Error::Request("choose a safe ZIP filename".into()));
    }
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let destination = open_output(parent, roots)?;
    let Some((name, output)) = reserve(&destination.dir, name, collision)? else {
        return Ok(ArchiveRecovery {
            status: "skipped".into(),
            archive: None,
            partial_archive: None,
            staging: None,
            sha256: None,
            results: Vec::new(),
            error: None,
        });
    };
    let path = destination.path.join(name);
    let mut result = ArchiveRecovery {
        status: "incomplete".into(),
        archive: None,
        partial_archive: Some(path.clone()),
        staging: None,
        sha256: None,
        results: batch.failures,
        error: None,
    };
    let attempt = (|| -> Result<()> {
        let mut zip = zip::ZipWriter::new(output);
        let options = zip::write::SimpleFileOptions::default()
            .compression_method(zip::CompressionMethod::Stored)
            .large_file(true);
        let mut names = BTreeSet::from(["rescue-report.json".to_owned()]);
        for mut file in batch.files {
            if session.filesystem == FileSystem::Disk {
                file.relative =
                    PathBuf::from(format!("P{}", file.partition.ok_or(Error::SourceChanged)?))
                        .join(&file.relative);
            }
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let base = file.relative.to_string_lossy().replace('\\', "/");
            let mut name = base.clone();
            for n in 1..100_000 {
                if names.insert(name.to_lowercase()) {
                    break;
                }
                let leaf = file.relative.file_name().unwrap().to_string_lossy();
                name = file
                    .relative
                    .with_file_name(renamed(&leaf, n, false))
                    .to_string_lossy()
                    .replace('\\', "/");
            }
            zip.start_file(&name, options)
                .map_err(|e| Error::Io(std::io::Error::other(e)))?;
            zip.write_all(&file.bytes)?;
            file.result.status = "completed".into();
            file.result.bytes_written = file.bytes.len() as u64;
            file.result.archive_entry = Some(name);
            file.result.path = Some(path.clone());
            result.results.push(file.result);
        }
        zip.start_file("rescue-report.json", options)
            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        serde_json::to_writer_pretty(
            &mut zip,
            &serde_json::json!({"schema_version":1,"session_id":session.id,"results":result.results}),
        )?;
        let mut output = zip
            .finish()
            .map_err(|e| Error::Io(std::io::Error::other(e)))?;
        output.sync_all()?;
        use std::io::{Seek, SeekFrom};
        output.seek(SeekFrom::Start(0))?;
        let mut hash = Sha256::new();
        let mut buf = [0u8; 64 * 1024];
        loop {
            let n = output.read(&mut buf)?;
            if n == 0 {
                break;
            }
            hash.update(&buf[..n]);
        }
        destination.verify()?;
        result.sha256 = Some(format!("{:x}", hash.finalize()));
        result.archive = Some(path);
        result.partial_archive = None;
        result.status = if result.results.iter().all(|r| r.status == "completed") {
            "completed"
        } else {
            "incomplete"
        }
        .into();
        Ok(())
    })();
    if let Err(e) = attempt {
        result.error = Some(format!("{}: {e}", e.code()));
        if matches!(e, Error::Cancelled) {
            result.status = "cancelled".into();
        }
    }
    Ok(result)
}
