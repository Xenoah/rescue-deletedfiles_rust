//! Read-only native disk access and output placement checks.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    fs::File,
    path::{Path, PathBuf},
};
#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "macos")]
mod macos;
#[cfg(windows)]
mod windows;
#[cfg(target_os = "linux")]
use linux as native;
#[cfg(target_os = "macos")]
use macos as native;
#[cfg(windows)]
use windows as native;

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct DiskSource {
    pub path: PathBuf,
    pub name: String,
    pub kind: String,
    pub bytes: u64,
    pub sector_size: u32,
    pub read_only: bool,
    pub mounts: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct DeviceInfo {
    pub bytes: u64,
    pub sector: u32,
    pub identity: String,
}
impl DeviceInfo {
    pub fn validate(self) -> Result<Self> {
        if self.bytes == 0
            || !(512..=65536).contains(&self.sector)
            || !self.sector.is_power_of_two()
            || !self.bytes.is_multiple_of(u64::from(self.sector))
        {
            return Err(Error::Invalid("invalid disk geometry".into()));
        }
        Ok(self)
    }
}
pub fn list_disks() -> Result<Vec<DiskSource>> {
    native::list()
}
pub fn canonical_source_path(path: &Path) -> Result<PathBuf> {
    native::canonical(path)
}
pub(crate) fn open(path: &Path) -> Result<(File, Option<DeviceInfo>)> {
    native::open(path)
}
pub(crate) fn device_info(file: &File, path: &Path) -> Result<DeviceInfo> {
    native::info(file, path)?.validate()
}
pub fn is_device(path: &Path) -> bool {
    native::is_device(path)
}
fn existing_ancestor(path: &Path) -> Result<PathBuf> {
    let absolute = if path.is_absolute() {
        path.to_owned()
    } else {
        std::env::current_dir()?.join(path)
    };
    let mut current = absolute.as_path();
    loop {
        match current.try_exists() {
            Ok(true) => return Ok(current.canonicalize()?),
            Ok(false) => {}
            Err(e) => return Err(e.into()),
        }
        current = current
            .parent()
            .ok_or_else(|| Error::Request("no existing output ancestor".into()))?;
    }
}
/// Before creating work or output directories, reject the source's physical media.
/// Unknown topology is rejected rather than assuming two paths mean two disks.
pub fn guard_output_path(source: &Path, output: &Path) -> Result<()> {
    if !is_device(source) {
        return Ok(());
    }
    let source = native::source_disks(source)?;
    let ancestor = existing_ancestor(output)?;
    let destination = native::output_disks(&ancestor)?;
    compare_disks(&source, &destination)
}
fn compare_disks(source: &BTreeSet<String>, destination: &BTreeSet<String>) -> Result<()> {
    if source.is_empty() || destination.is_empty() {
        return Err(Error::UnsafeDestination(
            "cannot establish the output disk; choose a separate identifiable disk".into(),
        ));
    }
    if !source.is_disjoint(destination) {
        return Err(Error::UnsafeDestination(
            "the work/recovery destination is on the scanned disk; choose another physical disk"
                .into(),
        ));
    }
    Ok(())
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn every_member_of_a_spanned_disk_is_protected() {
        let source = BTreeSet::from(["disk1".into(), "disk2".into()]);
        assert!(compare_disks(&source, &BTreeSet::from(["disk2".into()])).is_err());
        assert!(compare_disks(&source, &BTreeSet::new()).is_err());
        assert!(compare_disks(&source, &BTreeSet::from(["disk3".into()])).is_ok());
    }
}
