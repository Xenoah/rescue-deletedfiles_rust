//! Read standalone disk containers and external forensic images into logical RAW files.
mod lvm;
pub use lvm::{VolumeInfo as LvmVolumeInfo, import_lvm_volume, list_lvm_volumes};
mod split_raw;
pub use split_raw::{RawExpectation, RawSegment, RawVerification};
mod ddrescue;
pub use ddrescue::{DdrescueLayout, DdrescueVerification, import_ddrescue};
mod android;
pub use android::{AndroidVerification, import_android};
mod apfs;
mod btrfs;
pub use apfs::{ApfsRequest, apfs};
pub use btrfs::{BtrfsRequest, btrfs};
mod compressed_raw;
mod ewf;
mod ext4_journal;
pub(crate) use ext4_journal::Volume as Ext4Volume;
mod extents;
pub use ext4_journal::{Ext4JournalRequest, ext4_journal};
mod fingerprint;
pub use extents::recover_extents;
pub use fingerprint::{FingerprintedFile, MatchedFile, fingerprint_file, recover_fingerprinted};
mod whole_hash;
pub use whole_hash::{
    DEFAULT_HASH_WORK, HashExtent, HashMatch, HashSearch, MatchedStride, StrideSearch,
    StrideSpacing, recover_by_hash, recover_by_stride,
};
mod udif;
mod virtual_image;
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    process::{ChildStdout, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};
pub use virtual_image::{Qcow2Snapshot, Qcow2SnapshotSelector, list_qcow2_snapshots};

/// Logical RAW file or recovered inode that has been published without overwriting an existing file.
#[derive(Debug, serde::Serialize)]
pub struct ImportedFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub ewf: Option<EwfVerification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub android_sparse: Option<AndroidVerification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub raw: Option<RawVerification>,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ddrescue: Option<DdrescueVerification>,
}

#[derive(Debug, serde::Serialize)]
pub struct EwfVerification {
    pub reader: &'static str,
    pub logical_sha256_verified: bool,
}

#[derive(Debug, Clone)]
pub struct ImageInput {
    /// Read-only disk, volume, or first image segment.
    pub source: PathBuf,
    /// Additional image segments in logical order, named VMDK extent files, or explicit VHD or QCOW2 backing files from nearest parent to base.
    pub segment: Vec<PathBuf>,
    /// Image type such as raw, ewf, vhd, vhdx or vmdk. import-image also accepts qed, vdi, qcow1, qcow2, dmg, cd-raw and compressed-raw. Default: auto-detect.
    pub image_type: Option<String>,
    /// Logical sector size in bytes; default is supplied by the reader.
    pub sector_size: Option<u32>,
}

impl ImageInput {
    fn arguments(&self, destination: Option<&Path>) -> Result<(Vec<OsString>, Vec<OsString>)> {
        let mut options = Vec::new();
        if let Some(kind) = &self.image_type {
            // Sleuth Kit names the reader for both Microsoft container versions "vhd".
            let driver = match kind.as_str() {
                "vhdx" => "vhd",
                "e01" | "ewf-verified" => "ewf",
                v => v,
            };
            options.extend([OsString::from("-i"), driver.into()]);
        }
        if let Some(size) = self.sector_size {
            if !(512..=65536).contains(&size) || !size.is_power_of_two() {
                return Err(Error::Request("sector size must be a power of two".into()));
            }
            options.extend([OsString::from("-b"), size.to_string().into()]);
        }
        let mut paths = Vec::new();
        let mut seen = std::collections::HashSet::new();
        for path in std::iter::once(&self.source).chain(&self.segment) {
            let path = crate::canonical_source_path(path)?;
            if crate::is_device(&path) {
                if !self.segment.is_empty()
                    || self.image_type.as_deref().is_some_and(|t| t != "raw")
                {
                    return Err(Error::Request(
                        "native disks require a single raw source".into(),
                    ));
                }
                if let Some(output) = destination {
                    crate::guard_output_path(&path, output)?;
                }
                if self.image_type.is_none() {
                    options.extend([OsString::from("-i"), "raw".into()]);
                }
                paths.push(path.into_os_string());
                continue;
            }
            let mut map = path.as_os_str().to_owned();
            map.push(".rmap");
            if !path.is_file()
                || crate::is_raid_path(&path)
                || Path::new(&map).try_exists()?
                || path
                    .file_name()
                    .is_some_and(|n| n.to_string_lossy().ends_with(".rescue.raw"))
            {
                return Err(Error::Request("choose a native disk or regular image without a Rescue read map or RAID manifest".into()));
            }
            let name = path.into_os_string();
            if !seen.insert(name.clone()) {
                return Err(Error::Request("duplicate disk image segment".into()));
            }
            paths.push(name);
        }
        Ok((options, paths))
    }
}

pub fn executable_in_path(name: &str) -> Option<PathBuf> {
    let executable = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    std::env::split_paths(&std::env::var_os("PATH")?)
        .filter(|p| p.is_absolute())
        .map(|p| p.join(&executable))
        .find(|p| p.is_file())
}

pub fn single_raw_source(source: &Path, destination: &Path) -> Result<PathBuf> {
    let raw = ImageInput {
        source: source.to_owned(),
        segment: vec![],
        image_type: Some("raw".into()),
        sector_size: None,
    };
    let (_, paths) = raw.arguments(Some(destination))?;
    Ok(PathBuf::from(&paths[0]))
}

// Consume the child's stdout concurrently with waiting, so cancellation also
// interrupts a reader blocked on a slow input. stderr stays out of binary output.
fn read_tool<T>(
    name: &str,
    args: &[OsString],
    cancel: &AtomicBool,
    consume: impl FnOnce(&mut ChildStdout) -> Result<T>,
) -> Result<T> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let program = executable_in_path(name).ok_or_else(|| {
        let package = if name.starts_with("ewf") {
            "libewf tools"
        } else if name == "xorriso" {
            "xorriso"
        } else {
            "The Sleuth Kit"
        };
        Error::Unsupported(format!("install {package} and put {name} on PATH"))
    })?;
    let mut command = Command::new(program);
    if name.starts_with("ewf") {
        command.env("LC_ALL", "C");
    }
    let mut child = command
        .args(args)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .spawn()?;
    let mut stdout = child.stdout.take().expect("piped stdout");
    let stop = AtomicBool::new(false);
    std::thread::scope(|scope| {
        let wait = scope.spawn(|| -> io::Result<_> {
            loop {
                if cancel.load(Ordering::Relaxed) || stop.load(Ordering::Relaxed) {
                    let _ = child.kill();
                    return child.wait();
                }
                if let Some(status) = child.try_wait()? {
                    return Ok(status);
                }
                std::thread::sleep(Duration::from_millis(20));
            }
        });
        let result = consume(&mut stdout);
        drop(stdout);
        stop.store(result.is_err(), Ordering::Relaxed);
        let status = wait.join().expect("child wait thread")?;
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let value = result?;
        if !status.success() {
            return Err(Error::Invalid(format!(
                "{name} exited with {status}; see reader diagnostic"
            )));
        }
        Ok(value)
    })
}

fn write_output(
    tool: &str,
    args: &[OsString],
    output: &Path,
    expected: RawExpectation,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    publish_output(output, cancel, |file| {
        let mut digest = Sha256::new();
        let bytes = read_tool(tool, args, cancel, |reader| {
            let mut buf = [0; 64 * 1024];
            let mut bytes = 0u64;
            loop {
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                bytes += n as u64;
                if expected.bytes.is_some_and(|size| bytes > size) {
                    return Err(Error::Invalid("output exceeds the recorded length".into()));
                }
                file.write_all(&buf[..n])?;
                digest.update(&buf[..n]);
            }
            Ok(bytes)
        })?;
        if expected.bytes.is_some_and(|size| bytes != size)
            || (expected.bytes.is_none() && bytes == 0)
        {
            return Err(Error::Invalid(
                "reader returned an incomplete output".into(),
            ));
        }
        split_raw::verify_digest(&digest, expected.expected_digest(bytes)?)?;
        Ok((bytes, digest))
    })
}

// Called only on the new, empty temporary output; callers set its final logical length.
fn write_sparse_chunk(output: &mut fs::File, bytes: &[u8]) -> Result<()> {
    if bytes.iter().all(|&b| b == 0) {
        output.seek(SeekFrom::Current(bytes.len() as i64))?;
    } else {
        output.write_all(bytes)?;
    }
    Ok(())
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn publish_directory(staged: &Path, output: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, staged, CWD, output, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(windows)]
fn publish_directory(staged: &Path, output: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in output path",
            ));
        }
        value.push(0);
        Ok(value)
    }
    let from = wide(staged)?;
    let to = wide(output)?;
    // Both paths are owned, NUL-terminated UTF-16. No replace or cross-volume copy flag.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn publish_directory(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic directory publication unavailable on this platform",
    ))
}

fn publish_output(
    output: &Path,
    cancel: &AtomicBool,
    write: impl FnOnce(&mut fs::File) -> Result<(u64, Sha256)>,
) -> Result<ImportedFile> {
    if output.try_exists()? {
        return Err(Error::Request(format!(
            "output exists: {}",
            output.display()
        )));
    }
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let mut temp = tempfile::NamedTempFile::new_in(parent)?;
    let (bytes, digest) = write(temp.as_file_mut())?;
    temp.as_file().sync_all()?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    temp.persist_noclobber(output).map_err(|e| e.error)?;
    Ok(ImportedFile {
        path: output.to_owned(),
        bytes,
        sha256: format!("{:x}", digest.finalize()),
        ewf: None,
        android_sparse: None,
        ddrescue: None,
        raw: None,
    })
}

pub fn import(image: &ImageInput, output: &Path, cancel: &AtomicBool) -> Result<ImportedFile> {
    import_with_raw_expectation(image, output, RawExpectation::default(), cancel)
}

pub fn import_with_raw_expectation(
    image: &ImageInput,
    output: &Path,
    expected: RawExpectation<'_>,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    import_with_snapshot(image, output, expected, None, cancel)
}

pub fn import_with_snapshot(
    image: &ImageInput,
    output: &Path,
    expected: RawExpectation<'_>,
    selector: Option<Qcow2SnapshotSelector<'_>>,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    let (mut args, paths) = image.arguments(Some(output))?;
    let first = Path::new(&paths[0]);
    let kind = image.image_type.as_deref();
    if kind == Some("lvm2") {
        return Err(Error::Request("select a logical volume with --lvm-volume; lvm-volumes lists available names and UUIDs".into()));
    }
    let regular_raw = !crate::is_device(first);
    if (expected.sha256.is_some() || expected.bytes.is_some())
        && (!matches!(
            kind,
            Some("raw" | "qed" | "vdi" | "qcow" | "qcow1" | "qcow2" | "vhd" | "vhdx" | "vmdk")
        ) || !regular_raw)
    {
        return Err(Error::Request(
            "expected logical length and SHA-256 require an explicit RAW, QED, VDI, QCOW, VHD, VHDX or VMDK image type and regular files".into(),
        ));
    }
    if selector.is_some() {
        if paths.len() != 1 || !regular_raw || !matches!(kind, None | Some("qcow" | "qcow2")) {
            return Err(Error::Request(
                "select an internal snapshot from a single QCOW2 file".into(),
            ));
        }
        return virtual_image::import(first, Some("qcow2"), selector, output, expected, cancel)?
            .ok_or_else(|| Error::Unsupported("a QCOW2 image is required".into()));
    }
    if regular_raw {
        if kind == Some("raw") {
            return split_raw::import(&paths, output, expected, cancel);
        }
        if kind == Some("android-sparse") || (kind.is_none() && android::has_magic(first)?) {
            return Err(Error::Request("use import-android with a new destination directory to preserve unrecorded Android sparse ranges".into()));
        }
        if let Some(result) = ewf::import(&paths, kind, output, cancel)? {
            return Ok(result);
        }
        if paths.len() > 1
            && let Some(result) =
                virtual_image::import_chain(&paths, kind, output, expected, cancel)?
        {
            return Ok(result);
        }
        if paths.len() > 1
            && let Some(result) =
                virtual_image::import_vhd_chain(&paths, kind, output, expected, cancel)?
        {
            return Ok(result);
        }
        if paths.len() > 1
            && let Some(result) =
                virtual_image::import_vhdx_chain(&paths, kind, output, expected, cancel)?
        {
            return Ok(result);
        }
        if let Some(result) =
            virtual_image::import_segments(&paths, kind, output, expected, cancel)?
        {
            return Ok(result);
        }
        if paths.len() > 1
            && let Some(result) =
                virtual_image::import_vdi_chain(&paths, kind, output, expected, cancel)?
        {
            return Ok(result);
        }
        if paths.len() == 1
            && let Some(result) =
                virtual_image::import(first, kind, None, output, expected, cancel)?
        {
            return Ok(result);
        }
        if let Some(result) = virtual_image::import_qed(&paths, kind, output, expected, cancel)? {
            return Ok(result);
        }
        if paths.len() == 1 {
            if let Some(result) = udif::import(first, kind, output, cancel)? {
                return Ok(result);
            }
            if let Some(result) = optical::import(first, kind, output, cancel)? {
                return Ok(result);
            }
            // A UDIF data fork can itself begin with gzip, bzip2 or XZ bytes.
            if let Some(result) = compressed_raw::import(first, kind, output, cancel)? {
                return Ok(result);
            }
        }
    }
    if regular_raw && kind.is_none() && split_raw::numbered_start(first) {
        return split_raw::import(&paths, output, expected, cancel);
    }
    if matches!(
        image.image_type.as_deref(),
        Some(
            "cd-raw"
                | "compressed-raw"
                | "udif"
                | "dmg"
                | "qcow"
                | "qcow1"
                | "qcow2"
                | "vdi"
                | "vhd"
        )
    ) {
        return Err(Error::Unsupported(
            "this image format requires a single standalone file".into(),
        ));
    }
    args.extend(paths);
    write_output("img_cat", &args, output, expected, cancel)
}

mod filesystem;
mod iso_sessions;
mod optical;
pub use filesystem::{FilesystemRequest, filesystem};
pub use iso_sessions::{IsoRequest, iso_sessions};
