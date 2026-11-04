use super::{DeviceInfo, DiskSource};
use crate::{Error, Result};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    os::unix::{
        fs::{FileTypeExt, MetadataExt},
        io::AsRawFd,
    },
    path::{Path, PathBuf},
};
const BLKGETSIZE64: libc::c_ulong = 0x80081272;
const BLKSSZGET: libc::c_ulong = 0x1268;
pub fn canonical(path: &Path) -> Result<PathBuf> {
    Ok(path.canonicalize()?)
}
pub fn is_device(path: &Path) -> bool {
    path.metadata()
        .is_ok_and(|m| m.file_type().is_block_device())
}
pub fn open(path: &Path) -> Result<(File, Option<DeviceInfo>)> {
    let file = File::open(path)?;
    let metadata = file.metadata()?;
    if metadata.is_file() {
        return Ok((file, None));
    }
    if !metadata.file_type().is_block_device() {
        return Err(Error::Unsupported(
            "source must be a disk, volume or regular image".into(),
        ));
    }
    let info = info(&file, path)?.validate()?;
    Ok((file, Some(info)))
}
pub fn info(file: &File, _path: &Path) -> Result<DeviceInfo> {
    let mut bytes = 0u64;
    let mut sector = 0u32;
    // These ioctls only query geometry; the handle was opened with O_RDONLY.
    if unsafe { libc::ioctl(file.as_raw_fd(), BLKGETSIZE64, &mut bytes) } < 0
        || unsafe { libc::ioctl(file.as_raw_fd(), BLKSSZGET, &mut sector) } < 0
    {
        return Err(std::io::Error::last_os_error().into());
    }
    let device = file.metadata()?.rdev();
    let node = PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        libc::major(device),
        libc::minor(device)
    ));
    let serial = ["device/wwid", "device/serial", "wwid"]
        .iter()
        .find_map(|p| fs::read_to_string(node.join(p)).ok())
        .unwrap_or_default();
    Ok(DeviceInfo {
        bytes,
        sector,
        identity: format!("linux:{device}:{}", serial.trim()),
    })
}
fn disk_for(node: &Path, seen: &mut BTreeSet<PathBuf>, depth: usize) -> Result<BTreeSet<String>> {
    if depth > 32 {
        return Err(Error::UnsafeDestination("disk topology too deep".into()));
    }
    let node = node
        .canonicalize()
        .map_err(|_| Error::UnsafeDestination("disk topology unavailable".into()))?;
    if !seen.insert(node.clone()) {
        return Err(Error::UnsafeDestination("cyclic disk topology".into()));
    }
    if node.join("partition").exists() {
        return disk_for(
            node.parent()
                .ok_or_else(|| Error::Invalid("partition parent".into()))?,
            seen,
            depth + 1,
        );
    }
    let slaves = fs::read_dir(node.join("slaves"))?.collect::<std::io::Result<Vec<_>>>()?;
    if slaves.is_empty() {
        return Ok(BTreeSet::from([fs::read_to_string(node.join("dev"))?
            .trim()
            .to_owned()]));
    }
    let mut result = BTreeSet::new();
    for slave in slaves {
        result.extend(disk_for(&slave.path(), &mut seen.clone(), depth + 1)?);
    }
    Ok(result)
}
fn from_device(device: u64) -> Result<BTreeSet<String>> {
    disk_for(
        &PathBuf::from(format!(
            "/sys/dev/block/{}:{}",
            libc::major(device),
            libc::minor(device)
        )),
        &mut BTreeSet::new(),
        0,
    )
}
pub fn source_disks(path: &Path) -> Result<BTreeSet<String>> {
    from_device(path.metadata()?.rdev())
}
pub fn output_disks(path: &Path) -> Result<BTreeSet<String>> {
    from_device(path.metadata()?.dev())
}
pub fn mounted_root(path: &Path, region: Option<&crate::SourceRegion>) -> Result<Option<PathBuf>> {
    let device = path.metadata()?.rdev();
    let source_node = PathBuf::from(format!(
        "/sys/dev/block/{}:{}",
        libc::major(device),
        libc::minor(device)
    ))
    .canonicalize()?;
    let target = if let Some(region) = region {
        let mut found = None;
        for entry in fs::read_dir(&source_node)? {
            let node = entry?.path();
            let number = fs::read_to_string(node.join("partition"))
                .ok()
                .and_then(|n| n.trim().parse::<u32>().ok());
            let offset = fs::read_to_string(node.join("start"))
                .ok()
                .and_then(|n| n.trim().parse::<u64>().ok())
                .and_then(|n| n.checked_mul(512));
            let length = fs::read_to_string(node.join("size"))
                .ok()
                .and_then(|n| n.trim().parse::<u64>().ok())
                .and_then(|n| n.checked_mul(512));
            if number == Some(region.index)
                && offset == Some(region.offset)
                && length == Some(region.length)
            {
                found = Some(fs::read_to_string(node.join("dev"))?.trim().to_owned());
                break;
            }
        }
        match found {
            Some(value) => value,
            None => return Ok(None),
        }
    } else {
        format!("{}:{}", libc::major(device), libc::minor(device))
    };
    for line in fs::read_to_string("/proc/self/mountinfo")?.lines() {
        let fields: Vec<_> = line.split_whitespace().collect();
        if fields.len() > 5 && fields[2] == target && fields[3] == "/" {
            let mount = fields[4]
                .replace("\\040", " ")
                .replace("\\011", "\t")
                .replace("\\012", "\n")
                .replace("\\134", "\\");
            return Ok(Some(PathBuf::from(mount)));
        }
    }
    Ok(None)
}
pub fn list() -> Result<Vec<DiskSource>> {
    let mut disks = Vec::new();
    for item in fs::read_dir("/sys/class/block")? {
        let item = item?;
        let name = item.file_name();
        let path = PathBuf::from("/dev").join(&name);
        if !is_device(&path) {
            continue;
        }
        let node = item.path();
        let value = |name: &str| {
            fs::read_to_string(node.join(name))
                .ok()
                .map(|s| s.trim().to_owned())
        };
        let Some(bytes) = value("size")
            .and_then(|s| s.parse::<u64>().ok())
            .and_then(|n| n.checked_mul(512))
        else {
            continue;
        };
        if bytes == 0 {
            continue;
        }
        disks.push(DiskSource {
            path,
            name: value("device/model")
                .filter(|s| !s.is_empty())
                .unwrap_or_else(|| name.to_string_lossy().into_owned()),
            kind: if node.join("partition").exists() {
                "volume"
            } else {
                "disk"
            }
            .into(),
            bytes,
            sector_size: value("queue/logical_block_size")
                .and_then(|s| s.parse().ok())
                .unwrap_or(512),
            read_only: value("ro").as_deref() == Some("1"),
            mounts: Vec::new(),
        });
    }
    disks.sort_by(|a, b| a.path.cmp(&b.path));
    Ok(disks)
}
