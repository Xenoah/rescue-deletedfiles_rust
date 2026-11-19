//! Restore historical inline files from a retained, read-only Btrfs snapshot.
mod chunks;
mod tree;
use super::{ImageInput, publish_output};
use crate::{Error, Result};
use flate2::{Decompress, FlushDecompress, Status};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    fs,
    io::Write,
    path::{Path, PathBuf},
    sync::atomic::AtomicBool,
};
use tree::{Item, Key, Node, Volume};
pub struct BtrfsRequest {
    pub source: PathBuf,
    pub current_root: Option<u64>,
    pub snapshot_root: Option<u64>,
    pub id: Vec<u64>,
    pub all: bool,
    pub destination: Option<PathBuf>,
}
fn bad(reason: &str) -> Error {
    Error::Invalid(format!("Btrfs: {reason}"))
}
fn u16le(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes(b[i..i + 2].try_into().unwrap())
}
fn u32le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}
fn u64le(b: &[u8], i: usize) -> u64 {
    u64::from_le_bytes(b[i..i + 8].try_into().unwrap())
}
struct Root {
    node: Node,
    directory: u64,
    readonly: bool,
    uuid: [u8; 16],
    parent: [u8; 16],
}
fn roots(volume: &mut Volume) -> Result<BTreeMap<u64, Root>> {
    let mut roots = BTreeMap::new();
    volume.walk(volume.root, |Key(id, kind, _), _, b| {
        if kind != 132 || (id != 5 && !(256..=i64::MAX as u64).contains(&id)) {
            return Ok(());
        }
        if b.len() < 279 {
            return Err(bad("root item lacks subvolume lineage"));
        }
        let root = Root {
            node: Node {
                generation: u64le(b, 160),
                address: u64le(b, 176),
                level: b[238],
            },
            directory: u64le(b, 168),
            readonly: u64le(b, 208) & 1 != 0,
            uuid: b[247..263].try_into().unwrap(),
            parent: b[263..279].try_into().unwrap(),
        };
        if roots.len() >= 65536 {
            return Err(Error::Limit("too many Btrfs roots".into()));
        }
        if roots.insert(id, root).is_some() {
            return Err(bad("duplicate subvolume root"));
        }
        Ok(())
    })?;
    Ok(roots)
}
struct Entry {
    name: Vec<u8>,
    id: u64,
    kind: u8,
    inode: bool,
}
struct FileInfo {
    size: u64,
    mode: u32,
}
struct Namespace {
    files: BTreeMap<u64, FileInfo>,
    paths: BTreeMap<Vec<u8>, Option<u64>>,
    extents: BTreeMap<u64, Option<Item>>,
}
fn namespace(volume: &mut Volume, root: &Root, history: bool) -> Result<Namespace> {
    let mut files = BTreeMap::new();
    let mut dirs: BTreeMap<u64, Vec<Entry>> = BTreeMap::new();
    let mut extents = BTreeMap::new();
    let mut items = 0usize;
    volume.walk(root.node, |Key(id, kind, offset), item, b| {
        items += 1;
        if items > 1_000_000 {
            return Err(Error::Limit(
                "Btrfs selection exceeds one million items".into(),
            ));
        }
        match kind {
            1 => {
                if b.len() < 160 || offset != 0 {
                    return Err(bad("incomplete inode"));
                }
                if files
                    .insert(
                        id,
                        FileInfo {
                            size: u64le(b, 16),
                            mode: u32le(b, 52),
                        },
                    )
                    .is_some()
                {
                    return Err(bad("duplicate inode"));
                }
            }
            96 => {
                if b.len() < 30 {
                    return Err(bad("incomplete directory entry"));
                }
                let n = usize::from(u16le(b, 27));
                let data = usize::from(u16le(b, 25));
                if n == 0 || n > 255 || 30 + n + data != b.len() {
                    return Err(bad("inconsistent directory entry"));
                }
                let name = &b[30..30 + n];
                if name.contains(&0) || name.contains(&b'/') || name == b"." || name == b".." {
                    return Err(bad("invalid directory component"));
                }
                dirs.entry(id).or_default().push(Entry {
                    name: name.into(),
                    id: u64le(b, 0),
                    kind: b[29],
                    inode: b[8] == 1,
                });
            }
            108 if history => {
                if b.len() < 21 {
                    return Err(bad("truncated file extent"));
                }
                let value = (offset == 0 && b[20] == 0).then_some(item);
                extents.entry(id).and_modify(|v| *v = None).or_insert(value);
            }
            _ => {}
        }
        Ok(())
    })?;
    let mut paths = BTreeMap::new();
    let mut stack = vec![(root.directory, Vec::<u8>::new(), 0u16)];
    let mut visited = BTreeSet::new();
    let mut bytes = 0usize;
    while let Some((parent, prefix, depth)) = stack.pop() {
        if !visited.insert(parent) || depth > 128 {
            return Err(bad("cyclic or excessive directory depth"));
        }
        for entry in dirs.remove(&parent).unwrap_or_default() {
            let mut path = prefix.clone();
            path.push(b'/');
            path.extend(&entry.name);
            bytes = bytes.saturating_add(path.len());
            if bytes > 64 << 20 || path.len() > 16384 {
                return Err(Error::Limit("Btrfs directory path budget exceeded".into()));
            }
            // Nested subvolumes have independent roots and are listed as boundary entries.
            let regular = entry.inode && entry.kind == 1;
            if entry.inode && matches!(entry.kind, 1 | 2) {
                let mode = files
                    .get(&entry.id)
                    .ok_or_else(|| bad("directory references a missing inode"))?
                    .mode
                    & 0xf000;
                if mode != if regular { 0x8000 } else { 0x4000 } {
                    return Err(bad("directory and inode types disagree"));
                }
            }
            if entry.inode && entry.kind == 2 {
                stack.push((entry.id, path.clone(), depth + 1));
            }
            if paths.insert(path, regular.then_some(entry.id)).is_some() {
                return Err(bad("duplicate directory path"));
            }
        }
    }
    Ok(Namespace {
        files,
        paths,
        extents,
    })
}
fn contents(volume: &mut Volume, item: Item, size: u64) -> Result<Vec<u8>> {
    let raw = volume.item(item)?;
    let size = usize::try_from(size).map_err(|_| bad("oversized inline file"))?;
    if raw.len() < 21
        || u64le(&raw, 8) != size as u64
        || size > 65536
        || raw[17..21] != [0, 0, 0, 0]
    {
        return Err(bad("inconsistent inline length or encoding"));
    }
    let stored = &raw[21..];
    match raw[16] {
        0 if stored.len() == size => Ok(stored.to_vec()),
        0 => Err(bad("short inline file")),
        1 => {
            let padded = size.div_ceil(volume.sector) * volume.sector;
            let mut decoded = vec![0; padded + 1];
            let mut stream = Decompress::new(true);
            let status = stream
                .decompress(stored, &mut decoded, FlushDecompress::Finish)
                .map_err(|_| bad("invalid inline zlib stream"))?;
            let n = stream.total_out() as usize;
            if status != Status::StreamEnd
                || stream.total_in() != stored.len() as u64
                || !(n == size || n == padded)
                || decoded[size..n].iter().any(|&b| b != 0)
            {
                return Err(bad("invalid compressed inline data or padding"));
            }
            decoded.truncate(size);
            Ok(decoded)
        }
        _ => Err(Error::Unsupported(
            "this inline compression codec is unsupported".into(),
        )),
    }
}
fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}
pub fn btrfs(req: BtrfsRequest, cancel: &AtomicBool) -> Result<(Value, bool)> {
    let image = ImageInput {
        source: req.source,
        segment: vec![],
        image_type: Some("raw".into()),
        sector_size: None,
    };
    let (_, paths) = image.arguments(req.destination.as_deref())?;
    let mut vol = Volume::open(Path::new(&paths[0]), cancel)?;
    let roots = roots(&mut vol)?;
    let inventory=roots.iter().map(|(id,r)|json!({"id":id,"readonly":r.readonly,"uuid":hex(&r.uuid),"parent_uuid":hex(&r.parent)})).collect::<Vec<_>>();
    let mut result = json!({"roots":inventory});
    let (current_id, snapshot_id) = match (req.current_root, req.snapshot_root) {
        (None, None) => return Ok((result, false)),
        (Some(c), Some(s)) if c != s => (c, s),
        _ => {
            return Err(Error::Request(
                "choose distinct current and snapshot root IDs".into(),
            ));
        }
    };
    let current = roots
        .get(&current_id)
        .ok_or_else(|| Error::Request("unknown current root".into()))?;
    let historic = roots
        .get(&snapshot_id)
        .ok_or_else(|| Error::Request("unknown snapshot root".into()))?;
    if !historic.readonly || current.uuid == [0; 16] || historic.parent != current.uuid {
        return Err(Error::Request(
            "the read-only snapshot must name the selected subvolume as its parent".into(),
        ));
    }
    let current = namespace(&mut vol, current, false)?.paths;
    let historical = namespace(&mut vol, historic, true)?;
    let mut removed: BTreeMap<u64, Vec<&Vec<u8>>> = BTreeMap::new();
    for (path, id) in &historical.paths {
        if let Some(id) = id
            && !current.contains_key(path)
        {
            removed.entry(*id).or_default().push(path);
        }
    }
    result["current_root"] = json!(current_id);
    result["snapshot_root"] = json!(snapshot_id);
    result["evidence"] = json!("snapshot_only_regular_file_paths");
    let mut candidates = Vec::new();
    for (&id, names) in &removed {
        let size = historical.files[&id].size;
        candidates.push(json!({"id":id,"size":size,"original_paths":names.iter().map(|p|String::from_utf8_lossy(p)).collect::<Vec<_>>(),
            "original_paths_hex":names.iter().map(|p|hex(p)).collect::<Vec<_>>(),"inline_extent":matches!(historical.extents.get(&id),Some(Some(_))) }));
    }
    result["candidates"] = json!(candidates);
    let Some(dest) = req.destination else {
        return Ok((result, false));
    };
    if req.all != req.id.is_empty() {
        return Err(Error::Request("choose --all or at least one --id".into()));
    }
    let ids: BTreeSet<_> = if req.all {
        removed.keys().copied().collect()
    } else {
        req.id.into_iter().collect()
    };
    if !ids.iter().all(|id| removed.contains_key(id)) {
        return Err(Error::Request(
            "ID is absent from historical-only results".into(),
        ));
    }
    fs::create_dir_all(&dest)?;
    let mut rows = Vec::new();
    let mut failed = false;
    for id in ids {
        let names: Vec<_> = removed[&id]
            .iter()
            .map(|v| String::from_utf8_lossy(v))
            .collect();
        let mut out = dest.join(format!("snapshot-{snapshot_id}-inode-{id}"));
        let ext = crate::file_extension(&names[0], None);
        if !ext.is_empty() && ext.len() <= 32 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
            out.set_extension(ext);
        }
        let size = historical.files[&id].size;
        let saved = publish_output(&out, cancel, |dst| {
            let bytes=match historical.extents.get(&id){
                Some(Some(item))=>contents(&mut vol,*item,size)?,
                None if size==0=>Vec::new(),
                _=>return Err(Error::Unsupported("file uses external Btrfs extents; a mounted snapshot or another reader is required".into())),
            };
            dst.write_all(&bytes)?;
            vol.unchanged()?;
            Ok((size, Sha256::new_with_prefix(bytes)))
        });
        match saved{
            Ok(f)=>rows.push(json!({"id":id,"original_paths":names,"path":f.path,"bytes":f.bytes,"sha256":f.sha256})),
            Err(Error::Cancelled)=>return Err(Error::Cancelled),
            Err(Error::SourceChanged)=>return Err(Error::SourceChanged),
            Err(e)=>{failed=true;rows.push(json!({"id":id,"original_paths":names,"error":e.to_string()}));}
        }
    }
    result["results"] = json!(rows);
    Ok((result, failed))
}
