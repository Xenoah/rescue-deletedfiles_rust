use crate::{
    Error, FileSystem, Progress, RecoveryDestination, RecoveryResult, Result, ScanOptions,
    ScannedVolume, Session, inspect_source, source::Image,
};
use std::{
    collections::BTreeSet,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

/// Scan a volume or all recognized volumes on an MBR/GPT disk, with a global budget.
pub fn scan_source(
    path: &Path,
    options: ScanOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<Session> {
    let layout = inspect_source(path)?;
    if layout.scheme == "volume" {
        let mut session = crate::scan_image(path, options, cancel, progress)?;
        if layout.device {
            session.schema_version = 5;
            session.warnings.push(LIVE_WARNING.into());
        }
        return Ok(session);
    }
    let mut image = Image::open(path)?;
    let fingerprint = image.fingerprint()?;
    let mut session = Session {
        schema_version: 5,
        filesystem: FileSystem::Disk,
        id: uuid::Uuid::new_v4().to_string(),
        source: image.path.clone(),
        fingerprint,
        candidates: Vec::new(),
        scanned_records: 0,
        total_records: 0,
        cancelled: false,
        truncated: false,
        invalid_records: 0,
        warnings: layout.warnings,
        search: None,
        region: None,
        volumes: Vec::new(),
    };
    if layout.device {
        session.warnings.push(LIVE_WARNING.into());
    }
    for region in layout.regions {
        if cancel.load(Ordering::Relaxed) {
            session.cancelled = true;
            break;
        }
        if region.filesystem.is_none() {
            session.truncated = true;
            session.warnings.push(format!("Partition {} ({}) skipped: unsupported filesystem; full-disk PNG carving is available separately", region.index, region.name));
            continue;
        }
        let remaining = ScanOptions {
            max_records: options.max_records.saturating_sub(session.scanned_records),
            max_candidates: options
                .max_candidates
                .saturating_sub(session.candidates.len()),
        };
        if remaining.max_records == 0 || remaining.max_candidates == 0 {
            session.truncated = true;
            break;
        }
        let previous = session.scanned_records;
        let previous_candidates = session.candidates.len();
        let volume = crate::scan_volume(path, Some(&region), remaining, cancel, |mut p| {
            p.scanned_records = p.scanned_records.saturating_add(previous);
            p.total_records = p.total_records.saturating_add(previous);
            p.candidates += previous_candidates;
            progress(p);
        });
        let mut volume = match volume {
            Ok(volume) => volume,
            Err(error @ (Error::Invalid(_) | Error::Unsupported(_) | Error::Limit(_))) => {
                session.truncated = true;
                session.warnings.push(format!(
                    "Partition {} could not be scanned: {error}",
                    region.index
                ));
                continue;
            }
            Err(error) => return Err(error),
        };
        for candidate in &mut volume.candidates {
            candidate.id = format!("p{}:{}", region.index, candidate.id);
            candidate.original_path = format!(
                "P{}/{}",
                region.index,
                candidate.original_path.trim_start_matches('/')
            );
        }
        session.scanned_records = session
            .scanned_records
            .saturating_add(volume.scanned_records);
        session.total_records = session.total_records.saturating_add(volume.total_records);
        session.invalid_records = session
            .invalid_records
            .saturating_add(volume.invalid_records);
        session.cancelled |= volume.cancelled;
        session.truncated |= volume.truncated;
        session.warnings.extend(
            volume
                .warnings
                .into_iter()
                .map(|w| format!("P{}: {w}", region.index)),
        );
        session.volumes.push(ScannedVolume {
            region,
            fingerprint: volume.fingerprint,
        });
        session.candidates.extend(volume.candidates);
        if session.cancelled {
            break;
        }
    }
    if session.volumes.is_empty() && !session.cancelled {
        return Err(Error::Unsupported(
            "no readable NTFS/FAT32/exFAT partition; select PNG carving to search raw bytes".into(),
        ));
    }
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    session.bound_candidates()?;
    Ok(session)
}
const LIVE_WARNING: &str = "Direct disk access is read-only. Mounted volumes may change externally during scanning; take the source offline or use a read-only snapshot/image when possible. SSD TRIM or overwritten bytes cannot be reconstructed.";

pub(crate) fn validate_region(session: &Session) -> Result<()> {
    if let Some(region) = &session.region {
        if session.schema_version != 5
            || !session.volumes.is_empty()
            || region.filesystem != Some(session.filesystem)
            || !inspect_source(&session.source)?.regions.contains(region)
        {
            return Err(Error::SourceChanged);
        }
    } else if !session.volumes.is_empty() {
        return Err(Error::Request("unexpected volume list".into()));
    }
    Ok(())
}

pub(crate) fn recover_disk(
    session: &Session,
    ids: &[String],
    destination: &RecoveryDestination,
    cancel: &AtomicBool,
) -> Result<Vec<RecoveryResult>> {
    if session.schema_version != 5
        || session.region.is_some()
        || session.volumes.is_empty()
        || session.volumes.len() > 256
    {
        return Err(Error::Request("invalid disk session".into()));
    }
    if ids.is_empty() {
        return Err(Error::Request("select at least one candidate".into()));
    }
    let layout = inspect_source(&session.source)?;
    let mut image = Image::open(&session.source)?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    let mut indices = BTreeSet::new();
    for volume in &session.volumes {
        if !indices.insert(volume.region.index)
            || !layout.regions.contains(&volume.region)
            || !matches!(
                volume.region.filesystem,
                Some(FileSystem::Ntfs | FileSystem::Fat32 | FileSystem::Exfat)
            )
        {
            return Err(Error::SourceChanged);
        }
    }
    let mut requested = BTreeSet::new();
    for id in ids {
        let candidate = session
            .candidates
            .iter()
            .find(|c| &c.id == id)
            .ok_or_else(|| Error::Request(format!("unknown candidate: {id}")))?;
        let (prefix, _) = candidate.id.split_once(':').ok_or(Error::SourceChanged)?;
        if !session
            .volumes
            .iter()
            .any(|v| prefix == format!("p{}", v.region.index))
        {
            return Err(Error::SourceChanged);
        }
        requested.insert(id.clone());
    }
    let mut results = Vec::new();
    for volume in &session.volumes {
        let prefix = format!("p{}:", volume.region.index);
        let candidates = session
            .candidates
            .iter()
            .filter(|c| requested.contains(&c.id) && c.id.starts_with(&prefix))
            .map(|c| {
                let mut c = c.clone();
                c.id = c.id[prefix.len()..].to_owned();
                c
            })
            .collect::<Vec<_>>();
        if candidates.is_empty() {
            continue;
        }
        let mut child = session.clone();
        child.filesystem = volume.region.filesystem.unwrap();
        child.fingerprint = volume.fingerprint.clone();
        child.region = Some(volume.region.clone());
        child.volumes.clear();
        child.candidates = candidates;
        let child_ids = child
            .candidates
            .iter()
            .map(|c| c.id.clone())
            .collect::<Vec<_>>();
        for mut result in crate::recover_to(&child, &child_ids, destination, cancel)? {
            result.candidate_id = format!("{prefix}{}", result.candidate_id);
            results.push(result);
        }
    }
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    Ok(results)
}
