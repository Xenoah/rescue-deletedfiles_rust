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

fn tagged(mut bytes: &[u8], large: bool, alignment: usize) -> Result<()> {
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
        let padding = (alignment - value.len() % alignment) % alignment;
        take(&mut bytes, padding)?;
    }
    if bytes.len() > 3 || bytes.iter().any(|&b| b != 0) {
        return Err(invalid("additional layer padding"));
    }
    Ok(())
}

fn layer_info(mut bytes: &[u8], large: bool) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    let count = i16::from_be_bytes(take(&mut bytes, 2)?.try_into().unwrap()).unsigned_abs();
    if count > 4096 {
        return Err(invalid("layer count"));
    }
    let mut lengths = Vec::new();
    for _ in 0..count {
        let bounds = take(&mut bytes, 16)?;
        let coord = |pos| i32::from_be_bytes(bounds[pos..pos + 4].try_into().unwrap());
        if coord(8) < coord(0) || coord(12) < coord(4) {
            return Err(invalid("layer bounds"));
        }
        let channels = number(&mut bytes, 2)?;
        if channels > 56 || lengths.len() + channels > 65536 {
            return Err(invalid("layer channel count"));
        }
        for _ in 0..channels {
            take(&mut bytes, 2)?;
            let len = number(&mut bytes, if large { 8 } else { 4 })?;
            if len < 2 {
                return Err(invalid("layer channel length"));
            }
            lengths.push(len);
        }
        if take(&mut bytes, 4)? != b"8BIM" {
            return Err(invalid("layer blend signature"));
        }
        take(&mut bytes, 8)?; // Blend key, opacity, clipping, flags, reserved.
        let mut extra = section(&mut bytes, 4)?;
        section(&mut extra, 4)?; // Mask parameters and blending ranges are preserved.
        section(&mut extra, 4)?;
        let name = number(&mut extra, 1)?;
        take(&mut extra, (name + 1).next_multiple_of(4) - 1)?;
        tagged(extra, large, 1)?;
    }
    for size in lengths {
        let data = take(&mut bytes, size)?;
        if u16::from_be_bytes(data[..2].try_into().unwrap()) > 3 {
            return Err(invalid("layer channel compression"));
        }
    }
    if bytes.len() > 3 || bytes.iter().any(|&b| b != 0) {
        return Err(invalid("layer channel lengths disagree with table"));
    }
    Ok(())
}

fn layers(mut bytes: &[u8], large: bool) -> Result<()> {
    if bytes.is_empty() {
        return Ok(());
    }
    layer_info(section(&mut bytes, if large { 8 } else { 4 })?, large)?;
    // Some writers pad the layer section to its boundary without a global mask length.
    if bytes.len() >= 4 && !(bytes.starts_with(b"8BIM") || bytes.starts_with(b"8B64")) {
        section(&mut bytes, 4)?; // Global layer mask.
    }
    tagged(bytes, large, 4)
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

fn rle(input: &mut Input, pos: usize, rows: usize, width: usize, large: bool) -> Result<usize> {
    let field = if large { 4 } else { 2 };
    let table_end = pos + rows * field;
    input.ensure(table_end)?;
    let mut end = table_end;
    for i in 0..rows {
        let len = number(&mut &input.bytes[pos + i * field..], field)?;
        end = end
            .checked_add(len)
            .filter(|&n| n <= input.limit())
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

fn zlib(input: &mut Input, mut pos: usize, expected: usize) -> Result<usize> {
    let mut decoder = Decompress::new(true);
    let mut buf = [0; 16384];
    loop {
        input.check()?;
        if pos == input.bytes.len() && pos < input.limit() {
            input.ensure(pos + 1)?;
        }
        let before_in = decoder.total_in();
        let before_out = decoder.total_out();
        let status = decoder
            .decompress(&input.bytes[pos..], &mut buf, FlushDecompress::None)
            .map_err(|_| invalid("zlib data or checksum"))?;
        pos += (decoder.total_in() - before_in) as usize;
        if decoder.total_out() > expected as u64 {
            return Err(invalid("excess composite samples"));
        }
        if status == Status::StreamEnd {
            if decoder.total_out() != expected as u64 {
                return Err(invalid("incomplete composite samples"));
            }
            return Ok(pos);
        }
        if before_in == decoder.total_in() && before_out == decoder.total_out() {
            return Err(invalid("truncated zlib stream"));
        }
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
    let expected = row
        .checked_mul(rows)
        .filter(|&v| v <= MAX_DECODED)
        .ok_or_else(|| invalid("composite exceeds 128 MiB"))?;
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
    layers(&input.bytes[layer], large)?;
    input.ensure(pos + 2)?;
    let method = number(&mut &input.bytes[pos..], 2)?;
    pos += 2;
    input.work(expected)?;
    match method {
        0 => {
            let end = pos + expected;
            input.ensure(end)?;
            Ok(end)
        }
        1 => rle(input, pos, rows, row, large),
        2 | 3 if method == 2 || depth != 1 => zlib(input, pos, expected),
        _ => Err(invalid("composite compression")),
    }
}
