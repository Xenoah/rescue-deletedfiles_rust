//! Native FLAC with a known sample count, complete frame CRCs and optional stream MD5.
use super::container::{Input, InputCursor};
use crate::{Error, Result};
use claxon::{
    frame::FrameReader,
    input::ReadBytes,
    metadata::{MetadataBlock, StreamInfo},
};
use md5::{Digest, Md5};
use std::io::{self, BufRead, Cursor, Read};

fn invalid(msg: &str) -> Error {
    Error::Invalid(format!("FLAC: {msg}"))
}

impl ReadBytes for InputCursor<'_, '_> {
    fn read_u8_or_eof(&mut self) -> io::Result<Option<u8>> {
        let value = self.fill_buf()?.first().copied();
        self.consume(usize::from(value.is_some()));
        Ok(value)
    }
    fn read_u8(&mut self) -> io::Result<u8> {
        self.read_u8_or_eof()?
            .ok_or_else(|| io::Error::from(io::ErrorKind::UnexpectedEof))
    }
    fn read_into(&mut self, buf: &mut [u8]) -> io::Result<()> {
        self.read_exact(buf)
    }
    fn skip(&mut self, amount: u32) -> io::Result<()> {
        let mut remaining = amount as usize;
        while remaining > 0 {
            let n = remaining.min(self.fill_buf()?.len());
            if n == 0 {
                return Err(io::Error::from(io::ErrorKind::UnexpectedEof));
            }
            self.consume(n);
            remaining -= n;
        }
        Ok(())
    }
}

fn metadata(input: &mut Input<'_>) -> Result<(usize, StreamInfo)> {
    input.ensure(42)?;
    if !input.bytes.starts_with(b"fLaC")
        || input.bytes[4] & 127 != 0
        || input.bytes[5..8] != [0, 0, 34]
    {
        return Err(invalid("STREAMINFO header"));
    }
    let MetadataBlock::StreamInfo(info) =
        claxon::metadata::read_metadata_block_with_header(&mut Cursor::new(&input.bytes[4..42]))
            .map_err(|e| invalid(&e.to_string()))?
    else {
        return Err(invalid("missing STREAMINFO"));
    };
    let mut end = 42;
    let mut last = input.bytes[4] & 128 != 0;
    let mut blocks = 1;
    while !last {
        input.check()?;
        input.ensure(end + 4)?;
        let kind = input.bytes[end];
        if !matches!(kind & 127, 1..=6) {
            return Err(Error::Unsupported(
                "unknown or duplicate FLAC metadata block".into(),
            ));
        }
        let size = u32::from_be_bytes([
            0,
            input.bytes[end + 1],
            input.bytes[end + 2],
            input.bytes[end + 3],
        ]) as usize;
        end += 4 + size;
        input.ensure(end)?;
        last = kind & 128 != 0;
        blocks += 1;
        if blocks > 65536 {
            return Err(Error::Limit("FLAC metadata block count".into()));
        }
    }
    Ok((end, info))
}

fn frame_settings(
    input: &mut Input<'_>,
    pos: usize,
    info: &StreamInfo,
    frame_no: u64,
    samples: u64,
) -> Result<bool> {
    input.ensure(pos + 5)?;
    let h = &input.bytes[pos..pos + 5];
    if h[0] != 255 || h[1] & 0xfe != 0xf8 {
        return Err(invalid("frame sync"));
    }
    let variable = h[1] & 1 != 0;
    let depth = [0, 8, 12, 0, 16, 20, 24, 32][usize::from(h[3] >> 1 & 7)];
    if depth == 0 || depth == 32 {
        return Err(Error::Unsupported(
            "FLAC frames must explicitly record a supported sample depth of 8–24 bits".into(),
        ));
    }
    if depth != info.bits_per_sample {
        return Err(invalid("frame sample depth disagrees with STREAMINFO"));
    }
    let block_code = h[2] >> 4;
    let rate_code = h[2] & 15;
    let first = h[4];
    let leading = first.leading_ones() as usize;
    if leading == 1 || leading > 7 {
        return Err(invalid("UTF-8 frame/sample number"));
    }
    let n = leading.max(1);
    let mut number = if n == 1 {
        u64::from(first)
    } else {
        u64::from(first & (0x7f >> n))
    };
    input.ensure(pos + 4 + n)?;
    for &byte in &input.bytes[pos + 5..pos + 4 + n] {
        if byte & 0xc0 != 0x80 {
            return Err(invalid("invalid frame number continuation"));
        }
        number = number * 64 + u64::from(byte & 0x3f);
    }
    if number != if variable { samples } else { frame_no } {
        return Err(invalid("frame sequence or sample offset"));
    }
    let at = pos
        + 4
        + n
        + match block_code {
            6 => 1,
            7 => 2,
            _ => 0,
        };
    let rate = match rate_code {
        0 => info.sample_rate,
        1..=11 => [
            0, 88200, 176400, 192000, 8000, 16000, 22050, 24000, 32000, 44100, 48000, 96000,
        ][usize::from(rate_code)],
        12 => {
            input.ensure(at + 1)?;
            u32::from(input.bytes[at]) * 1000
        }
        13 | 14 => {
            input.ensure(at + 2)?;
            u32::from(u16::from_be_bytes([input.bytes[at], input.bytes[at + 1]]))
                * if rate_code == 14 { 10 } else { 1 }
        }
        _ => return Err(invalid("reserved sample rate")),
    };
    if rate != info.sample_rate {
        return Err(invalid("sample rate disagrees with STREAMINFO"));
    }
    Ok(variable)
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<usize> {
    let (mut pos, info) = metadata(input)?;
    let total = info
        .samples
        .ok_or_else(|| Error::Unsupported("FLAC stream has no declared sample count".into()))?;
    if total * u64::from(info.channels) * 4 > 128 * 1024 * 1024 {
        return Err(Error::Limit(
            "FLAC decoded i32 samples exceed 128 MiB".into(),
        ));
    }
    let width = info.bits_per_sample.div_ceil(8) as usize;
    let mut digest = Md5::new();
    let mut samples = 0u64;
    let mut frame_no = 0u64;
    let mut blocking = None;
    let mut buffer = Vec::new();
    while samples < total {
        input.check()?;
        let variable = frame_settings(input, pos, &info, frame_no, samples)?;
        if blocking
            .replace(variable)
            .is_some_and(|value| value != variable)
        {
            return Err(invalid("inconsistent blocking strategy"));
        }
        let limit = info
            .max_frame_size
            .map_or(input.limit(), |n| input.limit().min(pos + n as usize));
        let mut source = InputCursor {
            input,
            cursor: pos,
            failure: None,
            limit,
        };
        let decoded = FrameReader::new(&mut source).read_next_or_eof(buffer);
        if let Some(error) = source.failure {
            return Err(error);
        }
        let block = decoded
            .map_err(|err| invalid(&err.to_string()))?
            .ok_or_else(|| invalid("missing audio frame"))?;
        let size = source.cursor - pos;
        let duration = block.duration();
        samples += u64::from(duration);
        if size == 0
            || info.min_frame_size.is_some_and(|n| size < n as usize)
            || duration == 0
            || duration > u32::from(info.max_block_size)
            || (duration < u32::from(info.min_block_size) && samples != total)
            || block.channels() != info.channels
            || samples > total
        {
            return Err(invalid("frame geometry or total sample count"));
        }
        source.input.work(block.len() as usize * 4)?;
        if info.md5sum != [0; 16] {
            let mut pcm = Vec::with_capacity(block.len() as usize * width);
            for i in 0..duration as usize {
                for channel in 0..info.channels {
                    pcm.extend_from_slice(&block.channel(channel)[i].to_le_bytes()[..width]);
                }
            }
            digest.update(&pcm);
        }
        pos = source.cursor;
        buffer = block.into_buffer();
        frame_no += 1;
        if frame_no > 1048576 {
            return Err(Error::Limit("FLAC frame count".into()));
        }
    }
    if info.md5sum != [0; 16] && digest.finalize().as_slice() != info.md5sum {
        return Err(invalid("PCM MD5 mismatch"));
    }
    Ok(pos)
}
