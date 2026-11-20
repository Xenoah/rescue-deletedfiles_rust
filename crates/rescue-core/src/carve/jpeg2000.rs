//! Bounded JPEG 2000 Part 1 images. Tile lengths delimit entropy data even on raw media.
mod icc;
use super::{
    Format,
    bmff::{self, BoxRef, be32, be64},
    container::{Input, usize64},
};
use crate::{Error, Result};
use hayro_jpeg2000::{DecodeSettings, DecoderContext, Image};

pub(super) const BOX_SIGNATURE: &[u8] = b"\0\0\0\x0cjP  \r\n\x87\n";
pub(super) const CODESTREAM_SIGNATURE: &[u8] = b"\xff\x4f\xff\x51";

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("JPEG 2000: {reason}"))
}

fn be16(b: &[u8], p: usize) -> u16 {
    u16::from_be_bytes([b[p], b[p + 1]])
}

#[derive(Clone, Copy)]
struct Geometry {
    width: u32,
    height: u32,
    components: u16,
    bpc: u8,
    tile_width: u32,
    tile_height: u32,
    tiles: usize,
}

impl Geometry {
    fn parse(b: &[u8]) -> Result<Self> {
        if b.len() < 39 || be16(b, 0) != 0 {
            return Err(invalid("unsupported SIZ profile"));
        }
        let (width, height) = (be32(b, 2), be32(b, 6));
        let (tw, th) = (be32(b, 18), be32(b, 22));
        let components = be16(b, 34);
        if !(1..=8192).contains(&width)
            || !(1..=8192).contains(&height)
            || !(1..=8192).contains(&tw)
            || !(1..=8192).contains(&th)
            || b[10..18] != [0; 8]
            || b[26..34] != [0; 8]
            || !(1..=4).contains(&components)
            || b.len() != 36 + 3 * usize::from(components)
            || width * height * u32::from(components) > 4_194_304
        {
            return Err(invalid("image grid, component count or sample limit"));
        }
        let bpc = b[36];
        if bpc > 15 || !b[36..].as_chunks::<3>().0.iter().all(|c| *c == [bpc, 1, 1]) {
            return Err(invalid("component precision, sign or sampling"));
        }
        let tiles = (width.div_ceil(tw) * height.div_ceil(th)) as usize;
        if tiles > 1024 {
            return Err(invalid("tile count"));
        }
        Ok(Self {
            width,
            height,
            components,
            bpc,
            tile_width: tw.min(width),
            tile_height: th.min(height),
            tiles,
        })
    }

    fn samples(self) -> usize {
        self.width as usize * self.height as usize * usize::from(self.components)
    }

    fn coding(self, b: &[u8]) -> Result<u8> {
        if b.len() < 10 {
            return Err(invalid("short COD"));
        }
        let levels = b[5];
        let layers = be16(b, 2);
        if b[0] > 1
            || b[1] > 4
            || !(1..=16).contains(&layers)
            || b[4] > 1
            || (b[4] == 1 && self.components < 3)
            || levels > 8
            || b[6] > 8
            || b[7] > 8
            || u16::from(b[6]) + u16::from(b[7]) > 8
            || b[8] != 0
            || b[9] > 1
            || b.len() != 10 + usize::from(b[0]) * (1 + usize::from(levels))
        {
            return Err(invalid("unsupported coding style, layers or decomposition"));
        }
        let tile_components = self.tiles as u64 * u64::from(self.components);
        let mut packets = 0u64;
        let mut blocks = 0u64;
        for res in 0..=levels {
            let scale = 1 << (levels - res);
            let (w, h) = (
                self.tile_width.div_ceil(scale),
                self.tile_height.div_ceil(scale),
            );
            let precinct = b.get(10 + usize::from(res)).copied().unwrap_or(0xff);
            let (px, py) = (precinct & 15, precinct >> 4);
            if res > 0 && (px == 0 || py == 0) {
                return Err(invalid("precinct too small for subbands"));
            }
            // An unaligned tile can intersect one additional precinct or codeblock per axis.
            let cells = |x: u8, y: u8| {
                u64::from(w.div_ceil(1 << x) + 1) * u64::from(h.div_ceil(1 << y) + 1)
            };
            packets += cells(px, py) * tile_components * u64::from(layers);
            blocks += cells((b[6] + 2).min(px), (b[7] + 2).min(py)) * tile_components * 3;
        }
        if packets > 262_144 || blocks > 131_072 {
            return Err(invalid("packet or codeblock work limit"));
        }
        Ok(levels)
    }
}

fn segment(input: &mut Input<'_>, pos: usize, bound: usize) -> Result<usize> {
    if pos + 4 > bound {
        return Err(invalid("short marker segment"));
    }
    input.ensure(pos + 4)?;
    let size = usize::from(be16(&input.bytes, pos + 2));
    let end = pos + 2 + size;
    if size < 2 || end > bound {
        return Err(invalid("marker length"));
    }
    input.ensure(end)?;
    Ok(end)
}

fn codestream(input: &mut Input<'_>, start: usize, bound: usize) -> Result<(usize, Geometry)> {
    if start + 4 > bound {
        return Err(invalid("short codestream"));
    }
    input.ensure(start + 4)?;
    if &input.bytes[start..start + 4] != CODESTREAM_SIGNATURE {
        return Err(invalid("missing SOC or SIZ"));
    }
    let mut pos = segment(input, start + 2, bound)?;
    let grid = Geometry::parse(&input.bytes[start + 6..pos])?;
    let mut levels = None;
    let mut quantized = false;
    let mut seen = vec![false; grid.tiles];
    let mut finished = 0;
    for _ in 0..4096 {
        if pos + 2 > bound {
            return Err(invalid("missing EOC"));
        }
        input.ensure(pos + 2)?;
        match be16(&input.bytes, pos) {
            0xffd9 if finished == grid.tiles => return Ok((pos + 2, grid)),
            0xff90 if quantized => {
                let end = segment(input, pos, bound)?;
                if end != pos + 12 || input.bytes[pos + 10] != 0 || input.bytes[pos + 11] > 1 {
                    return Err(invalid("expected one tile-part per tile"));
                }
                let index = usize::from(be16(&input.bytes, pos + 4));
                if index >= seen.len() || seen[index] {
                    return Err(invalid("invalid or duplicate tile"));
                }
                let length = usize64(u64::from(be32(&input.bytes, pos + 6)))?;
                let next = pos + length;
                if length < 15 || next > bound {
                    return Err(invalid("tile-part extent"));
                }
                input.ensure(next)?;
                if be16(&input.bytes, end) != 0xff93 {
                    return Err(invalid("missing SOD or unsupported tile header"));
                }
                if input.bytes[end + 2..next]
                    .windows(2)
                    .any(|pair| pair[0] == 0xff && pair[1] > 0x8f)
                {
                    return Err(invalid("unexpected marker inside tile data"));
                }
                seen[index] = true;
                finished += 1;
                pos = next;
            }
            marker if finished == 0 => {
                let end = segment(input, pos, bound)?;
                let b = &input.bytes[pos + 4..end];
                match marker {
                    0xff52 if levels.is_none() => levels = Some(grid.coding(b)?),
                    0xff5c if !quantized => {
                        let levels = levels.ok_or_else(|| invalid("QCD before COD"))?;
                        let subbands = 1 + 3 * usize::from(levels);
                        let steps = match b.first().map(|v| v & 31) {
                            Some(0) if b[1..].iter().all(|v| v & 7 == 0) => subbands,
                            Some(1) => 2,
                            Some(2) => subbands * 2,
                            _ => return Err(invalid("quantization style")),
                        };
                        if b.len() != steps + 1 {
                            return Err(invalid("quantization table length"));
                        }
                        quantized = true;
                    }
                    0xff64 if b.len() >= 2 && be16(b, 0) <= 1 => {}
                    _ => return Err(invalid("unsupported main-header marker or order")),
                }
                pos = end;
            }
            _ => return Err(invalid("unexpected marker or incomplete tiles")),
        }
    }
    Err(invalid("marker count"))
}

fn box_at(input: &mut Input<'_>, pos: usize) -> Result<BoxRef> {
    input.ensure(pos + 8)?;
    let mut size = u64::from(be32(&input.bytes, pos));
    if size == 1 {
        input.ensure(pos + 16)?;
        size = be64(&input.bytes, pos + 8);
    }
    if size == 0 {
        return Err(invalid("unbounded box"));
    }
    let kind = &input.bytes[pos + 4..pos + 8];
    if (kind == b"ftyp" && size > 4096) || (kind == b"jp2h" && size > 1024 * 1024) {
        return Err(invalid("header box size limit"));
    }
    let end = pos + usize64(size)?;
    input.ensure(end)?;
    bmff::header(&input.bytes, pos, end)
}

fn image_header(b: &[u8], header: BoxRef, grid: Geometry) -> Result<()> {
    let children = bmff::children(b, header)?;
    let ihdr = bmff::one(&children, b"ihdr")?;
    let data = &b[ihdr.data..ihdr.end];
    if children.len() > 4
        || children[0].kind != ihdr.kind
        || data.len() != 14
        || be32(data, 0) != grid.height
        || be32(data, 4) != grid.width
        || be16(data, 8) != grid.components
        || data[10] != grid.bpc
        || !matches!(data[11..], [7, 0 | 1, 0])
    {
        return Err(invalid(
            "image header and codestream disagree or unsupported IHDR",
        ));
    }
    let colr = bmff::one(&children, b"colr")?;
    let color = &b[colr.data..colr.end];
    match color {
        [1, 0, 0, space @ ..]
            if space.len() == 4
                && matches!((be32(space, 0), grid.components), (16, 3 | 4) | (17, 1 | 2)) => {}
        [2, 0, 0, profile @ ..] => icc::validate(profile, grid.components)?,
        _ => return Err(invalid("unsupported color specification")),
    }
    for item in &children {
        match &item.kind {
            b"ihdr" | b"colr" => {}
            b"cdef" => {
                bmff::one(&children, b"cdef")?;
                let data = &b[item.data..item.end];
                if data.len() != 2 + 6 * usize::from(grid.components)
                    || be16(data, 0) != grid.components
                {
                    return Err(invalid("channel definition count"));
                }
                let mut seen = [false; 4];
                for entry in data[2..].as_chunks::<6>().0 {
                    let n = usize::from(be16(entry, 0));
                    if n >= usize::from(grid.components) || std::mem::replace(&mut seen[n], true) {
                        return Err(invalid("invalid or repeated channel mapping"));
                    }
                    let alpha =
                        matches!(grid.components, 2 | 4) && n + 1 == usize::from(grid.components);
                    if be16(entry, 2) != u16::from(alpha)
                        || usize::from(be16(entry, 4)) != if alpha { 0 } else { n + 1 }
                    {
                        return Err(invalid("unsupported channel mapping"));
                    }
                }
            }
            _ => return Err(invalid("unsupported JP2 header child")),
        }
    }
    Ok(())
}

fn metadata(kind: &[u8]) -> bool {
    matches!(kind, b"xml " | b"uuid" | b"free")
}

fn trailing_metadata(input: &mut Input<'_>, mut pos: usize) -> Result<usize> {
    for count in 0..=256 {
        if pos + 8 > input.limit() {
            return Ok(pos);
        }
        match input.ensure(pos + 8) {
            Err(Error::MissingRanges(_)) => return Ok(pos),
            other => other?,
        }
        let kind = &input.bytes[pos + 4..pos + 8];
        if matches!(kind, b"jp2c" | b"jp2h") {
            return Err(invalid("multiple JP2 streams or headers"));
        }
        if !metadata(kind) {
            return Ok(pos);
        }
        if count == 256 {
            break;
        }
        pos = box_at(input, pos)?.end;
    }
    Err(invalid("metadata box count"))
}

fn boxed(input: &mut Input<'_>) -> Result<(usize, usize, usize, Geometry)> {
    input.ensure(12)?;
    if &input.bytes[..12] != BOX_SIGNATURE {
        return Err(invalid("signature box"));
    }
    let ftyp = box_at(input, 12)?;
    let brands = &input.bytes[ftyp.data..ftyp.end];
    if &ftyp.kind != b"ftyp"
        || brands.len() < 12
        || !brands.len().is_multiple_of(4)
        || &brands[..8] != b"jp2 \0\0\0\0"
        || !brands[8..].as_chunks::<4>().0.contains(b"jp2 ")
    {
        return Err(invalid("JP2 brand or version"));
    }
    let mut pos = ftyp.end;
    let mut header = None;
    for _ in 0..256 {
        input.ensure(pos + 8)?;
        let kind = &input.bytes[pos + 4..pos + 8];
        if kind == b"jp2c" {
            let header = header.ok_or_else(|| invalid("missing JP2 header"))?;
            // A final LBox=0 codestream extends to EOF. Its EOC provides the independent
            // end on raw media, even with unrelated sectors or the next file immediately after it.
            if be32(&input.bytes, pos) == 0 {
                let start = pos + 8;
                let (end, grid) = codestream(input, start, input.limit())?;
                image_header(&input.bytes, header, grid)?;
                return Ok((end, start, end, grid));
            }
            let item = box_at(input, pos)?;
            let (end, grid) = codestream(input, item.data, item.end)?;
            if end != item.end {
                return Err(invalid("bytes after codestream EOC inside box"));
            }
            image_header(&input.bytes, header, grid)?;
            return Ok((trailing_metadata(input, end)?, item.data, end, grid));
        }
        if kind != b"jp2h" && !metadata(kind) {
            return Err(invalid("unrecognized top-level box"));
        }
        let item = box_at(input, pos)?;
        match &item.kind {
            b"jp2h" if header.is_none() => header = Some(item),
            kind if metadata(kind) => {}
            _ => return Err(invalid("unrecognized top-level box")),
        }
        pos = item.end;
    }
    Err(invalid("box count"))
}

pub(super) fn parse(input: &mut Input<'_>, format: Format) -> Result<(usize, Option<usize>)> {
    let (end, start, stream_end, grid) = if format == Format::Jp2 {
        boxed(input)?
    } else {
        let (end, grid) = codestream(input, 0, input.limit())?;
        (end, 0, end, grid)
    };
    input.work(grid.samples() * 16 + stream_end - start)?;
    let settings = DecodeSettings {
        strict: true,
        ..Default::default()
    };
    // The envelope and validated limits apply before invoking the independent entropy decoder.
    let decoded = std::panic::catch_unwind(|| {
        let image = Image::new(&input.bytes[start..stream_end], &settings)
            .map_err(|e| invalid(&e.to_string()))?;
        let mut ctx = DecoderContext::default();
        let decoded = image
            .decode(&mut ctx)
            .map_err(|e| invalid(&e.to_string()))?;
        if decoded.components().len() != usize::from(grid.components)
            || !decoded
                .components()
                .iter()
                .all(|c| c.samples().len() == grid.width as usize * grid.height as usize)
        {
            return Err(invalid("incomplete decoded planes"));
        }
        Ok(())
    })
    .map_err(|_| invalid("decoder rejected codestream"))?;
    decoded?;
    input.check()?;
    Ok((end, (start != 0).then_some(start)))
}
