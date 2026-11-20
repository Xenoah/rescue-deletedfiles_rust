//! Contiguous Photoshop documents. The composite image determines the final byte.
use super::{Format, container::Input};
use crate::{Error, Result};
use flate2::{Decompress, FlushDecompress, Status};

const MAX_DECODED: usize = 128 << 20;
const MAX_ROWS: usize = 2_000_000;

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!(
        "invalid or unsupported Photoshop document: {reason}"
    ))
}

pub(super) fn signature(bytes: &[u8]) -> Option<Format<'static>> {
    match bytes.get(..6)? {
        b"8BPS\0\x01" => Some(Format::Psd),
        b"8BPS\0\x02" => Some(Format::Psb),
        _ => None,
    }
}

fn take<'a>(bytes: &mut &'a [u8], length: usize) -> Result<&'a [u8]> {
    let (value, rest) = bytes
        .split_at_checked(length)
        .ok_or_else(|| invalid("field exceeds section"))?;
    *bytes = rest;
    Ok(value)
}

fn number(bytes: &mut &[u8], width: usize) -> Result<usize> {
    let data = take(bytes, width)?;
    let mut padded = [0; 8];
    padded[8 - width..].copy_from_slice(data);
    usize::try_from(u64::from_be_bytes(padded)).map_err(|_| invalid("length overflow"))
}

fn section<'a>(bytes: &mut &'a [u8], width: usize) -> Result<&'a [u8]> {
    let len = number(bytes, width)?;
    take(bytes, len)
}

fn resources(mut bytes: &[u8]) -> Result<()> {
    let mut count = 0;
    while !bytes.is_empty() {
        count += 1;
        if count > 65536 || take(&mut bytes, 4)? != b"8BIM" {
            return Err(invalid("image resource block"));
        }
        take(&mut bytes, 2)?; // Resource ID.
        let name = number(&mut bytes, 1)?;
        take(&mut bytes, name + (1 + name) % 2)?;
        let payload = section(&mut bytes, 4)?;
        take(&mut bytes, payload.len() % 2)?;
    }
    Ok(())
}

fn tagged(
    mut bytes: &[u8],
    large: bool,
    alignment: usize,
    mut visit: impl FnMut(&[u8], &[u8], usize) -> Result<()>,
) -> Result<()> {
    let mut count = 0;
    while bytes.len() >= 12 {
        count += 1;
        if count > 65536 || !matches!(take(&mut bytes, 4)?, b"8BIM" | b"8B64") {
            return Err(invalid("additional layer signature"));
        }
        let key = take(&mut bytes, 4)?;
        let wide = large
            && matches!(
                key,
                b"Alph"
                    | b"FELS"
                    | b"FEid"
                    | b"FMsk"
                    | b"FXid"
                    | b"LMsk"
                    | b"Layr"
                    | b"Lr16"
                    | b"Lr32"
                    | b"Mt16"
                    | b"Mt32"
                    | b"Mtrn"
                    | b"PxSD"
                    | b"artd"
                    | b"cinf"
                    | b"extd"
                    | b"extn"
                    | b"lnk2"
                    | b"lnk3"
                    | b"lnkE"
                    | b"pths"
            );
        let value = section(&mut bytes, if wide { 8 } else { 4 })?;
        visit(key, value, bytes.len())?;
        let padding = (alignment - value.len() % alignment) % alignment;
        take(&mut bytes, padding)?;
    }
    if bytes.len() > 3 || bytes.iter().any(|&b| b != 0) {
        return Err(invalid("additional layer padding"));
    }
    Ok(())
}

#[derive(Clone, Copy)]
struct Plane {
    row_bytes: usize,
    rows: usize,
}

impl Plane {
    fn from_bounds(bounds: &[u8], depth: usize) -> Result<Self> {
        let mut fields = bounds.chunks_exact(4);
        let mut coord = || {
            fields
                .next()
                .and_then(|b| b.try_into().ok())
                .map(i32::from_be_bytes)
                .map(i64::from)
                .ok_or_else(|| invalid("missing layer bounds"))
        };
        let (top, left, bottom, right) = (coord()?, coord()?, coord()?, coord()?);
        let (width, height) = (right - left, bottom - top);
        if !(0..=300000).contains(&width) || !(0..=300000).contains(&height) {
            return Err(invalid("layer bounds"));
        }
        Ok(Self {
            row_bytes: (width as usize * depth).div_ceil(8),
            rows: height as usize,
        })
    }

    fn size(self) -> Result<usize> {
        self.row_bytes
            .checked_mul(self.rows)
            .filter(|&n| n <= MAX_DECODED)
            .ok_or_else(|| invalid("layer plane exceeds limit"))
    }
}

struct LayerChannel {
    start: usize,
    end: usize,
    plane: Plane,
}

fn layer_info(
    mut bytes: &[u8],
    end: usize,
    large: bool,
    depth: usize,
    output: &mut Vec<LayerChannel>,
) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let count = i16::from_be_bytes(take(&mut bytes, 2)?.try_into().unwrap()).unsigned_abs();
    if count > 4096 {
        return Err(invalid("layer count"));
    }
    let mut pending = Vec::new();
    for _ in 0..count {
        let layer = Plane::from_bounds(take(&mut bytes, 16)?, depth)?;
        let channels = number(&mut bytes, 2)?;
        if channels > 56 || output.len() + pending.len() + channels > 65536 {
            return Err(invalid("layer channel count"));
        }
        let mut metadata = Vec::with_capacity(channels);
        let mut ids = 0u64;
        for _ in 0..channels {
            let id = i16::from_be_bytes(take(&mut bytes, 2)?.try_into().unwrap());
            if !(-3..=55).contains(&id) || ids & (1u64 << (id + 3)) != 0 {
                return Err(invalid("layer channel id"));
            }
            ids |= 1u64 << (id + 3);
            let len = number(&mut bytes, if large { 8 } else { 4 })?;
            if len < 2 {
                return Err(invalid("layer channel length"));
            }
            metadata.push((id, len));
        }
        if take(&mut bytes, 4)? != b"8BIM" {
            return Err(invalid("layer blend signature"));
        }
        take(&mut bytes, 8)?;
        let mut extra = section(&mut bytes, 4)?;
        let mask = section(&mut extra, 4)?;
        for (id, len) in metadata {
            let plane = match id {
                -2 => Plane::from_bounds(
                    mask.get(..16).ok_or_else(|| invalid("missing user mask"))?,
                    depth,
                )?,
                -3 => Plane::from_bounds(
                    mask.get(20..36)
                        .ok_or_else(|| invalid("missing real user mask"))?,
                    depth,
                )?,
                _ => layer,
            };
            pending.push((len, plane));
        }
        section(&mut extra, 4)?; // Blending ranges.
        let name = number(&mut extra, 1)?;
        take(&mut extra, (name + 1).next_multiple_of(4) - 1)?;
        tagged(extra, large, 1, |_, _, _| Ok(()))?;
    }
    for (len, plane) in pending {
        let start = end - bytes.len();
        take(&mut bytes, len)?;
        output.push(LayerChannel {
            start,
            end: start + len,
            plane,
        });
    }
    if bytes.len() > 3 || bytes.iter().any(|&b| b != 0) {
        return Err(invalid("layer channel lengths disagree with table"));
    }
    Ok(())
}

fn layers(mut bytes: &[u8], end: usize, large: bool, depth: usize) -> Result<Vec<LayerChannel>> {
    let mut channels = Vec::new();
    if !bytes.is_empty() {
        let info = section(&mut bytes, if large { 8 } else { 4 })?;
        layer_info(info, end - bytes.len(), large, depth, &mut channels)?;
        // Some writers pad the layer section without a global mask length.
        if bytes.len() >= 4 && !(bytes.starts_with(b"8BIM") || bytes.starts_with(b"8B64")) {
            section(&mut bytes, 4)?;
        }
        tagged(bytes, large, 4, |key, value, remaining| {
            if matches!(key, b"Layr" | b"Lr16" | b"Lr32") {
                let embedded_depth = match key {
                    b"Lr16" => 16,
                    b"Lr32" => 32,
                    _ => depth,
                };
                layer_info(value, end - remaining, large, embedded_depth, &mut channels)?;
            }
            Ok(())
        })?;
    }
    Ok(channels)
}

fn document_section(
    input: &mut Input,
    pos: &mut usize,
    width: usize,
) -> Result<std::ops::Range<usize>> {
    input.ensure(*pos + width)?;
    let size = number(&mut &input.bytes[*pos..], width)?;
    let start = *pos + width;
    *pos = start
        .checked_add(size)
        .ok_or_else(|| invalid("section length overflow"))?;
    input.ensure(*pos)?;
    Ok(start..*pos)
}

fn packbits(mut bytes: &[u8], width: usize) -> Result<()> {
    let mut output = 0usize;
    while let Some((&tag, rest)) = bytes.split_first() {
        bytes = rest;
        match tag {
            0..=127 => {
                let n = usize::from(tag) + 1;
                take(&mut bytes, n)?;
                output += n;
            }
            128 => (),
            _ => {
                take(&mut bytes, 1)?;
                output += 257 - usize::from(tag);
            }
        }
        if output > width {
            return Err(invalid("PackBits row overflow"));
        }
    }
    if output != width {
        return Err(invalid("PackBits row length"));
    }
    Ok(())
}

fn rle(
    input: &mut Input,
    pos: usize,
    rows: usize,
    width: usize,
    large: bool,
    limit: usize,
) -> Result<usize> {
    let field = if large { 4 } else { 2 };
    let table_end = pos + rows * field;
    if table_end > limit {
        return Err(invalid("RLE table exceeds channel"));
    }
    input.ensure(table_end)?;
    let mut end = table_end;
    for i in 0..rows {
        let len = number(&mut &input.bytes[pos + i * field..], field)?;
        end = end
            .checked_add(len)
            .filter(|&n| n <= limit)
            .ok_or_else(|| invalid("RLE row extent"))?;
    }
    input.ensure(end)?;
    let mut pixels = &input.bytes[table_end..end];
    let mut table = &input.bytes[pos..table_end];
    for i in 0..rows {
        if i % 1024 == 0 {
            input.check()?;
        }
        let n = number(&mut table, field)?;
        packbits(take(&mut pixels, n)?, width)?;
    }
    Ok(end)
}

fn zlib(input: &mut Input, mut pos: usize, expected: usize, limit: usize) -> Result<usize> {
    let mut decoder = Decompress::new(true);
    let mut buf = [0; 16384];
    loop {
        input.check()?;
        if pos == input.bytes.len() && pos < limit {
            input.ensure(pos + 1)?;
        }
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(
                &input.bytes[pos..input.bytes.len().min(limit)],
                &mut buf,
                FlushDecompress::None,
            )
            .map_err(|_| invalid("zlib data or checksum"))?;
        pos += (decoder.total_in() - before_in) as usize;
        if decoder.total_out() > expected as u64 {
            return Err(invalid("excess image samples"));
        }
        if status == Status::StreamEnd {
            if decoder.total_out() != expected as u64 {
                return Err(invalid("incomplete image samples"));
            }
            return Ok(pos);
        }
        if before_in == decoder.total_in() && before_out == decoder.total_out() {
            return Err(invalid("truncated zlib stream"));
        }
    }
}

fn compressed_plane(
    input: &mut Input,
    pos: usize,
    plane: Plane,
    large: bool,
    limit: usize,
) -> Result<usize> {
    if pos + 2 > limit {
        return Err(invalid("compression field exceeds section"));
    }
    input.ensure(pos + 2)?;
    let method = number(&mut &input.bytes[pos..], 2)?;
    let start = pos + 2;
    let expected = plane.size()?;
    input.work(expected)?;
    match method {
        0 => {
            let end = start + expected;
            if end > limit {
                return Err(invalid("uncompressed samples exceed section"));
            }
            input.ensure(end)?;
            Ok(end)
        }
        1 => rle(input, start, plane.rows, plane.row_bytes, large, limit),
        2 | 3 => zlib(input, start, expected, limit),
        _ => Err(invalid("image compression")),
    }
}

pub(super) fn parse(input: &mut Input, format: Format) -> Result<usize> {
    input.ensure(26)?;
    let data = &input.bytes;
    let large = format == Format::Psb;
    if signature(data) != Some(format) || data[6..12] != [0; 6] {
        return Err(invalid("header"));
    }
    let mut fields = &data[12..26];
    let channels = number(&mut fields, 2)?;
    let height = number(&mut fields, 4)?;
    let width = number(&mut fields, 4)?;
    let depth = number(&mut fields, 2)?;
    let mode = number(&mut fields, 2)?;
    let max = if large { 300000 } else { 30000 };
    let valid = match mode {
        0 => channels == 1 && depth == 1,
        1 | 7 => matches!(depth, 8 | 16 | 32),
        2 => channels == 1 && depth == 8,
        3 => channels >= 3 && matches!(depth, 8 | 16 | 32),
        4 => channels >= 4 && matches!(depth, 8 | 16),
        9 => channels >= 3 && matches!(depth, 8 | 16),
        _ => false,
    };
    if !valid
        || !(1..=56).contains(&channels)
        || !(1..=max).contains(&width)
        || !(1..=max).contains(&height)
    {
        return Err(invalid("dimensions, depth, color mode or channels"));
    }
    let row = (width * depth).div_ceil(8);
    let rows = height * channels;
    let composite = Plane {
        rows,
        row_bytes: row,
    };
    composite.size()?;
    if rows > MAX_ROWS {
        return Err(invalid("too many scanlines"));
    }
    let mut pos = 26;
    let color = document_section(input, &mut pos, 4)?;
    if color.len() != if mode == 2 { 768 } else { 0 } {
        return Err(invalid("color mode data length"));
    }
    let resource = document_section(input, &mut pos, 4)?;
    resources(&input.bytes[resource])?;
    let layer = document_section(input, &mut pos, if large { 8 } else { 4 })?;
    let layer_end = layer.end;
    let channels = layers(&input.bytes[layer], layer_end, large, depth)?;
    let mut layer_bytes = 0usize;
    for channel in channels {
        layer_bytes += channel.plane.size()?;
        if layer_bytes > MAX_DECODED {
            return Err(invalid("combined layer samples exceed limit"));
        }
        let end = compressed_plane(input, channel.start, channel.plane, large, channel.end)?;
        if end != channel.end {
            return Err(invalid("channel length disagrees with samples"));
        }
    }
    input.ensure(pos + 2)?;
    if depth == 1 && input.bytes[pos..pos + 2] == [0, 3] {
        return Err(invalid("one-bit prediction"));
    }
    compressed_plane(input, pos, composite, large, input.limit())
}
