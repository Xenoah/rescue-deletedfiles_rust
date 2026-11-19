//! Portable bitmap, graymap, pixmap and arbitrary map (Netpbm P1–P7).
use super::{Format, container::Input};
use crate::{Error, Result};

fn error(message: &str) -> Error {
    Error::Invalid(format!("Netpbm: {message}"))
}

pub(super) fn signature(bytes: &[u8]) -> Option<Format<'static>> {
    let [b'P', kind, sep, next, ..] = bytes else {
        return None;
    };
    if !sep.is_ascii_whitespace()
        || !(next.is_ascii_digit()
            || next.is_ascii_whitespace()
            || *next == b'#'
            || *kind == b'7' && next.is_ascii_uppercase())
    {
        return None;
    }
    match kind {
        b'1' | b'4' => Some(Format::Pbm),
        b'2' | b'5' => Some(Format::Pgm),
        b'3' | b'6' => Some(Format::Ppm),
        b'7' => Some(Format::Pam),
        _ => None,
    }
}

struct Cursor<'a, 'b> {
    input: &'a mut Input<'b>,
    pos: usize,
}

impl Cursor<'_, '_> {
    fn peek(&mut self) -> Result<Option<u8>> {
        if self.pos == self.input.limit() {
            return Ok(None);
        }
        if self.pos == self.input.bytes.len() {
            self.input.ensure(self.pos + 1)?;
        }
        Ok(Some(self.input.bytes[self.pos]))
    }

    fn skip_separators(&mut self) -> Result<()> {
        loop {
            match self.peek()? {
                Some(b'#') => {
                    while self.peek()?.is_some_and(|b| !matches!(b, b'\r' | b'\n')) {
                        self.pos += 1;
                    }
                }
                Some(b) if b.is_ascii_whitespace() => self.pos += 1,
                _ => return Ok(()),
            }
        }
    }

    fn number(&mut self) -> Result<usize> {
        self.skip_separators()?;
        let start = self.pos;
        while self.peek()?.is_some_and(|b| b.is_ascii_digit()) {
            self.pos += 1;
            if self.pos - start > 10 {
                return Err(error("numeric field too long"));
            }
        }
        decimal(&self.input.bytes[start..self.pos])
    }

    fn pam_line(&mut self) -> Result<Vec<u8>> {
        let start = self.pos;
        if start > 8192 {
            return Err(error("PAM header limit"));
        }
        while self.peek()?.is_some_and(|b| b != b'\n') {
            self.pos += 1;
            if self.pos - start > 1024 || self.pos > 8192 {
                return Err(error("PAM header limit"));
            }
        }
        if self.peek()? != Some(b'\n') {
            return Err(error("unterminated PAM header line"));
        }
        let line = self.input.bytes[start..self.pos].trim_ascii().to_vec();
        self.pos += 1;
        Ok(line)
    }
}

fn decimal(bytes: &[u8]) -> Result<usize> {
    if bytes.is_empty() || bytes.len() > 10 || !bytes.iter().all(u8::is_ascii_digit) {
        return Err(error("expected unsigned decimal field"));
    }
    std::str::from_utf8(bytes)
        .unwrap()
        .parse()
        .map_err(|_| error("numeric overflow"))
}

struct Header {
    width: usize,
    height: usize,
    depth: usize,
    maxval: usize,
}
fn pam(reader: &mut Cursor<'_, '_>) -> Result<Header> {
    if !reader.pam_line()?.is_empty() {
        return Err(error("invalid P7 magic line"));
    }
    let mut fields = [None; 4];
    let mut tuple = None;
    loop {
        let line = reader.pam_line()?;
        if line.is_empty() || line.starts_with(b"#") {
            continue;
        }
        if line == b"ENDHDR" {
            break;
        }
        let split = line
            .iter()
            .position(u8::is_ascii_whitespace)
            .ok_or_else(|| error("missing PAM value"))?;
        let value = line[split..].trim_ascii();
        let index = match &line[..split] {
            b"WIDTH" => 0,
            b"HEIGHT" => 1,
            b"DEPTH" => 2,
            b"MAXVAL" => 3,
            b"TUPLTYPE" if tuple.is_none() => {
                tuple = Some(value.to_vec());
                continue;
            }
            _ => return Err(error("unsupported or repeated PAM field")),
        };
        if fields[index].replace(decimal(value)?).is_some() {
            return Err(error("duplicate PAM field"));
        }
    }
    let [Some(width), Some(height), Some(depth), Some(maxval)] = fields else {
        return Err(error("incomplete PAM header"));
    };
    let matches_tuple = match tuple.as_deref() {
        None => true,
        Some(b"BLACKANDWHITE") => depth == 1 && maxval == 1,
        Some(b"BLACKANDWHITE_ALPHA") => depth == 2 && maxval == 1,
        Some(b"GRAYSCALE") => depth == 1,
        Some(b"GRAYSCALE_ALPHA") => depth == 2,
        Some(b"RGB") => depth == 3,
        Some(b"RGB_ALPHA") => depth == 4,
        _ => false,
    };
    if !matches_tuple {
        return Err(error("invalid or unsupported PAM tuple type"));
    }
    Ok(Header {
        width,
        height,
        depth,
        maxval,
    })
}

pub(super) fn parse(
    format: Format<'_>,
    input: &mut Input<'_>,
    known_length: bool,
) -> Result<usize> {
    input.ensure(4)?;
    if signature(&input.bytes) != Some(format) {
        return Err(error("incorrect image magic"));
    }
    let kind = input.bytes[1];
    let mut reader = Cursor { input, pos: 2 };
    let header = if kind == b'7' {
        pam(&mut reader)?
    } else {
        let width = reader.number()?;
        let height = reader.number()?;
        let maxval = if format == Format::Pbm {
            1
        } else {
            reader.number()?
        };
        let depth = if format == Format::Ppm { 3 } else { 1 };
        if !reader.peek()?.is_some_and(|b| b.is_ascii_whitespace()) {
            return Err(error("missing raster separator"));
        }
        if kind >= b'4' {
            let separator = reader.peek()?;
            reader.pos += 1;
            // A CR followed by LF can also mean a CR delimiter and a first
            // pixel whose value is 10. Do not choose between these extents.
            if separator == Some(b'\r') && reader.peek()? == Some(b'\n') {
                return Err(error("ambiguous CR/LF raw raster boundary"));
            }
        }
        Header {
            width,
            height,
            depth,
            maxval,
        }
    };
    let Header {
        width,
        height,
        depth,
        maxval,
    } = header;
    if width == 0 || height == 0 || !(1..=16).contains(&depth) || !(1..=65535).contains(&maxval) {
        return Err(error(
            "invalid image dimensions, channels or maximum sample",
        ));
    }
    if kind == b'4' {
        let end = width
            .div_ceil(8)
            .checked_mul(height)
            .and_then(|n| reader.pos.checked_add(n))
            .ok_or_else(|| error("bitmap length overflow"))?;
        reader.input.ensure(end)?;
        return Ok(end);
    }
    let count = width
        .checked_mul(height)
        .and_then(|n| n.checked_mul(depth))
        .ok_or_else(|| error("raster length overflow"))?;
    if kind <= b'3' {
        if count > reader.input.limit() - reader.pos {
            return Err(error("short plain raster"));
        }
        for i in 0..count {
            if i % 4096 == 0 {
                reader.input.work((count - i).min(4096))?;
            }
            if kind == b'1' {
                reader.skip_separators()?;
                if !matches!(reader.peek()?, Some(b'0' | b'1')) {
                    return Err(error("invalid bitmap pixel"));
                }
                reader.pos += 1;
            } else if reader.number()? > maxval {
                return Err(error("pixel exceeds MAXVAL"));
            }
        }
        // Many writers add a horizontal separator after the last number
        // and then terminate its line. Additional blank lines are ambiguous.
        let after_sample = reader.pos;
        while matches!(reader.peek()?, Some(b' ' | b'\t')) && reader.pos - after_sample < 64 {
            reader.pos += 1;
        }
        match reader.peek()? {
            Some(b'\r') => {
                reader.pos += 1;
                if reader.peek()? == Some(b'\n') {
                    reader.pos += 1;
                }
            }
            Some(b'\n') => reader.pos += 1,
            None if known_length || reader.input.is_source_end(reader.pos) => (),
            None => return Err(error("no plain raster terminator before scan limit")),
            Some(b) if !b.is_ascii_whitespace() && after_sample == reader.pos => {
                return Err(error("invalid plain raster end"));
            }
            _ => (),
        }
        return Ok(reader.pos);
    }
    let sample_size = if maxval < 256 { 1 } else { 2 };
    let end = count
        .checked_mul(sample_size)
        .and_then(|n| reader.pos.checked_add(n))
        .ok_or_else(|| error("raster length overflow"))?;
    reader.input.ensure(end)?;
    if maxval != 255 && maxval != 65535 {
        for start in (reader.pos..end).step_by(16384) {
            let stop = (start + 16384).min(end);
            reader.input.work(stop - start)?;
            let samples = &reader.input.bytes[start..stop];
            let invalid = if sample_size == 1 {
                samples.iter().any(|&v| usize::from(v) > maxval)
            } else {
                samples
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .any(|&v| usize::from(u16::from_be_bytes(v)) > maxval)
            };
            if invalid {
                return Err(error("pixel exceeds MAXVAL"));
            }
        }
    }
    Ok(end)
}
