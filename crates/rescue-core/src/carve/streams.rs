//! Individually delimited gzip members and single-serial Ogg logical streams.
use super::container::{Input, u16le, u32le};
use crate::{Error, Result};
use flate2::{Decompress, FlushDecompress, Status};

fn gzip_error() -> Error {
    Error::Invalid("invalid gzip member header, DEFLATE stream, CRC or output length".into())
}

pub(super) fn gzip(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(10)?;
    let flags = input.bytes[3];
    if !input.bytes.starts_with(&[0x1f, 0x8b, 8]) || flags & 0xe0 != 0 {
        return Err(gzip_error());
    }
    let mut pos = 10;
    if flags & 4 != 0 {
        input.ensure(pos + 2)?;
        pos += 2 + usize::from(u16le(&input.bytes, pos));
        input.ensure(pos)?;
    }
    for mask in [8, 16] {
        if flags & mask != 0 {
            let start = pos;
            loop {
                input.ensure(pos + 1)?;
                let block = &input.bytes[pos..];
                if let Some(index) = block.iter().position(|b| *b == 0) {
                    pos += index + 1;
                    if pos - start > 65536 {
                        return Err(gzip_error());
                    }
                    break;
                }
                pos += block.len();
                if pos - start >= 65536 {
                    return Err(gzip_error());
                }
            }
        }
    }
    if flags & 2 != 0 {
        input.ensure(pos + 2)?;
        if crc32fast::hash(&input.bytes[..pos]) as u16 != u16le(&input.bytes, pos) {
            return Err(gzip_error());
        }
        pos += 2;
    }
    let mut stream = Decompress::new(false);
    let mut crc = crc32fast::Hasher::new();
    let mut buffer = vec![0; 32 * 1024];
    loop {
        input.ensure(pos + 1)?;
        let before_in = stream.total_in();
        let before_out = stream.total_out();
        let status = stream
            .decompress(&input.bytes[pos..], &mut buffer, FlushDecompress::None)
            .map_err(|_| gzip_error())?;
        let consumed = (stream.total_in() - before_in) as usize;
        let written = (stream.total_out() - before_out) as usize;
        input.work(written)?;
        if stream.total_out() > 128 * 1024 * 1024 {
            return Err(Error::Limit("gzip decoded member exceeds 128 MiB".into()));
        }
        crc.update(&buffer[..written]);
        pos += consumed;
        if status == Status::StreamEnd {
            break;
        }
        if consumed == 0 && written == 0 {
            return Err(gzip_error());
        }
    }
    input.ensure(pos + 8)?;
    if crc.finalize() != u32le(&input.bytes, pos)
        || stream.total_out() != u64::from(u32le(&input.bytes, pos + 4))
    {
        return Err(gzip_error());
    }
    Ok(pos + 8)
}

// Ogg uses the non-reflected CRC-32 polynomial with zero initial/final XOR.
const OGG_CRC: [u32; 256] = {
    let mut table = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut value = (i as u32) << 24;
        let mut bit = 0;
        while bit < 8 {
            value = (value << 1)
                ^ if value & 0x80000000 != 0 {
                    0x04c11db7
                } else {
                    0
                };
            bit += 1;
        }
        table[i] = value;
        i += 1;
    }
    table
};

pub(super) fn ogg(input: &mut Input<'_>) -> Result<usize> {
    let invalid = || Error::Invalid("inconsistent Ogg pages, lacing, sequence or checksum".into());
    let mut pos = 0;
    let mut serial = None;
    let mut continued = false;
    let mut packets = 0;
    for seq in 0..65536 {
        input.ensure(pos + 27)?;
        let header = &input.bytes[pos..pos + 27];
        let flags = header[5];
        let stream = u32le(header, 14);
        if &header[..5] != b"OggS\0"
            || flags & !7 != 0
            || (flags & 2 != 0) != (seq == 0)
            || (flags & 1 != 0) != continued
            || u32le(header, 18) != seq
            || *serial.get_or_insert(stream) != stream
        {
            return Err(invalid());
        }
        let end_laces = pos + 27 + usize::from(header[26]);
        input.ensure(end_laces)?;
        let laces = &input.bytes[pos + 27..end_laces];
        let end = end_laces + laces.iter().map(|v| usize::from(*v)).sum::<usize>();
        packets += laces.iter().filter(|v| **v < 255).count();
        if let Some(last) = laces.last() {
            continued = *last == 255;
        }
        input.ensure(end)?;
        input.work(end - pos)?;
        let page = &input.bytes[pos..end];
        let mut crc = 0u32;
        for (index, &value) in page.iter().enumerate() {
            let byte = if (22..26).contains(&index) { 0 } else { value };
            crc = (crc << 8) ^ OGG_CRC[((crc >> 24) as u8 ^ byte) as usize];
        }
        if crc != u32le(page, 22) {
            return Err(invalid());
        }
        if flags & 4 != 0 {
            return if !continued && packets > 0 {
                Ok(end)
            } else {
                Err(invalid())
            };
        }
        pos = end;
    }
    Err(Error::Limit("Ogg page count".into()))
}
