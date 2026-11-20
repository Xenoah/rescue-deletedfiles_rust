//! Deleted metadata on filesystems supported by the installed Sleuth Kit.
use super::{ImageInput, read_tool, write_output};
use crate::{Error, Result};
use serde_json::{Value, json};
use std::{
    collections::BTreeMap, ffi::OsString, fs, io::Read, path::PathBuf, sync::atomic::AtomicBool,
};
#[derive(Debug, Clone)]
pub struct FilesystemRequest {
    pub image: ImageInput,
    /// File system type, such as ext4, hfs or ufs. Default: auto-detect.
    pub fs_type: Option<String>,
    /// Starting sector of the volume within a whole disk image.
    pub sector_offset: u64,
    /// Restore selected file IDs from the listing.
    pub id: Vec<String>,
    /// Restore every listed deleted regular file.
    pub all: bool,
    /// Existing output files are preserved. Omit to list deleted metadata.
    pub destination: Option<PathBuf>,
}

struct Entry {
    path: String,
    size: u64,
}

// fls -dFrpl: metadata identity, tab, full name, four times, size, gid and uid.
// Only an unallocated regular entry with an unreallocated regular inode qualifies.
fn parse_entry(line: &str) -> Option<(&str, Entry)> {
    let (header, path, size) = listing_row(line)?;
    let id = header.strip_prefix("r/r * ")?;
    if id.len() > 64
        || !id
            .split('-')
            .all(|s| !s.is_empty() && s.bytes().all(|b| b.is_ascii_digit()))
    {
        return None;
    }
    Some((
        id,
        Entry {
            path: path.to_owned(),
            size,
        },
    ))
}

pub(super) fn listing_row(line: &str) -> Option<(&str, &str, u64)> {
    let (header, row) = line.split_once(":\t")?;
    let mut columns = row.rsplitn(8, '\t');
    let size = columns.nth(2)?.parse().ok()?;
    let path = columns.nth(4)?;
    Some((header, path, size))
}

pub fn filesystem(request: FilesystemRequest, cancel: &AtomicBool) -> Result<(Value, bool)> {
    if request.image.image_type.as_deref() == Some("ewf-verified") {
        return Err(Error::Request("use import-image --image-type ewf-verified to verify and export the logical RAW before scanning its filesystem".into()));
    }
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
        return Ok((
            json!({"candidates":entries,"skipped_rows":skipped,"warning":warning}),
            false,
        ));
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
        let ext = crate::file_extension(&entry.path, None);
        if !ext.is_empty() && ext.len() <= 32 && ext.bytes().all(|b| b.is_ascii_alphanumeric()) {
            output.set_extension(ext);
        }
        extract.push(id.into());
        let mut row = match write_output(
            "icat",
            &extract,
            &output,
            super::RawExpectation {
                bytes: Some(entry.size),
                sha256: None,
            },
            cancel,
        ) {
            Ok(v) => json!(v),
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
    Ok((
        json!({"results":results,"skipped_rows":skipped,"warning":warning}),
        failed,
    ))
}
