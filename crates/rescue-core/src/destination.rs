//! Pin verified directory capabilities through recovery and report creation.
use crate::{Error, Result};
use cap_std::{ambient_authority, fs::Dir};
use std::path::{Path, PathBuf};

pub struct OutputRoot {
    pub(crate) path: PathBuf,
    dir: Dir,
}
impl OutputRoot {
    pub fn open(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let dir = Dir::open_ambient_dir(&path, ambient_authority())?;
        Ok(Self { path, dir })
    }
    /// Pin an existing subdirectory without broadening the approved roots.
    pub fn narrow(path: &Path, roots: &[OutputRoot]) -> Result<Self> {
        let destination = RecoveryDestination::within(path, roots)?;
        Ok(Self {
            path: destination.path,
            dir: destination.dir,
        })
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
        Self::open_existing(path)
    }
    pub(crate) fn open_existing(path: &Path) -> Result<Self> {
        let path = path.canonicalize()?;
        let dir = Dir::open_ambient_dir(&path, ambient_authority())?;
        Self::pinned(path, dir)
    }
    /// Resolution is confined by the pinned root, even if a path is swapped.
    pub fn within(path: &Path, roots: &[OutputRoot]) -> Result<Self> {
        for root in roots {
            Self::pinned(root.path.clone(), root.dir.try_clone()?)?;
        }
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
    // Called only after the entire selected batch has been read. Creation is
    // confined to an already pinned MCP output root, including symlink checks.
    pub(crate) fn within_create(path: &Path, roots: &[OutputRoot]) -> Result<Self> {
        let absolute = if path.is_absolute() {
            path.to_owned()
        } else {
            std::env::current_dir()?.join(path)
        };
        if absolute
            .components()
            .any(|c| matches!(c, std::path::Component::ParentDir))
        {
            return Err(Error::Request("parent traversal in output path".into()));
        }
        for root in roots {
            if let Ok(relative) = absolute.strip_prefix(&root.path) {
                Self::pinned(root.path.clone(), root.dir.try_clone()?)?;
                root.dir.create_dir_all(relative)?;
                return Self::within(&absolute, roots);
            }
        }
        Err(Error::Request(
            "destination is outside allowed output roots".into(),
        ))
    }
    pub(crate) fn child(&self, name: &str) -> Result<Self> {
        self.verify()?;
        Self::pinned(self.path.join(name), self.dir.open_dir(name)?)
    }
    pub(crate) fn try_clone(&self) -> Result<Self> {
        self.verify()?;
        Self::pinned(self.path.clone(), self.dir.try_clone()?)
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
