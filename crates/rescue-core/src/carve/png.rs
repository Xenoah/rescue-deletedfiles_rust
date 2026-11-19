//! PNG/APNG frame validation; text and ICC metadata are retained without interpretation.
use super::{Format, charge};
use crate::{Error, Result, source::Image};
use flate2::{Decompress, FlushDecompress, Status};
use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, Ordering},
};
const SIGNATURE: &[u8; 8] = b"\x89PNG\r\n\x1a\n";
const MAX_PNG: usize = 64 * 1024 * 1024;
const MAX_CHUNKS: usize = 65_536;

fn invalid(message: &str) -> Error {
    Error::Invalid(format!("PNG: {message}"))
}

/// Exact filtered scanline length, including the seven Adam7 subimages.
fn inflated_size(info: &png::Info<'_>, width: u32, height: u32) -> Result<usize> {
    let passes = if info.interlaced {
        vec![
            (0, 0, 8, 8),
            (4, 0, 8, 8),
            (0, 4, 4, 8),
            (2, 0, 4, 4),
            (0, 2, 2, 4),
            (1, 0, 2, 2),
            (0, 1, 1, 2),
        ]
    } else {
        vec![(0, 0, 1, 1)]
    };
    let bits = info.color_type.samples() as u64 * info.bit_depth as u64;
    let mut size = 0u64;
    for (x, y, dx, dy) in passes {
        let width = u64::from(width).saturating_sub(x).div_ceil(dx);
        let height = u64::from(height).saturating_sub(y).div_ceil(dy);
        if width == 0 || height == 0 {
            continue;
        }
        size = size
            .checked_add(
                (width * bits)
                    .div_ceil(8)
                    .checked_add(1)
                    .and_then(|row| row.checked_mul(height))
                    .ok_or_else(|| invalid("scanline size overflow"))?,
            )
            .ok_or_else(|| invalid("image size overflow"))?;
    }
    if size > (MAX_PNG * 2) as u64 {
        return Err(Error::Limit("PNG filtered scanlines exceed 128 MiB".into()));
    }
    Ok(size as usize)
}

fn strict_zlib(input: &[u8], expected: usize, cancel: &AtomicBool) -> Result<()> {
    let mut stream = Decompress::new(true);
    let mut output = vec![0; 32 * 1024];
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let before = (stream.total_in(), stream.total_out());
        let status = stream
            .decompress(
                &input[before.0 as usize..],
                &mut output,
                FlushDecompress::None,
            )
            .map_err(|e| invalid(&format!("zlib: {e}")))?;
        if stream.total_out() > expected as u64 {
            return Err(invalid("extra decoded scanline data"));
        }
        if status == Status::StreamEnd {
            if stream.total_out() != expected as u64 || stream.total_in() != input.len() as u64 {
                return Err(invalid("zlib extent or decoded length mismatch"));
            }
            return Ok(());
        }
        if before == (stream.total_in(), stream.total_out()) {
            return Err(invalid("incomplete zlib stream or checksum"));
        }
    }
}

#[derive(Default)]
struct Animation {
    declared: Option<u32>,
    sequence: u32,
    first_in_idat: bool,
    frames: Vec<Frame>,
}

struct Frame {
    width: u32,
    height: u32,
    compressed: Vec<u8>,
}

fn be32(bytes: &[u8], pos: usize) -> u32 {
    u32::from_be_bytes(bytes[pos..pos + 4].try_into().unwrap())
}

impl Animation {
    fn check_sequence(&mut self, data: &[u8]) -> Result<()> {
        if self.declared.is_none() || be32(data, 0) != self.sequence {
            return Err(invalid("APNG missing control or out-of-order sequence"));
        }
        self.sequence += 1;
        Ok(())
    }

    fn chunk(&mut self, kind: &[u8], data: &[u8], prior: &[u8], has_idat: bool) -> Result<()> {
        match kind {
            b"acTL" => {
                if has_idat || self.declared.is_some() || data.len() != 8 {
                    return Err(invalid("APNG animation control order or length"));
                }
                let count = be32(data, 0);
                if !(1..=4096).contains(&count) {
                    return Err(invalid("APNG frame count (1..4096)"));
                }
                self.declared = Some(count);
            }
            b"fcTL" => {
                if data.len() != 26 {
                    return Err(invalid("APNG frame control length"));
                }
                self.check_sequence(data)?;
                let width = be32(data, 4);
                let height = be32(data, 8);
                let x = be32(data, 12);
                let y = be32(data, 16);
                let canvas_width = be32(prior, 16);
                let canvas_height = be32(prior, 20);
                if width == 0
                    || height == 0
                    || x.checked_add(width).is_none_or(|end| end > canvas_width)
                    || y.checked_add(height).is_none_or(|end| end > canvas_height)
                    || data[24] > 2
                    || data[25] > 1
                {
                    return Err(invalid("APNG frame rectangle or disposal/blend operation"));
                }
                if has_idat {
                    if self.frames.last().is_some_and(|f| f.compressed.is_empty())
                        || self.frames.len() + usize::from(self.first_in_idat)
                            >= self.declared.unwrap() as usize
                    {
                        return Err(invalid("APNG missing or excess frame"));
                    }
                    self.frames.push(Frame {
                        width,
                        height,
                        compressed: Vec::new(),
                    });
                } else {
                    if self.first_in_idat || width != canvas_width || height != canvas_height {
                        return Err(invalid("APNG default frame does not cover canvas"));
                    }
                    self.first_in_idat = true;
                }
            }
            b"fdAT" => {
                if !has_idat || data.len() < 4 {
                    return Err(invalid("APNG frame data order or length"));
                }
                self.check_sequence(data)?;
                let frame = self
                    .frames
                    .last_mut()
                    .ok_or_else(|| invalid("APNG frame data without a control"))?;
                frame.compressed.extend_from_slice(&data[4..]);
            }
            _ => {}
        }
        Ok(())
    }
    fn finish(&self) -> Result<()> {
        if let Some(count) = self.declared
            && (count as usize != self.frames.len() + usize::from(self.first_in_idat)
                || self.frames.last().is_some_and(|f| f.compressed.is_empty()))
        {
            return Err(invalid("APNG declared frame count or missing frame data"));
        }
        Ok(())
    }
}

/// Independently determines the extent, validates every chunk CRC, then decodes pixels.
/// Text/ICC payloads are retained verbatim; their compressed contents are not interpreted.
pub(super) fn validate(
    image: &mut Image,
    offset: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
    boundary: u64,
) -> Result<(Format<'static>, Vec<u8>)> {
    if offset.checked_add(8).is_none_or(|end| end > boundary) {
        return Err(invalid("signature outside selected range"));
    }
    let mut bytes = image.read(offset, 8)?;
    charge(budget, 8)?;
    if bytes != SIGNATURE {
        return Err(invalid("signature"));
    }
    let mut has_idat = false;
    let mut ended_idat = false;
    let mut has_palette = false;
    let mut finished = false;
    let mut idat = Vec::new();
    let mut animation = Animation::default();
    let mut palette_entries = 0;
    for index in 0..MAX_CHUNKS {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let start = offset
            .checked_add(bytes.len() as u64)
            .ok_or_else(|| invalid("offset overflow"))?;
        if start.checked_add(8).is_none_or(|end| end > boundary) {
            return Err(invalid("chunk header outside selected range"));
        }
        let header = image.read(start, 8)?;
        charge(budget, 8)?;
        let length = u32::from_be_bytes(header[..4].try_into().unwrap()) as usize;
        let kind = &header[4..8];
        if length > MAX_PNG || bytes.len().saturating_add(length + 12) > MAX_PNG {
            return Err(Error::Limit("PNG encoded extent exceeds 64 MiB".into()));
        }
        if !kind.iter().all(u8::is_ascii_alphabetic) || !kind[2].is_ascii_uppercase() {
            return Err(invalid("chunk type"));
        }
        if (index == 0 && (kind != b"IHDR" || length != 13)) || (index != 0 && kind == b"IHDR") {
            return Err(invalid("IHDR order or length"));
        }
        match kind {
            b"PLTE" => {
                if has_palette
                    || has_idat
                    || length == 0
                    || length > 768
                    || !length.is_multiple_of(3)
                {
                    return Err(invalid("PLTE order or length"));
                }
                has_palette = true;
            }
            b"IDAT" => {
                if ended_idat {
                    return Err(invalid("nonconsecutive IDAT"));
                }
                has_idat = true;
            }
            b"IEND" => {
                if length != 0 || !has_idat {
                    return Err(invalid("IEND without image data"));
                }
            }
            b"IHDR" => {}
            _ if kind[0].is_ascii_uppercase() => {
                return Err(Error::Unsupported("unknown critical PNG chunk".into()));
            }
            _ => {}
        }
        if has_idat && kind != b"IDAT" {
            ended_idat = true;
        }
        if start
            .checked_add(length as u64 + 12)
            .is_none_or(|end| end > boundary)
        {
            return Err(invalid("PNG extends outside selected range"));
        }
        charge(budget, length + 4)?;
        let body = image.read(start + 8, length + 4)?;
        let mut crc = crc32fast::Hasher::new();
        crc.update(kind);
        crc.update(&body[..length]);
        if crc.finalize() != u32::from_be_bytes(body[length..].try_into().unwrap()) {
            return Err(invalid("chunk CRC mismatch"));
        }
        animation.chunk(kind, &body[..length], &bytes, has_idat)?;
        if kind == b"PLTE" {
            palette_entries = length / 3;
        }
        if kind == b"IDAT" {
            idat.extend_from_slice(&body[..length]);
        }
        bytes.extend_from_slice(&header);
        bytes.extend_from_slice(&body);
        if kind == b"IEND" {
            finished = true;
            break;
        }
    }
    if !finished {
        return Err(Error::Limit("PNG chunk count exceeds 65536".into()));
    }
    animation.finish()?;
    let mut decoder =
        png::Decoder::new_with_limits(Cursor::new(&bytes), png::Limits { bytes: MAX_PNG });
    decoder.ignore_checksums(false);
    decoder.set_ignore_text_chunk(true);
    decoder.set_ignore_iccp_chunk(true);
    let mut reader = decoder.read_info().map_err(|e| invalid(&e.to_string()))?;
    let info = reader.info();
    let depth = info.bit_depth as usize;
    let indexed = info.color_type == png::ColorType::Indexed;
    if ((info.color_type == png::ColorType::Grayscale
        || info.color_type == png::ColorType::GrayscaleAlpha)
        && has_palette)
        || (indexed && (!has_palette || palette_entries > 1usize << depth))
    {
        return Err(invalid("PLTE does not match color type or bit depth"));
    }
    let canvas = (info.width, info.height);
    let mut frames = vec![Frame {
        width: canvas.0,
        height: canvas.1,
        compressed: idat,
    }];
    frames.append(&mut animation.frames);
    let length = reader
        .output_buffer_size()
        .filter(|n| *n <= MAX_PNG)
        .ok_or_else(|| Error::Limit("PNG decoded pixels exceed 64 MiB".into()))?;
    let mut pixels = Vec::new();
    let mut total = 0;
    for frame in frames {
        let raw_size = inflated_size(reader.info(), frame.width, frame.height)?;
        total += raw_size;
        if total > MAX_PNG * 2 {
            return Err(Error::Limit(
                "PNG/APNG total filtered scanlines exceed 128 MiB".into(),
            ));
        }
        charge(budget, raw_size)?;
        strict_zlib(&frame.compressed, raw_size, cancel)?;
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        // The filtered stream includes at least the packed pixels for each row.
        // Charge decoding before invoking the pixel decoder, including subframes.
        charge(budget, raw_size)?;
        if pixels.is_empty() {
            pixels.resize(length, 0);
        }
        let output = reader
            .next_frame(&mut pixels)
            .map_err(|e| invalid(&e.to_string()))?;
        if (output.width, output.height) != (frame.width, frame.height) {
            return Err(invalid("PNG frame dimensions"));
        }
        if indexed {
            let mask = (1usize << depth) - 1;
            for row in pixels[..output.buffer_size()].chunks_exact(output.line_size) {
                for x in 0..output.width as usize {
                    let bit = x * depth;
                    if (usize::from(row[bit / 8]) >> (8 - depth - bit % 8)) & mask
                        >= palette_entries
                    {
                        return Err(invalid("pixel index outside PLTE"));
                    }
                }
            }
        }
    }
    reader.finish().map_err(|e| invalid(&e.to_string()))?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok((
        if animation.declared.is_some() {
            Format::Apng
        } else {
            Format::Png
        },
        bytes,
    ))
}
