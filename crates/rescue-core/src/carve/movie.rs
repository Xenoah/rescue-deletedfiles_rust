//! Contiguous ISO BMFF/QuickTime containers with a complete non-fragmented sample map.
//! Codec payloads are retained; container validation is not a decoded-frame integrity test.
use super::{Format, container::*};
use crate::{Error, Result, source::Image};
use std::sync::atomic::AtomicBool;

fn invalid(s: &str) -> Error {
    Error::Invalid(format!("MP4/MOV: {s}"))
}
fn be32(b: &[u8], p: usize) -> u32 {
    u32::from_be_bytes(b[p..p + 4].try_into().unwrap())
}
fn be64(b: &[u8], p: usize) -> u64 {
    u64::from_be_bytes(b[p..p + 8].try_into().unwrap())
}
#[derive(Clone, Copy)]
struct BoxRef {
    kind: [u8; 4],
    data: usize,
    end: usize,
}
fn header(b: &[u8], p: usize, boundary: usize) -> Result<BoxRef> {
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
            "MP4/MOV zero-sized box has no independent carving boundary".into(),
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
fn children(b: &[u8], parent: BoxRef) -> Result<Vec<BoxRef>> {
    let mut p = parent.data;
    let mut boxes = Vec::new();
    while p < parent.end {
        if boxes.len() >= 65536 {
            return Err(Error::Limit("MP4 box count".into()));
        }
        let item = header(b, p, parent.end)?;
        p = item.end;
        boxes.push(item);
    }
    Ok(boxes)
}
fn one(items: &[BoxRef], kind: &[u8; 4]) -> Result<BoxRef> {
    let mut found = items.iter().filter(|x| &x.kind == kind);
    let item = *found
        .next()
        .ok_or_else(|| invalid("required box missing"))?;
    if found.next().is_some() {
        return Err(invalid("duplicate singleton box"));
    }
    Ok(item)
}
fn full_zero(b: &[u8], item: BoxRef) -> Result<usize> {
    if item.data + 4 > item.end || b[item.data..item.data + 4] != [0; 4] {
        return Err(Error::Unsupported("MP4 sample-table version/flags".into()));
    }
    Ok(item.data + 4)
}
fn table(b: &[u8], item: BoxRef, width: usize) -> Result<(usize, usize)> {
    let p = full_zero(b, item)?;
    if p + 4 > item.end {
        return Err(invalid("sample-table count"));
    }
    let count = be32(b, p) as usize;
    if count > 1048576 {
        return Err(Error::Limit("MP4 table exceeds 1048576 entries".into()));
    }
    if p + 4 + count * width != item.end {
        return Err(invalid("sample-table extent"));
    }
    Ok((p + 4, count))
}

fn sample_table(
    b: &[u8],
    stbl: BoxRef,
    media: &[(usize, usize)],
    input: &mut Input<'_>,
) -> Result<()> {
    let items = children(b, stbl)?;
    for kind in [b"ctts", b"stss"] {
        if items.iter().filter(|item| &item.kind == kind).count() > 1 {
            return Err(invalid("duplicate optional sample table"));
        }
    }
    if items.iter().any(|x| &x.kind == b"stz2") {
        return Err(Error::Unsupported("MP4 compact stz2 table".into()));
    }
    let stsd = one(&items, b"stsd")?;
    let p = full_zero(b, stsd)?;
    if p + 4 > stsd.end {
        return Err(invalid("sample descriptions"));
    }
    let descriptions = be32(b, p);
    if descriptions == 0 || descriptions > 256 {
        return Err(invalid("sample description count"));
    }
    let desc = children(
        b,
        BoxRef {
            data: p + 4,
            ..stsd
        },
    )?;
    if desc.len() != descriptions as usize {
        return Err(invalid("sample description entries"));
    }
    for d in desc {
        if d.data + 8 > d.end
            || b[d.data..d.data + 6] != [0; 6]
            || u16::from_be_bytes(b[d.data + 6..d.data + 8].try_into().unwrap()) != 1
        {
            return Err(Error::Unsupported(
                "external/invalid MP4 sample reference".into(),
            ));
        }
        if matches!(&d.kind, b"encv" | b"enca") {
            return Err(Error::Unsupported("encrypted MP4 track".into()));
        }
    }
    let stsz = one(&items, b"stsz")?;
    let p = full_zero(b, stsz)?;
    if p + 8 > stsz.end {
        return Err(invalid("sample sizes"));
    }
    let uniform = be32(b, p) as usize;
    let count = be32(b, p + 4) as usize;
    if count == 0 || count > 1048576 {
        return Err(Error::Limit("MP4 sample count must be 1..1048576".into()));
    }
    if p + 8 + if uniform == 0 { count * 4 } else { 0 } != stsz.end {
        return Err(invalid("sample size table extent"));
    }
    let sample_size = |i: usize| {
        if uniform == 0 {
            be32(b, p + 8 + i * 4) as usize
        } else {
            uniform
        }
    };
    let (times, nt) = table(b, one(&items, b"stts")?, 8)?;
    let mut timed = 0u64;
    for i in 0..nt {
        let n = be32(b, times + i * 8);
        let delta = be32(b, times + i * 8 + 4);
        if n == 0 || delta == 0 {
            return Err(invalid("sample duration/count"));
        }
        timed += u64::from(n);
    }
    if timed != count as u64 {
        return Err(invalid("duration/sample counts disagree"));
    }
    if let Some(ctts) = items.iter().find(|x| &x.kind == b"ctts") {
        if ctts.data + 8 > ctts.end || b[ctts.data] > 1 || b[ctts.data + 1..ctts.data + 4] != [0; 3]
        {
            return Err(invalid("composition table version"));
        }
        let n = be32(b, ctts.data + 4) as usize;
        if n > 1048576 || ctts.data + 8 + n * 8 != ctts.end {
            return Err(invalid("composition table length"));
        }
        let sum = (0..n)
            .map(|i| u64::from(be32(b, ctts.data + 8 + i * 8)))
            .sum::<u64>();
        if sum != count as u64 {
            return Err(invalid("composition sample count"));
        }
    }
    if let Some(stss) = items.iter().find(|x| &x.kind == b"stss") {
        let (p, n) = table(b, *stss, 4)?;
        let mut last = 0;
        for i in 0..n {
            let sample = be32(b, p + i * 4);
            if sample <= last || sample as usize > count {
                return Err(invalid("sync sample range/order"));
            }
            last = sample;
        }
    }
    let offsets = items
        .iter()
        .filter(|x| matches!(&x.kind, b"stco" | b"co64"))
        .copied()
        .collect::<Vec<_>>();
    if offsets.len() != 1 {
        return Err(invalid("chunk offset table missing/duplicated"));
    }
    let width = if &offsets[0].kind == b"co64" { 8 } else { 4 };
    let (offsets, nchunks) = table(b, offsets[0], width)?;
    let (sc, ns) = table(b, one(&items, b"stsc")?, 12)?;
    if ns == 0 || nchunks == 0 {
        return Err(invalid("empty chunk mapping"));
    }
    let mut mappings = Vec::new();
    let mut previous = 0;
    for i in 0..ns {
        let first = be32(b, sc + i * 12) as usize;
        let samples = be32(b, sc + i * 12 + 4) as usize;
        let description = be32(b, sc + i * 12 + 8);
        if first <= previous
            || first > nchunks
            || (i == 0 && first != 1)
            || samples == 0
            || description == 0
            || description > descriptions
        {
            return Err(invalid("chunk map order/range"));
        }
        previous = first;
        mappings.push((first, samples));
    }
    let mut sample = 0usize;
    let mut mapping = 0;
    let mut extents = Vec::new();
    for chunk in 1..=nchunks {
        if chunk.is_multiple_of(256) {
            input.work(256)?;
        }
        while mapping + 1 < mappings.len() && mappings[mapping + 1].0 <= chunk {
            mapping += 1;
        }
        let samples = mappings[mapping].1;
        if sample.checked_add(samples).is_none_or(|n| n > count) {
            return Err(invalid("too many mapped samples"));
        }
        let start = usize64(if width == 8 {
            be64(b, offsets + (chunk - 1) * 8)
        } else {
            u64::from(be32(b, offsets + (chunk - 1) * 4))
        })?;
        let mut end = start;
        for index in sample..sample + samples {
            let n = sample_size(index);
            if n == 0 {
                return Err(invalid("zero-sized media sample"));
            }
            end = end
                .checked_add(n)
                .ok_or_else(|| invalid("sample length overflow"))?;
        }
        if !media.iter().any(|(a, z)| start >= *a && end <= *z) {
            return Err(invalid("sample data outside mdat or missing media"));
        }
        sample += samples;
        extents.push((start, end));
    }
    if sample != count {
        return Err(invalid("unmapped samples"));
    }
    extents.sort_unstable();
    if extents.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(invalid("overlapping media chunks"));
    }
    Ok(())
}

fn movie(b: &[u8], moov: BoxRef, media: &[(usize, usize)], input: &mut Input<'_>) -> Result<()> {
    let items = children(b, moov)?;
    if items.iter().any(|x| &x.kind == b"mvex") {
        return Err(Error::Unsupported(
            "fragmented MP4/MOV requires a different sample-map profile".into(),
        ));
    }
    let mvhd = one(&items, b"mvhd")?;
    if mvhd.end - mvhd.data < 100 || ![0, 1].contains(&b[mvhd.data]) {
        return Err(invalid("movie header"));
    }
    let track_items = items
        .iter()
        .filter(|x| &x.kind == b"trak")
        .collect::<Vec<_>>();
    if track_items.is_empty() || track_items.len() > 256 {
        return Err(invalid("track count"));
    }
    let mut ids = std::collections::BTreeSet::new();
    for track in track_items {
        input.check()?;
        let track = children(b, *track)?;
        let tkhd = one(&track, b"tkhd")?;
        if tkhd.data + 4 > tkhd.end {
            return Err(invalid("track header"));
        }
        let id_offset = match b[tkhd.data] {
            0 => 12,
            1 => 20,
            _ => return Err(invalid("track header version")),
        };
        if tkhd.data + id_offset + 4 > tkhd.end {
            return Err(invalid("track header extent"));
        }
        let id = be32(b, tkhd.data + id_offset);
        if id == 0 || !ids.insert(id) {
            return Err(invalid("duplicate/zero track ID"));
        }
        let mdia = children(b, one(&track, b"mdia")?)?;
        let mdhd = one(&mdia, b"mdhd")?;
        if mdhd.data + 4 > mdhd.end {
            return Err(invalid("media header"));
        }
        let scale = match b[mdhd.data] {
            0 => 12,
            1 => 20,
            _ => return Err(invalid("media header version")),
        };
        if mdhd.data + scale + 8 > mdhd.end || be32(b, mdhd.data + scale) == 0 {
            return Err(invalid("media time scale"));
        }
        let handler = one(&mdia, b"hdlr")?;
        if handler.end - handler.data < 24 {
            return Err(invalid("media handler"));
        }
        let minf = children(b, one(&mdia, b"minf")?)?;
        let dinf = children(b, one(&minf, b"dinf")?)?;
        let dref = one(&dinf, b"dref")?;
        let p = full_zero(b, dref)?;
        if p + 4 > dref.end || be32(b, p) != 1 {
            return Err(Error::Unsupported(
                "multiple/external MP4 data references".into(),
            ));
        }
        let refs = children(
            b,
            BoxRef {
                data: p + 4,
                ..dref
            },
        )?;
        if refs.len() != 1
            || &refs[0].kind != b"url "
            || refs[0].end - refs[0].data != 4
            || b[refs[0].data..refs[0].end] != [0, 0, 0, 1]
        {
            return Err(Error::Unsupported("external MP4 media location".into()));
        }
        sample_table(b, one(&minf, b"stbl")?, media, input)?;
    }
    Ok(())
}

pub(super) fn validate(
    image: &mut Image,
    at: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
    boundary: u64,
) -> Result<(Format, Vec<u8>)> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    let mut p = 0;
    let mut moov = None;
    let mut media = Vec::new();
    let mut format = Format::Mp4;
    let mut boxes_seen = 0;
    for index in 0..65536 {
        input.check()?;
        if p + 8 > input.limit() {
            break;
        }
        input.ensure(p + 8)?;
        let kind = &input.bytes[p + 4..p + 8];
        if !matches!(
            kind,
            b"ftyp"
                | b"moov"
                | b"mdat"
                | b"free"
                | b"skip"
                | b"wide"
                | b"uuid"
                | b"pdin"
                | b"sidx"
                | b"moof"
                | b"mfra"
        ) {
            break;
        }
        if index > 0 && kind == b"ftyp" {
            break;
        }
        if matches!(kind, b"moof" | b"mfra" | b"sidx") {
            return Err(Error::Unsupported("fragmented MP4/MOV".into()));
        }
        let n = be32(&input.bytes, p);
        let length = if n == 1 {
            input.ensure(p + 16)?;
            usize64(be64(&input.bytes, p + 8))?
        } else if n == 0 {
            return Err(Error::Unsupported(
                "MP4 zero-sized box boundary is unknown".into(),
            ));
        } else {
            n as usize
        };
        let end = p
            .checked_add(length)
            .filter(|n| *n <= input.limit())
            .ok_or_else(|| invalid("top-level box outside selected range/limit"))?;
        input.ensure(end)?;
        let item = header(&input.bytes, p, end)?;
        if index == 0 {
            if &item.kind != b"ftyp"
                || item.end - item.data < 8
                || !(item.end - item.data).is_multiple_of(4)
            {
                return Err(invalid("ftyp"));
            }
            let brand = &input.bytes[item.data..item.data + 4];
            if brand == b"qt  " {
                format = Format::Mov;
            } else if ![
                b"isom".as_slice(),
                b"iso2",
                b"iso3",
                b"iso4",
                b"iso5",
                b"iso6",
                b"mp41",
                b"mp42",
                b"avc1",
                b"M4V ",
                b"M4A ",
            ]
            .contains(&brand)
            {
                return Err(Error::Unsupported("ISO BMFF major brand".into()));
            }
        }
        match &item.kind {
            b"moov" => {
                if moov.replace(item).is_some() {
                    return Err(invalid("duplicate moov"));
                }
            }
            b"mdat" => {
                if item.data == item.end {
                    return Err(invalid("empty mdat"));
                }
                media.push((item.data, item.end));
            }
            _ => {}
        }
        if item.end <= p {
            return Err(invalid("zero-progress box"));
        }
        p = item.end;
        boxes_seen += 1;
    }
    if boxes_seen == 65536 && p < input.limit() {
        return Err(Error::Limit("MP4 top-level box count".into()));
    }
    let moov = moov.ok_or_else(|| invalid("moov missing; media cannot be declared complete"))?;
    if media.is_empty() {
        return Err(invalid("mdat missing"));
    }
    // Validation uses a bounded immutable view while cancellation/budget remain live.
    let bytes = input.bytes[..p].to_vec();
    movie(&bytes, moov, &media, &mut input)?;
    Ok((format, input.finish(p)?))
}
