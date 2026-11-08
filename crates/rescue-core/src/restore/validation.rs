//! Revalidate only selected metadata; never trust serialized paths or lengths.
use super::*;

pub(crate) fn layout(session: &Session) -> Result<()> {
    let mut image = Image::open_region(&session.source, session.region.as_ref())?;
    image.validate_raid(session)?;
    if image.fingerprint()? != session.fingerprint {
        return Err(Error::SourceChanged);
    }
    if session.filesystem != FileSystem::Disk {
        return disk_scan::validate_region(session);
    }
    if ![5, 8, 9].contains(&session.schema_version)
        || session.region.is_some()
        || session.volumes.is_empty()
        || session.volumes.len() > 256
    {
        return Err(Error::SourceChanged);
    }
    let layout = inspect_source(&session.source)?;
    let mut indices = BTreeSet::new();
    for volume in &session.volumes {
        if !indices.insert(volume.region.index)
            || !layout.regions.contains(&volume.region)
            || !matches!(
                volume.region.filesystem,
                Some(
                    FileSystem::Ntfs
                        | FileSystem::Fat12
                        | FileSystem::Fat16
                        | FileSystem::Fat32
                        | FileSystem::Exfat
                )
            )
            || Image::open_region(&session.source, Some(&volume.region))?.fingerprint()?
                != volume.fingerprint
        {
            return Err(Error::SourceChanged);
        }
    }
    Ok(())
}

pub(crate) fn fat_selection(
    image: &mut Image,
    ids: &BTreeSet<String>,
    estimate_fat_contiguous: bool,
    cancel: &AtomicBool,
) -> Result<fat::Scan> {
    let scan = fat::scan_selected(
        image,
        ScanOptions {
            max_records: 100_000_000,
            // Selection bounds retained entries; do not stop ownership traversal
            // when the last requested entry is found.
            max_candidates: usize::MAX,
            fragments: false,
            estimate_fat_contiguous,
        },
        Some(ids),
        cancel,
        |_| {},
    )?;
    if scan.cancelled {
        return Err(Error::Cancelled);
    }
    Ok(scan)
}

pub(crate) fn candidate(
    source: &RecoverySource,
    image: &mut Image,
    old: &Candidate,
    parents: &mut indexed_scan::Parents,
    session: &Session,
    cancel: &AtomicBool,
) -> Result<Candidate> {
    let candidate = match source {
        RecoverySource::Ntfs(ntfs) if old.fragment_offset.is_none() => {
            let record = ntfs
                .record(image, old.record)?
                .ok_or(Error::SourceChanged)?;
            if record.in_use || record.directory || record.base != 0 || record.name.is_none() {
                return Err(Error::SourceChanged);
            }
            let stream = record
                .streams
                .iter()
                .position(|s| s.name == old.stream_name)
                .ok_or(Error::SourceChanged)?;
            indexed_scan::make_candidate(ntfs, image, old.record, &record, stream, parents, cancel)?
        }
        RecoverySource::Ntfs(ntfs) => ntfs_remnants::revalidate_candidate(image, Some(ntfs), old)?,
        RecoverySource::NtfsFragments => ntfs_remnants::revalidate_candidate(image, None, old)?,
        RecoverySource::Fat(entries) => entries
            .get(&old.id)
            .ok_or(Error::SourceChanged)?
            .candidate
            .clone(),
        RecoverySource::Carved => {
            let mut candidate = carve::revalidate_candidate(image, old, cancel)?;
            // Saved lengths are untrusted: derive the complete extent from the
            // bounded format parser before consulting its current allocation.
            candidate.allocation_status = old.allocation_status;
            candidate.allocation_status =
                carve::validate_scope(image, session, &candidate, cancel)?;
            candidate
        }
    };
    if candidate.id != old.id
        || candidate.record_sha256 != old.record_sha256
        || candidate.record != old.record
        || candidate.sequence != old.sequence
        || candidate.stream_name != old.stream_name
        || candidate.fragment_offset != old.fragment_offset
    {
        return Err(Error::SourceChanged);
    }
    if let Some(settings) = &session.search {
        settings.validate()?;
        if !settings.matches(&candidate)
            || (candidate.fragment_offset.is_some() && !settings.fragments)
            || (session.filesystem == FileSystem::Raw
                && (candidate.record < settings.range_start
                    || candidate
                        .record
                        .checked_add(candidate.size.unwrap_or(0))
                        .is_none_or(|end| end > settings.range_end.unwrap_or(image.length))))
        {
            return Err(Error::SourceChanged);
        }
    }
    Ok(candidate)
}
