use super::{DeviceInfo, DiskSource};
use crate::{Error, Result};
use std::{
    collections::BTreeSet,
    fs::{File, OpenOptions},
    os::windows::{ffi::OsStrExt, fs::OpenOptionsExt, io::AsRawHandle, process::CommandExt},
    path::{Path, PathBuf},
    process::Command,
};
use windows_sys::Win32::{
    Storage::FileSystem::{
        FILE_SHARE_READ, FILE_SHARE_WRITE, GetVolumeNameForVolumeMountPointW, GetVolumePathNameW,
    },
    System::{
        IO::DeviceIoControl,
        Ioctl::{
            FSCTL_ALLOW_EXTENDED_DASD_IO, IOCTL_DISK_GET_DRIVE_GEOMETRY,
            IOCTL_DISK_GET_LENGTH_INFO, IOCTL_STORAGE_GET_DEVICE_NUMBER,
            IOCTL_STORAGE_QUERY_PROPERTY,
        },
    },
};
const IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS: u32 = 0x00560000;
fn normalized_device(path: &Path) -> Option<PathBuf> {
    let value = path.to_str()?;
    let upper = value.to_ascii_uppercase();
    if upper.len() == 2 && upper.as_bytes()[0].is_ascii_alphabetic() && upper.ends_with(':') {
        return Some(PathBuf::from(format!(r"\\.\{upper}")));
    }
    if let Some(number) = upper.strip_prefix(r"\\.\PHYSICALDRIVE")
        && !number.is_empty()
        && number.bytes().all(|b| b.is_ascii_digit())
        && let Ok(n) = number.parse::<u32>()
    {
        return Some(PathBuf::from(format!(r"\\.\PhysicalDrive{n}")));
    }
    if let Some(drive) = upper.strip_prefix(r"\\.\")
        && drive.len() == 2
        && drive.as_bytes()[0].is_ascii_alphabetic()
        && drive.ends_with(':')
    {
        return Some(PathBuf::from(format!(r"\\.\{drive}")));
    }
    if let Some(id) = upper
        .strip_prefix(r"\\?\VOLUME{")
        .and_then(|s| s.trim_end_matches('\\').strip_suffix('}'))
        && let Ok(id) = uuid::Uuid::parse_str(id)
    {
        return Some(PathBuf::from(format!(r"\\?\Volume{{{id}}}")));
    }
    None
}
pub fn canonical(path: &Path) -> Result<PathBuf> {
    normalized_device(path)
        .map(Ok)
        .unwrap_or_else(|| Ok(path.canonicalize()?))
}
pub fn is_device(path: &Path) -> bool {
    normalized_device(path).is_some()
}
fn device_file(path: &Path) -> std::io::Result<File> {
    if let Some(file) = crate::read_access::granted(path)? {
        return Ok(file);
    }
    OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
}
// Topology IOCTLs are FILE_ANY_ACCESS. Asking for GENERIC_READ here would
// require elevation for the destination volume, even after the user granted
// read access to only the selected source. A zero-access handle cannot read or
// write raw sectors; it is used exclusively for disk numbers and volume extents.
fn topology_file(path: &Path) -> std::io::Result<File> {
    if let Some(file) = crate::read_access::granted(path)? {
        return Ok(file);
    }
    OpenOptions::new()
        .read(true)
        .access_mode(0)
        .share_mode(FILE_SHARE_READ | FILE_SHARE_WRITE)
        .open(path)
}
pub fn open(path: &Path) -> Result<(File, Option<DeviceInfo>)> {
    if is_device(path) {
        let file = device_file(path)?;
        let _ = control(&file, FSCTL_ALLOW_EXTENDED_DASD_IO, &[], 0);
        let geometry = info(&file, path)?.validate()?;
        return Ok((file, Some(geometry)));
    }
    let file = File::open(path)?;
    if !file.metadata()?.is_file() {
        return Err(Error::Unsupported(
            "source must be a disk, volume or regular image".into(),
        ));
    }
    Ok((file, None))
}
fn control(file: &File, code: u32, input: &[u8], size: usize) -> Result<Vec<u8>> {
    let mut out = vec![0u8; size];
    let mut returned = 0;
    // Valid, bounded input/output allocations remain live for this synchronous IOCTL.
    let ok = unsafe {
        DeviceIoControl(
            file.as_raw_handle(),
            code,
            if input.is_empty() {
                std::ptr::null()
            } else {
                input.as_ptr().cast()
            },
            input.len() as u32,
            out.as_mut_ptr().cast(),
            size as u32,
            &mut returned,
            std::ptr::null_mut(),
        )
    };
    if ok == 0 {
        let error = std::io::Error::last_os_error();
        return Err(
            std::io::Error::new(error.kind(), format!("disk query 0x{code:08x}: {error}")).into(),
        );
    }
    if returned as usize > out.len() {
        return Err(Error::Invalid("disk query exceeded buffer".into()));
    }
    out.truncate(returned as usize);
    Ok(out)
}
fn u32_at(bytes: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        bytes
            .get(at..at + 4)
            .ok_or_else(|| Error::Invalid("short disk query".into()))?
            .try_into()
            .unwrap(),
    ))
}
pub fn info(file: &File, _path: &Path) -> Result<DeviceInfo> {
    let number = control(file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], 12)?;
    if u32_at(&number, 0)? != 7 {
        return Err(Error::Unsupported("source is not a disk device".into()));
    }
    let length = control(file, IOCTL_DISK_GET_LENGTH_INFO, &[], 8)?;
    let bytes = u64::from_le_bytes(
        length
            .as_slice()
            .try_into()
            .map_err(|_| Error::Invalid("disk length query".into()))?,
    );
    // Extended geometry also requests partition metadata. A readable disk with
    // a damaged/uninitialized layout can reject that query (ERROR_IO_DEVICE).
    // Recovery needs the basic sector geometry and the independent length,
    // never a valid OS partition inventory as a prerequisite for opening it.
    let geometry = control(file, IOCTL_DISK_GET_DRIVE_GEOMETRY, &[], 24)?;
    let sector = u32_at(&geometry, 20)?;
    let serial = control(file, IOCTL_STORAGE_QUERY_PROPERTY, &[0; 12], 4096)
        .ok()
        .and_then(|v| {
            let offset = u32_at(&v, 24).ok()? as usize;
            if offset == 0 || offset >= v.len() {
                return None;
            }
            let bytes = &v[offset..v.len().min(offset + 256)];
            Some(
                String::from_utf8_lossy(
                    &bytes[..bytes.iter().position(|b| *b == 0).unwrap_or(bytes.len())],
                )
                .trim()
                .to_owned(),
            )
        })
        .unwrap_or_default();
    Ok(DeviceInfo {
        bytes,
        sector,
        identity: format!(
            "windows:{}:{}:{serial}",
            u32_at(&number, 4)?,
            u32_at(&number, 8)?
        ),
    })
}
pub fn source_disks(path: &Path) -> Result<BTreeSet<String>> {
    let path = canonical(path)?;
    let file = topology_file(&path)?;
    if path.to_string_lossy().starts_with(r"\\.\PhysicalDrive") {
        let value = control(&file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], 12)?;
        return Ok(BTreeSet::from([format!(
            "windows-disk-{}",
            u32_at(&value, 4)?
        )]));
    }
    let value = control(&file, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], 65536)?;
    let count = u32_at(&value, 0)? as usize;
    if count == 0 || count > 1024 || 8 + count * 24 > value.len() {
        return Err(Error::UnsafeDestination(
            "invalid volume disk extents".into(),
        ));
    }
    (0..count)
        .map(|i| Ok(format!("windows-disk-{}", u32_at(&value, 8 + i * 24)?)))
        .collect()
}
pub fn output_disks(path: &Path) -> Result<BTreeSet<String>> {
    let text: Vec<u16> = path.as_os_str().encode_wide().chain(Some(0)).collect();
    let mut mount = vec![0u16; 32768];
    let mut volume = vec![0u16; 32768];
    // Buffers are UTF-16 and terminated; API lengths are their allocated element counts.
    if unsafe { GetVolumePathNameW(text.as_ptr(), mount.as_mut_ptr(), mount.len() as u32) } == 0
        || unsafe {
            GetVolumeNameForVolumeMountPointW(
                mount.as_ptr(),
                volume.as_mut_ptr(),
                volume.len() as u32,
            )
        } == 0
    {
        return Err(Error::UnsafeDestination(format!(
            "cannot identify output volume: {}",
            std::io::Error::last_os_error()
        )));
    }
    let end = volume
        .iter()
        .position(|c| *c == 0)
        .ok_or_else(|| Error::Invalid("unterminated volume path".into()))?;
    source_disks(Path::new(
        &String::from_utf16_lossy(&volume[..end]).trim_end_matches('\\'),
    ))
}
pub fn mounted_root(path: &Path, region: Option<&crate::SourceRegion>) -> Result<Option<PathBuf>> {
    use windows_sys::Win32::{
        Foundation::INVALID_HANDLE_VALUE,
        Storage::FileSystem::{
            FindFirstVolumeW, FindNextVolumeW, FindVolumeClose, GetVolumePathNamesForVolumeNameW,
        },
    };
    let source = canonical(path)?;
    let source_file = topology_file(&source)?;
    let disk = if region.is_some() {
        Some(u32_at(
            &control(&source_file, IOCTL_STORAGE_GET_DEVICE_NUMBER, &[], 12)?,
            4,
        )?)
    } else {
        None
    };
    let extents = if region.is_none() {
        Some(control(
            &source_file,
            IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS,
            &[],
            65536,
        )?)
    } else {
        None
    };
    let mut name = vec![0u16; 32768];
    let handle = unsafe { FindFirstVolumeW(name.as_mut_ptr(), name.len() as u32) };
    if handle == INVALID_HANDLE_VALUE {
        return Err(std::io::Error::last_os_error().into());
    }
    struct Volumes(windows_sys::Win32::Foundation::HANDLE);
    impl Drop for Volumes {
        fn drop(&mut self) {
            unsafe {
                FindVolumeClose(self.0);
            }
        }
    }
    let _volumes = Volumes(handle);
    loop {
        let end = name
            .iter()
            .position(|c| *c == 0)
            .ok_or_else(|| Error::Invalid("volume name".into()))?;
        let text = String::from_utf16_lossy(&name[..end]);
        let matches = (|| -> Result<bool> {
            let file = topology_file(Path::new(text.trim_end_matches('\\')))?;
            let value = control(&file, IOCTL_VOLUME_GET_VOLUME_DISK_EXTENTS, &[], 65536)?;
            if let Some(region) = region {
                if value.len() < 32 || u32_at(&value, 0)? != 1 || Some(u32_at(&value, 8)?) != disk {
                    return Ok(false);
                }
                let offset = u64::from_le_bytes(value[16..24].try_into().unwrap());
                let length = u64::from_le_bytes(value[24..32].try_into().unwrap());
                Ok(offset == region.offset && length == region.length)
            } else {
                Ok(extents.as_ref() == Some(&value))
            }
        })()
        .unwrap_or(false);
        if matches {
            let mut paths = vec![0u16; 32768];
            let mut length = 0;
            if unsafe {
                GetVolumePathNamesForVolumeNameW(
                    name.as_ptr(),
                    paths.as_mut_ptr(),
                    paths.len() as u32,
                    &mut length,
                )
            } != 0
            {
                let end = paths.iter().position(|c| *c == 0).unwrap_or(0);
                if end > 0 {
                    return Ok(Some(PathBuf::from(String::from_utf16_lossy(&paths[..end]))));
                }
            }
        }
        if unsafe { FindNextVolumeW(handle, name.as_mut_ptr(), name.len() as u32) } == 0 {
            break;
        }
    }
    Ok(None)
}
pub fn list() -> Result<Vec<DiskSource>> {
    // No source path or user text is interpolated into this fixed inventory query.
    let script = r#"[Console]::OutputEncoding=[System.Text.UTF8Encoding]::new($false); $d=@(Get-CimInstance Win32_DiskDrive -ErrorAction Stop | ForEach-Object {[pscustomobject]@{path=$_.DeviceID;name=$_.Model;kind='disk';bytes=[uint64]$_.Size;sector_size=[uint32]$_.BytesPerSector;read_only=$false;mounts=@()}}); $v=@(Get-CimInstance Win32_LogicalDisk -Filter 'DriveType=2 OR DriveType=3' | ForEach-Object {[pscustomobject]@{path=('\\.\'+$_.DeviceID);name=($_.DeviceID+' '+$_.VolumeName);kind='volume';bytes=[uint64]$_.Size;sector_size=[uint32]512;read_only=$false;mounts=@($_.DeviceID+'\')}}); ConvertTo-Json -InputObject @($d+$v) -Depth 4 -Compress"#;
    let result = Command::new("powershell.exe")
        .args([
            "-NoLogo",
            "-NoProfile",
            "-NonInteractive",
            "-Command",
            script,
        ])
        .creation_flags(0x08000000)
        .output()?;
    if !result.status.success() {
        return Err(Error::Request(format!(
            "cannot enumerate disks: {}",
            String::from_utf8_lossy(&result.stderr)
        )));
    }
    if result.stdout.len() > 4 * 1024 * 1024 {
        return Err(Error::Limit("disk inventory".into()));
    }
    let mut disks: Vec<DiskSource> = serde_json::from_slice(&result.stdout)?;
    disks.retain(|d| d.bytes > 0 && is_device(&d.path));
    for d in &mut disks {
        d.path = canonical(&d.path)?;
    }
    disks.truncate(512);
    Ok(disks)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn device_names_are_narrowly_recognized() {
        assert_eq!(
            canonical(Path::new("c:")).unwrap(),
            PathBuf::from(r"\\.\C:")
        );
        assert_eq!(
            canonical(Path::new(r"\\.\physicaldrive012")).unwrap(),
            PathBuf::from(r"\\.\PhysicalDrive12")
        );
        for path in [
            r"\\.\NUL",
            r"\\.\PhysicalDrive-1",
            r"\\.\PhysicalDrive0\file",
            r"\\?\GLOBALROOT\Device\Harddisk0",
        ] {
            assert!(!is_device(Path::new(path)));
        }
    }
}
