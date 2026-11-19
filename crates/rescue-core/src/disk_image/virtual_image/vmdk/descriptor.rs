//! Parse descriptor extent lines as data. File access is bound to caller-supplied handles.
use super::{Error, Result, invalid, sectors};

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
}

fn extent(line: &str) -> Result<Extent<'_>> {
    let (fields, name) = match line.split_once('"') {
        Some((before, rest)) => {
            let (name, after) = rest
                .split_once('"')
                .ok_or_else(|| invalid("unterminated VMDK extent name"))?;
            if name.is_empty()
                || name.contains(['/', '\\', ':'])
                || matches!(name, "." | "..")
                || name.chars().any(char::is_control)
            {
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

impl<'a> Descriptor<'a> {
    pub fn parse(bytes: &'a [u8]) -> Result<Self> {
        let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
        if bytes[end..].iter().any(|&b| b != 0) {
            return Err(invalid("VMDK descriptor has data after NUL"));
        }
        let text =
            std::str::from_utf8(&bytes[..end]).map_err(|_| invalid("non-UTF-8 VMDK descriptor"))?;
        let (mut version, mut cid, mut parent, mut kind) = (None, None, None, None);
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
                    "parentFileNameHint" => {
                        return Err(Error::Unsupported("VMDK parent reference".into()));
                    }
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
        let kind = kind
            .and_then(|s| s.strip_prefix('"')?.strip_suffix('"'))
            .ok_or_else(|| invalid("missing VMDK createType"))?;
        if version != Some("1")
            || !cid.is_some_and(|s| {
                (1..=8).contains(&s.len()) && s.bytes().all(|c| c.is_ascii_hexdigit())
            })
            || !parent.is_some_and(|s| s.eq_ignore_ascii_case("ffffffff"))
            || extents.is_empty()
        {
            return Err(Error::Unsupported(
                "VMDK descriptor is incomplete or requires a parent".into(),
            ));
        }
        Ok(Self { kind, extents })
    }
}
