//! Classic DNG directory trees and referenced Bayer sensor data.
use super::{Extents, Input, Layout, MAX_EXTENTS, RangeKind, checked_end, invalid, known_tag};
use crate::{Error, Result};
use std::{collections::BTreeMap, ops::Range};

const MAX_SAMPLES: usize = 32 * 1024 * 1024;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Directory {
    Root,
    Image,
    Exif,
    Gps,
    Interop,
}

impl Directory {
    fn accepts(self, tag: u16) -> bool {
        match self {
            // Only scalar values or self-contained arrays are accepted here.
            // ExtraCameraProfiles, MakerNote, private offsets and embedded DNGs
            // require separate interpretation before their extent is known.
            Self::Root | Self::Image => {
                known_tag(u64::from(tag))
                    || matches!(tag,
                330 | 339 | 34665 | 34853 | 33421 | 33422 | 34858 | 37393 | 37398 |
                50706..=50736 | 50738..=50739 | 50741 | 50778..=50781 | 50827 |
                50829..=50834 | 50931..=50932 | 50936..=50942 | 50964..=50972 |
                50981..=50982 | 51008..=51009 | 51022 | 51041 | 51043..=51044 |
                51058 | 51081 | 51109 | 51111)
            }
            Self::Exif => matches!(tag, 33434 | 33437 | 34850 | 34855 | 34864..=34869 |
                36864 | 36867..=36868 | 36880..=36882 | 37121..=37122 | 37377..=37386 |
                37510 | 37520..=37522 | 40960..=40965 | 41486..=41488 | 41495 | 41728..=41730 |
                41985..=41996 | 42016 | 42032..=42037 | 42240),
            Self::Gps => tag <= 31,
            Self::Interop => matches!(tag, 1 | 2 | 4096..=4098),
        }
    }
}

struct Field {
    data: Range<usize>,
    ty: u16,
    count: usize,
}

struct Fields {
    table: BTreeMap<u16, Field>,
    layout: Layout,
}

impl Fields {
    fn numbers(&self, tag: u16, input: &Input<'_>, limit: usize) -> Result<Vec<usize>> {
        let f = self
            .table
            .get(&tag)
            .ok_or_else(|| invalid("DNG required tag missing"))?;
        let width = match f.ty {
            1 => 1,
            3 => 2,
            4 | 13 => 4,
            _ => return Err(invalid("DNG unsigned integer type")),
        };
        if f.count == 0 || f.count > limit {
            return Err(invalid("DNG number of values"));
        }
        input.bytes[f.data.clone()]
            .chunks_exact(width)
            .map(|b| {
                usize::try_from(self.layout.number(b)).map_err(|_| invalid("DNG field overflow"))
            })
            .collect()
    }

    fn one(&self, tag: u16, default: usize, input: &Input<'_>) -> Result<usize> {
        if self.table.contains_key(&tag) {
            Ok(self.numbers(tag, input, 1)?[0])
        } else {
            Ok(default)
        }
    }

    fn read(
        input: &mut Input<'_>,
        extents: &mut Extents,
        layout: Layout,
        offset: usize,
        kind: Directory,
    ) -> Result<(Self, usize)> {
        let start = checked_end(offset, 2, input.limit())?;
        input.ensure(start)?;
        let count = layout.offset(&input.bytes[offset..start], 4096)?;
        let next = checked_end(start, count * 12, input.limit())?;
        let end = checked_end(next, 4, input.limit())?;
        input.ensure(end)?;
        extents.add(offset, end, RangeKind::Directory)?;
        let mut table = BTreeMap::new();
        for pos in (start..next).step_by(12) {
            input.check()?;
            let row = &input.bytes[pos..pos + 12];
            let tag = layout.number(&row[..2]) as u16;
            if !kind.accepts(tag) {
                return Err(Error::Unsupported(format!("DNG directory tag {tag}")));
            }
            let ty = layout.number(&row[2..4]) as u16;
            let width = match ty {
                1 | 2 | 6 | 7 => 1,
                3 | 8 => 2,
                4 | 9 | 11 => 4,
                5 | 10 | 12 => 8,
                13 if matches!(tag, 330 | 34665 | 34853 | 40965) => 4,
                _ => return Err(invalid("DNG unknown field type or pointer")),
            };
            let count = layout.offset(&row[4..8], input.limit())?;
            let len = count
                .checked_mul(width)
                .ok_or_else(|| invalid("DNG field size"))?;
            let data = if len <= 4 {
                pos + 8..pos + 8 + len
            } else {
                let at = layout.offset(&row[8..], input.limit())?;
                let end = checked_end(at, len, input.limit())?;
                extents.add(at, end, RangeKind::Values)?;
                input.ensure(end)?;
                at..end
            };
            if table.insert(tag, Field { ty, count, data }).is_some() {
                return Err(invalid("duplicate DNG tag"));
            }
        }
        let next = layout.offset(&input.bytes[next..end], input.limit())?;
        Ok((Self { table, layout }, next))
    }

    fn version(&self, tag: u16, input: &Input<'_>) -> Result<[u8; 4]> {
        let f = self
            .table
            .get(&tag)
            .ok_or_else(|| invalid("DNG version absent"))?;
        if f.ty != 1 || f.count != 4 {
            return Err(invalid("DNG version field type or size"));
        }
        let v: [u8; 4] = input.bytes[f.data.clone()].try_into().unwrap();
        if v[0] != 1 || v[1] > 4 || v[2..] != [0, 0] {
            return Err(Error::Unsupported(format!("DNG version {v:?}")));
        }
        Ok(v)
    }
}

fn checked_product(values: &[usize]) -> Result<usize> {
    values.iter().try_fold(1usize, |product, &n| {
        product
            .checked_mul(n)
            .filter(|&v| v > 0 && v <= MAX_SAMPLES)
            .ok_or_else(|| Error::Limit("DNG exceeds 32 million coded samples".into()))
    })
}

fn image(
    fields: &Fields,
    input: &mut Input<'_>,
    extents: &mut Extents,
    remaining: &mut usize,
) -> Result<bool> {
    let width = fields.one(256, 0, input)?;
    let height = fields.one(257, 0, input)?;
    let photo = fields.one(262, 0, input)?;
    let samples = fields.one(277, 1, input)?;
    let compression = fields.one(259, 1, input)?;
    let sensor = photo == 32803;
    if sensor {
        if fields.one(254, 0, input)? != 0
            || samples != 1
            || fields.numbers(33421, input, 2)? != [2, 2]
        {
            return Err(invalid("DNG full-resolution 2 by 2 CFA layout"));
        }
        let pattern = fields.numbers(33422, input, 4)?;
        if !matches!(
            pattern.as_slice(),
            [0, 1, 1, 2] | [2, 1, 1, 0] | [1, 0, 2, 1] | [1, 2, 0, 1]
        ) || fields.one(50711, 1, input)? != 1
        {
            return Err(invalid("DNG Bayer pattern or layout"));
        }
        if fields.table.contains_key(&50710) && fields.numbers(50710, input, 3)? != [0, 1, 2] {
            return Err(invalid("DNG CFA plane colors"));
        }
    } else if !matches!((photo, samples), (2 | 6, 3)) || fields.one(254, 0, input)? & 1 == 0 {
        return Err(Error::Unsupported(
            "DNG companion image is not an RGB/YCbCr preview".into(),
        ));
    }
    let bits = fields.numbers(258, input, 3)?;
    if bits.len() != samples
        || bits.iter().any(|b| *b != bits[0])
        || !matches!(bits[0], 8 | 10 | 12 | 14 | 16)
        || (!sensor && bits[0] != 8)
        || fields.one(266, 1, input)? != 1
        || fields.one(284, 1, input)? != 1
        || fields.one(317, 1, input)? != 1
        || fields.one(339, 1, input)? != 1
    {
        return Err(Error::Unsupported(
            "DNG bit depth, sample type or storage order".into(),
        ));
    }
    let tiled = fields.table.contains_key(&324);
    let (tw, th, offsets, counts) = if tiled {
        if [273, 278, 279]
            .iter()
            .any(|tag| fields.table.contains_key(tag))
        {
            return Err(invalid("DNG mixed tiles and strips"));
        }
        (
            fields.one(322, 0, input)?,
            fields.one(323, 0, input)?,
            324,
            325,
        )
    } else {
        if [322, 323, 325]
            .iter()
            .any(|tag| fields.table.contains_key(tag))
        {
            return Err(invalid("DNG incomplete tile geometry"));
        }
        (width, fields.one(278, height, input)?.min(height), 273, 279)
    };
    checked_product(&[width, height, samples])?;
    checked_product(&[tw, th, samples])?;
    let across = width.div_ceil(tw);
    let n = across
        .checked_mul(height.div_ceil(th))
        .filter(|v| *v <= MAX_EXTENTS)
        .ok_or_else(|| invalid("DNG chunk count"))?;
    let offsets = fields.numbers(offsets, input, MAX_EXTENTS)?;
    let counts = fields.numbers(counts, input, MAX_EXTENTS)?;
    if offsets.len() != n || counts.len() != n {
        return Err(invalid("DNG strip/tile table dimensions"));
    }
    for (i, (at, len)) in offsets.into_iter().zip(counts).enumerate() {
        let h = if tiled { th } else { th.min(height - i * th) };
        let total = checked_product(&[tw, h, samples])?;
        *remaining = remaining
            .checked_sub(total)
            .ok_or_else(|| invalid("DNG aggregate sample limit"))?;
        let end = checked_end(at, len, input.limit())?;
        extents.add(at, end, RangeKind::Pixels)?;
        input.ensure(end)?;
        input.work(total)?;
        match compression {
            1 if (sensor || photo == 2) && len == (tw * samples * bits[0]).div_ceil(8) * h => (),
            7 if sensor => super::super::jpeg::check_cfa_tile(
                &input.bytes[at..end],
                total,
                bits[0],
                input.cancel_flag(),
            )?,
            7 => input.check_jpeg_preview(at..end, (tw, h))?,
            _ => {
                return Err(Error::Unsupported(
                    "DNG compression or coded chunk length".into(),
                ));
            }
        }
    }
    Ok(sensor)
}

pub(super) fn validate(
    mut input: Input<'_>,
    layout: Layout,
    root: usize,
    mut extents: Extents,
) -> Result<Vec<u8>> {
    let mut stack = vec![(root, Directory::Root, 0usize)];
    let mut sensors = 0;
    let mut remaining = MAX_SAMPLES;
    while let Some((offset, kind, depth)) = stack.pop() {
        if depth > 12 || extents.seen.len() >= 64 || offset < 8 || !extents.seen.insert(offset) {
            return Err(invalid("cyclic or oversized DNG directory tree"));
        }
        let (fields, next) = Fields::read(&mut input, &mut extents, layout, offset, kind)?;
        if kind == Directory::Root {
            let version = fields.version(50706, &input)?;
            if fields.table.contains_key(&50707) && fields.version(50707, &input)? > version {
                return Err(invalid("DNG backward version exceeds version"));
            }
        }
        if matches!(kind, Directory::Root | Directory::Image) {
            sensors += usize::from(image(&fields, &mut input, &mut extents, &mut remaining)?);
        }
        for (tag, child) in [
            (330, Directory::Image),
            (34665, Directory::Exif),
            (34853, Directory::Gps),
            (40965, Directory::Interop),
        ] {
            if let Some(f) = fields.table.get(&tag) {
                if !matches!(f.ty, 4 | 13) {
                    return Err(invalid("DNG subsidiary IFD pointer type"));
                }
                let pointers = fields.numbers(tag, &input, if tag == 330 { 32 } else { 1 })?;
                stack.extend(pointers.into_iter().map(|p| (p, child, depth + 1)));
            }
        }
        if next != 0 {
            stack.push((
                next,
                if kind == Directory::Root {
                    Directory::Image
                } else {
                    kind
                },
                depth,
            ));
        }
    }
    if sensors != 1 {
        return Err(invalid("expected one full-resolution DNG Bayer image"));
    }
    extents.ranges.sort_unstable();
    if extents
        .ranges
        .windows(2)
        .any(|p| p[0] == p[1] && p[0].2 != RangeKind::Values)
    {
        return Err(invalid("aliased DNG images or directories"));
    }
    let (end, _) = extents.finish()?;
    input.finish(end)
}
