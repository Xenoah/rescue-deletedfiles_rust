//! Read standalone disk containers and external forensic images into logical RAW files.
mod ewf;
mod extents;
pub use extents::recover_extents;
mod udif;
mod virtual_image;
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStdout, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

/// Logical RAW file or recovered inode that has been published without overwriting an existing file.
#[derive(Debug, serde::Serialize)]
pub struct ImportedFile {
    pub path: PathBuf,
    pub bytes: u64,
    pub sha256: String,
    #[serde(flatten, skip_serializing_if = "Option::is_none")]
    pub ewf: Option<EwfVerification>,
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
    /// Additional image segments in logical order, for explicitly split images.
    pub segment: Vec<PathBuf>,
    /// Image type such as raw, ewf, vhd, vhdx or vmdk. import-image also accepts standalone vdi and qcow2. Default: auto-detect.
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
            if paths.contains(&name) {
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
    expected_size: Option<u64>,
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
                if expected_size.is_some_and(|size| bytes > size) {
                    return Err(Error::Invalid("output exceeds the recorded length".into()));
                }
                file.write_all(&buf[..n])?;
                digest.update(&buf[..n]);
            }
            Ok(bytes)
        })?;
        if expected_size.is_some_and(|size| bytes != size)
            || (expected_size.is_none() && bytes == 0)
        {
            return Err(Error::Invalid(
                "reader returned an incomplete output".into(),
            ));
        }
        Ok((bytes, digest))
    })
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
    })
}

pub fn import(image: &ImageInput, output: &Path, cancel: &AtomicBool) -> Result<ImportedFile> {
    let (mut args, paths) = image.arguments(Some(output))?;
    let first = Path::new(&paths[0]);
    let kind = image.image_type.as_deref();
    if !crate::is_device(first) {
        if let Some(result) = ewf::import(&paths, kind, output, cancel)? {
            return Ok(result);
        }
        if paths.len() == 1 {
            if let Some(result) = virtual_image::import(first, kind, output, cancel)? {
                return Ok(result);
            }
            if let Some(result) = udif::import(first, kind, output, cancel)? {
                return Ok(result);
            }
        }
    }
    if matches!(
        image.image_type.as_deref(),
        Some("udif" | "dmg" | "qcow2" | "vdi")
    ) {
        return Err(Error::Unsupported(
            "this image format requires a single standalone file".into(),
        ));
    }
    args.extend(paths);
    write_output("img_cat", &args, output, None, cancel)
}

mod filesystem;
pub use filesystem::{FilesystemRequest, filesystem};
