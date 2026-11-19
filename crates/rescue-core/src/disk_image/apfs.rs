//! Compare a retained APFS snapshot with the current directory tree using The Sleuth Kit.
use super::{ImageInput, filesystem::listing_row, read_tool, write_output};
use crate::{Error, Result};
use serde::Serialize;
use serde_json::{Value, json};
use std::{
    collections::{BTreeMap, BTreeSet},
    ffi::OsString,
    io::Read,
    path::PathBuf,
    sync::atomic::AtomicBool,
};

#[derive(Debug, Clone)]
pub struct ApfsRequest {
    pub image: ImageInput,
    pub sector_offset: u64,
    pub volume_block: Option<u64>,
    pub snapshot_xid: Option<u64>,
    pub id: Vec<String>,
    pub all: bool,
    pub destination: Option<PathBuf>,
}

#[derive(Serialize)]
struct Snapshot {
    xid: u64,
    description: String,
    dataless: bool,
}
#[derive(Serialize)]
struct Volume {
    uuid: String,
    name_and_role: String,
    volume_block: u64,
    encrypted: bool,
    snapshots: Vec<Snapshot>,
    #[serde(skip_serializing_if = "Option::is_none")]
    snapshot_error: Option<String>,
}

fn text(tool: &str, args: &[OsString], cancel: &AtomicBool) -> Result<String> {
    let bytes = read_tool(tool, args, cancel, |stdout| {
        let mut data = Vec::new();
        const LIMIT: u64 = 32 << 20;
        stdout.take(LIMIT + 1).read_to_end(&mut data)?;
        if data.len() as u64 > LIMIT {
            return Err(Error::Limit("APFS tool listing exceeds 32 MiB".into()));
        }
        Ok(data)
    })?;
    String::from_utf8(bytes)
        .map_err(|_| Error::Invalid("APFS reader returned non-UTF-8 names".into()))
}
fn arguments(opts: &[OsString], extra: &[&str], paths: &[OsString]) -> Vec<OsString> {
    let mut args = opts.to_vec();
    args.extend(extra.iter().map(OsString::from));
    args.extend_from_slice(paths);
    args
}

fn volumes(list: &str) -> Result<Vec<Volume>> {
    let mut results = Vec::new();
    let mut blocks = BTreeSet::new();
    let mut volume = None;
    for line in list.lines().chain(std::iter::once("+-> End")) {
        if line.starts_with("+-> ") {
            if let Some(v) = volume.take() {
                let v: Volume = v;
                if v.volume_block == 0 || !blocks.insert(v.volume_block) {
                    return Err(Error::Invalid("invalid APFS volume block listing".into()));
                }
                results.push(v);
            }
            if let Some(id) = line.strip_prefix("+-> Volume ") {
                let id = uuid::Uuid::parse_str(id.trim())
                    .map_err(|_| Error::Invalid("invalid APFS volume UUID".into()))?;
                volume = Some(Volume {
                    uuid: id.to_string(),
                    name_and_role: String::new(),
                    volume_block: 0,
                    encrypted: false,
                    snapshots: Vec::new(),
                    snapshot_error: None,
                });
            }
        } else if let Some(v) = &mut volume {
            let field = line.trim_start_matches('|').trim();
            if let Some(value) = field.strip_prefix("APSB Block Number:") {
                v.volume_block = value
                    .trim()
                    .parse()
                    .map_err(|_| Error::Invalid("invalid APFS block".into()))?;
            } else if let Some(value) = field.strip_prefix("Name (Role):") {
                v.name_and_role = value.trim().to_owned();
            } else if let Some(value) = field.strip_prefix("Encrypted:") {
                v.encrypted = value.trim().eq_ignore_ascii_case("yes");
            }
        }
    }
    if results.is_empty() || results.len() > 100 {
        return Err(Error::Invalid(
            "expected one to 100 APFS volumes in the container".into(),
        ));
    }
    Ok(results)
}

fn snapshots(list: &str) -> Result<Vec<Snapshot>> {
    let mut result = Vec::new();
    let lines = list.lines().skip_while(|s| s.trim() != "Snapshots").skip(2);
    for line in lines.take_while(|s| !s.trim().is_empty()) {
        let (id, description) = line
            .trim()
            .strip_prefix('[')
            .and_then(|s| s.split_once(']'))
            .ok_or_else(|| Error::Invalid("cannot interpret APFS snapshot identifier".into()))?;
        let xid = id
            .parse()
            .map_err(|_| Error::Invalid("invalid APFS snapshot XID".into()))?;
        let description = description.trim();
        result.push(Snapshot {
            xid,
            description: description.to_owned(),
            dataless: description.ends_with("(dataless)"),
        });
    }
    Ok(result)
}
fn snapshot_list(
    v: &Volume,
    opts: &[OsString],
    paths: &[OsString],
    cancel: &AtomicBool,
) -> Result<Vec<Snapshot>> {
    let path = arguments(
        opts,
        &["-f", "apfs", "-B", &v.volume_block.to_string()],
        paths,
    );
    snapshots(&text("fsstat", &path, cancel)?)
}

struct FileEntry {
    paths: BTreeSet<String>,
    size: u64,
}
const EVIDENCE: &str = "These regular file paths exist in the specified APFS snapshot and are absent by the same spelling in the current namespace. Renamed and moved paths can also qualify. Bytes come from the snapshot.";

pub fn apfs(request: ApfsRequest, cancel: &AtomicBool) -> Result<(Value, bool)> {
    if request.image.image_type.as_deref() == Some("ewf-verified") {
        return Err(Error::Request(
            "first use import-image to verify the EWF source".into(),
        ));
    }
    if request.all && !request.id.is_empty()
        || (request.destination.is_some() != (request.all || !request.id.is_empty()))
        || request.snapshot_xid.is_none() && request.destination.is_some()
    {
        return Err(Error::Request(
            "recovery needs a snapshot XID, a destination, and either --id or --all".into(),
        ));
    }
    let (mut opts, paths) = request.image.arguments(request.destination.as_deref())?;
    opts.extend([
        "-P".into(),
        "apfs".into(),
        "-o".into(),
        request.sector_offset.to_string().into(),
    ]);
    let mut pool = volumes(&text("pstat", &arguments(&opts, &[], &paths), cancel)?)?;
    if let Some(block) = request.volume_block {
        pool.retain(|v| v.volume_block == block);
        if pool.is_empty() {
            return Err(Error::Request(
                "APFS volume block is absent from this container".into(),
            ));
        }
    }
    let Some(xid) = request.snapshot_xid else {
        for v in &mut pool {
            match snapshot_list(v, &opts, &paths, cancel) {
                Ok(ids) => v.snapshots = ids,
                Err(Error::Cancelled) => return Err(Error::Cancelled),
                Err(e) => v.snapshot_error = Some(e.to_string()),
            }
        }
        return Ok((
            json!({"volumes":pool,"sector_offset":request.sector_offset}),
            false,
        ));
    };
    let [volume] = pool.as_slice() else {
        return Err(Error::Request(
            "select --volume-block when an APFS container has multiple volumes".into(),
        ));
    };
    let snapshot = snapshot_list(volume, &opts, &paths, cancel)?
        .into_iter()
        .find(|s| s.xid == xid)
        .ok_or_else(|| {
            Error::Request("snapshot XID is absent from the selected APFS volume".into())
        })?;
    if snapshot.dataless {
        return Err(Error::Unsupported(
            "APFS snapshot is marked dataless".into(),
        ));
    }
    opts.extend([
        "-f".into(),
        "apfs".into(),
        "-B".into(),
        volume.volume_block.to_string().into(),
    ]);
    let live = text("fls", &arguments(&opts, &["-urpl"], &paths), cancel)?;
    let mut current = BTreeSet::new();
    for line in live.lines().filter(|line| !line.is_empty()) {
        let (_, path, _) = listing_row(line)
            .ok_or_else(|| Error::Invalid("incomplete live APFS directory listing".into()))?;
        current.insert(path);
    }
    opts.extend(["-S".into(), xid.to_string().into()]);
    let history = text("fls", &arguments(&opts, &["-uFrpl"], &paths), cancel)?;
    let mut files: BTreeMap<&str, FileEntry> = BTreeMap::new();
    for line in history.lines().filter(|line| !line.is_empty()) {
        let (head, path, size) = listing_row(line)
            .ok_or_else(|| Error::Invalid("incomplete historical APFS directory listing".into()))?;
        let Some(id) = head.strip_prefix("r/r ") else {
            continue;
        };
        if current.contains(path) {
            continue;
        }
        if id.parse::<u64>().is_err() {
            return Err(Error::Invalid("invalid APFS file identifier".into()));
        }
        let entry = files.entry(id).or_insert_with(|| FileEntry {
            paths: BTreeSet::new(),
            size,
        });
        if entry.size != size {
            return Err(Error::Invalid(
                "conflicting sizes for APFS snapshot inode".into(),
            ));
        }
        entry.paths.insert(path.to_owned());
    }
    let mut result = json!({"volume_uuid":volume.uuid,"volume_block":volume.volume_block,"snapshot_xid":xid,"evidence":"snapshot_only_paths","warning":EVIDENCE});
    let Some(dest) = request.destination else {
        result["candidates"] = json!(
            files
                .iter()
                .map(|(id, e)| json!({"id":id,"original_paths":e.paths,"size":e.size}))
                .collect::<Vec<_>>()
        );
        return Ok((result, false));
    };
    let ids = if request.all {
        files.keys().copied().collect::<BTreeSet<_>>()
    } else {
        request.id.iter().map(String::as_str).collect()
    };
    if ids.iter().any(|id| !files.contains_key(*id)) {
        return Err(Error::Request(
            "ID is absent from the snapshot-only result set".into(),
        ));
    }
    std::fs::create_dir_all(&dest)?;
    let mut args = arguments(&opts, &[], &paths);
    let mut rows = Vec::new();
    let mut failed = false;
    for id in ids {
        let entry = &files[id];
        let mut out = dest.join(format!("snapshot-{xid}-inode-{id}"));
        let ext = crate::file_extension(
            entry.paths.first().expect("at least one historical path"),
            None,
        );
        if !ext.is_empty() && ext.len() <= 32 && ext.bytes().all(|v| v.is_ascii_alphanumeric()) {
            out.set_extension(ext);
        }
        args.push(id.into());
        let mut row = match write_output("icat", &args, &out, Some(entry.size), cancel) {
            Ok(file) => json!(file),
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(e) => {
                failed = true;
                json!({"error":e.to_string()})
            }
        };
        row["id"] = json!(id);
        row["original_paths"] = json!(entry.paths);
        rows.push(row);
        args.pop();
    }
    result["results"] = json!(rows);
    Ok((result, failed))
}
