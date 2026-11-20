//! Contiguous classic TIFF and BigTIFF pages with bounded strips or tiles.
mod dng;
use super::{Format, charge, container::Input};
use crate::{Error, Result, source::Image};
use std::{
    collections::BTreeSet,
    io::Cursor,
    sync::atomic::{AtomicBool, Ordering},
};
use tiff::decoder::{Decoder, Limits};

fn invalid(msg: &str) -> Error {
    Error::Invalid(format!("TIFF: {msg}"))
}
const MAX_PIXELS_BYTES: usize = 128 * 1024 * 1024;
const MAX_EXTENTS: usize = 65536;

pub(super) fn signature(b: &[u8]) -> bool {
    matches!(
        b.get(..4),
        Some(b"II\x2a\0" | b"MM\0\x2a" | b"II\x2b\0" | b"MM\0\x2b")
    )
}

/// A root IFD outside the scan window must be inspected by the bounded reader.
pub(super) fn dng_hint(bytes: &[u8]) -> Option<bool> {
    if !matches!(bytes.get(..4), Some(b"II\x2a\0" | b"MM\0\x2a")) {
        return Some(false);
    }
    let layout = Layout {
        little: bytes[0] == b'I',
        big: false,
    };
    let root = usize::try_from(layout.number(bytes.get(4..8)?)).ok()?;
    let start = root.checked_add(2)?;
    let count = layout.number(bytes.get(root..start)?) as usize;
    if count > 4096 {
        return Some(false);
    }
    let rows = bytes.get(start..start.checked_add(count * 12)?)?;
    Some(
        rows.as_chunks::<12>()
            .0
            .iter()
            .any(|r| layout.number(&r[..2]) == 50706),
    )
}

#[derive(Copy, Clone)]
struct Layout {
    little: bool,
    big: bool,
}
impl Layout {
    fn number(self, b: &[u8]) -> u64 {
        if self.little {
            b.iter()
                .rev()
                .fold(0, |value, &next| (value << 8) | u64::from(next))
        } else {
            b.iter()
                .fold(0, |value, &next| (value << 8) | u64::from(next))
        }
    }
    fn offset(self, b: &[u8], limit: usize) -> Result<usize> {
        usize::try_from(self.number(b))
            .ok()
            .filter(|n| *n <= limit)
            .ok_or_else(|| invalid("offset or size outside carving range"))
    }
    fn width(self) -> usize {
        if self.big { 8 } else { 4 }
    }
}

fn known_tag(tag: u64) -> bool {
    matches!(tag,254..=259 | 262..=266 | 269..=274 | 277..=287 | 290..=293 | 296..=297 |
        301 | 305..=306 | 315..=321 | 322..=325 | 332..=334 | 336..=342 | 347 |
        529..=532 | 700 | 33432 | 34675)
}

#[derive(Copy, Clone, PartialEq, Eq, PartialOrd, Ord)]
enum RangeKind {
    Directory,
    Values,
    Pixels,
}

fn checked_end(start: usize, size: usize, limit: usize) -> Result<usize> {
    start
        .checked_add(size)
        .filter(|n| *n <= limit)
        .ok_or_else(|| invalid("referenced range exceeds source"))
}

struct Extents {
    ranges: Vec<(usize, usize, RangeKind)>,
    seen: BTreeSet<usize>,
    pages: usize,
}

impl Extents {
    fn add(&mut self, start: usize, end: usize, kind: RangeKind) -> Result<()> {
        if self.ranges.len() >= MAX_EXTENTS {
            return Err(Error::Limit("TIFF exceeds 65536 referenced extents".into()));
        }
        if start >= end {
            return Err(invalid("empty range"));
        }
        self.ranges.push((start, end, kind));
        Ok(())
    }

    fn page(&mut self, input: &mut Input<'_>, layout: Layout, offset: usize) -> Result<usize> {
        if !self.seen.insert(offset) {
            return Err(invalid("cyclic directory chain"));
        }
        self.pages += 1;
        if self.pages > 256 {
            return Err(Error::Limit("TIFF exceeds 256 pages".into()));
        }
        let width = layout.width();
        let count_size = if layout.big { 8 } else { 2 };
        let stride = 4 + width * 2;
        input.ensure(offset + count_size)?;
        let count = layout.offset(&input.bytes[offset..offset + count_size], 4096)?;
        let table = offset + count_size;
        let next = table + stride * count;
        input.ensure(next + width)?;
        self.add(offset, next + width, RangeKind::Directory)?;
        let mut tags = BTreeSet::new();
        let mut offsets = Vec::new();
        let mut counts = Vec::new();
        for i in 0..count {
            input.check()?;
            let pos = table + i * stride;
            let row = &input.bytes[pos..pos + stride];
            let tag = layout.number(&row[..2]);
            if !known_tag(tag) {
                return Err(Error::Unsupported(format!(
                    "TIFF tag {tag} may reference additional data"
                )));
            }
            if !tags.insert(tag) {
                return Err(invalid("duplicate tag"));
            }
            let ty = layout.number(&row[2..4]);
            let elem = match ty {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 => 8,
                16 | 17 if layout.big => 8,
                _ => {
                    return Err(Error::Unsupported(
                        "TIFF field type or subsidiary IFD".into(),
                    ));
                }
            };
            let n = layout.offset(&row[4..4 + width], input.limit())?;
            let length = n
                .checked_mul(elem)
                .filter(|v| *v <= input.limit())
                .ok_or_else(|| invalid("field extent"))?;
            let inline = pos + 4 + width;
            let start = if length <= width {
                inline
            } else {
                let p = layout.offset(&row[4 + width..], input.limit())?;
                let end = checked_end(p, length, input.limit())?;
                self.add(p, end, RangeKind::Values)?;
                input.ensure(end)?;
                p
            };
            if matches!(tag, 273 | 279 | 324 | 325) {
                if !matches!(ty, 3 | 4 | 16) || n == 0 || n > MAX_EXTENTS {
                    return Err(invalid("strip or tile geometry array"));
                }
                let values = if matches!(tag, 273 | 324) {
                    &mut offsets
                } else {
                    &mut counts
                };
                if !values.is_empty() {
                    return Err(invalid("mixed strip and tile arrays"));
                }
                for item in input.bytes[start..start + length].chunks_exact(elem) {
                    values.push(layout.offset(item, input.limit())?);
                }
            }
        }
        if offsets.is_empty()
            || offsets.len() != counts.len()
            || tags.contains(&273) != tags.contains(&279)
            || tags.contains(&324) != tags.contains(&325)
        {
            return Err(invalid("inconsistent strip or tile tables"));
        }
        for (start, size) in offsets.into_iter().zip(counts) {
            let end = checked_end(start, size, input.limit())?;
            self.add(start, end, RangeKind::Pixels)?;
        }
        layout.offset(&input.bytes[next..next + width], input.limit())
    }

    fn finish(mut self) -> Result<(usize, usize)> {
        self.ranges.sort_unstable();
        self.ranges.dedup();
        if self.ranges.windows(2).any(|r| r[0].1 > r[1].0) {
            return Err(invalid("overlapping directories, values or pixel data"));
        }
        Ok((self.ranges.last().unwrap().1, self.pages))
    }
}

pub(super) fn validate(
    image: &mut Image,
    at: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
    boundary: u64,
    require_dng: bool,
) -> Result<(Format<'static>, Vec<u8>)> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    input.ensure(8)?;
    if !signature(&input.bytes) {
        return Err(invalid("byte order or version"));
    }
    let mut layout = Layout {
        little: input.bytes[0] == b'I',
        big: false,
    };
    layout.big = layout.number(&input.bytes[2..4]) == 43;
    let head = if layout.big {
        input.ensure(16)?;
        if layout.number(&input.bytes[4..6]) != 8 || input.bytes[6..8] != [0, 0] {
            return Err(invalid("BigTIFF offset width or reserved field"));
        }
        16
    } else {
        8
    };
    let mut offset = layout.offset(&input.bytes[head - layout.width()..head], input.limit())?;
    let mut extents = Extents {
        ranges: vec![(0, head, RangeKind::Directory)],
        seen: BTreeSet::new(),
        pages: 0,
    };
    if offset == 0 {
        return Err(invalid("no image directories"));
    }
    if !layout.big {
        input.ensure(checked_end(offset, 2, input.limit())?)?;
        let count = layout.offset(&input.bytes[offset..offset + 2], 4096)?;
        input.ensure(checked_end(offset + 2, count * 12, input.limit())?)?;
        if dng_hint(&input.bytes) == Some(true) {
            let bytes = dng::validate(input, layout, offset, extents)?;
            return Ok((Format::Dng, bytes));
        }
    }
    if require_dng {
        return Err(invalid("missing classic DNG root directory"));
    }
    while offset != 0 {
        offset = extents.page(&mut input, layout, offset)?;
    }
    let (end, pages) = extents.finish()?;
    let bytes = input.finish(end)?;
    let err = |e: tiff::TiffError| invalid(&e.to_string());
    let mut limits = Limits::default();
    limits.decoding_buffer_size = MAX_PIXELS_BYTES;
    limits.intermediate_buffer_size = 16 * 1024 * 1024;
    let mut decoder = Decoder::new(Cursor::new(bytes.as_slice()))
        .map_err(err)?
        .with_limits(limits);
    let mut remaining = MAX_PIXELS_BYTES;
    for i in 0..pages {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let layout = decoder.image_buffer_layout().map_err(err)?;
        if layout.complete_len == 0 || layout.complete_len > remaining {
            return Err(Error::Limit("TIFF decoded pages exceed 128 MiB".into()));
        }
        charge(budget, layout.complete_len)?;
        remaining -= layout.complete_len;
        // read_image_bytes fills every plane when given complete_len bytes.
        let mut pixels = vec![0; layout.complete_len];
        decoder.read_image_bytes(&mut pixels).map_err(err)?;
        if decoder.more_images() != (i + 1 < pages) {
            return Err(invalid("decoder directory count differs"));
        }
        if i + 1 < pages {
            decoder.next_image().map_err(err)?;
        }
    }
    Ok((Format::Tiff, bytes))
}
