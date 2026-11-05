//! Read-only discovery of volume geometry without trusting a partition table.
use crate::{
    Error, FileSystem, Fingerprint, Progress, ProgressUnit, Result, ScanOptions, Session,
    SourceRegion, ntfs::Ntfs, source::Image,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_PROBES: usize = 4096;
const MAX_VOLUMES: usize = 256;
const PROBE_BYTES: u64 = 8 * 1024 * 1024;
const TOTAL_PROBE_BYTES: u64 = 256 * 1024 * 1024;
const CHUNK: u64 = 4 * 1024 * 1024;

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BootRole {
    Primary,
    Backup,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryBoot {
    pub role: BootRole,
    /// Absolute source offset. No bytes are written to the primary boot location.
    pub offset: u64,
    pub bytes: u32,
    pub sha256: String,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveredVolume {
    pub id: String,
    pub region: SourceRegion,
    pub evidence: Vec<String>,
    pub matches_current_table: bool,
    pub overlaps: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiscoveryResult {
    pub source: PathBuf,
    pub fingerprint: Fingerprint,
    pub volumes: Vec<DiscoveredVolume>,
    pub scanned_bytes: u64,
    pub probes: usize,
    pub metadata_bytes_read: u64,
    pub cancelled: bool,
    pub truncated: bool,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct DiscoveryOptions {
    pub range_start: u64,
    pub range_end: Option<u64>,
}

fn invalid(message: &str) -> Error {
    Error::Invalid(format!("volume discovery: {message}"))
}
fn u16_at(b: &[u8], i: usize) -> u64 {
    u64::from(u16::from_le_bytes(b[i..i + 2].try_into().unwrap()))
}
fn u32_at(b: &[u8], i: usize) -> u64 {
    u64::from(u32::from_le_bytes(b[i..i + 4].try_into().unwrap()))
}
fn u64_at(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}

#[derive(Clone)]
struct Geometry {
    filesystem: FileSystem,
    sector: u64,
    count: u64,
    backup: u64,
    boot_bytes: u32,
}
fn geometry(boot: &[u8]) -> Result<Geometry> {
    if boot.len() < 512 || boot[510..512] != [0x55, 0xaa] {
        return Err(invalid("boot signature"));
    }
    let (filesystem, sector, count, backup, boot_sectors) = if &boot[3..11] == b"NTFS    " {
        (
            FileSystem::Ntfs,
            u16_at(boot, 11),
            u64_at(boot, 40),
            u64_at(boot, 40),
            1,
        )
    } else if &boot[3..11] == b"EXFAT   " {
        if !(9..=12).contains(&boot[108]) {
            return Err(invalid("exFAT sector shift"));
        }
        (
            FileSystem::Exfat,
            1u64 << boot[108],
            u64_at(boot, 72),
            12,
            12,
        )
    } else if &boot[82..90] == b"FAT32   " {
        (
            FileSystem::Fat32,
            u16_at(boot, 11),
            u32_at(boot, 32),
            u16_at(boot, 50),
            1,
        )
    } else {
        return Err(invalid("unrecognized filesystem"));
    };
    if ![512, 1024, 2048, 4096].contains(&sector)
        || count < 24
        || count.checked_mul(sector).is_none()
    {
        return Err(invalid("volume geometry overflow"));
    }
    Ok(Geometry {
        filesystem,
        sector,
        count,
        backup,
        boot_bytes: (sector * boot_sectors) as u32,
    })
}
fn possible_starts(at: u64, g: &Geometry) -> Vec<(u64, BootRole)> {
    let mut starts = vec![(at, BootRole::Primary)];
    if g.backup > 0
        && g.backup != 0xffff
        && let Some(start) = g
            .backup
            .checked_mul(g.sector)
            .and_then(|n| at.checked_sub(n))
    {
        starts.push((start, BootRole::Backup));
    }
    // NTFS formatters use either a sector count or the final sector index.
    if g.filesystem == FileSystem::Ntfs
        && let Some(start) = (g.count - 1)
            .checked_mul(g.sector)
            .and_then(|n| at.checked_sub(n))
    {
        starts.push((start, BootRole::Backup));
    }
    starts
}
fn candidate_region(
    image: &mut Image,
    at: u64,
    start: u64,
    role: BootRole,
    boot: &[u8],
) -> Result<SourceRegion> {
    let g = geometry(boot)?;
    if !start.is_multiple_of(g.sector) || !possible_starts(at, &g).contains(&(start, role)) {
        return Err(invalid("boot location does not describe selected volume"));
    }
    let mut length = g.count * g.sector;
    if g.filesystem == FileSystem::Ntfs {
        if role == BootRole::Backup {
            length = at
                .checked_sub(start)
                .and_then(|n| n.checked_add(g.sector))
                .ok_or_else(|| invalid("NTFS backup extent"))?;
        } else if let Some(last) = start.checked_add(length)
            && last
                .checked_add(g.sector)
                .is_some_and(|end| end <= image.length)
            && image
                .read(last, 512)
                .is_ok_and(|b| b[3..84] == boot[3..84] && b[510..512] == [0x55, 0xaa])
        {
            length += g.sector;
        }
    }
    if length < u64::from(g.boot_bytes)
        || start
            .checked_add(length)
            .is_none_or(|end| end > image.length)
        || at
            .checked_add(u64::from(g.boot_bytes))
            .is_none_or(|end| end > start + length)
    {
        return Err(invalid("volume outside source"));
    }
    let bytes = image.read(at, g.boot_bytes as usize)?;
    Ok(SourceRegion {
        index: 0,
        name: format!("Discovered {:?} at {start}", g.filesystem),
        offset: start,
        length,
        filesystem: Some(g.filesystem),
        recovery_boot: Some(RecoveryBoot {
            role,
            offset: at,
            bytes: g.boot_bytes,
            sha256: format!("{:x}", Sha256::digest(&bytes)),
        }),
    })
}
fn probe(path: &Path, region: &SourceRegion, budget: u64) -> (Result<Vec<String>>, u64) {
    let mut image = match Image::open_region(path, Some(region)) {
        Ok(i) => i,
        Err(e) => return (Err(e), 0),
    };
    image.limit_reads(budget.min(PROBE_BYTES));
    let result = (|| {
        match region.filesystem {
            Some(FileSystem::Ntfs) => {
                let ntfs = Ntfs::open(&mut image)?;
                // Anchor the runlist to the independently read base MFT record.
                if ntfs.mft.read(&mut image, 0, ntfs.geometry.record)?
                    != image.read(ntfs.geometry.mft, ntfs.geometry.record)?
                {
                    return Err(invalid("MFT first extent disagrees with boot location"));
                }
                let root = ntfs
                    .record(&mut image, 5)?
                    .ok_or_else(|| invalid("MFT root directory absent"))?;
                if !root.in_use
                    || !root.directory
                    || root.base != 0
                    || root.parent != 5
                    || root.parent_sequence != root.sequence
                    || !root.unsupported.is_empty()
                {
                    return Err(invalid("MFT root directory ownership"));
                }
                let mut evidence = vec![
                    "Validated NTFS geometry, MFT fixups and complete MFT runlist".into(),
                    "Cross-checked MFT base extent and root record 5".into(),
                ];
                evidence.extend(ntfs.warnings.clone());
                Ok(evidence)
            }
            Some(FileSystem::Fat32 | FileSystem::Exfat) => crate::fat::probe(&mut image),
            _ => Err(invalid("unsupported filesystem")),
        }
    })();
    (result, image.bytes_read())
}

pub fn discover_volumes(
    path: &Path,
    options: &DiscoveryOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(Progress),
) -> Result<DiscoveryResult> {
    let mut image = Image::open(path)?;
    let fingerprint = image.fingerprint()?;
    let end = options.range_end.unwrap_or(image.length);
    if options.range_start > end || end > image.length {
        return Err(Error::Request("discovery range outside source".into()));
    }
    let mut cursor = options
        .range_start
        .checked_add(511)
        .ok_or_else(|| invalid("range overflow"))?
        / 512
        * 512;
    let current = crate::inspect_source(path).ok();
    let mut result = DiscoveryResult {
        source: image.path.clone(),
        fingerprint,
        volumes: Vec::new(),
        scanned_bytes: 0,
        probes: 0,
        metadata_bytes_read: 0,
        cancelled: false,
        truncated: false,
        warnings: Vec::new(),
    };
    let mut seen = BTreeSet::new();
    'scan: while cursor.checked_add(512).is_some_and(|next| next <= end) {
        if cancel.load(Ordering::Relaxed) {
            result.cancelled = true;
            break;
        }
        let size = (end - cursor).min(CHUNK) / 512 * 512;
        let (data, gaps) = image.read_available(cursor, size as usize)?;
        if !gaps.is_empty() && !result.truncated {
            result
                .warnings
                .push("Unread acquisition ranges were skipped during volume discovery".into());
            result.truncated = true;
        }
        for (index, boot) in data.as_chunks::<512>().0.iter().enumerate() {
            if cancel.load(Ordering::Relaxed) {
                result.cancelled = true;
                break 'scan;
            }
            let at = cursor + index as u64 * 512;
            if gaps
                .iter()
                .any(|g| g.source_offset < at + 512 && g.source_offset + g.length > at)
            {
                continue;
            }
            let Ok(g) = geometry(boot) else { continue };
            for (start, role) in possible_starts(at, &g) {
                if result.probes >= MAX_PROBES
                    || result.metadata_bytes_read >= TOTAL_PROBE_BYTES
                    || result.volumes.len() >= MAX_VOLUMES
                {
                    result.truncated = true;
                    result
                        .warnings
                        .push("RESOURCE_LIMIT: volume discovery metadata/candidate budget".into());
                    break 'scan;
                }
                result.probes += 1;
                let Ok(region) = candidate_region(&mut image, at, start, role, boot) else {
                    continue;
                };
                // Different valid geometries at overlapping offsets remain alternatives.
                // An identical primary/backup geometry is represented only once.
                let key = (start, region.length, boot[3..112].to_vec());
                if seen.contains(&key) {
                    continue;
                }
                let (evidence, read) = probe(
                    path,
                    &region,
                    TOTAL_PROBE_BYTES - result.metadata_bytes_read,
                );
                result.metadata_bytes_read += read;
                let evidence = match evidence {
                    Ok(e) => e,
                    Err(Error::Limit(_)) => {
                        result.truncated = true;
                        result
                            .warnings
                            .push(format!("Metadata probe budget reached at {start}"));
                        continue;
                    }
                    Err(_) => continue,
                };
                seen.insert(key);
                let matches_current_table = current.as_ref().is_some_and(|layout| {
                    layout.regions.iter().any(|r| {
                        r.offset == region.offset
                            && r.length >= region.length
                            && r.filesystem == region.filesystem
                    })
                });
                let descriptor = region.recovery_boot.as_ref().unwrap();
                let id = format!(
                    "v{start:x}-{:x}-{}",
                    descriptor.offset,
                    &descriptor.sha256[..16]
                );
                result.volumes.push(DiscoveredVolume {
                    id,
                    region,
                    evidence,
                    matches_current_table,
                    overlaps: Vec::new(),
                });
            }
        }
        cursor += size;
        result.scanned_bytes = cursor.saturating_sub(options.range_start);
        progress(Progress {
            scanned_records: result.scanned_bytes,
            total_records: end - options.range_start,
            candidates: result.volumes.len(),
            unit: ProgressUnit::Bytes,
        });
    }
    result
        .volumes
        .sort_by_key(|v| (v.region.offset, v.region.length, v.id.clone()));
    for i in 0..result.volumes.len() {
        result.volumes[i].region.index = (i + 1) as u32;
        let a = &result.volumes[i].region;
        let overlapping = result
            .volumes
            .iter()
            .enumerate()
            .filter(|(j, b)| {
                *j != i
                    && a.offset < b.region.offset + b.region.length
                    && b.region.offset < a.offset + a.length
            })
            .map(|(_, b)| b.id.clone())
            .collect();
        result.volumes[i].overlaps = overlapping;
    }
    if image.fingerprint()? != result.fingerprint {
        return Err(Error::SourceChanged);
    }
    Ok(result)
}

pub(crate) fn validate_region(path: &Path, region: &SourceRegion) -> Result<()> {
    let boot = region
        .recovery_boot
        .as_ref()
        .ok_or_else(|| invalid("missing boot evidence"))?;
    let mut image = Image::open(path)?;
    let data = image.read(boot.offset, 512)?;
    let expected = candidate_region(&mut image, boot.offset, region.offset, boot.role, &data)?;
    if expected.offset != region.offset
        || expected.length != region.length
        || expected.filesystem != region.filesystem
        || expected.recovery_boot != region.recovery_boot
    {
        return Err(Error::SourceChanged);
    }
    probe(path, region, PROBE_BYTES).0?;
    Ok(())
}
pub fn scan_discovered_volume(
    path: &Path,
    region: &SourceRegion,
    options: ScanOptions,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<Session> {
    validate_region(path, region)?;
    let mut session = crate::scan_volume(path, Some(region), options, cancel, progress)?;
    session.warnings.push(format!("Virtual recovered volume at byte {} using {:?} boot at {}. No partition table or filesystem was repaired.",region.offset,
        region.recovery_boot.as_ref().unwrap().role,region.recovery_boot.as_ref().unwrap().offset));
    Ok(session)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn adversarial_boot_geometry_is_bounded_before_any_probe() {
        let mut seed = 0x31415926u32;
        for n in 0..4096 {
            let mut b = [0u8; 512];
            for byte in &mut b {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *byte = seed as u8;
            }
            b[510..512].copy_from_slice(&[0x55, 0xaa]);
            match n % 3 {
                0 => b[3..11].copy_from_slice(b"NTFS    "),
                1 => b[82..90].copy_from_slice(b"FAT32   "),
                _ => b[3..11].copy_from_slice(b"EXFAT   "),
            }
            if let Ok(g) = geometry(&b) {
                assert!(g.count.checked_mul(g.sector).is_some());
                assert!(g.boot_bytes <= 49152);
                for at in [0, 511, 4096, u64::MAX] {
                    assert!(possible_starts(at, &g).len() <= 3);
                }
            }
        }
    }
}
