use crate::{Error, RecoveryDestination, RecoveryResult, Result, Session, recover_to};
use cap_std::fs::OpenOptions;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Write},
    path::{Component, Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
use uuid::Uuid;
use zip::{CompressionMethod, ZipWriter, write::SimpleFileOptions};

#[derive(Debug, Serialize, Deserialize)]
pub struct ArchiveRecovery {
    pub status: String,
    pub archive: Option<PathBuf>,
    pub partial_archive: Option<PathBuf>,
    pub staging: Option<PathBuf>,
    pub sha256: Option<String>,
    pub results: Vec<RecoveryResult>,
    pub error: Option<String>,
}
fn archive_error(e: zip::result::ZipError) -> Error {
    Error::Io(std::io::Error::other(e))
}

pub fn recover_zip(
    session: &Session,
    ids: &[String],
    work: &Path,
    archive: &Path,
    cancel: &AtomicBool,
) -> Result<ArchiveRecovery> {
    let parent = archive
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    let name = archive
        .file_name()
        .and_then(|n| n.to_str())
        .ok_or_else(|| Error::Request("invalid ZIP path".into()))?;
    recover_zip_to(
        session,
        ids,
        work,
        &RecoveryDestination::open(parent)?,
        name,
        cancel,
    )
}

/// Uses the same pinned output capability for ZIP data and its atomic publication.
pub fn recover_zip_to(
    session: &Session,
    ids: &[String],
    work: &Path,
    destination: &RecoveryDestination,
    name: &str,
    cancel: &AtomicBool,
) -> Result<ArchiveRecovery> {
    if name.len() > 240
        || name.contains(['/', '\\', ':', '\0'])
        || !name.to_lowercase().ends_with(".zip")
        || !matches!(
            Path::new(name).components().next(),
            Some(Component::Normal(_))
        )
        || Path::new(name).components().count() != 1
    {
        return Err(Error::Request(
            "choose a single ZIP filename of at most 240 bytes".into(),
        ));
    }
    destination.verify()?;
    match destination.dir.symlink_metadata(name) {
        Ok(_) => {
            return Err(std::io::Error::new(
                std::io::ErrorKind::AlreadyExists,
                "ZIP already exists",
            )
            .into());
        }
        Err(e) if e.kind() == std::io::ErrorKind::NotFound => {}
        Err(e) => return Err(e.into()),
    }
    std::fs::create_dir_all(work)?;
    let staging = tempfile::Builder::new()
        .prefix("rescue-stage-")
        .tempdir_in(work)?;
    let mut outcome = ArchiveRecovery {
        status: "failed".into(),
        archive: None,
        partial_archive: None,
        staging: None,
        sha256: None,
        results: Vec::new(),
        error: None,
    };
    let partial = format!("{}.zip.partial", Uuid::new_v4());
    let attempt = (|| -> Result<()> {
        {
            let target = RecoveryDestination::open(staging.path())?;
            outcome.results = recover_to(session, ids, &target, cancel)?;
        }
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        destination.verify()?;
        let file = destination.dir.open_with(
            &partial,
            OpenOptions::new().read(true).write(true).create_new(true),
        )?;
        outcome.partial_archive = Some(destination.path.join(&partial));
        let mut zip = ZipWriter::new(file);
        let options = SimpleFileOptions::default()
            .compression_method(CompressionMethod::Stored)
            .large_file(true);
        let stage =
            cap_std::fs::Dir::open_ambient_dir(staging.path(), cap_std::ambient_authority())?;
        let mut packed = outcome.results.clone();
        for result in &mut packed {
            let Some(path) = &result.path else {
                continue;
            };
            let leaf = path
                .file_name()
                .and_then(|n| n.to_str())
                .ok_or_else(|| Error::Request("invalid staged file".into()))?
                .to_owned();
            let mut source = stage.open(&leaf)?;
            zip.start_file(&leaf, options).map_err(archive_error)?;
            let mut bytes = [0; 256 * 1024];
            loop {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let count = source.read(&mut bytes)?;
                if count == 0 {
                    break;
                }
                zip.write_all(&bytes[..count])?;
            }
            result.archive_entry = Some(leaf);
            result.path = Some(destination.path.join(name));
        }
        zip.start_file("rescue-report.json", options)
            .map_err(archive_error)?;
        serde_json::to_writer_pretty(
            &mut zip,
            &serde_json::json!({"schema_version":1,"output_format":"zip","session_id":session.id,"source":session.source,"results":packed,
            "integrity_note":"Output hashes identify saved bytes; they do not prove equality with deleted originals."}),
        )?;
        let file = zip.finish().map_err(archive_error)?;
        file.sync_all()?;
        drop(file);
        let mut input = destination.dir.open(&partial)?;
        let mut hash = Sha256::new();
        let mut bytes = [0; 256 * 1024];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let count = input.read(&mut bytes)?;
            if count == 0 {
                break;
            }
            hash.update(&bytes[..count]);
        }
        drop(input);
        destination.verify()?;
        destination
            .dir
            .hard_link(&partial, &destination.dir, name)?;
        outcome.archive = Some(destination.path.join(name));
        destination.dir.remove_file(&partial)?;
        outcome.partial_archive = None;
        outcome.sha256 = Some(format!("{:x}", hash.finalize()));
        outcome.status = if packed.iter().all(|r| r.status == "completed") {
            "completed"
        } else {
            "partial"
        }
        .into();
        outcome.results = packed;
        Ok(())
    })();
    if let Err(error) = attempt {
        outcome.status = if matches!(error, Error::Cancelled) || cancel.load(Ordering::Relaxed) {
            "cancelled"
        } else {
            "failed"
        }
        .into();
        outcome.error = Some(format!("{}: {error}", error.code()));
        outcome.staging = Some(staging.keep());
    }
    Ok(outcome)
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(root: &Path) -> Session {
        let path = root.join("source.raw");
        std::fs::write(
            &path,
            include_bytes!("../../../tests/corpus/files/sample.png"),
        )
        .unwrap();
        crate::scan_carving(
            &path,
            crate::ScanOptions::default(),
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap()
    }
    #[test]
    fn cancelled_zip_preserves_staging_and_never_publishes_success() {
        let root = tempfile::tempdir().unwrap();
        let session = fixture(root.path());
        assert_eq!(session.candidates.len(), 1);
        let path = root.path().join("out.zip");
        let result = recover_zip(
            &session,
            &[session.candidates[0].id.clone()],
            root.path(),
            &path,
            &AtomicBool::new(true),
        )
        .unwrap();
        assert_eq!(result.status, "cancelled");
        assert!(result.archive.is_none() && !path.exists());
        assert!(result.staging.unwrap().is_dir());
    }
    #[test]
    fn swapped_zip_parent_is_rejected_before_writing() {
        let root = tempfile::tempdir().unwrap();
        let session = fixture(root.path());
        let parent = root.path().join("out");
        let destination = RecoveryDestination::open(&parent).unwrap();
        let moved = root.path().join("moved");
        match std::fs::rename(&parent, &moved) {
            Ok(()) => {}
            Err(error) if cfg!(windows) && matches!(error.raw_os_error(), Some(5) | Some(32)) => {
                // Windows can prevent replacement while the capability is held.
                // Releasing it must permit the same rename, proving that this
                // is the handle's protection, not a broken test directory.
                assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
                assert!(!moved.exists());
                drop(destination);
                std::fs::rename(&parent, &moved).unwrap();
                assert_eq!(std::fs::read_dir(&moved).unwrap().count(), 0);
                return;
            }
            Err(error) => panic!("unexpected parent rename failure: {error}"),
        }
        std::fs::create_dir(&parent).unwrap();
        let result = recover_zip_to(
            &session,
            &[session.candidates[0].id.clone()],
            root.path(),
            &destination,
            "out.zip",
            &AtomicBool::new(false),
        );
        assert!(matches!(result, Err(Error::DestinationChanged)));
        assert_eq!(std::fs::read_dir(&parent).unwrap().count(), 0);
        assert_eq!(
            std::fs::read_dir(root.path().join("moved"))
                .unwrap()
                .count(),
            0
        );
    }
}
