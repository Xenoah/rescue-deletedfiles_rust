//! Pin verified directory capabilities through recovery and report creation.
use crate::{Error, Result};
use cap_std::{ambient_authority, fs::Dir};
use std::path::{Path, PathBuf};

pub struct OutputRoot {
    path: PathBuf,
    dir: Dir,
}
impl OutputRoot {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let dir = Dir::open_ambient_dir(&path, ambient_authority())?;
        Ok(Self { path, dir })
    }
}

pub struct RecoveryDestination {
    pub(crate) dir: Dir,
    pub(crate) path: PathBuf,
    identity: same_file::Handle,
}
impl RecoveryDestination {
    fn pinned(path: PathBuf, dir: Dir) -> Result<Self> {
        let identity = same_file::Handle::from_file(dir.try_clone()?.into_std_file())?;
        let destination = Self {
            dir,
            path,
            identity,
        };
        destination.verify()?;
        Ok(destination)
    }
    pub fn open(path: &Path) -> Result<Self> {
        std::fs::create_dir_all(path)?;
        let path = path.canonicalize()?;
        let dir = Dir::open_ambient_dir(&path, ambient_authority())?;
        Self::pinned(path, dir)
    }
    /// Resolution is confined by the pinned root, even if a path is swapped.
    pub fn within(path: &Path, roots: &[OutputRoot]) -> Result<Self> {
        let path = path.canonicalize()?;
        for root in roots {
            if let Ok(relative) = path.strip_prefix(&root.path) {
                let relative = if relative.as_os_str().is_empty() {
                    Path::new(".")
                } else {
                    relative
                };
                let dir = root.dir.open_dir(relative)?;
                return Self::pinned(path, dir);
            }
        }
        Err(Error::Request(
            "destination is outside allowed existing directories".into(),
        ))
    }
    pub(crate) fn verify(&self) -> Result<()> {
        let current = Dir::open_ambient_dir(&self.path, ambient_authority())
            .map_err(|_| Error::DestinationChanged)?;
        let identity = same_file::Handle::from_file(current.into_std_file())
            .map_err(|_| Error::DestinationChanged)?;
        if identity != self.identity {
            return Err(Error::DestinationChanged);
        }
        Ok(())
    }
}
