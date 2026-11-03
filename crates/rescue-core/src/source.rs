use crate::{Error, Result};
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

/// Owns a read-only handle. The alpha accepts regular RAW files only.
pub(crate) struct Image {
    pub file: File,
    pub path: PathBuf,
    pub length: u64,
}

impl Image {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let file = File::open(&path)?;
        let metadata = file.metadata()?;
        if !metadata.is_file() {
            return Err(Error::Unsupported(
                "alpha accepts regular RAW images only".into(),
            ));
        }
        Ok(Self {
            file,
            path,
            length: metadata.len(),
        })
    }

    pub fn read(&mut self, offset: u64, length: usize) -> Result<Vec<u8>> {
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| Error::Invalid("read overflow".into()))?;
        if end > self.length {
            return Err(Error::Invalid("read extends beyond the image".into()));
        }
        let mut bytes = vec![0; length];
        self.file.seek(SeekFrom::Start(offset))?;
        self.file.read_exact(&mut bytes)?;
        Ok(bytes)
    }

    pub fn fingerprint(&mut self) -> Result<Fingerprint> {
        let metadata = self.file.metadata()?;
        if metadata.len() != self.length {
            return Err(Error::SourceChanged);
        }
        let mut hash = Sha256::new();
        hash.update(self.length.to_le_bytes());
        let size = self.length.min(64 * 1024) as usize;
        for offset in [
            0,
            self.length.saturating_sub(size as u64) / 2,
            self.length.saturating_sub(size as u64),
        ] {
            hash.update(offset.to_le_bytes());
            hash.update(self.read(offset, size)?);
        }
        let modified_ns = metadata
            .modified()?
            .duration_since(UNIX_EPOCH)
            .map_err(|_| Error::Invalid("image timestamp precedes Unix epoch".into()))?
            .as_nanos()
            .to_string();
        Ok(Fingerprint {
            length: self.length,
            modified_ns,
            sample_sha256: format!("{:x}", hash.finalize()),
        })
    }
}
