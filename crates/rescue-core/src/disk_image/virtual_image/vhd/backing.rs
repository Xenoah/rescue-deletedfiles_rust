//! Read explicit local parent VHDs. Locator paths are metadata, never paths to open.
use super::{Block, SECTOR, Vhd, be32, be64, checked_region, invalid, read_at};
use crate::{
    Error, Result,
    disk_image::{
        ImportedFile, RawExpectation,
        virtual_image::chain::{self, Layer},
    },
};
use std::{
    collections::HashSet, ffi::OsString, fs::File, ops::Range, path::Path,
    sync::atomic::AtomicBool, time::UNIX_EPOCH,
};

pub(super) struct Parent {
    uuid: [u8; 16],
    basename: String,
    modified: u32,
}

fn basename(data: &[u8], little_endian: bool) -> Result<String> {
    if data.is_empty() || !data.len().is_multiple_of(2) {
        return Err(invalid("invalid VHD parent name length"));
    }
    let units: Vec<u16> = data
        .as_chunks::<2>()
        .0
        .iter()
        .map(|&v| {
            if little_endian {
                u16::from_le_bytes(v)
            } else {
                u16::from_be_bytes(v)
            }
        })
        .collect();
    let end = units.iter().position(|&v| v == 0).unwrap_or(units.len());
    if units[end..].iter().any(|&v| v != 0) {
        return Err(invalid("VHD parent name contains an embedded terminator"));
    }
    let full =
        String::from_utf16(&units[..end]).map_err(|_| invalid("invalid UTF-16 VHD parent name"))?;
    let name = full.rsplit(['/', '\\']).next().unwrap_or("");
    if name.is_empty()
        || matches!(name, "." | "..")
        || name.contains(':')
        || full.chars().any(char::is_control)
    {
        return Err(invalid("invalid VHD parent basename"));
    }
    Ok(name.to_owned())
}

pub(super) fn parse(
    file: &mut File,
    header: &[u8],
    end: u64,
    regions: &mut Vec<Range<u64>>,
) -> Result<Parent> {
    let uuid = header[40..56].try_into().unwrap();
    let name = basename(&header[64..576], false)?;
    if uuid == [0; 16] || be32(header, 60) != 0 {
        return Err(invalid("invalid VHD parent identity"));
    }
    let mut locators = HashSet::new();
    for locator in header[576..768].as_chunks::<24>().0 {
        let code: [u8; 4] = locator[..4].try_into().unwrap();
        if code == [0; 4] && locator.iter().all(|&b| b == 0) {
            continue;
        }
        if !matches!(&code, b"W2ku" | b"W2ru") {
            return Err(Error::Unsupported(
                "VHD chain requires Windows Unicode parent locators".into(),
            ));
        }
        let space = u64::from(be32(locator, 4));
        // Windows reserves a sector-aligned byte count. Small sector counts
        // are also used by writers following the original VHD specification.
        let reserved = if space < SECTOR {
            space * SECTOR
        } else {
            space
        };
        let length = be32(locator, 8) as usize;
        if !locators.insert(code)
            || be32(locator, 12) != 0
            || length == 0
            || length > 65536
            || reserved < length as u64
            || !reserved.is_multiple_of(SECTOR)
        {
            return Err(invalid(
                "VHD locator length, duplication or reserved fields",
            ));
        }
        let region = checked_region(be64(locator, 16), reserved, end)?;
        let mut value = vec![0; length];
        read_at(file, region.start, &mut value)?;
        if !basename(&value, true)?.eq_ignore_ascii_case(&name) {
            return Err(invalid("VHD parent name and locators disagree"));
        }
        regions.push(region);
    }
    if locators.is_empty() {
        return Err(invalid("missing VHD parent locators"));
    }
    Ok(Parent {
        uuid,
        basename: name,
        modified: be32(header, 56),
    })
}

pub(in crate::disk_image) fn import_vhd_chain(
    paths: &[OsString],
    requested: Option<&str>,
    output: &Path,
    expected: RawExpectation,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if requested.is_some_and(|s| s != "vhd") {
        return Ok(None);
    }
    let mut first = File::open(&paths[0])?;
    let length = first.metadata()?.len();
    let mut front = [0; 8];
    let mut tail = [0; 8];
    if length >= SECTOR {
        read_at(&mut first, 0, &mut front)?;
        read_at(&mut first, length - SECTOR, &mut tail)?;
    }
    if front != *b"conectix" && tail != *b"conectix" {
        return Ok(None);
    }
    if paths.len() > 8 {
        return Err(Error::Limit("VHD chain exceeds eight files".into()));
    }
    let mut handles = HashSet::new();
    let mut uuids = HashSet::new();
    let mut members: Vec<Layer<Vhd>> = Vec::new();
    let mut opened = Some(first);
    for path in paths {
        let mut file = match opened.take() {
            Some(f) => f,
            None => File::open(path)?,
        };
        if !handles.insert(same_file::Handle::from_file(file.try_clone()?)?) {
            return Err(invalid("repeated VHD chain file"));
        }
        let before = file.metadata()?;
        let disk = Vhd::open_member(&mut file, before.len(), cancel, true)?;
        if !uuids.insert(disk.uuid) {
            return Err(invalid("repeated VHD disk ID"));
        }
        if let Some(child) = members.last() {
            let parent = child
                .layout
                .parent
                .as_ref()
                .ok_or_else(|| invalid("extra parent after standalone VHD"))?;
            if disk.uuid != parent.uuid
                || disk.size != child.layout.size
                || !Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(&parent.basename))
            {
                return Err(invalid(
                    "selected VHD parent name, disk ID or logical size disagrees with child",
                ));
            }
            // Windows often writes zero here. When a timestamp is recorded,
            // copies with a different host mtime require an exact logical hash.
            if parent.modified != 0 && expected.sha256.is_none() {
                let seconds = before
                    .modified()?
                    .duration_since(UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_secs());
                if seconds != Some(946_684_800 + u64::from(parent.modified)) {
                    return Err(invalid(
                        "VHD parent timestamp changed; supply the known logical disk SHA-256",
                    ));
                }
            }
        }
        members.push(Layer {
            file,
            before,
            layout: disk,
        });
    }
    if members.last().is_some_and(|m| m.layout.parent.is_some()) {
        return Err(Error::Request(
            "select all parent files in the VHD chain".into(),
        ));
    }
    let size = members[0].layout.size;
    chain::flatten(
        &mut members,
        size,
        output,
        expected,
        cancel,
        |m, offset, buf, limit| {
            let (block, span) = m.layout.block(&mut m.file, offset)?;
            *limit = (*limit as u64).min(span) as usize;
            match block {
                Block::Data(at) => read_at(&mut m.file, at, &mut buf[..*limit])?,
                Block::Zero if m.layout.parent.is_some() => return Ok(false),
                Block::Zero => buf[..*limit].fill(0),
                Block::Compressed { .. } => {
                    return Err(invalid("unexpected compressed VHD sector"));
                }
            }
            Ok(true)
        },
    )
    .map(Some)
}
