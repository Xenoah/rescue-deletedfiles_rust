//! Validate flat, single-part OpenEXR images before carving their last pixel chunk.
use super::container::{Input, u32le, u64le};
use crate::{Error, Result};
use exr::block::reader::ChunksReader;
use flate2::{Decompress, FlushDecompress, Status};
use std::{collections::BTreeMap, io::Cursor, ops::Range};

pub(super) const MAGIC: &[u8] = b"\x76\x2f\x31\x01";
const MAX_HEADER: usize = 1 << 20;
const MAX_SAMPLES: usize = 8 << 20;

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("invalid or unsupported OpenEXR: {reason}"))
}

fn name(input: &mut Input<'_>, pos: &mut usize, limit: usize) -> Result<String> {
    let start = *pos;
    for _ in 0..=limit {
        input.ensure(*pos + 1)?;
        let b = input.bytes[*pos];
        *pos += 1;
        if b == 0 {
            return String::from_utf8(input.bytes[start..*pos - 1].to_vec())
                .map_err(|_| invalid("attribute name encoding"));
        }
        if !b.is_ascii_graphic() {
            return Err(invalid("attribute name"));
        }
    }
    Err(invalid("attribute name length"))
}

type Attributes = BTreeMap<String, (String, Range<usize>)>;

fn attributes(input: &mut Input<'_>, pos: &mut usize, max_name: usize) -> Result<Attributes> {
    let mut fields = BTreeMap::new();
    loop {
        let key = name(input, pos, max_name)?;
        if key.is_empty() {
            return Ok(fields);
        }
        let kind = name(input, pos, max_name)?;
        input.ensure(*pos + 4)?;
        let len = u32le(&input.bytes, *pos) as usize;
        *pos += 4;
        let end = pos
            .checked_add(len)
            .filter(|&v| v <= MAX_HEADER)
            .ok_or_else(|| invalid("header length"))?;
        input.ensure(end)?;
        // Variable-length or nested attributes need their own allocation limits.
        // For this flat profile, pass only bounded scalar, vector and string types
        // (and the small channel list validated below) to the pixel decoder.
        let size_ok = match kind.as_str() {
            "compression" | "lineOrder" => len == 1,
            "int" | "float" => len == 4,
            "double" | "v2i" | "v2f" | "rational" | "timecode" => len == 8,
            "v3f" | "v3i" => len == 12,
            "box2i" | "box2f" | "v2d" => len == 16,
            "v3d" => len == 24,
            "keycode" => len == 28,
            "chromaticities" => len == 32,
            "m33f" => len == 36,
            "m44f" => len == 64,
            "m33d" => len == 72,
            "m44d" => len == 128,
            "tiledesc" => key == "tiles" && len == 9,
            "chlist" => key == "channels" && (1..=8192).contains(&len),
            "string" => len <= 65_536,
            _ => false,
        };
        if !size_ok || fields.len() >= 256 || fields.insert(key, (kind, *pos..end)).is_some() {
            return Err(invalid("attribute type, length or duplicate name"));
        }
        *pos = end;
    }
}

fn channels(data: &[u8], max_name: usize) -> Result<(usize, usize, usize)> {
    let (mut count, mut bytes, mut floats, mut pos) = (0, 0, 0, 0);
    let mut prev: &[u8] = &[];
    while data.get(pos).is_some_and(|&b| b != 0) {
        let length = data[pos..]
            .iter()
            .position(|&b| b == 0)
            .ok_or_else(|| invalid("channel name"))?;
        let channel = &data[pos..pos + length];
        pos += length + 1;
        let c = data
            .get(pos..pos + 16)
            .ok_or_else(|| invalid("channel record"))?;
        if length > max_name
            || count >= 16
            || channel <= prev
            || !channel.iter().all(u8::is_ascii_graphic)
            || c[4] > 1
            || c[5..8] != [0; 3]
            || u32le(c, 8) != 1
            || u32le(c, 12) != 1
        {
            return Err(invalid("channel order, sampling or reserved bits"));
        }
        bytes += match u32le(c, 0) {
            0 => 4,
            2 => {
                floats += 1;
                4
            }
            1 => 2,
            _ => return Err(invalid("channel sample type")),
        };
        prev = channel;
        count += 1;
        pos += 16;
    }
    if count == 0 || data.get(pos..) != Some(&[0][..]) {
        return Err(invalid("incomplete channel list"));
    }
    Ok((count, bytes, floats))
}

struct Grid {
    width: usize,
    height: usize,
    y: i32,
    block_width: usize,
    block_height: usize,
    bytes_per_pixel: usize,
    float_channels: usize,
    tiled: bool,
    compression: u8,
}

impl Grid {
    fn from_header(data: &[u8], fields: &Attributes, max_name: usize, tiled: bool) -> Result<Self> {
        let field = |key: &str, kind| {
            fields
                .get(key)
                .filter(|(ty, _)| ty == kind)
                .map(|(_, range)| &data[range.clone()])
                .ok_or_else(|| invalid(key))
        };
        let window = |key| -> Result<(i32, usize, usize)> {
            let b = field(key, "box2i")?;
            let [x, y, right, bottom] = [0, 4, 8, 12].map(|p| u32le(b, p) as i32);
            let side = |min: i32, max: i32| -> Result<usize> {
                let n = i64::from(max) - i64::from(min) + 1;
                if min.unsigned_abs() > 1 << 20
                    || max.unsigned_abs() > 1 << 20
                    || !(1..=8192).contains(&n)
                {
                    return Err(invalid("image window"));
                }
                Ok(n as usize)
            };
            Ok((y, side(x, right)?, side(y, bottom)?))
        };
        let (y, width, height) = window("dataWindow")?;
        window("displayWindow")?;
        let (count, bytes_per_pixel, float_channels) =
            channels(field("channels", "chlist")?, max_name)?;
        if width * height * count > MAX_SAMPLES {
            return Err(invalid("total samples"));
        }
        if fields.contains_key("type")
            && field("type", "string")?
                != if tiled {
                    b"tiledimage".as_slice()
                } else {
                    b"scanlineimage".as_slice()
                }
        {
            return Err(invalid("part type"));
        }
        let compression = field("compression", "compression")?[0];
        let rows = match compression {
            0..=2 => 1,
            3 | 5 => 16,
            4 | 6 | 7 => 32,
            _ => return Err(invalid("compression")),
        };
        let (block_width, block_height) = if tiled {
            let tile = field("tiles", "tiledesc")?;
            let (w, h) = (u32le(tile, 0), u32le(tile, 4));
            if !(1..=8192).contains(&w) || !(1..=8192).contains(&h) || !matches!(tile[8], 0 | 16) {
                return Err(invalid("tile size or levels"));
            }
            (w as usize, h as usize)
        } else {
            if fields.contains_key("tiles") {
                return Err(invalid("unexpected tile description"));
            }
            (width, rows)
        };
        let total = width.div_ceil(block_width) * height.div_ceil(block_height);
        if total > 65_536
            || (fields.contains_key("chunkCount")
                && u32le(field("chunkCount", "int")?, 0) as usize != total)
        {
            return Err(invalid("chunk count"));
        }
        Ok(Self {
            width,
            height,
            y,
            block_width,
            block_height,
            bytes_per_pixel,
            float_channels,
            tiled,
            compression,
        })
    }

    fn extent(&self, input: &mut Input<'_>, start: usize) -> Result<usize> {
        let columns = self.width.div_ceil(self.block_width);
        let count = columns * self.height.div_ceil(self.block_height);
        let mut end = start + count * 8;
        input.ensure(end)?;
        let mut offsets = (0..count)
            .map(|i| (u64le(&input.bytes, start + i * 8), i))
            .collect::<Vec<_>>();
        offsets.sort_unstable();
        let mut inflated = Vec::new();
        for (offset, index) in offsets {
            if offset != end as u64 {
                return Err(invalid("missing, overlapping or discontiguous chunk"));
            }
            let (x, y) = (index % columns, index / columns);
            let head = if self.tiled { 20 } else { 8 };
            input.ensure(end + head)?;
            let chunk = &input.bytes[end..end + head];
            if self.tiled {
                if u32le(chunk, 0) as usize != x
                    || u32le(chunk, 4) as usize != y
                    || u64le(chunk, 8) != 0
                {
                    return Err(invalid("tile identity"));
                }
            } else if u32le(chunk, 0) as i32 != self.y + (y * self.block_height) as i32 {
                return Err(invalid("scan-line identity"));
            }
            let raw = (self.width - x * self.block_width).min(self.block_width)
                * (self.height - y * self.block_height).min(self.block_height)
                * self.bytes_per_pixel;
            let packed = u32le(chunk, head - 4) as usize;
            if packed == 0 || packed > raw || (self.compression == 0 && packed != raw) {
                return Err(invalid("pixel block length"));
            }
            let payload = end + head;
            end = payload + packed;
            input.ensure(end)?;
            if matches!(self.compression, 2 | 3 | 5) && packed < raw {
                // ZIP blocks must consume the entire zlib member, including Adler-32.
                // PXR24 stores three filtered bytes for each full float sample.
                let n = if self.compression == 5 {
                    raw - raw / self.bytes_per_pixel * self.float_channels
                } else {
                    raw
                };
                input.work(n)?;
                inflated.resize(n + 1, 0);
                let mut zlib = Decompress::new(true);
                let status = zlib
                    .decompress(
                        &input.bytes[payload..end],
                        &mut inflated,
                        FlushDecompress::Finish,
                    )
                    .map_err(|_| invalid("zlib pixel block"))?;
                if status != Status::StreamEnd
                    || zlib.total_in() != packed as u64
                    || zlib.total_out() != n as u64
                {
                    return Err(invalid("zlib block length or checksum"));
                }
            }
        }
        Ok(end)
    }
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(8)?;
    let flags = u32le(&input.bytes, 4);
    if !input.bytes.starts_with(MAGIC) || flags & !0x600 != 2 {
        return Err(invalid("magic, version or multipart/deep flags"));
    }
    let max_name = if flags & 0x400 != 0 { 255 } else { 31 };
    let mut pos = 8;
    let fields = attributes(input, &mut pos, max_name)?;
    let grid = Grid::from_header(&input.bytes, &fields, max_name, flags & 0x200 != 0)?;
    let end = grid.extent(input, pos)?;
    let expected = grid.width * grid.height * grid.bytes_per_pixel;
    input.work(expected * 2)?;
    // Validate each compressed block and its decoded size. Keep only one block in memory.
    let decoded = std::panic::catch_unwind(|| -> Result<usize> {
        let reader = exr::block::read(Cursor::new(&input.bytes[..end]), true)
            .map_err(|e| invalid(&e.to_string()))?;
        let chunks = reader
            .all_chunks(true)
            .map_err(|e| invalid(&e.to_string()))?;
        let mut bytes = 0;
        for block in chunks.sequential_decompressor(true) {
            input.check()?;
            let b = block.map_err(|e| invalid(&e.to_string()))?;
            let n = b.index.pixel_size.area() * grid.bytes_per_pixel;
            if b.data.len() != n {
                return Err(invalid("decoded block length"));
            }
            bytes += n;
        }
        Ok(bytes)
    })
    .map_err(|_| invalid("decoder rejected pixel data"))??;
    if decoded != expected {
        return Err(invalid("incomplete decoded image"));
    }
    Ok(end)
}
