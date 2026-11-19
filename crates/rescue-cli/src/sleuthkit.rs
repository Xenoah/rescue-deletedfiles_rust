//! Optional local Sleuth Kit readers. Rescue owns output naming and publication.
use clap::Args;
use rescue_core::{Error, Result};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    ffi::OsString,
    fs,
    io::{self, Read, Write},
    path::{Path, PathBuf},
    process::{ChildStdout, Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(Args)]
pub struct ImageArgs {
    /// Read-only disk, volume, or first image segment.
    #[arg(long)]
    source: PathBuf,
    /// Additional image segments in logical order, for explicitly split images.
    #[arg(long)]
    segment: Vec<PathBuf>,
    /// Sleuth Kit image type, such as raw, ewf, vhd or vmdk. import-image also accepts standalone vdi and qcow2. Default: auto-detect.
    #[arg(long)]
    image_type: Option<String>,
    /// Logical sector size in bytes; default is supplied by the reader.
    #[arg(long, value_parser = clap::value_parser!(u32).range(512..=65536))]
    sector_size: Option<u32>,
}

impl ImageArgs {
    fn arguments(&self, destination: Option<&Path>) -> Result<(Vec<OsString>, Vec<OsString>)> {
        let mut options = Vec::new();
        if let Some(kind) = &self.image_type {
            options.extend([OsString::from("-i"), kind.into()]);
        }
        if let Some(size) = self.sector_size {
            if !size.is_power_of_two() {
                return Err(Error::Request("sector size must be a power of two".into()));
            }
            options.extend([OsString::from("-b"), size.to_string().into()]);
        }
        let mut paths = Vec::new();
        for path in std::iter::once(&self.source).chain(&self.segment) {
            let path = rescue_core::canonical_source_path(path)?;
            if rescue_core::is_device(&path) {
                if !self.segment.is_empty()
                    || self.image_type.as_deref().is_some_and(|t| t != "raw")
                {
                    return Err(Error::Request(
                        "native disks require a single raw source".into(),
                    ));
                }
                if let Some(output) = destination {
                    rescue_core::guard_output_path(&path, output)?;
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
                || rescue_core::is_raid_path(&path)
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

#[derive(Args)]
pub struct FilesystemArgs {
    #[command(flatten)]
    image: ImageArgs,
    /// File system type, such as ext4, hfs or ufs. Default: auto-detect.
    #[arg(long)]
    fs_type: Option<String>,
    /// Starting sector of the volume within a whole disk image.
    #[arg(long, default_value_t = 0)]
    sector_offset: u64,
    /// Restore selected file IDs from the listing.
    #[arg(long, requires = "destination", conflicts_with = "all")]
    id: Vec<String>,
    /// Restore every listed deleted regular file.
    #[arg(long, requires = "destination")]
    all: bool,
    /// Existing output files are preserved. Omit to list deleted metadata.
    #[arg(long)]
    destination: Option<PathBuf>,
    #[arg(long)]
    json: bool,
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
    let executable = format!("{name}{}", std::env::consts::EXE_SUFFIX);
    let program = std::env::var_os("PATH")
        .and_then(|path| {
            std::env::split_paths(&path)
                .filter(|p| p.is_absolute())
                .map(|p| p.join(&executable))
                .find(|p| p.is_file())
        })
        .ok_or_else(|| {
            Error::Unsupported(format!(
                "install The Sleuth Kit and put {executable} on PATH"
            ))
        })?;
    let mut child = Command::new(program)
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
) -> Result<Value> {
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

pub(super) fn publish_output(
    output: &Path,
    cancel: &AtomicBool,
    write: impl FnOnce(&mut fs::File) -> Result<(u64, Sha256)>,
) -> Result<Value> {
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
    Ok(json!({"path":output,"bytes":bytes,"sha256":format!("{:x}",digest.finalize())}))
}

pub fn import(image: ImageArgs, output: &Path, json: bool, cancel: &AtomicBool) -> Result<i32> {
    let (mut args, paths) = image.arguments(Some(output))?;
    if paths.len() == 1
        && !rescue_core::is_device(Path::new(&paths[0]))
        && let Some(result) = super::virtual_image::import(
            Path::new(&paths[0]),
            image.image_type.as_deref(),
            output,
            cancel,
        )?
    {
        super::print(result, json);
        return Ok(0);
    }
    args.extend(paths);
    let result = write_output("img_cat", &args, output, None, cancel)?;
    super::print(result, json);
    Ok(0)
}

struct Entry {
    path: String,
    size: u64,
}

// fls -dFrpl: metadata identity, tab, full name, four times, size, gid and uid.
// Only an unallocated regular entry with an unreallocated regular inode qualifies.
fn parse_entry(line: &str) -> Option<(&str, Entry)> {
    let (header, row) = line.split_once(":\t")?;
    let id = header.strip_prefix("r/r * ")?;
    if id.len() > 64
        || !id
            .split('-')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    let mut columns = row.rsplitn(8, '\t');
    let size = columns.nth(2)?.parse().ok()?;
    let path = columns.nth(4)?.to_owned();
    Some((id, Entry { path, size }))
}

pub fn filesystem(request: FilesystemArgs, cancel: &AtomicBool) -> Result<i32> {
    if request.destination.is_some() && !request.all && request.id.is_empty() {
        return Err(Error::Request(
            "choose --id or --all when specifying a destination".into(),
        ));
    }
    let (mut args, paths) = request.image.arguments(request.destination.as_deref())?;
    if let Some(kind) = request.fs_type {
        args.extend([OsString::from("-f"), kind.into()]);
    }
    args.extend([
        OsString::from("-o"),
        request.sector_offset.to_string().into(),
    ]);
    args.extend(paths);
    let mut listing = vec![OsString::from("-dFrpl")];
    listing.extend_from_slice(&args);
    let data = read_tool("fls", &listing, cancel, |stdout| {
        let mut bytes = Vec::new();
        const LIMIT: u64 = 32 * 1024 * 1024;
        stdout.take(LIMIT + 1).read_to_end(&mut bytes)?;
        if bytes.len() as u64 > LIMIT {
            return Err(Error::Limit("filesystem listing exceeds 32 MiB".into()));
        }
        Ok(bytes)
    })?;
    let mut files = BTreeMap::new();
    let mut skipped = 0usize;
    let listing = String::from_utf8_lossy(&data);
    for line in listing.lines() {
        if let Some((id, entry)) = parse_entry(line) {
            files.entry(id).or_insert(entry);
        } else {
            skipped += 1;
        }
    }
    let warning = "Deleted directory entries may refer to reused blocks. A matching length or output hash does not establish the original content.";
    let Some(dest) = request.destination else {
        let entries: Vec<_> = files
            .iter()
            .map(|(id, e)| json!({"id":id,"original_path":e.path,"size":e.size}))
            .collect();
        super::print(
            json!({"candidates":entries,"skipped_rows":skipped,"warning":warning}),
            request.json,
        );
        return Ok(0);
    };
    let ids: std::collections::BTreeSet<_> = if request.all {
        files.keys().copied().collect()
    } else {
        request.id.iter().map(String::as_str).collect()
    };
    if let Some(id) = ids.iter().find(|id| !files.contains_key(**id)) {
        return Err(Error::Request(format!(
            "deleted regular file ID not found: {id}"
        )));
    }
    fs::create_dir_all(&dest)?;
    let mut results = Vec::new();
    let mut failed = false;
    let mut extract = vec![OsString::from("-r")];
    extract.extend(args);
    for id in ids {
        let entry = &files[id];
        let mut output = dest.join(format!("inode-{id}"));
        let ext = rescue_core::file_extension(&entry.path, None);
        if !ext.is_empty() && ext.len() <= 32 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
            output.set_extension(ext);
        }
        extract.push(id.into());
        let mut row = match write_output("icat", &extract, &output, Some(entry.size), cancel) {
            Ok(v) => v,
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) => {
                failed = true;
                json!({"error":e.to_string()})
            }
        };
        row["id"] = json!(id);
        row["original_path"] = json!(entry.path);
        results.push(row);
        extract.pop();
    }
    super::print(
        json!({"results":results,"skipped_rows":skipped,"warning":warning}),
        request.json,
    );
    Ok(if failed { 5 } else { 0 })
}
