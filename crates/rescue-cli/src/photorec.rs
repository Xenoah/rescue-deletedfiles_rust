//! Optional broad-format whole-source carving with the installed PhotoRec reader.
use rescue_core::disk_image::{executable_in_path, single_raw_source};
use rescue_core::{Error, Result};
use serde_json::json;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{BufWriter, Read, Write},
    path::{Path, PathBuf},
    process::{Command, Stdio},
    sync::atomic::{AtomicBool, Ordering},
    time::Duration,
};

#[derive(clap::Args)]
pub(super) struct Args {
    /// A read-only disk, partition or regular RAW image. The entire source is searched.
    #[arg(long)]
    source: PathBuf,
    /// Directory for a separate result subfolder, files and SHA-256 manifest.
    #[arg(long)]
    destination: PathBuf,
    /// Enable every family offered by this PhotoRec build (including uncommon signatures).
    #[arg(long)]
    all_families: bool,
    /// Search alignment in bytes. Omit to let PhotoRec determine the block size.
    #[arg(long,value_parser=clap::value_parser!(u32).range(512..=1048576))]
    block_size: Option<u32>,
    #[arg(long)]
    json: bool,
}

fn stopped(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn carve(
    program: &Path,
    source: &Path,
    root: &Path,
    args: &Args,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut directives = String::from("partition_none,");
    if let Some(size) = args.block_size {
        directives.push_str(&format!("blocksize,{size},"));
    }
    if args.all_families {
        directives.push_str("fileopt,everything,enable,");
    }
    directives.push_str("search");
    let mut child = Command::new(program)
        .current_dir(root)
        .args(["/log", "/d"])
        .arg(root.join("files"))
        .arg("/cmd")
        .arg(source)
        .arg(directives)
        .env("LC_ALL", "C")
        .env("TERM", "dumb")
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn()?;
    loop {
        let status = stopped(cancel).and_then(|_| Ok(child.try_wait()?));
        match status {
            Ok(Some(status)) if status.success() => break,
            Ok(Some(status)) => {
                return Err(Error::Invalid(format!("PhotoRec exited with {status}")));
            }
            Ok(None) => std::thread::sleep(Duration::from_millis(50)),
            Err(error) => {
                let _ = child.kill();
                let _ = child.wait();
                return Err(error);
            }
        }
    }
    // The interactive tool can exit with status zero after abandoning/rescheduling a scan.
    // Its session file and final log line distinguish that from an exhausted search.
    use std::io::{Seek, SeekFrom};
    let mut log = File::open(root.join("photorec.log"))
        .map_err(|_| Error::Invalid("PhotoRec did not produce a completion log".into()))?;
    let length = log.metadata()?.len();
    log.seek(SeekFrom::Start(length.saturating_sub(4096)))?;
    let mut tail = Vec::new();
    log.take(4096).read_to_end(&mut tail)?;
    if root.join("photorec.ses").try_exists()?
        || !String::from_utf8_lossy(&tail)
            .trim_end()
            .ends_with("PhotoRec exited normally.")
    {
        return Err(Error::Invalid(
            "PhotoRec did not finish the requested scan".into(),
        ));
    }
    Ok(())
}

fn manifest(root: &Path, cancel: &AtomicBool) -> Result<(u64, u64)> {
    let mut count = 0u64;
    let mut total = 0u64;
    let mut out = BufWriter::new(File::create_new(root.join("manifest.jsonl"))?);
    let mut buf = vec![0; 128 * 1024];
    for entry in fs::read_dir(root)? {
        stopped(cancel)?;
        let entry = entry?;
        let name = entry.file_name();
        if !name
            .to_str()
            .and_then(|s| s.strip_prefix("files."))
            .is_some_and(|n| !n.is_empty() && n.bytes().all(|b| b.is_ascii_digit()))
        {
            continue;
        }
        if !entry.file_type()?.is_dir() {
            return Err(Error::Invalid(
                "unexpected PhotoRec output directory type".into(),
            ));
        }
        for file in fs::read_dir(entry.path())? {
            let file = file?;
            if !file.file_type()?.is_file() {
                return Err(Error::Invalid(
                    "unexpected PhotoRec output entry type".into(),
                ));
            }
            if file.file_name() == "report.xml" {
                continue;
            }
            let mut reader = File::open(file.path())?;
            let mut digest = Sha256::new();
            let mut bytes = 0u64;
            loop {
                stopped(cancel)?;
                let n = reader.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                digest.update(&buf[..n]);
                bytes += n as u64;
            }
            let path = PathBuf::from(&name).join(file.file_name());
            let mut row = json!({"path":path.to_string_lossy(),"bytes":bytes,"sha256":format!("{:x}",digest.finalize())});
            if path.to_str().is_none() {
                #[cfg(unix)]
                {
                    use std::os::unix::ffi::OsStrExt;
                    row["path_unix_bytes"] = json!(path.as_os_str().as_bytes());
                }
                #[cfg(windows)]
                {
                    use std::os::windows::ffi::OsStrExt;
                    row["path_utf16"] = json!(path.as_os_str().encode_wide().collect::<Vec<_>>());
                }
            }
            serde_json::to_writer(&mut out, &row)?;
            out.write_all(b"\n")?;
            total = total
                .checked_add(bytes)
                .ok_or_else(|| Error::Limit("PhotoRec output byte count".into()))?;
            count += 1;
        }
    }
    out.flush()?;
    out.get_ref().sync_all()?;
    Ok((count, total))
}

pub(super) fn run(args: Args, cancel: &AtomicBool) -> Result<i32> {
    if args.block_size.is_some_and(|n| !n.is_power_of_two()) {
        return Err(Error::Request(
            "PhotoRec block size must be a power of two".into(),
        ));
    }
    let program = executable_in_path("photorec")
        .or_else(|| {
            if cfg!(windows) {
                executable_in_path("photorec_win")
            } else {
                None
            }
        })
        .ok_or_else(|| {
            Error::Unsupported("install PhotoRec (TestDisk) and put its executable on PATH".into())
        })?;
    let source = single_raw_source(&args.source, &args.destination)?;
    let regular = !rescue_core::is_device(&source);
    let before = if regular {
        let m = fs::metadata(&source)?;
        Some((m.len(), m.modified()?))
    } else {
        None
    };
    stopped(cancel)?;
    fs::create_dir_all(&args.destination)?;
    let dest = args.destination.canonicalize()?;
    let working = tempfile::Builder::new()
        .prefix("rescue-photorec-")
        .tempdir_in(&dest)?;
    carve(&program, &source, working.path(), &args, cancel)?;
    let (files, bytes) = manifest(working.path(), cancel)?;
    if let Some((length, time)) = before {
        let after = fs::metadata(&source)?;
        if after.len() != length || after.modified()? != time {
            return Err(Error::SourceChanged);
        }
    }
    stopped(cancel)?;
    let path = working.keep();
    super::print(
        json!({"source":source.to_string_lossy(),"directory":path.to_string_lossy(),"manifest":path.join("manifest.jsonl").to_string_lossy(),
        "files":files,"bytes":bytes,"reader":program.to_string_lossy(),"all_families":args.all_families,"block_size":args.block_size,
        "evidence":"PhotoRec signature estimates. The manifest hashes the emitted bytes. Deletion status, original names, file boundaries and original contents remain unverified."}),
        args.json,
    );
    Ok(0)
}
