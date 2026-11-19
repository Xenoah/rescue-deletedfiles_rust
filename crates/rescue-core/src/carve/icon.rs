//! ICO and CUR with a complete directory and contiguous PNG or uncompressed DIB resources.
use super::{
    Format, SIGNATURE, charge,
    container::{Input, MAX_ENCODED, u16le, u32le},
    png,
};
use crate::{Error, Result, source::Image};
use std::{ops::Range, sync::atomic::AtomicBool};

fn invalid() -> Error {
    Error::Invalid("invalid or unsupported icon/cursor directory or image".into())
}

pub(super) fn signature(b: &[u8]) -> Option<Format<'static>> {
    let format = match b.get(..4)? {
        b"\0\0\x01\0" => Format::Ico,
        b"\0\0\x02\0" => Format::Cur,
        _ => return None,
    };
    if b.len() < 22
        || b[9] != 0
        || (format == Format::Ico
            && (u16le(b, 10) > 1 || !matches!(u16le(b, 12), 0 | 1 | 2 | 4 | 8 | 16 | 24 | 32)))
    {
        return None;
    }
    let count = usize::from(u16le(b, 4));
    let offset = u32le(b, 18) as usize;
    let size = u32le(b, 14) as usize;
    if (count == 1 && offset != 22)
        || !(1..=256).contains(&count)
        || size < 40
        || offset < 6 + 16 * count
        || offset.checked_add(size).is_none_or(|end| end > MAX_ENCODED)
    {
        return None;
    }
    Some(format)
}

struct Resource {
    range: Range<usize>,
    width: usize,
    height: usize,
    bits: u16,
}

fn dimension(n: u8) -> usize {
    if n == 0 { 256 } else { usize::from(n) }
}

fn dib(b: &[u8], r: &Resource) -> Result<()> {
    if b.len() < 40
        || u32le(b, 0) != 40
        || u32le(b, 4) as usize != r.width
        || u32le(b, 8) as usize != r.height * 2
        || u16le(b, 12) != 1
        || u32le(b, 16) != 0
    {
        return Err(invalid());
    }
    let bits = usize::from(u16le(b, 14));
    if !matches!(bits, 1 | 4 | 8 | 16 | 24 | 32) || (r.bits != 0 && usize::from(r.bits) != bits) {
        return Err(invalid());
    }
    let declared = u32le(b, 32) as usize;
    let colors = if bits <= 8 && declared == 0 {
        1 << bits
    } else {
        declared
    };
    if colors > 256 || (bits <= 8 && colors > 1 << bits) {
        return Err(invalid());
    }
    let start = 40 + colors * 4;
    let stride = (r.width * bits).div_ceil(32) * 4;
    let data_end = start + stride * r.height;
    let mask = r.width.div_ceil(32) * 4 * r.height;
    if b.len() != data_end + mask {
        return Err(invalid());
    }
    if bits <= 8 && colors < (1 << bits) {
        for row in b[start..data_end].chunks_exact(stride) {
            for x in 0..r.width {
                let bit = x * bits;
                let index = (usize::from(row[bit / 8]) >> (8 - bits - bit % 8)) & ((1 << bits) - 1);
                if index >= colors {
                    return Err(invalid());
                }
            }
        }
    }
    Ok(())
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
    input.ensure(22)?;
    if signature(&input.bytes) != Some(format) {
        return Err(invalid());
    }
    let count = usize::from(u16le(&input.bytes, 4));
    let table_end = 6 + 16 * count;
    input.ensure(table_end)?;
    let mut entries = Vec::with_capacity(count);
    for e in input.bytes[6..table_end].as_chunks::<16>().0 {
        let width = dimension(e[0]);
        let height = dimension(e[1]);
        if e[3] != 0 {
            return Err(invalid());
        }
        let bits = if format == Format::Ico {
            if u16le(e, 4) > 1 || !matches!(u16le(e, 6), 0 | 1 | 2 | 4 | 8 | 16 | 24 | 32) {
                return Err(invalid());
            }
            u16le(e, 6)
        } else {
            if usize::from(u16le(e, 4)) >= width || usize::from(u16le(e, 6)) >= height {
                return Err(invalid());
            }
            0
        };
        let start = u32le(e, 12) as usize;
        let size = u32le(e, 8) as usize;
        let end = start
            .checked_add(size)
            .filter(|v| *v <= input.limit())
            .ok_or_else(invalid)?;
        if size < 40 || start < table_end {
            return Err(invalid());
        }
        entries.push(Resource {
            range: start..end,
            width,
            height,
            bits,
        });
    }
    entries.sort_by_key(|r| r.range.start);
    let mut end = table_end;
    for r in &entries {
        if r.range.start != end {
            return Err(invalid());
        }
        end = r.range.end;
    }
    let bytes = input.finish(end)?;
    for r in entries {
        let data = &bytes[r.range.clone()];
        charge(budget, data.len())?;
        if data.starts_with(SIGNATURE) {
            if u32::from_be_bytes(data[16..20].try_into().unwrap()) as usize != r.width
                || u32::from_be_bytes(data[20..24].try_into().unwrap()) as usize != r.height
            {
                return Err(invalid());
            }
            let (kind, payload) = png::validate(
                image,
                at + r.range.start as u64,
                cancel,
                budget,
                at + r.range.end as u64,
            )?;
            if kind != Format::Png || payload != data {
                return Err(invalid());
            }
        } else {
            dib(data, &r)?;
        }
    }
    Ok(bytes)
}
