//! Structural extent checks for contiguous files. Payloads are never executed.
use super::{Format, container::*};
use crate::{Error, Result, source::Image};
use std::sync::atomic::AtomicBool;

fn invalid() -> Error {
    Error::Invalid("invalid or unsupported contiguous file structure".into())
}

pub(super) fn signature(bytes: &[u8]) -> Option<Format<'static>> {
    match bytes.first()? {
        b'R' if bytes.starts_with(b"RIFF") => match bytes.get(8..12)? {
            b"WAVE" => Some(Format::Wav),
            b"WEBP" => Some(Format::Webp),
            _ => None,
        },
        b'G' if bytes.starts_with(b"GIF87a") || bytes.starts_with(b"GIF89a") => Some(Format::Gif),
        b'S' if bytes.starts_with(b"SQLite format 3\0") => Some(Format::Sqlite),
        b'B' if bytes.starts_with(b"BM")
            && bytes.get(6..10) == Some(&[0; 4])
            && bytes.get(14..18) == Some(&40u32.to_le_bytes()) =>
        {
            Some(Format::Bmp)
        }
        _ => None,
    }
}

pub(super) fn validate(
    format: Format,
    image: &mut Image,
    at: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
    boundary: u64,
) -> Result<Vec<u8>> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    input.ensure(18.min(input.limit()))?;
    if signature(&input.bytes) != Some(format) {
        return Err(invalid());
    }
    let end = match format {
        Format::Wav | Format::Webp => riff(&mut input, format)?,
        Format::Gif => gif(&mut input)?,
        Format::Bmp => bmp(&mut input)?,
        Format::Sqlite => sqlite(&mut input)?,
        _ => return Err(invalid()),
    };
    input.finish(end)
}

// A classic RIFF file consists of bounded word-padded chunks.
fn riff(input: &mut Input<'_>, format: Format) -> Result<usize> {
    let end = usize64(8 + u64::from(u32le(&input.bytes, 4)))?;
    if end < 20 || end > input.limit() {
        return Err(invalid());
    }
    let mut pos = 12;
    let mut alignment = None;
    let mut payload = false;
    for _ in 0..65_536 {
        if pos == end && payload {
            return Ok(end);
        }
        if pos + 8 > end {
            return Err(invalid());
        }
        input.check()?;
        input.ensure(pos + 8)?;
        let length = u32le(&input.bytes, pos + 4) as u64;
        let body = pos + 8;
        pos = usize64(body as u64 + length + length % 2)?;
        if pos > end {
            return Err(invalid());
        }
        input.ensure(pos)?;
        let chunk = &input.bytes[body..body + length as usize];
        match (format, &input.bytes[body - 8..body - 4]) {
            (Format::Wav, b"fmt ") => {
                if alignment.is_some() || chunk.len() < 16 || !matches!(u16le(chunk, 0), 1 | 3) {
                    return Err(invalid());
                }
                let channels = u32::from(u16le(chunk, 2));
                let bits = u32::from(u16le(chunk, 14));
                let align = u16le(chunk, 12);
                let rate = u32le(chunk, 4);
                if align == 0
                    || rate == 0
                    || channels * bits.div_ceil(8) != u32::from(align)
                    || rate.checked_mul(u32::from(align)) != Some(u32le(chunk, 8))
                {
                    return Err(invalid());
                }
                alignment = Some(align);
            }
            (Format::Wav, b"data") => {
                let align = alignment.ok_or_else(invalid)?;
                if payload || !length.is_multiple_of(u64::from(align)) {
                    return Err(invalid());
                }
                payload = true;
            }
            (Format::Webp, b"VP8 " | b"VP8L") => {
                let valid = if &input.bytes[body - 8..body - 4] == b"VP8L" {
                    chunk.len() > 5 && chunk[0] == 0x2f && chunk[4] & 0xe0 == 0
                } else {
                    chunk.len() > 10
                        && chunk[0] & 1 == 0
                        && chunk[3..6] == [0x9d, 1, 0x2a]
                        && u16le(chunk, 6) & 0x3fff != 0
                        && u16le(chunk, 8) & 0x3fff != 0
                };
                if !valid || payload {
                    return Err(invalid());
                }
                payload = true;
            }
            (Format::Webp, b"ANIM" | b"ANMF") => return Err(invalid()),
            _ => {}
        }
    }
    Err(Error::Limit("RIFF chunk count".into()))
}

fn bmp(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(54)?;
    let b = &input.bytes;
    let size = u32le(b, 2);
    let pixels = u32le(b, 10);
    let width = u32le(b, 18) as i32;
    let height = (u32le(b, 22) as i32).unsigned_abs();
    let bits = u16le(b, 28);
    if width <= 0
        || height == 0
        || !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32)
        || u16le(b, 26) != 1
        || u32le(b, 30) != 0
    {
        return Err(invalid());
    }
    let colors = match (u32le(b, 46), bits) {
        (0, 1 | 4 | 8) => 1 << bits,
        (n, 1 | 4 | 8) if n > 1 << bits => return Err(invalid()),
        (n, _) => n,
    };
    let stride = (width as u64 * u64::from(bits)).div_ceil(32) * 4;
    let payload = stride.checked_mul(u64::from(height)).ok_or_else(invalid)?;
    if u64::from(pixels) < 54 + 4 * u64::from(colors)
        || u64::from(size.checked_sub(pixels).ok_or_else(invalid)?) != payload
        || (u32le(b, 34) != 0 && u64::from(u32le(b, 34)) != payload)
    {
        return Err(invalid());
    }
    usize64(u64::from(size))
}

fn palette(packed: u8) -> usize {
    if packed & 0x80 == 0 {
        0
    } else {
        6 << (packed & 7)
    }
}

fn gif(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(13)?;
    let width = u32::from(u16le(&input.bytes, 6));
    let height = u32::from(u16le(&input.bytes, 8));
    let global = palette(input.bytes[10]);
    if width == 0 || height == 0 {
        return Err(invalid());
    }
    let mut pos = 13 + global;
    let mut has_frame = false;
    let mut sub_blocks = 0;
    for _ in 0..65_536 {
        input.check()?;
        input.ensure(pos + 1)?;
        match input.bytes[pos] {
            0x3b if has_frame => return Ok(pos + 1),
            0x21 => {
                pos += 2;
            }
            0x2c => {
                input.ensure(pos + 10)?;
                let desc = &input.bytes[pos + 1..pos + 10];
                let w = u32::from(u16le(desc, 4));
                let h = u32::from(u16le(desc, 6));
                let local = palette(desc[8]);
                if w == 0
                    || h == 0
                    || w + u32::from(u16le(desc, 0)) > width
                    || h + u32::from(u16le(desc, 2)) > height
                    || global + local == 0
                {
                    return Err(invalid());
                }
                pos += 10 + local;
                input.ensure(pos + 2)?;
                if !(2..=8).contains(&input.bytes[pos]) || input.bytes[pos + 1] == 0 {
                    return Err(invalid());
                }
                pos += 1;
                has_frame = true;
            }
            _ => return Err(invalid()),
        }
        loop {
            if sub_blocks == 1_048_576 {
                return Err(Error::Limit("GIF sub-block count".into()));
            }
            sub_blocks += 1;
            input.ensure(pos + 1)?;
            let n = usize::from(input.bytes[pos]);
            pos += 1 + n;
            if n == 0 {
                break;
            }
        }
    }
    Err(Error::Limit("GIF block count".into()))
}

fn sqlite(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(108)?;
    let b = &input.bytes;
    let pages = u32::from_be_bytes(b[28..32].try_into().unwrap());
    let page_size = match u16::from_be_bytes([b[16], b[17]]) {
        1 => 65_536,
        n if n >= 512 && n.is_power_of_two() => u32::from(n),
        _ => return Err(invalid()),
    };
    // The in-header page count is authoritative only if both counters agree.
    let usable = page_size - u32::from(b[20]);
    let cells = u32::from(u16::from_be_bytes([b[103], b[104]]));
    let cell_start = match u16::from_be_bytes([b[105], b[106]]) {
        0 => 65_536,
        n => u32::from(n),
    };
    let header = match b[100] {
        5 => 12,
        13 => 8,
        _ => return Err(invalid()),
    };
    if pages == 0
        || b[24..28] != b[92..96]
        || b[21..24] != [64, 32, 32]
        || !matches!(b[18], 1 | 2)
        || !matches!(b[19], 1 | 2)
        || b[72..92] != [0; 20]
        || usable < 480
        || cell_start > usable
        || 100 + header + 2 * cells > cell_start
        || b[107] > 60
    {
        return Err(invalid());
    }
    usize64(u64::from(page_size) * u64::from(pages))
}
