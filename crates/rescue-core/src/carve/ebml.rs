//! Finite Matroska/WebM segments. Live streams with unknown sizes have no independent end.
use super::{
    Format,
    container::{Input, usize64},
};
use crate::{Error, Result};
use std::{collections::BTreeSet, ops::Range};

fn invalid() -> Error {
    Error::Invalid("invalid or unsupported Matroska/WebM element".into())
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Vint {
    Id,
    Size,
    Value,
}

fn vint(input: &mut Input<'_>, pos: &mut usize, end: usize, kind: Vint) -> Result<u64> {
    if *pos >= end {
        return Err(invalid());
    }
    input.ensure(*pos + 1)?;
    let first = input.bytes[*pos];
    let n = first.leading_zeros() as usize + 1;
    if n > if kind == Vint::Id { 4 } else { 8 } || *pos + n > end {
        return Err(invalid());
    }
    input.ensure(*pos + n)?;
    let mut value = u64::from(if kind == Vint::Id {
        first
    } else {
        first & (0xffu16 >> n) as u8
    });
    for &byte in &input.bytes[*pos + 1..*pos + n] {
        value = (value << 8) | u64::from(byte);
    }
    *pos += n;
    if kind == Vint::Size && value == (1u64 << (7 * n)) - 1 {
        return Err(invalid());
    }
    Ok(value)
}

fn element(input: &mut Input<'_>, pos: &mut usize, end: usize) -> Result<(u64, Range<usize>)> {
    let id = vint(input, pos, end, Vint::Id)?;
    let size = usize64(vint(input, pos, end, Vint::Size)?)?;
    let body = *pos..*pos + size;
    if body.end > end {
        return Err(invalid());
    }
    *pos = body.end;
    Ok((id, body))
}

fn children(
    input: &mut Input<'_>,
    parent: Range<usize>,
    remaining: &mut usize,
    mut visit: impl FnMut(&mut Input<'_>, u64, Range<usize>, &mut usize) -> Result<()>,
) -> Result<()> {
    let mut pos = parent.start;
    let mut checksum = None;
    while pos < parent.end {
        if *remaining == 0 {
            return Err(Error::Limit("Matroska element count".into()));
        }
        *remaining -= 1;
        input.check()?;
        let start = pos;
        let (id, data) = element(input, &mut pos, parent.end)?;
        if id == 0xbf {
            if checksum.is_some() || data.len() != 4 {
                return Err(invalid());
            }
            input.ensure(data.end)?;
            let expected = u32::from_le_bytes(input.bytes[data.clone()].try_into().unwrap());
            checksum = Some((start..data.end, expected));
        } else {
            visit(input, id, data, remaining)?;
        }
    }
    if let Some((skip, expected)) = checksum {
        input.ensure(parent.end)?;
        let mut crc = crc32fast::Hasher::new();
        for range in [parent.start..skip.start, skip.end..parent.end] {
            input.work(range.len())?;
            for block in input.bytes[range].chunks(65536) {
                input.check()?;
                crc.update(block);
            }
        }
        if crc.finalize() != expected {
            return Err(invalid());
        }
    }
    Ok(())
}

fn uint(input: &mut Input<'_>, data: Range<usize>) -> Result<u64> {
    if data.is_empty() || data.len() > 8 {
        return Err(invalid());
    }
    input.ensure(data.end)?;
    Ok(input.bytes[data]
        .iter()
        .fold(0, |n, &b| (n << 8) | u64::from(b)))
}

fn track(input: &mut Input<'_>, data: Range<usize>, remaining: &mut usize) -> Result<u64> {
    let mut number = None;
    let mut kind = None;
    let mut codec = false;
    children(input, data, remaining, |input, id, field, _| {
        match id {
            0xd7 if number.is_none() => number = Some(uint(input, field)?),
            0x83 if kind.is_none() => kind = Some(uint(input, field)?),
            0x86 if !codec && (1..=255).contains(&field.len()) => {
                input.ensure(field.end)?;
                if !input.bytes[field].iter().all(u8::is_ascii_graphic) {
                    return Err(invalid());
                }
                codec = true;
            }
            0xd7 | 0x83 | 0x86 => return Err(invalid()),
            _ => {}
        }
        Ok(())
    })?;
    number
        .filter(|n| *n != 0 && kind.is_some_and(|k| k != 0) && codec)
        .ok_or_else(invalid)
}

// Check frame boundaries without decoding codec payloads. The final lace consumes the
// remaining bytes; earlier laces use Xiph sizes, a fixed frame size, or signed EBML deltas.
fn block_track(input: &mut Input<'_>, field: Range<usize>) -> Result<u64> {
    let mut pos = field.start;
    let number = vint(input, &mut pos, field.end, Vint::Value)?;
    if number == 0 || pos + 3 >= field.end {
        return Err(invalid());
    }
    input.ensure(pos + 3)?;
    let lacing = (input.bytes[pos + 2] >> 1) & 3;
    pos += 3;
    if lacing == 0 {
        return Ok(number);
    }
    input.ensure(pos + 1)?;
    let frames = usize::from(input.bytes[pos]) + 1;
    pos += 1;
    if frames < 2 {
        return Err(invalid());
    }
    if lacing == 2 {
        if field.end == pos || !(field.end - pos).is_multiple_of(frames) {
            return Err(invalid());
        }
        return Ok(number);
    }
    let mut total = 0u64;
    let mut previous = 0i64;
    for index in 0..frames - 1 {
        let size = if lacing == 1 {
            let mut size = 0;
            loop {
                if pos >= field.end {
                    return Err(invalid());
                }
                input.ensure(pos + 1)?;
                let available = &input.bytes[pos..field.end.min(input.bytes.len())];
                let full = available
                    .iter()
                    .position(|v| *v != 255)
                    .unwrap_or(available.len());
                size += 255 * full as i64;
                pos += full;
                if full < available.len() {
                    size += i64::from(available[full]);
                    pos += 1;
                    break;
                }
            }
            size
        } else {
            let start = pos;
            let value = vint(input, &mut pos, field.end, Vint::Value)? as i64;
            if index == 0 {
                value
            } else {
                let bias = (1i64 << (7 * (pos - start) - 1)) - 1;
                previous.checked_add(value - bias).ok_or_else(invalid)?
            }
        };
        if size < 0 || size as u64 > (field.end - pos) as u64 {
            return Err(invalid());
        }
        total += size as u64;
        previous = size;
    }
    if total > (field.end - pos) as u64 {
        return Err(invalid());
    }
    Ok(number)
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<(Format<'static>, usize)> {
    let mut pos = 0;
    let limit = input.limit();
    let (id, header) = element(input, &mut pos, limit)?;
    if id != 0x1a45dfa3 || header.len() > 4096 {
        return Err(invalid());
    }
    let mut format = None;
    let mut remaining = 262144;
    children(input, header, &mut remaining, |input, id, body, _| {
        if id == 0x4282 {
            input.ensure(body.end)?;
            if format.is_some() {
                return Err(invalid());
            }
            format = Some(match &input.bytes[body] {
                b"matroska" => Format::Mkv,
                b"webm" => Format::Webm,
                _ => return Err(invalid()),
            });
        }
        Ok(())
    })?;
    let format = format.ok_or_else(invalid)?;
    let (id, segment) = element(input, &mut pos, limit)?;
    if id != 0x18538067 {
        return Err(invalid());
    }
    let mut info = false;
    let mut tracks = false;
    let mut track_numbers = BTreeSet::new();
    let mut referenced = BTreeSet::new();
    children(
        input,
        segment,
        &mut remaining,
        |input, id, data, remaining| {
            match id {
                0x1549a966 if !info => {
                    if data.is_empty() {
                        return Err(invalid());
                    }
                    children(input, data, remaining, |_, _, _, _| Ok(()))?;
                    info = true;
                }
                0x1654ae6b if !tracks => {
                    let mut count = 0;
                    children(input, data, remaining, |input, id, field, remaining| {
                        if id == 0xae {
                            if count >= 256
                                || !track_numbers.insert(track(input, field, remaining)?)
                            {
                                return Err(invalid());
                            }
                            count += 1;
                        }
                        Ok(())
                    })?;
                    if count == 0 {
                        return Err(invalid());
                    }
                    tracks = true;
                }
                0x1549a966 | 0x1654ae6b => return Err(invalid()),
                0x1f43b675 => {
                    let mut timestamp = false;

                    children(input, data, remaining, |input, id, field, remaining| {
                        match id {
                            0xe7 => {
                                uint(input, field)?;
                                timestamp = true;
                            }
                            0xa3 => {
                                referenced.insert(block_track(input, field)?);
                            }
                            0xa0 => {
                                children(input, field, remaining, |input, id, block_data, _| {
                                    if id == 0xa1 {
                                        referenced.insert(block_track(input, block_data)?);
                                    }
                                    Ok(())
                                })?
                            }
                            _ => {}
                        }
                        Ok(())
                    })?;
                    if !timestamp {
                        return Err(invalid());
                    }
                    if referenced.len() > 256 {
                        return Err(invalid());
                    }
                }
                _ => {}
            }
            Ok(())
        },
    )?;
    if !info || !tracks || referenced.is_empty() || !referenced.is_subset(&track_numbers) {
        return Err(invalid());
    }
    Ok((format, pos))
}
