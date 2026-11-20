//! Contiguous item-based HEIF still images with explicit local media extents.
mod coded;
use super::{Format, bmff::*, container::Input};
use crate::{Error, Result, source::Image};
use std::{
    collections::{BTreeMap, BTreeSet},
    ops::Range,
    sync::atomic::AtomicBool,
};

const MAX_ITEMS: usize = 4096;
fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("HEIF: {reason}"))
}
fn unsupported(reason: &str) -> Error {
    Error::Unsupported(format!("HEIF: {reason}"))
}

pub(super) fn signature(bytes: &[u8]) -> Option<Format<'static>> {
    if bytes.get(4..8)? != b"ftyp" {
        return None;
    }
    let n = usize::try_from(be32(bytes, 0)).ok()?;
    if !(16..=4096).contains(&n) || !n.is_multiple_of(4) {
        return None;
    }
    let major = bytes.get(8..12)?;
    match major {
        b"heic" | b"heix" => Some(Format::Heic),
        b"avif" => Some(Format::Avif),
        b"mif1" => bytes
            .get(16..n)?
            .as_chunks::<4>()
            .0
            .iter()
            .find_map(|b| match b {
                b"heic" | b"heix" => Some(Format::Heic),
                b"avif" => Some(Format::Avif),
                _ => None,
            }),
        _ => None,
    }
}

struct Fields<'a> {
    rest: &'a [u8],
}
impl<'a> Fields<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (head, rest) = self
            .rest
            .split_at_checked(n)
            .ok_or_else(|| invalid("truncated item metadata"))?;
        self.rest = rest;
        Ok(head)
    }
    fn num(&mut self, n: usize) -> Result<usize> {
        if n > 8 {
            return Err(invalid("integer width"));
        }
        let mut bytes = [0; 8];
        bytes[8 - n..].copy_from_slice(self.take(n)?);
        usize::try_from(u64::from_be_bytes(bytes)).map_err(|_| invalid("integer too large"))
    }
    fn finish(self) -> Result<()> {
        if self.rest.is_empty() {
            Ok(())
        } else {
            Err(invalid("unexpected trailing fields"))
        }
    }
}
fn full(bytes: &[u8], b: BoxRef) -> Result<(usize, usize, Fields<'_>)> {
    let mut f = Fields {
        rest: &bytes[b.data..b.end],
    };
    Ok((f.num(1)?, f.num(3)?, f))
}
fn inside(b: BoxRef, fields: &Fields<'_>) -> BoxRef {
    BoxRef {
        data: b.end - fields.rest.len(),
        ..b
    }
}
fn dim(w: usize, h: usize) -> Result<(usize, usize)> {
    if w == 0 || h == 0 || w > 65535 || h > 65535 {
        Err(invalid("image dimensions"))
    } else {
        Ok((w, h))
    }
}

struct Item {
    kind: [u8; 4],
    extent: Option<Range<usize>>,
    dimensions: Option<(usize, usize)>,
    codec: Option<BoxRef>,
    tiles: Vec<usize>,
}
type Items = BTreeMap<usize, Item>;

fn items(bytes: &[u8], boxref: BoxRef, format: Format<'_>) -> Result<Items> {
    let (ver, flags, mut f) = full(bytes, boxref)?;
    if ver > 1 || flags != 0 {
        return Err(unsupported("iinf version or flags"));
    }
    let count = f.num(if ver == 0 { 2 } else { 4 })?;
    if !(1..=MAX_ITEMS).contains(&count) {
        return Err(invalid("item count"));
    }
    let mut items = Items::new();
    for b in children(bytes, inside(boxref, &f))? {
        if &b.kind != b"infe" {
            return Err(invalid("unexpected item declaration"));
        }
        let (ver, flags, mut r) = full(bytes, b)?;
        if !matches!(ver, 2 | 3) || flags & !1 != 0 {
            return Err(unsupported("infe version or flags"));
        }
        let id = r.num(if ver == 2 { 2 } else { 4 })?;
        if id == 0 || r.num(2)? != 0 {
            return Err(unsupported("zero identifier or protected item"));
        }
        let kind: [u8; 4] = r.take(4)?.try_into().unwrap();
        if !matches!(
            (&kind, format),
            (b"grid" | b"Exif" | b"mime", _) | (b"hvc1", Format::Heic) | (b"av01", Format::Avif)
        ) {
            return Err(unsupported("image item type or mixed codecs"));
        }
        if !r.rest.contains(&0) {
            return Err(invalid("unterminated item name"));
        }
        if items
            .insert(
                id,
                Item {
                    kind,
                    extent: None,
                    dimensions: None,
                    codec: None,
                    tiles: Vec::new(),
                },
            )
            .is_some()
        {
            return Err(invalid("duplicate item identifier"));
        }
    }
    if items.len() != count {
        return Err(invalid("item declaration count"));
    }
    Ok(items)
}
fn locations(
    bytes: &[u8],
    loc: BoxRef,
    items: &mut Items,
    media: &Range<usize>,
    idat: Option<BoxRef>,
) -> Result<()> {
    let (ver, flags, mut f) = full(bytes, loc)?;
    if ver > 2 || flags != 0 {
        return Err(unsupported("iloc version or flags"));
    }
    let a = f.num(1)?;
    let b = f.num(1)?;
    let (off, len, base, index) = (a >> 4, a & 15, b >> 4, b & 15);
    if ![off, len, base, index]
        .iter()
        .all(|v| matches!(v, 0 | 4 | 8))
        || len == 0
        || (ver == 0 && index != 0)
    {
        return Err(unsupported("iloc field widths or implicit length"));
    }
    let id_width = if ver < 2 { 2 } else { 4 };
    if f.num(id_width)? != items.len() {
        return Err(invalid("iloc item count"));
    }
    let mut media_spans = Vec::new();
    let mut local_spans = Vec::new();
    for _ in 0..items.len() {
        let id = f.num(id_width)?;
        let method = if ver == 0 { 0 } else { f.num(2)? };
        if f.num(2)? != 0 {
            return Err(unsupported("external item data reference"));
        }
        let origin = f.num(base)?;
        if f.num(2)? != 1 {
            return Err(unsupported("each item must have one local extent"));
        }
        if index != 0 && f.num(index)? != 0 {
            return Err(unsupported("indexed item extent"));
        }
        let relative = origin
            .checked_add(f.num(off)?)
            .ok_or_else(|| invalid("offset overflow"))?;
        let size = f.num(len)?;
        let (start, allowed) = match method {
            0 => (relative, media.clone()),
            1 => {
                let d = idat.ok_or_else(|| invalid("idat is missing"))?;
                (
                    d.data
                        .checked_add(relative)
                        .ok_or_else(|| invalid("idat offset overflow"))?,
                    d.data..d.end,
                )
            }
            _ => return Err(unsupported("item construction method")),
        };
        let end = start
            .checked_add(size)
            .ok_or_else(|| invalid("item length overflow"))?;
        if size == 0 || start < allowed.start || end > allowed.end {
            return Err(invalid("item outside media box"));
        }
        let item = items
            .get_mut(&id)
            .ok_or_else(|| invalid("undeclared item location"))?;
        if item.extent.replace(start..end).is_some() {
            return Err(invalid("duplicate item location"));
        }
        if method == 0 {
            media_spans.push((start, end));
        } else {
            local_spans.push((start, end));
        }
    }
    f.finish()?;
    coverage(media_spans, media.clone())?;
    if let Some(d) = idat {
        coverage(local_spans, d.data..d.end)?;
    }
    Ok(())
}
fn coverage(mut spans: Vec<(usize, usize)>, media: Range<usize>) -> Result<()> {
    spans.sort_unstable();
    let mut end = media.start;
    for (a, b) in spans {
        if a != end {
            return Err(invalid("overlapping or unreferenced media bytes"));
        }
        end = b;
    }
    if end != media.end {
        return Err(invalid("incomplete media coverage"));
    }
    Ok(())
}
fn properties(bytes: &[u8], iprp: BoxRef, items: &mut Items) -> Result<()> {
    let boxes = children(bytes, iprp)?;
    let props = children(bytes, one(&boxes, b"ipco")?)?;
    let assoc = one(&boxes, b"ipma")?;
    let (ver, flags, mut f) = full(bytes, assoc)?;
    if ver > 1 || flags > 1 || props.len() > MAX_ITEMS {
        return Err(unsupported("property association version or count"));
    }
    let count = f.num(4)?;
    if count > items.len() {
        return Err(invalid("property item count"));
    }
    let mut seen = BTreeSet::new();
    for _ in 0..count {
        let id = f.num(if ver == 0 { 2 } else { 4 })?;
        if !seen.insert(id) {
            return Err(invalid("duplicate property associations"));
        }
        let item = items
            .get_mut(&id)
            .ok_or_else(|| invalid("unknown property item"))?;
        let width = flags + 1;
        let essential = 1 << (width * 8 - 1);
        for _ in 0..f.num(1)? {
            let value = f.num(width)?;
            let at = value & (essential - 1);
            if value == 0 {
                continue;
            }
            let prop = at
                .checked_sub(1)
                .and_then(|n| props.get(n))
                .ok_or_else(|| invalid("property index"))?;
            let data = &bytes[prop.data..prop.end];
            match &prop.kind {
                b"ispe" => {
                    if data.len() != 12 || data[..4] != [0; 4] || item.dimensions.is_some() {
                        return Err(invalid("ispe dimensions or duplicate"));
                    }
                    item.dimensions = Some(dim(be32(data, 4) as usize, be32(data, 8) as usize)?);
                }
                b"hvcC" | b"av1C" => {
                    if item.codec.replace(*prop).is_some() {
                        return Err(invalid("duplicate codec configuration"));
                    }
                }
                b"pixi" | b"colr" | b"clap" | b"irot" | b"imir" | b"auxC" | b"pasp" => {}
                _ if value & essential != 0 => {
                    return Err(unsupported("unknown essential image property"));
                }
                _ => {}
            }
        }
    }
    f.finish()
}
fn references(bytes: &[u8], iref: BoxRef, items: &mut Items) -> Result<()> {
    let (ver, flags, f) = full(bytes, iref)?;
    if ver > 1 || flags != 0 {
        return Err(unsupported("iref version or flags"));
    }
    let width = if ver == 0 { 2 } else { 4 };
    for r in children(bytes, inside(iref, &f))? {
        if !matches!(&r.kind, b"dimg" | b"thmb" | b"auxl" | b"cdsc" | b"prem") {
            return Err(unsupported("item reference type"));
        }
        let mut refs = Fields {
            rest: &bytes[r.data..r.end],
        };
        let from = refs.num(width)?;
        let count = refs.num(2)?;
        if !items.contains_key(&from) || count == 0 || count > MAX_ITEMS {
            return Err(invalid("reference source or count"));
        }
        let mut targets = Vec::new();
        for _ in 0..count {
            let to = refs.num(width)?;
            if to == from || !items.contains_key(&to) {
                return Err(invalid("missing or self-referencing image item"));
            }
            if &r.kind == b"dimg" {
                targets.push(to);
            }
        }
        refs.finish()?;
        if &r.kind == b"dimg" {
            let item = items.get_mut(&from).unwrap();
            if &item.kind != b"grid" || !item.tiles.is_empty() {
                return Err(invalid("grid reference source"));
            }
            item.tiles = targets;
        }
    }
    Ok(())
}
fn grid(data: &[u8], item: &Item, items: &Items) -> Result<()> {
    let mut f = Fields { rest: data };
    if f.num(1)? != 0 {
        return Err(unsupported("grid version"));
    }
    let width = match f.num(1)? {
        0 => 2,
        1 => 4,
        _ => return Err(invalid("grid flags")),
    };
    let rows = f.num(1)? + 1;
    let cols = f.num(1)? + 1;
    let (w, h) = dim(f.num(width)?, f.num(width)?)?;
    f.finish()?;
    if rows * cols != item.tiles.len() || item.dimensions != Some((w, h)) {
        return Err(invalid("grid dimensions or tile count"));
    }
    let mut shape = None;
    for key in &item.tiles {
        let tile = &items[key];
        if !matches!(&tile.kind, b"hvc1" | b"av01")
            || tile.dimensions.is_none()
            || shape.is_some() && shape != tile.dimensions
        {
            return Err(invalid("grid tile type or dimensions"));
        }
        shape = tile.dimensions;
    }
    let (tw, th) = shape.ok_or_else(|| invalid("empty tile grid"))?;
    if w <= tw * (cols - 1) || h <= th * (rows - 1) || w > tw * cols || h > th * rows {
        return Err(invalid("grid output exceeds its tiles"));
    }
    Ok(())
}

fn optional(boxes: &[BoxRef], kind: &[u8; 4]) -> Result<Option<BoxRef>> {
    let mut it = boxes.iter().filter(|b| &b.kind == kind);
    let first = it.next().copied();
    if it.next().is_some() {
        return Err(invalid("duplicate optional box"));
    }
    Ok(first)
}
fn validate_meta(
    bytes: &[u8],
    meta: BoxRef,
    media: Range<usize>,
    format: Format<'_>,
) -> Result<()> {
    let (ver, flags, f) = full(bytes, meta)?;
    if ver != 0 || flags != 0 {
        return Err(unsupported("meta version or flags"));
    }
    let boxes = children(bytes, inside(meta, &f))?;
    let h = one(&boxes, b"hdlr")?;
    let (ver, flags, mut h) = full(bytes, h)?;
    if ver != 0 || flags != 0 || h.num(4)? != 0 || h.take(4)? != b"pict" {
        return Err(invalid("picture handler"));
    }
    if h.take(12)? != [0; 12] {
        return Err(invalid("picture handler reserved fields"));
    }
    if boxes.iter().any(|b| &b.kind == b"ipro") {
        return Err(unsupported("protected media"));
    }
    let (ver, flags, mut p) = full(bytes, one(&boxes, b"pitm")?)?;
    if ver > 1 || flags != 0 {
        return Err(unsupported("primary item version"));
    }
    let primary = p.num(if ver == 0 { 2 } else { 4 })?;
    p.finish()?;
    let mut entries = items(bytes, one(&boxes, b"iinf")?, format)?;
    if !entries
        .get(&primary)
        .is_some_and(|i| matches!(&i.kind, b"grid" | b"hvc1" | b"av01"))
    {
        return Err(invalid("missing primary image"));
    }
    locations(
        bytes,
        one(&boxes, b"iloc")?,
        &mut entries,
        &media,
        optional(&boxes, b"idat")?,
    )?;
    properties(bytes, one(&boxes, b"iprp")?, &mut entries)?;
    if let Some(r) = optional(&boxes, b"iref")? {
        references(bytes, r, &mut entries)?;
    }
    let mut config_bytes = 0usize;
    for item in entries.values() {
        let payload = &bytes[item
            .extent
            .clone()
            .ok_or_else(|| invalid("missing item extent"))?];
        if matches!(&item.kind, b"Exif" | b"mime") {
            continue;
        }
        if item.dimensions.is_none() {
            return Err(invalid("missing image dimensions"));
        }
        if &item.kind == b"grid" {
            grid(payload, item, &entries)?;
            continue;
        }
        let c = item
            .codec
            .ok_or_else(|| invalid("missing configuration of coded image"))?;
        // Multiple images can share a configuration property. Bound repeated parsing too.
        config_bytes = config_bytes
            .checked_add(c.end - c.data)
            .filter(|n| *n <= super::container::MAX_ENCODED)
            .ok_or_else(|| Error::Limit("HEIF configuration validation budget".into()))?;
        match (&item.kind, &c.kind) {
            (b"hvc1", b"hvcC") => coded::hevc(&bytes[c.data..c.end], payload)?,
            (b"av01", b"av1C") => coded::av1(&bytes[c.data..c.end], payload)?,
            _ => return Err(invalid("item and codec configuration disagree")),
        }
    }
    Ok(())
}
pub(super) fn validate(
    format: Format<'_>,
    image: &mut Image,
    at: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
    boundary: u64,
) -> Result<Vec<u8>> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    let mut pos = 0;
    let mut meta = None;
    for _ in 0..128 {
        input.ensure(pos + 8)?;
        let kind: [u8; 4] = input.bytes[pos + 4..pos + 8].try_into().unwrap();
        let mut length = be32(&input.bytes, pos) as u64;
        if length == 1 {
            input.ensure(pos + 16)?;
            length = be64(&input.bytes, pos + 8);
        }
        let end = usize::try_from(length)
            .ok()
            .and_then(|n| pos.checked_add(n))
            .filter(|v| *v <= input.limit() && *v >= pos + 8)
            .ok_or_else(|| invalid("unbounded or truncated top-level box"))?;
        if !matches!(&kind, b"ftyp" | b"meta" | b"mdat" | b"free" | b"skip") {
            return Err(unsupported("top-level box in still image"));
        }
        input.ensure(end)?;
        let b = header(&input.bytes, pos, end)?;
        match &kind {
            b"ftyp" if pos == 0 && signature(&input.bytes) == Some(format) => {
                if input.bytes[b.data + 8..b.end]
                    .as_chunks::<4>()
                    .0
                    .iter()
                    .any(|c| matches!(c, b"avis" | b"msf1" | b"hevc" | b"hevx"))
                {
                    return Err(unsupported("timed image sequences"));
                }
            }
            b"meta" if pos > 0 && meta.is_none() => {
                meta = Some(b);
            }
            b"free" | b"skip" if pos > 0 => {}
            b"mdat" if pos > 0 => {
                let m = meta.ok_or_else(|| invalid("item metadata must precede media"))?;
                input.work(end)?;
                validate_meta(&input.bytes, m, b.data..b.end, format)?;
                return input.finish(end);
            }
            _ => return Err(invalid("unexpected or duplicate top-level box")),
        }
        pos = end;
    }
    Err(Error::Limit("HEIF box count".into()))
}
