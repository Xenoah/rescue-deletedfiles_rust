use crate::{
    AcquisitionIdentity, Error, MissingRange, Result, device, mapped_source::MappedSource,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::UNIX_EPOCH,
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct Fingerprint {
    pub length: u64,
    pub modified_ns: String,
    pub sample_sha256: String,
}
/// Owns a strictly read-only file/device handle and a bounded volume window.
pub(crate) struct Image {
    file: File,
    raid: Option<Box<crate::raid::RaidSource>>,
    pub path: PathBuf,
    pub length: u64,
    base: u64,
    total: u64,
    device: Option<device::DeviceInfo>,
    mapped: Option<MappedSource>,
    boot: Option<(crate::RecoveryBoot, Vec<u8>)>,
    read_budget: Option<u64>,
    bytes_read: u64,
}
pub(crate) fn map_path(image: &Path) -> PathBuf {
    let mut name = image.as_os_str().to_owned();
    name.push(".rmap");
    PathBuf::from(name)
}
fn regular_file_identity(file: &File) -> Result<String> {
    #[cfg(unix)]
    {
        use std::os::unix::fs::MetadataExt;
        let metadata = file.metadata()?;
        Ok(format!("file:{}:{}", metadata.dev(), metadata.ino()))
    }
    #[cfg(windows)]
    {
        use std::os::windows::io::AsRawHandle;
        use windows_sys::Win32::Storage::FileSystem::{
            BY_HANDLE_FILE_INFORMATION, GetFileInformationByHandle,
        };
        let mut info: BY_HANDLE_FILE_INFORMATION = unsafe { std::mem::zeroed() };
        // A read-only identity query on the already opened source handle.
        if unsafe { GetFileInformationByHandle(file.as_raw_handle(), &mut info) } == 0 {
            return Err(std::io::Error::last_os_error().into());
        }
        Ok(format!(
            "file:{}:{}:{}",
            info.dwVolumeSerialNumber, info.nFileIndexHigh, info.nFileIndexLow
        ))
    }
}
impl Image {
    pub(crate) fn from_raid(raid: crate::raid::RaidSource) -> Result<Self> {
        let path = raid
            .evidence
            .config
            .members
            .iter()
            .find_map(|m| m.path.clone())
            .ok_or_else(|| Error::Invalid("RAID has no available image".into()))?;
        crate::source_scope::check(&path)?;
        let file = File::open(&path)?;
        let total = raid.evidence.logical_bytes;
        Ok(Self {
            file,
            raid: Some(Box::new(raid)),
            path,
            length: total,
            base: 0,
            total,
            device: None,
            mapped: None,
            boot: None,
            read_budget: None,
            bytes_read: 0,
        })
    }
    pub(crate) fn select_window(&mut self, offset: u64, length: u64) -> Result<()> {
        if length == 0 || offset.checked_add(length).is_none_or(|e| e > self.total) {
            return Err(Error::Invalid("virtual source window".into()));
        }
        self.base = offset;
        self.length = length;
        self.boot = None;
        Ok(())
    }
    pub fn open(path: &Path) -> Result<Self> {
        let path = device::canonical_source_path(path)?;
        crate::source_scope::check(&path)?;
        let (mut file, device) = device::open(&path)?;
        let raid = if crate::is_raid_path(&path) {
            Some(Box::new(crate::raid::RaidSource::open(&mut file, &path)?))
        } else {
            None
        };
        // Windows raw handles do not implement ordinary file metadata queries.
        // Do not eagerly evaluate a file-length fallback for a device.
        let mapped = if raid.is_none()
            && device.is_none()
            && (path
                .file_name()
                .is_some_and(|n| n.to_string_lossy().ends_with(".rescue.raw"))
                || map_path(&path).try_exists()?)
        {
            Some(MappedSource::open(&map_path(&path), &file)?)
        } else {
            None
        };
        let total = if let Some(raid) = &raid {
            raid.evidence.logical_bytes
        } else if let Some(map) = &mapped {
            map.length()
        } else if let Some(device) = &device {
            device.bytes
        } else {
            file.metadata()?.len()
        };
        Ok(Self {
            file,
            raid,
            path,
            length: total,
            base: 0,
            total,
            device,
            mapped,
            boot: None,
            read_budget: None,
            bytes_read: 0,
        })
    }
    pub fn open_region(path: &Path, region: Option<&crate::SourceRegion>) -> Result<Self> {
        let mut image = Self::open(path)?;
        if let Some(region) = region {
            if region.length == 0
                || region
                    .offset
                    .checked_add(region.length)
                    .is_none_or(|end| end > image.total)
            {
                return Err(Error::Invalid("volume extends beyond source".into()));
            }
            if let Some(d) = &image.device
                && (!region.offset.is_multiple_of(u64::from(d.sector))
                    || !region.length.is_multiple_of(u64::from(d.sector)))
            {
                return Err(Error::Invalid("unaligned volume boundary".into()));
            }
            if let Some(boot) = &region.recovery_boot {
                if boot.bytes < 512
                    || boot.bytes > 12 * 4096
                    || boot.offset < region.offset
                    || boot
                        .offset
                        .checked_add(u64::from(boot.bytes))
                        .is_none_or(|end| end > region.offset + region.length)
                {
                    return Err(Error::Invalid("recovery boot outside volume".into()));
                }
                let bytes = image.read(boot.offset, boot.bytes as usize)?;
                if format!("{:x}", Sha256::digest(&bytes)) != boot.sha256 {
                    return Err(Error::SourceChanged);
                }
                image.boot = Some((boot.clone(), bytes));
            }
            image.base = region.offset;
            image.length = region.length;
        }
        Ok(image)
    }
    /// Only filesystem geometry parsers use the selected boot copy. Payload reads
    /// always use read(), so a backup never substitutes bytes in recovered files.
    pub fn boot_read(&mut self, length: usize) -> Result<Vec<u8>> {
        if let Some((_, bytes)) = &self.boot {
            return bytes
                .get(..length)
                .map(|b| b.to_vec())
                .ok_or_else(|| Error::Invalid("selected boot copy is too short".into()));
        }
        self.read(0, length)
    }
    pub fn uses_backup_boot(&self) -> bool {
        self.boot
            .as_ref()
            .is_some_and(|(b, _)| b.role == crate::BootRole::Backup)
    }
    pub fn limit_reads(&mut self, bytes: u64) {
        self.read_budget = Some(bytes);
        self.bytes_read = 0;
    }
    pub fn bytes_read(&self) -> u64 {
        self.bytes_read
    }
    pub fn sector_size(&self) -> u32 {
        self.raid
            .as_ref()
            .map(|r| r.evidence.config.sector_size)
            .or_else(|| self.mapped.as_ref().map(|m| m.sector()))
            .or_else(|| self.device.as_ref().map(|d| d.sector))
            .unwrap_or(512)
    }
    pub fn has_read_map(&self) -> bool {
        self.mapped.is_some()
    }
    pub fn take_raid_trace(&mut self) -> Option<crate::raid::RaidReadTrace> {
        self.raid.as_mut().map(|r| r.take_trace())
    }
    pub fn validate_raid(&self, session: &crate::Session) -> Result<()> {
        if self.raid.as_ref().map(|r| &r.evidence) != session.raid.as_ref() {
            return Err(Error::SourceChanged);
        }
        if let Some(expected) = &session.source_info
            && (!expected.same_object(&self.source_info()?)
                || expected.offset != session.region.as_ref().map_or(0, |r| r.offset)
                || expected.length != session.fingerprint.length)
        {
            return Err(Error::SourceChanged);
        }
        Ok(())
    }
    pub fn annotate_coverage(&self, session: &mut crate::Session) -> Result<()> {
        session.source_info = Some(self.source_info()?);
        if let Some(raid) = &self.raid {
            session.schema_version = session.schema_version.max(9);
            session.raid = Some(raid.evidence.clone());
            session.warnings.extend(raid.evidence.warnings.clone());
        }
        if let Some(map) = &self.mapped
            && map.missing_bytes() > 0
        {
            session.truncated = true;
            session.warnings.push(format!("MISSING_RANGES: the acquired source contains {} unread/failed bytes; metadata and payloads crossing these ranges are unavailable.",map.missing_bytes()));
        }
        Ok(())
    }
    pub fn source_info(&self) -> Result<crate::SourceInfo> {
        use crate::{SourceInfo, SourceKind};
        let read_map = self.mapped.as_ref().map(|map| map.source_info());
        let (kind, identity) = if let Some(raid) = &self.raid {
            (SourceKind::Raid, raid.evidence.identity_sha256.clone())
        } else if let Some(map) = &self.mapped {
            map.check(&self.file)?;
            (SourceKind::Acquisition, map.source_info().summary.image_id)
        } else {
            let identity = self.acquisition_identity()?.identity;
            (
                if self.device.is_some() {
                    SourceKind::Device
                } else {
                    SourceKind::Image
                },
                identity,
            )
        };
        Ok(SourceInfo {
            id: SourceInfo::identifier(kind, &identity),
            kind,
            bytes: self.total,
            sector_size: self.sector_size(),
            offset: self.base,
            length: self.length,
            identity,
            read_map,
        })
    }
    pub fn acquisition_identity(&self) -> Result<AcquisitionIdentity> {
        if let Some(raid) = &self.raid {
            return Ok(AcquisitionIdentity {
                path: self.path.clone(),
                length: self.total,
                sector_size: self.sector_size(),
                identity: format!("raid:{}", raid.identity(&self.file)?),
                modified_ns: None,
            });
        }
        let modified_ns = if self.device.is_none() {
            Some(
                self.file
                    .metadata()?
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .map_err(|_| Error::Invalid("source timestamp precedes Unix epoch".into()))?
                    .as_nanos()
                    .to_string(),
            )
        } else {
            None
        };
        let identity = if let Some(original) = &self.device {
            let current = device::device_info(&self.file, &self.path)?;
            if original != &current {
                return Err(Error::SourceChanged);
            }
            current.identity
        } else {
            if self.file.metadata()?.len() != self.total {
                return Err(Error::SourceChanged);
            }
            let current = File::open(&self.path)?;
            if same_file::Handle::from_file(current)?
                != same_file::Handle::from_file(self.file.try_clone()?)?
            {
                return Err(Error::SourceChanged);
            }
            regular_file_identity(&self.file)?
        };
        Ok(AcquisitionIdentity {
            path: self.path.clone(),
            length: self.total,
            sector_size: self.sector_size(),
            identity,
            modified_ns,
        })
    }
    pub fn reject_same_file(&self, other: &File) -> Result<()> {
        if let Some(raid) = &self.raid {
            raid.reject_same_file(other)?;
        }
        // Acquisition outputs have already been checked to be regular files on
        // separate media. Windows raw-device handles do not support file IDs.
        if self.device.is_some() {
            return Ok(());
        }
        if same_file::Handle::from_file(self.file.try_clone()?)?
            == same_file::Handle::from_file(other.try_clone()?)?
        {
            return Err(Error::UnsafeDestination("output aliases the source".into()));
        }
        Ok(())
    }
    pub fn is_device(&self) -> bool {
        self.device.is_some()
    }
    #[cfg(windows)]
    pub(crate) fn read_grant_paths(&self) -> Vec<PathBuf> {
        if let Some(raid) = &self.raid {
            raid.evidence
                .config
                .members
                .iter()
                .filter_map(|member| member.path.clone())
                .collect()
        } else {
            vec![self.path.clone()]
        }
    }
    pub fn absolute_offset(&self, offset: u64) -> u64 {
        self.base + offset
    }
    pub fn read(&mut self, offset: u64, length: usize) -> Result<Vec<u8>> {
        if let Some(budget) = &mut self.read_budget {
            *budget = budget
                .checked_sub(length as u64)
                .ok_or_else(|| Error::Limit("metadata probe read budget".into()))?;
        }
        self.bytes_read = self.bytes_read.saturating_add(length as u64);
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| Error::Invalid("read overflow".into()))?;
        if end > self.length {
            return Err(Error::Invalid("read extends beyond selected volume".into()));
        }
        if length == 0 {
            return Ok(Vec::new());
        }
        let absolute = self
            .base
            .checked_add(offset)
            .ok_or_else(|| Error::Invalid("read overflow".into()))?;
        if let Some(raid) = &mut self.raid {
            let (bytes, gaps) = raid.read_available(absolute, length)?;
            return if gaps.is_empty() {
                Ok(bytes)
            } else {
                Err(Error::MissingRanges(gaps))
            };
        }
        if let Some(map) = &mut self.mapped {
            return map.read(&mut self.file, absolute, length);
        }
        if let Some(device) = &self.device {
            let sector = u64::from(device.sector);
            let start = absolute / sector * sector;
            let end = absolute
                .checked_add(length as u64)
                .and_then(|n| n.checked_add(sector - 1))
                .map(|n| n / sector * sector)
                .ok_or_else(|| Error::Invalid("aligned read overflow".into()))?;
            if end > self.total {
                return Err(Error::Invalid("aligned read extends beyond disk".into()));
            }
            let count =
                usize::try_from(end - start).map_err(|_| Error::Limit("disk read size".into()))?;
            crate::preview::charge_read(count)?;
            // Raw Windows volumes require sector-aligned offset, length and buffer.
            // A padded allocation permits alignment using a safe slice, without raw allocation.
            let mut storage = vec![
                0u8;
                count
                    .checked_add(device.sector as usize)
                    .ok_or_else(|| Error::Limit("disk read size".into()))?
            ];
            let align = storage.as_ptr().align_offset(device.sector as usize);
            let buffer = &mut storage[align..align + count];
            #[cfg(not(windows))]
            {
                self.file.seek(SeekFrom::Start(start))?;
                self.file.read_exact(buffer)?;
            }
            #[cfg(windows)]
            {
                use std::os::windows::fs::FileExt;
                // Duplicated broker handles share a file pointer. Explicit
                // offsets make concurrent scan/preview reads independent.
                let mut at = 0;
                while at < buffer.len() {
                    let read = self.file.seek_read(&mut buffer[at..], start + at as u64)?;
                    if read == 0 {
                        return Err(std::io::Error::from(std::io::ErrorKind::UnexpectedEof).into());
                    }
                    at += read;
                }
            }
            let skip = (absolute - start) as usize;
            return Ok(buffer[skip..skip + length].to_vec());
        }
        crate::preview::charge_read(length)?;
        let mut bytes = vec![0; length];
        self.file.seek(SeekFrom::Start(absolute))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    /// Only scanners/fingerprints use this. Parsers and recovered payloads always use strict read().
    pub fn read_available(
        &mut self,
        offset: u64,
        length: usize,
    ) -> Result<(Vec<u8>, Vec<MissingRange>)> {
        if offset
            .checked_add(length as u64)
            .is_none_or(|end| end > self.length)
        {
            return Err(Error::Invalid("read extends beyond selected volume".into()));
        }
        if let Some(raid) = &mut self.raid {
            return raid.read_available(self.base + offset, length);
        }
        if let Some(map) = &mut self.mapped {
            return map.read_available(&mut self.file, self.base + offset, length);
        }
        self.read(offset, length).map(|b| (b, Vec::new()))
    }
    pub fn fingerprint(&mut self) -> Result<Fingerprint> {
        let modified_ns = if let Some(raid) = &mut self.raid {
            raid.check(&mut self.file)?;
            format!("raid:{}:{}", raid.evidence.identity_sha256, self.base)
        } else if let Some(map) = &self.mapped {
            map.check(&self.file)?;
            format!(
                "mapped:{}:{}:{}",
                map.digest(),
                self.file.metadata()?.len(),
                self.base
            )
        } else if let Some(original) = &self.device {
            let current = device::device_info(&self.file, &self.path)?;
            if &current != original {
                return Err(Error::SourceChanged);
            }
            format!(
                "device:{}:{}:{}:{}",
                current.identity, current.bytes, current.sector, self.base
            )
        } else {
            let metadata = self.file.metadata()?;
            if metadata.len() != self.total {
                return Err(Error::SourceChanged);
            }
            metadata
                .modified()?
                .duration_since(UNIX_EPOCH)
                .map_err(|_| Error::Invalid("image timestamp precedes Unix epoch".into()))?
                .as_nanos()
                .to_string()
        };
        let mut hash = Sha256::new();
        hash.update(self.length.to_le_bytes());
        if let Some((boot, _)) = self.boot.clone() {
            let current = self.read(boot.offset - self.base, boot.bytes as usize)?;
            if format!("{:x}", Sha256::digest(&current)) != boot.sha256 {
                return Err(Error::SourceChanged);
            }
            hash.update(b"rescue-recovery-boot-v1");
            hash.update(serde_json::to_vec(&boot)?);
        }
        // Preserve legacy regular-volume fingerprints exactly; bound partition sessions to their start.
        if self.base != 0 {
            hash.update(b"rescue-volume-window");
            hash.update(self.base.to_le_bytes());
        }
        let size = self.length.min(64 * 1024) as usize;
        for offset in [
            0,
            self.length.saturating_sub(size as u64) / 2,
            self.length.saturating_sub(size as u64),
        ] {
            hash.update(offset.to_le_bytes());
            let (bytes, gaps) = self.read_available(offset, size)?;
            hash.update(bytes);
            if !gaps.is_empty() {
                hash.update(serde_json::to_vec(&gaps)?);
            }
        }
        Ok(Fingerprint {
            length: self.length,
            modified_ns,
            sample_sha256: format!("{:x}", hash.finalize()),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn selected_backup_is_geometry_only_and_changes_invalidate_its_identity() {
        let file = tempfile::NamedTempFile::new().unwrap();
        let mut raw = vec![0x11; 2048];
        raw[512..1024].fill(0x22);
        std::fs::write(file.path(), &raw).unwrap();
        let region = crate::SourceRegion {
            index: 1,
            name: "test".into(),
            offset: 0,
            length: 2048,
            filesystem: Some(crate::FileSystem::Ntfs),
            recovery_boot: Some(crate::RecoveryBoot {
                role: crate::BootRole::Backup,
                offset: 512,
                bytes: 512,
                sha256: format!("{:x}", Sha256::digest(&raw[512..1024])),
            }),
        };
        let mut image = Image::open_region(file.path(), Some(&region)).unwrap();
        assert_eq!(image.boot_read(512).unwrap(), vec![0x22; 512]);
        assert_eq!(image.read(0, 1024).unwrap(), raw[..1024]);
        image.fingerprint().unwrap();
        raw[512] = 0x33;
        std::fs::write(file.path(), raw).unwrap();
        assert!(matches!(image.fingerprint(), Err(Error::SourceChanged)));
    }
    #[test]
    fn device_reads_align_without_exposing_adjacent_volume_bytes() {
        use std::io::Write;
        let mut file = tempfile::NamedTempFile::new().unwrap();
        let bytes = (0..16384).map(|i| (i % 251) as u8).collect::<Vec<_>>();
        file.write_all(&bytes).unwrap();
        let mut image = Image::open(file.path()).unwrap();
        image.device = Some(device::DeviceInfo {
            bytes: 16384,
            sector: 4096,
            identity: "test".into(),
        });
        image.base = 4096;
        image.length = 8192;
        for (offset, size) in [(0, 1), (3, 501), (4000, 200), (8191, 1), (0, 8192)] {
            assert_eq!(
                image.read(offset, size).unwrap(),
                bytes[4096 + offset as usize..4096 + offset as usize + size]
            );
        }
        assert!(image.read(8191, 2).is_err());
        assert!(image.read(u64::MAX, 1).is_err());
    }
}
