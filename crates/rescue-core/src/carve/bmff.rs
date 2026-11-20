//! Bounded ISO Base Media File Format box ranges.
use super::container::usize64;
use crate::{Error, Result};
fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("ISO BMFF: {reason}"))
}
pub(super) fn be32(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(b[p..p + 4].try_into().unwrap())
}
pub(super) fn be64(b: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(b[p..p + 8].try_into().unwrap())
}
#[derive(Clone, Copy)]
pub(super) struct BoxRef {
    pub kind: [u8; 4],
    pub data: usize,
    pub end: usize,
}
pub(super) fn header(b: &[u8], p: usize, boundary: usize) -> Result<BoxRef> {
    if p.checked_add(8).is_none_or(|n| n > boundary) || boundary > b.len() {
        return Err(invalid("truncated box header"));
    }
    let size = be32(b, p);
    let kind = b[p + 4..p + 8].try_into().unwrap();
    let mut data = p + 8;
    let size = if size == 1 {
        if p + 16 > boundary {
            return Err(invalid("extended box size"));
        }
        data += 8;
        usize64(be64(b, p + 8))?
    } else if size == 0 {
        return Err(Error::Unsupported(
            "ISO BMFF zero-sized box has no independent carving boundary".into(),
        ));
    } else {
        size as usize
    };
    let end = p
        .checked_add(size)
        .filter(|n| *n <= boundary && *n >= data)
        .ok_or_else(|| invalid("box extends beyond parent"))?;
    if &kind == b"uuid" {
        data += 16;
        if data > end {
            return Err(invalid("UUID box header"));
        }
    }
    Ok(BoxRef { kind, data, end })
}
pub(super) fn children(b: &[u8], parent: BoxRef) -> Result<Vec<BoxRef>> {
    let mut p = parent.data;
    let mut boxes = Vec::new();
    while p < parent.end {
        if boxes.len() >= 65536 {
            return Err(Error::Limit("ISO BMFF box count".into()));
        }
        let item = header(b, p, parent.end)?;
        p = item.end;
        boxes.push(item);
    }
    Ok(boxes)
}
pub(super) fn one(items: &[BoxRef], kind: &[u8; 4]) -> Result<BoxRef> {
    let mut found = items.iter().filter(|x| &x.kind == kind);
    let item = *found
        .next()
        .ok_or_else(|| invalid("required box missing"))?;
    if found.next().is_some() {
        return Err(invalid("duplicate singleton box"));
    }
    Ok(item)
}
