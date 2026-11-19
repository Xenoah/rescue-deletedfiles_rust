//! AIFF and AIFF-C IFF chunk boundaries and byte-addressable audio layouts.
use super::{Format, container::Input};
use crate::{Error, Result};

fn invalid() -> Error {
    Error::Invalid("invalid or unsupported AIFF audio structure".into())
}

fn be32(b: &[u8], pos: usize) -> u32 {
    u32::from_be_bytes(b[pos..pos + 4].try_into().unwrap())
}

pub(super) fn signature(bytes: &[u8]) -> Option<Format<'static>> {
    if bytes.get(..4)? != b"FORM" {
        return None;
    }
    match bytes.get(8..12)? {
        b"AIFF" => Some(Format::Aiff),
        b"AIFC" => Some(Format::Aifc),
        _ => None,
    }
}

fn sound_bytes(comm: &[u8], compressed: bool) -> Result<u64> {
    if comm.len() < 18 {
        return Err(invalid());
    }
    let channels = u16::from_be_bytes(comm[..2].try_into().unwrap());
    let frames = be32(comm, 2);
    let bits = u16::from_be_bytes(comm[6..8].try_into().unwrap());
    let exponent = u16::from_be_bytes(comm[8..10].try_into().unwrap());
    // A sample rate is a positive, finite, normalized 80-bit extended float.
    if channels == 0 || !(1..0x7fff).contains(&exponent) || comm[10] & 0x80 == 0 {
        return Err(invalid());
    }
    let codec = if compressed {
        if comm.len() < 23 || comm.len() != (23 + usize::from(comm[22])).next_multiple_of(2) {
            return Err(invalid());
        }
        &comm[18..22]
    } else if comm.len() == 18 {
        b"NONE"
    } else {
        return Err(invalid());
    };
    let width = match codec {
        b"NONE" | b"twos" | b"sowt" if (1..=32).contains(&bits) => bits.div_ceil(8),
        b"fl32" | b"FL32" if bits == 32 => 4,
        b"fl64" | b"FL64" if bits == 64 => 8,
        b"ulaw" | b"ULAW" | b"alaw" | b"ALAW" if matches!(bits, 8 | 16) => 1,
        _ => return Err(invalid()),
    };
    Ok(u64::from(width) * u64::from(channels) * u64::from(frames))
}

pub(super) fn parse(input: &mut Input, format: Format) -> Result<usize> {
    input.ensure(12)?;
    if signature(&input.bytes) != Some(format) {
        return Err(invalid());
    }
    let end = u64::from(be32(&input.bytes, 4)) + 8;
    if end < 12 || end > input.limit() as u64 {
        return Err(invalid());
    }
    let end = end as usize;
    let compressed = format == Format::Aifc;
    let mut expected = None;
    let mut data = None;
    let mut version = false;
    let mut pos = 12;
    let mut chunks = 0;
    while pos < end {
        if end - pos < 8 || chunks == 65536 {
            return Err(invalid());
        }
        input.ensure(pos + 8)?;
        let size = u64::from(be32(&input.bytes, pos + 4));
        let start = pos + 8;
        let next = start as u64 + size.next_multiple_of(2);
        if next > end as u64 {
            return Err(invalid());
        }
        let body_end = start + size as usize;
        match &input.bytes[pos..pos + 4] {
            b"COMM" if expected.is_none() && size <= 278 => {
                input.ensure(body_end)?;
                expected = Some(sound_bytes(&input.bytes[start..body_end], compressed)?);
            }
            b"SSND" if data.is_none() && size >= 8 => {
                input.ensure(start + 8)?;
                let offset = u64::from(be32(&input.bytes, start));
                let block = u64::from(be32(&input.bytes, start + 4));
                if offset > size - 8 {
                    return Err(invalid());
                }
                data = Some((size - 8 - offset, block, offset));
            }
            b"FVER" if compressed && !version && size == 4 => {
                input.ensure(body_end)?;
                if be32(&input.bytes, start) != 0xa2805140 {
                    return Err(invalid());
                }
                version = true;
            }
            b"COMM" | b"SSND" | b"FVER" => return Err(invalid()),
            _ => {}
        }
        chunks += 1;
        pos = next as usize;
    }
    let required = expected.ok_or_else(invalid)?;
    let (available, block, offset) = data.ok_or_else(invalid)?;
    // SSND may include alignment bytes after the last sample for block storage.
    if compressed != version
        || available < required
        || (available != required
            && (block == 0
                || available - required >= block
                || !(available + offset).is_multiple_of(block)))
    {
        return Err(invalid());
    }
    Ok(end)
}
