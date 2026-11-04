use crate::{Error, Result, device};
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
    pub path: PathBuf,
    pub length: u64,
    base: u64,
    total: u64,
    device: Option<device::DeviceInfo>,
}
impl Image {
    pub fn open(path: &Path) -> Result<Self> {
        let path = device::canonical_source_path(path)?;
        let (file, device) = device::open(&path)?;
        let total = device
            .as_ref()
            .map(|d| d.bytes)
            .unwrap_or(file.metadata()?.len());
        Ok(Self {
            file,
            path,
            length: total,
            base: 0,
            total,
            device,
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
            image.base = region.offset;
            image.length = region.length;
        }
        Ok(image)
    }
    pub fn sector_size(&self) -> u32 {
        self.device.as_ref().map(|d| d.sector).unwrap_or(512)
    }
    pub fn is_device(&self) -> bool {
        self.device.is_some()
    }
    pub fn read(&mut self, offset: u64, length: usize) -> Result<Vec<u8>> {
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
            self.file.seek(SeekFrom::Start(start))?;
            self.file.read_exact(buffer)?;
            let skip = (absolute - start) as usize;
            return Ok(buffer[skip..skip + length].to_vec());
        }
        let mut bytes = vec![0; length];
        self.file.seek(SeekFrom::Start(absolute))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }
    pub fn fingerprint(&mut self) -> Result<Fingerprint> {
        let modified_ns = if let Some(original) = &self.device {
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
            hash.update(self.read(offset, size)?);
        }
        Ok(Fingerprint {
            length: self.length,
            modified_ns,
            sample_sha256: format!("{:x}", hash.finalize()),
        })
    }
}
