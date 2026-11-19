//! Bounded single-frame Zstandard and LZ4 carving.
use super::{Format, container::Input};
use crate::{Error, Result, source::Image};
use std::{
    io::Read,
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_OUTPUT: u64 = 128 * 1024 * 1024;
const MAX_WINDOW: u64 = 64 * 1024 * 1024;
const MAX_BLOCKS: usize = 65536;

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("compressed frame: {reason}"))
}

pub(super) fn signature(head: &[u8]) -> Option<Format<'static>> {
    match head.get(..4)? {
        b"\x28\xb5\x2f\xfd" => Some(Format::Zstd),
        b"\x04\x22\x4d\x18" => Some(Format::Lz4),
        _ => None,
    }
}

fn zstd_frame(input: &mut Input<'_>) -> Result<(usize, Option<u64>)> {
    input.ensure(5)?;
    let flags = input.bytes[4];
    if flags & 0x08 != 0 || flags & 3 != 0 {
        return Err(Error::Unsupported(
            "Zstandard reserved flag or dictionary ID".into(),
        ));
    }
    let single_segment = flags & 0x20 != 0;
    let size_length = match flags >> 6 {
        0 => usize::from(single_segment),
        1 => 2,
        2 => 4,
        _ => 8,
    };
    let start = 5 + usize::from(!single_segment);
    let mut cursor = start + size_length;
    input.ensure(cursor)?;
    let size = if size_length == 0 {
        None
    } else {
        let mut field = [0; 8];
        field[..size_length].copy_from_slice(&input.bytes[start..cursor]);
        Some(u64::from_le_bytes(field) + if size_length == 2 { 256 } else { 0 })
    };
    if size.is_some_and(|s| s > MAX_OUTPUT || single_segment && s > MAX_WINDOW) {
        return Err(Error::Limit(
            "Zstandard content or window exceeds carving limit".into(),
        ));
    }
    for _ in 0..MAX_BLOCKS {
        input.check()?;
        input.ensure(cursor + 3)?;
        let header = u32::from_le_bytes([
            input.bytes[cursor],
            input.bytes[cursor + 1],
            input.bytes[cursor + 2],
            0,
        ]);
        let length = (header >> 3) as usize;
        if length > 128 * 1024 {
            return Err(invalid("Zstandard block exceeds 128 KiB"));
        }
        let stored = match (header >> 1) & 3 {
            0 | 2 => length,
            1 => 1,
            _ => return Err(invalid("reserved Zstandard block type")),
        };
        cursor += 3 + stored;
        if header & 1 != 0 {
            cursor += if flags & 4 != 0 { 4 } else { 0 };
            input.ensure(cursor)?;
            return Ok((cursor, size));
        }
    }
    Err(Error::Limit(
        "Zstandard frame exceeds block count limit".into(),
    ))
}

fn lz4_frame(input: &mut Input<'_>) -> Result<(usize, Option<u64>)> {
    input.ensure(7)?;
    let flags = input.bytes[4];
    let descriptor = input.bytes[5];
    if flags >> 6 != 1 || flags & 3 != 0 || descriptor & 0x8f != 0 {
        return Err(Error::Unsupported(
            "LZ4 frame flags, version or dictionary ID".into(),
        ));
    }
    let max_block = match descriptor >> 4 {
        4 => 64 * 1024,
        5 => 256 * 1024,
        6 => 1024 * 1024,
        7 => 4 * 1024 * 1024,
        _ => return Err(invalid("LZ4 maximum block size")),
    };
    let size = if flags & 8 != 0 {
        input.ensure(15)?;
        Some(super::container::u64le(&input.bytes, 6))
    } else {
        None
    };
    if size.is_some_and(|s| s > MAX_OUTPUT) {
        return Err(Error::Limit(
            "LZ4 frame exceeds decoded output limit".into(),
        ));
    }
    let mut cursor = if size.is_some() { 15 } else { 7 };
    for _ in 0..=MAX_BLOCKS {
        input.check()?;
        input.ensure(cursor + 4)?;
        let block = super::container::u32le(&input.bytes, cursor);
        cursor += 4;
        if block == 0 {
            cursor += if flags & 4 != 0 { 4 } else { 0 };
            input.ensure(cursor)?;
            return Ok((cursor, size));
        }
        let length = (block & 0x7fff_ffff) as usize;
        if length > max_block {
            return Err(invalid("LZ4 block exceeds declared maximum"));
        }
        cursor += length + if flags & 0x10 != 0 { 4 } else { 0 };
    }
    Err(Error::Limit("LZ4 frame exceeds block count limit".into()))
}

fn decode(
    decoder: &mut impl Read,
    total: &mut u64,
    cancel: &AtomicBool,
    budget: &mut u64,
) -> Result<()> {
    let mut buf = [0; 32768];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let n = decoder
            .read(&mut buf)
            .map_err(|e| invalid(&e.to_string()))?;
        if n == 0 {
            return Ok(());
        }
        super::charge(budget, n)?;
        *total += n as u64;
        if *total > MAX_OUTPUT {
            return Err(Error::Limit("decoded frame exceeds 128 MiB".into()));
        }
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
    input.ensure(4)?;
    if signature(&input.bytes) != Some(format) {
        return Err(invalid("unexpected magic"));
    }
    let (end, size) = match format {
        Format::Zstd => zstd_frame(&mut input)?,
        Format::Lz4 => lz4_frame(&mut input)?,
        _ => return Err(invalid("unknown frame type")),
    };
    let bytes = input.finish(end)?;
    let mut total = 0;
    if format == Format::Zstd {
        let mut decoder = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
            bytes.as_slice(),
            MAX_WINDOW,
        )
        .map_err(|e| invalid(&e.to_string()))?;
        decode(&mut decoder, &mut total, cancel, budget)?;
        if !decoder.get_ref().is_empty()
            || !decoder.decoder.is_finished()
            || decoder
                .decoder
                .get_checksum_from_data()
                .is_some_and(|v| Some(v) != decoder.decoder.get_calculated_checksum())
        {
            return Err(invalid("Zstandard end or content checksum"));
        }
    } else {
        let mut decoder = lz4_flex::frame::FrameDecoder::new(bytes.as_slice());
        // A legal empty uncompressed block can yield an empty read before the end marker.
        while !decoder.get_ref().is_empty() {
            let before = decoder.get_ref().len();
            decode(&mut decoder, &mut total, cancel, budget)?;
            if before == decoder.get_ref().len() {
                return Err(invalid("LZ4 decoder made no progress"));
            }
        }
    }
    if size.is_some_and(|stored| stored != total) {
        return Err(invalid("declared content length differs from decoded size"));
    }
    Ok(bytes)
}
