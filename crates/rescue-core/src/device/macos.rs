use super::{DeviceInfo, DiskSource};
use crate::{Error, Result};
use serde_json::Value;
use std::{
    collections::BTreeSet,
    ffi::CString,
    fs::File,
    io::Write,
    os::unix::{
        ffi::OsStrExt,
        fs::{FileTypeExt, MetadataExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
    process::{Command, Stdio},
};
const DKIOCGETBLOCKSIZE: libc::c_ulong = 0x40046418;
const DKIOCGETBLOCKCOUNT: libc::c_ulong = 0x40086419;
fn identifier(path: &Path) -> Option<String> {
    let text = path.to_str()?;
    let value = text
        .strip_prefix("/dev/")?
        .strip_prefix('r')
        .unwrap_or(text.strip_prefix("/dev/")?);
    let suffix = value.strip_prefix("disk")?;
    let parts: Vec<_> = suffix.split('s').collect();
    if parts.is_empty()
        || parts
            .iter()
            .any(|p| p.is_empty() || !p.bytes().all(|c| c.is_ascii_digit()))
    {
        return None;
    }
    Some(value.into())
}
pub fn canonical(path: &Path) -> Result<PathBuf> {
    Ok(path.canonicalize()?)
}
pub fn is_device(path: &Path) -> bool {
    identifier(path).is_some()
        && path
            .metadata()
            .is_ok_and(|m| m.file_type().is_block_device() || m.file_type().is_char_device())
}
pub fn open(path: &Path) -> Result<(File, Option<DeviceInfo>)> {
    let file = File::open(path)?;
    if file.metadata()?.is_file() {
        return Ok((file, None));
    }
    if !is_device(path) {
        return Err(Error::Unsupported(
            "source must be a disk, volume or regular image".into(),
        ));
    }
    let value = info(&file, path)?.validate()?;
    Ok((file, Some(value)))
}
pub fn info(file: &File, path: &Path) -> Result<DeviceInfo> {
    let mut sector = 0u32;
    let mut count = 0u64;
    // DKIOCGET* query geometry only, on an O_RDONLY descriptor.
    if unsafe { libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKSIZE, &mut sector) } < 0
        || unsafe { libc::ioctl(file.as_raw_fd(), DKIOCGETBLOCKCOUNT, &mut count) } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    Ok(DeviceInfo {
        bytes: count
            .checked_mul(u64::from(sector))
            .ok_or_else(|| Error::Invalid("disk geometry overflow".into()))?,
        sector,
        identity: format!(
            "macos:{}:{}",
            identifier(path).unwrap_or_default(),
            file.metadata()?.rdev()
        ),
    })
}
fn query(args: &[&str]) -> Result<Value> {
    let result = Command::new("/usr/sbin/diskutil").args(args).output()?;
    if !result.status.success() {
        return Err(Error::Request(format!(
            "diskutil: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    if result.stdout.len() > 16 * 1024 * 1024 {
        return Err(Error::Limit("disk inventory".into()));
    }
    let mut convert = Command::new("/usr/bin/plutil")
        .args(["-convert", "json", "-o", "-", "--", "-"])
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()?;
    let mut input = convert
        .stdin
        .take()
        .ok_or_else(|| Error::Request("plist pipe".into()))?;
    // Drain converter output while feeding its input: a large APFS inventory
    // can fill both pipes if write_all is performed before wait_with_output.
    let writer = std::thread::spawn(move || input.write_all(&result.stdout));
    let data = convert.wait_with_output()?;
    let written = writer
        .join()
        .map_err(|_| Error::Request("plist writer failed".into()))?;
    if !data.status.success() {
        return Err(Error::Request(format!(
            "disk inventory conversion: {}",
            String::from_utf8_lossy(&data.stderr)
        )));
    }
    written?;
    Ok(serde_json::from_slice(&data.stdout)?)
}
fn is_whole(info: &Value) -> bool {
    info.get("WholeDisk")
        .or_else(|| info.get("Whole"))
        .and_then(Value::as_bool)
        .unwrap_or(false)
}
fn parent<'a>(info: &'a Value, id: &'a str) -> &'a str {
    info["ParentWholeDisk"]
        .as_str()
        .or_else(|| info["PartOfWhole"].as_str())
        .unwrap_or(id)
}
fn roots(
    id: &str,
    apfs: &Value,
    seen: &mut BTreeSet<String>,
    depth: usize,
) -> Result<BTreeSet<String>> {
    if depth > 32 || !seen.insert(id.into()) {
        return Err(Error::UnsafeDestination("cyclic disk topology".into()));
    }
    let info = query(&["info", "-plist", id])?;
    let whole = parent(&info, id);
    if let Some(container) = apfs["Containers"].as_array().and_then(|items| {
        items
            .iter()
            .find(|c| c["ContainerReference"].as_str() == Some(whole))
    }) {
        let stores = container["PhysicalStores"]
            .as_array()
            .ok_or_else(|| Error::UnsafeDestination("APFS physical stores unavailable".into()))?;
        let mut result = BTreeSet::new();
        for store in stores {
            let id = store["DeviceIdentifier"]
                .as_str()
                .ok_or_else(|| Error::UnsafeDestination("APFS store identity".into()))?;
            result.extend(roots(id, apfs, &mut seen.clone(), depth + 1)?);
        }
        return Ok(result);
    }
    if whole != id {
        return roots(whole, apfs, seen, depth + 1);
    }
    // Virtual disk images are separate media; physical partition parents still collapse.
    if !is_whole(&info) {
        return Err(Error::UnsafeDestination(format!(
            "cannot identify physical output disk {id}; WholeDisk={:?}, Whole={:?}",
            info.get("WholeDisk"),
            info.get("Whole")
        )));
    }
    Ok(BTreeSet::from([format!("macos-{whole}")]))
}
fn mapped(id: &str) -> Result<BTreeSet<String>> {
    let apfs = query(&["apfs", "list", "-plist"])?;
    roots(id, &apfs, &mut BTreeSet::new(), 0)
}
pub fn source_disks(path: &Path) -> Result<BTreeSet<String>> {
    mapped(&identifier(path).ok_or_else(|| Error::Unsupported("disk path".into()))?)
}
pub fn output_disks(path: &Path) -> Result<BTreeSet<String>> {
    let path = CString::new(path.as_os_str().as_bytes())
        .map_err(|_| Error::Request("output path contains NUL".into()))?;
    let mut metadata = std::mem::MaybeUninit::<libc::statfs>::zeroed();
    // statfs resolves an arbitrary existing directory to its actual filesystem,
    // including APFS firmlinks. diskutil info only accepts a device/mount point.
    if unsafe { libc::statfs(path.as_ptr(), metadata.as_mut_ptr()) } != 0 {
        return Err(Error::UnsafeDestination(format!(
            "cannot identify output filesystem: {}",
            std::io::Error::last_os_error()
        )));
    }
    // statfs succeeded; the zero-initialized structure is fully initialized.
    let metadata = unsafe { metadata.assume_init() };
    let end = metadata
        .f_mntfromname
        .iter()
        .position(|c| *c == 0)
        .ok_or_else(|| Error::UnsafeDestination("unterminated mount device".into()))?;
    let bytes = metadata.f_mntfromname[..end]
        .iter()
        .map(|c| *c as u8)
        .collect::<Vec<_>>();
    let device = std::str::from_utf8(&bytes)
        .map_err(|_| Error::UnsafeDestination("unrecognized output mount device".into()))?;
    let id = identifier(Path::new(device)).ok_or_else(|| {
        Error::UnsafeDestination("output is not an identifiable local disk filesystem".into())
    })?;
    mapped(&id)
}
pub fn list() -> Result<Vec<DiskSource>> {
    let inventory = query(&["list", "-plist"])?;
    let mut out = Vec::new();
    let Some(items) = inventory["AllDisks"].as_array() else {
        return Err(Error::Invalid("disk inventory".into()));
    };
    for value in items.iter().take(512) {
        let Some(id) = value.as_str() else {
            continue;
        };
        let path = PathBuf::from(format!("/dev/r{id}"));
        if !is_device(&path) {
            continue;
        }
        let Ok(info) = query(&["info", "-plist", id]) else {
            continue;
        };
        out.push(DiskSource {
            path,
            name: info["VolumeName"]
                .as_str()
                .or_else(|| info["MediaName"].as_str())
                .unwrap_or(id)
                .into(),
            kind: if is_whole(&info) { "disk" } else { "volume" }.into(),
            bytes: info["TotalSize"]
                .as_u64()
                .or_else(|| info["Size"].as_u64())
                .unwrap_or(0),
            sector_size: info["DeviceBlockSize"].as_u64().unwrap_or(512) as u32,
            read_only: info["MediaReadOnly"]
                .as_bool()
                .or_else(|| info["ReadOnlyMedia"].as_bool())
                .unwrap_or(false),
            mounts: info["MountPoint"]
                .as_str()
                .map(|s| vec![s.into()])
                .unwrap_or_default(),
        });
    }
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;
    #[test]
    fn whole_disk_plist_flags_and_partition_parents_are_distinguished() {
        assert!(is_whole(&json!({"WholeDisk":true})));
        assert!(is_whole(&json!({"Whole":true})));
        assert!(!is_whole(&json!({"WholeDisk":false,"Whole":true})));
        assert!(!is_whole(&json!({})));
        assert_eq!(
            parent(&json!({"ParentWholeDisk":"disk3"}), "disk3s1"),
            "disk3"
        );
        assert_eq!(parent(&json!({"PartOfWhole":"disk4"}), "disk4s1"), "disk4");
    }
}
