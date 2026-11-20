//! Parse descriptor extent lines as data. File access is bound to caller-supplied handles.
use super::{Error, Result, invalid, sectors};
use std::{ffi::OsStr, path::Path};
use uuid::Uuid;

#[derive(Clone, Copy)]
pub(super) enum Storage<'a> {
    Sparse(&'a str),
    Flat(&'a str, u64),
    Zero,
}
pub(super) struct Extent<'a> {
    pub size: u64,
    pub storage: Storage<'a>,
}
pub(super) struct Descriptor<'a> {
    pub kind: &'a str,
    pub extents: Vec<Extent<'a>>,
    pub lineage: Option<Lineage>,
}

pub(super) struct Lineage {
    pub cid: u32,
    pub image: Option<Uuid>,
    modification: Option<Uuid>,
    parent_cid: Option<u32>,
    hint: Option<String>,
    parent_uuid: Option<Uuid>,
    parent_modification: Option<Uuid>,
}

impl Lineage {
    pub fn requires_parent(&self) -> bool {
        self.parent_cid.is_some() || self.parent_uuid.is_some()
    }
    pub fn accepts(&self, parent: &Self, selected: &Path) -> bool {
        self.requires_parent()
            && self.parent_cid.is_none_or(|cid| cid == parent.cid)
            && self
                .hint
                .as_ref()
                .is_none_or(|hint| selected.file_name() == Some(OsStr::new(hint)))
            && self.parent_uuid.is_none_or(|id| {
                parent.image == Some(id) && parent.modification == self.parent_modification
            })
    }
}

fn simple_name(name: &str) -> bool {
    !name.is_empty()
        && !matches!(name, "." | "..")
        && !name.contains(['/', '\\', ':', '"'])
        && !name.chars().any(char::is_control)
}

fn unquote(value: &str) -> Option<&str> {
    value.strip_prefix('"')?.strip_suffix('"')
}

fn hex_id(value: &str) -> Result<u32> {
    if !(1..=8).contains(&value.len()) || !value.bytes().all(|b| b.is_ascii_hexdigit()) {
        return Err(invalid("invalid VMDK content ID"));
    }
    u32::from_str_radix(value, 16).map_err(|_| invalid("invalid VMDK content ID"))
}

#[derive(Default)]
struct UuidFields<'a> {
    image: Option<&'a str>,
    modification: Option<&'a str>,
    parent: Option<&'a str>,
    parent_modification: Option<&'a str>,
}

impl UuidFields<'_> {
    fn lineage(&self, cid: u32, parent_cid: u32, hint: Option<&str>) -> Result<Lineage> {
        let parse = |value: &str| {
            let id = unquote(value).ok_or_else(|| invalid("unquoted VMDK UUID"))?;
            Uuid::try_parse_ascii(id.as_bytes())
                .ok()
                .filter(|value| value.hyphenated().to_string().eq_ignore_ascii_case(id))
                .ok_or_else(|| invalid("invalid VMDK UUID"))
        };
        let image = self.image.map(parse).transpose()?;
        let modification = self.modification.map(parse).transpose()?;
        let parent_uuid = self.parent.map(parse).transpose()?;
        let parent_modification = self.parent_modification.map(parse).transpose()?;
        let uuid_set = [image, modification, parent_uuid, parent_modification];
        if uuid_set.iter().any(Option::is_some)
            && (uuid_set.iter().any(Option::is_none)
                || image.is_some_and(|id| id.is_nil())
                || parent_uuid.is_some_and(|id| id.is_nil())
                    && parent_modification.is_some_and(|id| !id.is_nil()))
        {
            return Err(invalid("incomplete or inconsistent VMDK UUID lineage"));
        }
        let hint = hint
            .map(|value| {
                unquote(value)
                    .filter(|name| simple_name(name))
                    .map(str::to_owned)
                    .ok_or_else(|| invalid("VMDK parent hint must be a simple quoted file name"))
            })
            .transpose()?;
        let parent_cid = (parent_cid != u32::MAX).then_some(parent_cid);
        if cid == u32::MAX || parent_cid.is_some() != hint.is_some() {
            return Err(invalid(
                "VMDK snapshot requires a valid CID and matching parent CID and filename fields",
            ));
        }
        Ok(Lineage {
            cid,
            image,
            modification,
            parent_cid,
            hint,
            parent_uuid: parent_uuid.filter(|id| !id.is_nil()),
            parent_modification,
        })
    }
}

fn extent(line: &str) -> Result<Extent<'_>> {
    let (fields, name) = match line.split_once('"') {
        Some((before, rest)) => {
            let (name, after) = rest
                .split_once('"')
                .ok_or_else(|| invalid("unterminated VMDK extent name"))?;
            if !simple_name(name) {
                return Err(invalid("VMDK extent must have a simple file name"));
            }
            (before, Some((name, after.trim())))
        }
        None => (line, None),
    };
    let mut words = fields.split_ascii_whitespace();
    if !matches!(words.next(), Some("RW" | "RDONLY")) {
        return Err(invalid("VMDK extent access mode"));
    }
    let count = words
        .next()
        .and_then(|v| v.parse::<u64>().ok())
        .filter(|&s| s != 0)
        .ok_or_else(|| invalid("VMDK extent sector count"))?;
    let storage = match (words.next(), name) {
        (Some("SPARSE"), Some((name, ""))) => Storage::Sparse(name),
        (Some("FLAT"), Some((name, offset))) => {
            let start = if offset.is_empty() {
                0
            } else {
                offset
                    .parse::<u64>()
                    .map_err(|_| invalid("VMDK flat extent offset"))?
            };
            Storage::Flat(name, sectors(start)?)
        }
        (Some("ZERO"), None) => Storage::Zero,
        _ => {
            return Err(Error::Unsupported(
                "unsupported VMDK extent type or arguments".into(),
            ));
        }
    };
    if words.next().is_some() {
        return Err(invalid("extra VMDK extent fields"));
    }
    Ok(Extent {
        size: sectors(count)?,
        storage,
    })
}

// QEMU changes its hexadecimal CID in place. A shorter replacement can leave
// a few bytes from the end of the preceding descriptor after the new NUL.
fn valid_padding(text: &str, padding: &[u8], cid: &str) -> bool {
    let Some(last) = padding.iter().rposition(|&b| b != 0) else {
        return true;
    };
    // At most eight hexadecimal digits were replaced. One of the vacated
    // positions is occupied by the NUL itself.
    if last >= 8 - cid.len() || padding.first() != Some(&0) {
        return false;
    }
    let Some(version) = text
        .lines()
        .last()
        .and_then(|line| line.strip_prefix("ddb.toolsVersion = \""))
        .and_then(|v| v.strip_suffix('"'))
    else {
        return false;
    };
    let suffix = &padding[1..=last];
    !version.is_empty()
        && version.bytes().all(|b| b.is_ascii_digit())
        && suffix
            .iter()
            .all(|b| b.is_ascii_digit() || matches!(b, b'"' | b'\n' | b'\r'))
        && text.ends_with('\n')
        && text.as_bytes().ends_with(suffix)
}

impl<'a> Descriptor<'a> {
    pub fn parse(bytes: &'a [u8], chain: bool) -> Result<Self> {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        let text =
            std::str::from_utf8(&bytes[..end]).map_err(|_| invalid("non-UTF-8 VMDK descriptor"))?;
        let (mut version, mut cid, mut parent, mut kind) = (None, None, None, None);
        let mut uuids = UuidFields::default();
        let mut hint = None;
        let mut extents = Vec::new();
        for line in text
            .lines()
            .map(str::trim)
            .filter(|s| !s.is_empty() && !s.starts_with('#'))
        {
            if let Some((key, value)) = line
                .split_once('=')
                .filter(|(k, _)| !k.trim().contains(char::is_whitespace))
            {
                let slot = match key.trim() {
                    "version" => &mut version,
                    "CID" => &mut cid,
                    "parentCID" => &mut parent,
                    "createType" => &mut kind,
                    key if key.eq_ignore_ascii_case("ddb.uuid.parent") => &mut uuids.parent,
                    key if key.eq_ignore_ascii_case("ddb.uuid.parentmodification") => {
                        &mut uuids.parent_modification
                    }
                    key if chain && key.eq_ignore_ascii_case("ddb.uuid.image") => &mut uuids.image,
                    key if chain && key.eq_ignore_ascii_case("ddb.uuid.modification") => {
                        &mut uuids.modification
                    }
                    "parentFileNameHint" => &mut hint,
                    _ => continue,
                };
                if slot.replace(value.trim()).is_some() {
                    return Err(invalid("duplicate VMDK descriptor field"));
                }
            } else {
                if extents.len() >= 2048 {
                    return Err(Error::Limit("VMDK descriptor exceeds 2048 extents".into()));
                }
                extents.push(extent(line)?);
            }
        }
        // VirtualBox can omit the filename hint and leave parentCID at ffffffff even for
        // differencing VMDKs. Its parent UUID still makes free grains depend on an ancestor.
        if !chain
            && (hint.is_some()
                || [uuids.parent, uuids.parent_modification]
                    .into_iter()
                    .flatten()
                    .any(|v| v != "\"00000000-0000-0000-0000-000000000000\""))
        {
            return Err(Error::Unsupported(
                "VMDK records a parent; select all parents through the base".into(),
            ));
        }
        let kind = kind
            .and_then(|s| s.strip_prefix('"')?.strip_suffix('"'))
            .ok_or_else(|| invalid("missing VMDK createType"))?;
        let cid = cid
            .filter(|s| (1..=8).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_hexdigit()))
            .ok_or_else(|| {
                Error::Unsupported("VMDK descriptor is incomplete or requires a parent".into())
            })?;
        if !valid_padding(text, &bytes[end..], cid) {
            return Err(invalid("VMDK descriptor has data after NUL"));
        }
        let parent = parent.ok_or_else(|| invalid("missing VMDK parent CID"))?;
        if version != Some("1")
            || extents.is_empty()
            || (!chain && !parent.eq_ignore_ascii_case("ffffffff"))
        {
            return Err(Error::Unsupported(
                "VMDK descriptor is incomplete or requires a parent".into(),
            ));
        }
        let lineage = if chain {
            Some(uuids.lineage(hex_id(cid)?, hex_id(parent)?, hint)?)
        } else {
            None
        };
        Ok(Self {
            kind,
            extents,
            lineage,
        })
    }
}
