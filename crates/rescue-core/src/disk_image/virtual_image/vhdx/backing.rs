//! Match explicitly selected VHDX parents by their active data-write GUID and disk identity.
use super::{Block, Vhdx, disjoint, extent, guid, invalid, le16, le32, read_at};
use crate::{
    Error, Result,
    disk_image::{
        ImportedFile, RawExpectation,
        virtual_image::chain::{self, Layer},
    },
};
use std::{
    collections::HashSet, ffi::OsString, fs::File, ops::Range, path::Path, sync::atomic::AtomicBool,
};
use uuid::Uuid;

const LOCATOR: u128 = 0xb04aefb7_d19e_4a81_b789_25b8e9445913;

pub(super) struct Parent {
    pub write_id: u128,
    pub basename: String,
}

fn string(data: &[u8], offset: u32, size: u16, ranges: &mut Vec<Range<u64>>) -> Result<String> {
    let range = extent(u64::from(offset), u64::from(size), data.len() as u64)?;
    if !size.is_multiple_of(2) || !offset.is_multiple_of(2) {
        return Err(invalid("unaligned VHDX locator string"));
    }
    let units: Vec<_> = data[range.start as usize..range.end as usize]
        .as_chunks::<2>()
        .0
        .iter()
        .copied()
        .map(u16::from_le_bytes)
        .collect();
    ranges.push(range);
    let s = String::from_utf16(&units).map_err(|_| invalid("invalid VHDX locator UTF-16"))?;
    if s.chars().any(char::is_control) {
        return Err(invalid("control character in VHDX locator"));
    }
    Ok(s)
}

impl Parent {
    pub(super) fn parse(data: &[u8]) -> Result<Self> {
        if data.len() < 20 || guid(data) != LOCATOR || le16(data, 16) != 0 {
            return Err(invalid("unsupported VHDX parent locator type or flags"));
        }
        let count = usize::from(le16(data, 18));
        let end = 20 + count * 12;
        if !(1..=64).contains(&count) || end > data.len() {
            return Err(invalid("VHDX locator entry count"));
        }
        let mut ranges = Vec::with_capacity(1 + 2 * count);
        ranges.push(0..end as u64);
        let mut keys = HashSet::new();
        let mut write_id = None;
        let mut basename: Option<String> = None;
        for entry in data[20..end].as_chunks::<12>().0 {
            let key = string(data, le32(entry, 0), le16(entry, 8), &mut ranges)?;
            let value = string(data, le32(entry, 4), le16(entry, 10), &mut ranges)?;
            if !keys.insert(key.clone()) {
                return Err(invalid("duplicate VHDX locator key"));
            }
            match key.as_str() {
                "parent_linkage" | "parent_linkage2" => {
                    let id = Uuid::parse_str(&value)
                        .map_err(|_| invalid("invalid VHDX parent linkage GUID"))?;
                    match (key.as_str(), id.is_nil()) {
                        ("parent_linkage", false) => write_id = Some(id.as_u128()),
                        ("parent_linkage2", true) => {}
                        _ => return Err(invalid("unsupported primary or alternate VHDX linkage")),
                    }
                }
                "relative_path" | "absolute_win32_path" | "volume_path" => {
                    let name = value.rsplit(['/', '\\']).next().unwrap_or("");
                    if name.is_empty()
                        || matches!(name, "." | "..")
                        || name.contains(':')
                        || basename
                            .as_ref()
                            .is_some_and(|n| !n.eq_ignore_ascii_case(name))
                    {
                        return Err(invalid("conflicting or invalid VHDX locator basenames"));
                    }
                    basename = Some(name.to_owned());
                }
                _ => return Err(Error::Unsupported("unknown VHDX parent locator key".into())),
            }
        }
        disjoint(&mut ranges)?;
        Ok(Self {
            write_id: write_id.ok_or_else(|| invalid("missing VHDX parent linkage GUID"))?,
            basename: basename.ok_or_else(|| invalid("missing VHDX parent basename"))?,
        })
    }
}

pub(in crate::disk_image) fn import_vhdx_chain(
    paths: &[OsString],
    requested: Option<&str>,
    output: &Path,
    expected: RawExpectation,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if requested.is_some_and(|s| !matches!(s, "vhdx" | "vhd")) {
        return Ok(None);
    }
    let mut first = File::open(&paths[0])?;
    let mut magic = [0; 8];
    if first.metadata()?.len() < 8 {
        return Ok(None);
    }
    read_at(&mut first, 0, &mut magic)?;
    if &magic != b"vhdxfile" {
        return Ok(None);
    }
    if paths.len() > 8 {
        return Err(Error::Limit("VHDX chain exceeds eight files".into()));
    }
    let mut handles = HashSet::new();
    let mut write_ids = HashSet::new();
    let mut opened = Some(first);
    let mut layers: Vec<Layer<Vhdx>> = Vec::new();
    for path in paths {
        let mut file = match opened.take() {
            Some(f) => f,
            None => File::open(path)?,
        };
        if !handles.insert(same_file::Handle::from_file(file.try_clone()?)?) {
            return Err(invalid("repeated VHDX chain file"));
        }
        let before = file.metadata()?;
        let disk = Vhdx::open_member(&mut file, before.len(), cancel, true)?;
        if disk.page83 == 0 || disk.write_id == 0 || !write_ids.insert(disk.write_id) {
            return Err(invalid(
                "empty disk identity or empty/repeated VHDX data-write GUID",
            ));
        }
        if let Some(child) = layers.last() {
            let parent = child
                .layout
                .parent
                .as_ref()
                .ok_or_else(|| invalid("extra parent after standalone VHDX"))?;
            if disk.write_id != parent.write_id
                || disk.page83 != child.layout.page83
                || disk.size != child.layout.size
                || disk.logical != child.layout.logical
                || !Path::new(path)
                    .file_name()
                    .and_then(|n| n.to_str())
                    .is_some_and(|n| n.eq_ignore_ascii_case(&parent.basename))
            {
                return Err(invalid(
                    "selected VHDX parent name, active data-write GUID, disk identity or geometry disagrees with child",
                ));
            }
        }
        layers.push(Layer {
            file,
            before,
            layout: disk,
        });
    }
    if layers.last().is_some_and(|m| m.layout.parent.is_some()) {
        return Err(Error::Request(
            "select all parent files in the VHDX chain".into(),
        ));
    }
    let size = layers[0].layout.size;
    chain::flatten(
        &mut layers,
        size,
        output,
        expected,
        cancel,
        |m, offset, buf, limit| {
            match m.layout.layer_span(&mut m.file, offset, limit)? {
                None => return Ok(false),
                Some(Block::Data(at)) => read_at(&mut m.file, at, &mut buf[..*limit])?,
                Some(Block::Zero) => buf[..*limit].fill(0),
                Some(Block::Compressed { .. }) => {
                    return Err(invalid("unexpected compressed VHDX sector"));
                }
            }
            Ok(true)
        },
    )
    .map(Some)
}
