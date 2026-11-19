//! Use xorriso to read retained ISO 9660 sessions, including Rock Ridge names and filtered files.
use super::{ImageInput, publish_output, read_tool};
use crate::{Error, Result};
use serde::Serialize;
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::{OsStr, OsString},
    fs,
    io::{Read, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};
pub struct IsoRequest {
    pub source: PathBuf,
    pub session: Option<u32>,
    pub paths: Vec<String>,
    pub all: bool,
    pub destination: Option<PathBuf>,
}

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("ISO sessions: {reason}"))
}

struct Reader<'a> {
    source: &'a Path,
    initial: fs::Metadata,
    cancel: &'a AtomicBool,
}

impl Reader<'_> {
    fn unchanged(&self) -> Result<()> {
        let latest = fs::metadata(self.source)?;
        if latest.len() != self.initial.len() || latest.modified()? != self.initial.modified()? {
            return Err(Error::SourceChanged);
        }
        Ok(())
    }

    fn command(&self, session: Option<u32>, args: &[&OsStr]) -> Result<String> {
        self.unchanged()?;
        let mut cmd: Vec<OsString> = [
            "-no_rc",
            "-abort_on",
            "SORRY",
            "-return_with",
            "SORRY",
            "32",
            "-report_about",
            "SORRY",
            "-local_charset",
            "UTF-8",
            "-backslash_codes",
            "encode_results",
            "-iso_rr_pattern",
            "off",
            "-disk_pattern",
            "off",
        ]
        .into_iter()
        .map(Into::into)
        .collect();
        if let Some(n) = session {
            cmd.extend(["-load".into(), "session".into(), n.to_string().into()]);
        }
        let mut dev = OsString::from("stdio:");
        dev.push(self.source);
        cmd.extend(["-indev".into(), dev]);
        cmd.extend(args.iter().map(|a| a.to_os_string()));
        let text = read_tool("xorriso", &cmd, self.cancel, |stdout| {
            let limit = 32 << 20;
            let mut data = Vec::new();
            stdout.take(limit + 1).read_to_end(&mut data)?;
            if data.len() as u64 > limit {
                return Err(Error::Limit("ISO listing exceeds 32 MiB".into()));
            }
            String::from_utf8(data).map_err(|_| invalid("malformed xorriso result"))
        })?;
        self.unchanged()?;
        Ok(text)
    }

    fn current_paths(&self, session: u32) -> Result<BTreeSet<String>> {
        let args = ["-find", "/", "-exec", "echo", "--"].map(OsStr::new);
        let mut paths = BTreeSet::new();
        for line in self.command(Some(session), &args)?.lines() {
            if line == "'/'" {
                continue;
            }
            let quoted = line
                .strip_prefix('\'')
                .ok_or_else(|| invalid("missing quoted ISO path"))?;
            paths.insert(decode_path(quoted)?);
        }
        Ok(paths)
    }

    fn files(&self, session: u32) -> Result<BTreeMap<String, u64>> {
        let args = ["-find", "/", "-type", "f", "-exec", "lsdl", "--"].map(OsStr::new);
        let mut files = BTreeMap::new();
        for line in self.command(Some(session), &args)?.lines() {
            let (attrs, name) = line
                .split_once('\'')
                .ok_or_else(|| invalid("missing quoted ISO path"))?;
            let fields: Vec<_> = attrs.split_ascii_whitespace().collect();
            if fields.len() != 8 || !fields[0].starts_with('-') {
                return Err(invalid("unexpected regular-file listing"));
            }
            let bytes = fields[4]
                .parse()
                .map_err(|_| invalid("invalid logical file length"))?;
            let path = decode_path(name)?;
            if files.insert(path, bytes).is_some() {
                return Err(invalid("duplicate historical file path"));
            }
        }
        Ok(files)
    }

    fn extract(&self, n: u32, path: &str, size: u64, out: &Path) -> Result<super::ImportedFile> {
        // xorriso decodes the ISO's logical file (including supported zisofs data).
        // Only the controlled staging name reaches the host filesystem.
        publish_output(out, self.cancel, |target| {
            let stage = tempfile::tempdir_in(out.parent().unwrap_or(Path::new(".")))?;
            let item = stage.path().join("file");
            self.command(
                Some(n),
                &[
                    OsStr::new("-osirrox"),
                    OsStr::new("on"),
                    OsStr::new("-extract_single"),
                    OsStr::new(path),
                    item.as_os_str(),
                ],
            )?;
            let meta = fs::symlink_metadata(&item)?;
            if !meta.file_type().is_file() || meta.len() != size {
                return Err(invalid("reader produced an incomplete or non-regular file"));
            }
            // Archived Unix read permissions need not allow the user to open this private copy.
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                fs::set_permissions(&item, fs::Permissions::from_mode(0o600))?;
            }
            let mut input = fs::File::open(item)?;
            let mut digest = Sha256::new();
            let mut buf = vec![0; 256 << 10];
            let mut copied = 0u64;
            loop {
                if self.cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let n = input.read(&mut buf)?;
                if n == 0 {
                    break;
                }
                copied += n as u64;
                if copied > size {
                    return Err(invalid("excess logical file bytes"));
                }
                target.write_all(&buf[..n])?;
                digest.update(&buf[..n]);
            }
            if copied != size {
                return Err(invalid("short logical file"));
            }
            self.unchanged()?;
            Ok((copied, digest))
        })
    }
}

fn decode_path(quoted: &str) -> Result<String> {
    let literal = quoted
        .strip_suffix('\'')
        .ok_or_else(|| invalid("unterminated ISO path"))?
        .replace("'\"'\"'", "'");
    let mut iter = literal.bytes();
    let mut data = Vec::new();
    while let Some(b) = iter.next() {
        if b != b'\\' {
            data.push(b);
            continue;
        }
        let v = match iter.next() {
            Some(b'\\') => b'\\',
            Some(b'a') => 7,
            Some(b'b') => 8,
            Some(b't') => 9,
            Some(b'n') => 10,
            Some(b'v') => 11,
            Some(b'f') => 12,
            Some(b'r') => 13,
            Some(b'e') => 27,
            Some(a @ b'0'..=b'3') => {
                let mut oct = a - b'0';
                for _ in 0..2 {
                    let b = iter
                        .next()
                        .filter(|x| (b'0'..=b'7').contains(x))
                        .ok_or_else(|| invalid("invalid octal path escape"))?;
                    oct = oct * 8 + (b - b'0');
                }
                oct
            }
            _ => return Err(invalid("unrecognized ISO path escape")),
        };
        data.push(v);
    }
    let path = String::from_utf8(data).map_err(|_| invalid("ISO path is not UTF-8"))?;
    if !path.starts_with('/')
        || path.contains('\0')
        || path[1..].split('/').any(|c| matches!(c, "" | "." | ".."))
    {
        return Err(invalid("unsafe or incomplete ISO path"));
    }
    Ok(path)
}

#[derive(Serialize)]
struct Session {
    number: u32,
    start_block: u64,
    blocks: u64,
}

fn sessions(list: &str, len: u64) -> Result<Vec<Session>> {
    let mut sessions = Vec::new();
    for row in list
        .lines()
        .filter_map(|l| l.strip_prefix("ISO session  :"))
    {
        let cols: Vec<_> = row.splitn(4, ',').map(str::trim).collect();
        if cols.len() != 4 {
            return Err(invalid("incomplete session table"));
        }
        let number = cols[0]
            .parse()
            .map_err(|_| invalid("invalid session number"))?;
        let start_block = cols[1]
            .parse()
            .map_err(|_| invalid("invalid session start"))?;
        let blocks = cols[2]
            .strip_suffix('s')
            .and_then(|s| s.parse::<u64>().ok())
            .ok_or_else(|| invalid("invalid session size"))?;
        if number != sessions.len() as u32 + 1
            || number > 1024
            || blocks == 0
            || blocks
                .checked_add(start_block)
                .is_none_or(|end| end > len / 2048)
        {
            return Err(invalid("inconsistent ISO session table"));
        }
        sessions.push(Session {
            number,
            start_block,
            blocks,
        });
    }
    if sessions.is_empty() {
        return Err(invalid("no ISO 9660 session table"));
    }
    Ok(sessions)
}

pub fn iso_sessions(request: IsoRequest, cancel: &AtomicBool) -> Result<(Value, bool)> {
    let image = ImageInput {
        source: request.source,
        segment: vec![],
        image_type: Some("raw".into()),
        sector_size: None,
    };
    let (_, inputs) = image.arguments(request.destination.as_deref())?;
    let source = Path::new(&inputs[0]);
    if !source.is_file() {
        return Err(Error::Request("choose a cooked, regular ISO image".into()));
    }
    let reader = Reader {
        source,
        initial: fs::metadata(source)?,
        cancel,
    };
    let table = sessions(
        &reader.command(None, &[OsStr::new("-toc")])?,
        reader.initial.len(),
    )?;
    let latest = table.last().expect("nonempty session table").number;
    let Some(n) = request.session else {
        return Ok((json!({"sessions":table,"current_session":latest}), false));
    };
    if n == 0 || n >= latest {
        return Err(Error::Request(
            "select an existing session older than the current one".into(),
        ));
    }
    let current = reader.current_paths(latest)?;
    let mut history = reader.files(n)?;
    history.retain(|path, _| !current.contains(path));
    let mut result = json!({"session":n,"current_session":latest,"evidence":"historical_only_regular_file_paths","candidates":history.iter().map(|(path,size)|json!({"original_path":path,"size":size})).collect::<Vec<_>>()});
    let Some(dest) = request.destination else {
        return Ok((result, false));
    };
    if request.all != request.paths.is_empty() {
        return Err(Error::Request("choose --all or at least one --path".into()));
    }
    let selected: BTreeSet<_> = if request.all {
        history.keys().collect()
    } else {
        request.paths.iter().collect()
    };
    if selected.iter().any(|p| !history.contains_key(*p)) {
        return Err(Error::Request(
            "path is absent from historical-only results".into(),
        ));
    }
    fs::create_dir_all(&dest)?;
    let mut rows = Vec::new();
    let mut failed = false;
    for path in selected {
        let mut out = dest.join(format!("session-{n}-{:x}", Sha256::digest(path.as_bytes())));
        let ext = crate::file_extension(path, None);
        if !ext.is_empty() && ext.len() <= 32 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
            out.set_extension(ext);
        }
        match reader.extract(n,path,history[path],&out){
            Ok(file)=>rows.push(json!({"original_path":path,"path":file.path,"bytes":file.bytes,"sha256":file.sha256})),
            Err(Error::Cancelled)=>return Err(Error::Cancelled),
            Err(Error::SourceChanged)=>return Err(Error::SourceChanged),
            Err(e)=>{failed=true;rows.push(json!({"original_path":path,"error":e.to_string()}));}
        }
    }
    result["results"] = json!(rows);
    Ok((result, failed))
}
