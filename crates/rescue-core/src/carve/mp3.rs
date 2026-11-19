//! Layer III streams with a Xing/Info header that declares both audio frames and bytes.
use super::container::Input;
use crate::{Error, Result};

fn invalid(why: &str) -> Error {
    Error::Invalid(format!("MP3: {why}"))
}

struct Header {
    size: usize,
    side_end: usize,
    rate: usize,
    mono: bool,
    version: u8,
}

fn header(bytes: &[u8]) -> Option<Header> {
    let h: &[u8; 4] = bytes.get(..4)?.try_into().ok()?;
    if h[0] != 0xff || h[1] & 0xe6 != 0xe2 || h[3] & 3 == 2 {
        return None;
    }
    let version = (h[1] >> 3) & 3;
    let divider = match version {
        3 => 1,
        2 => 2,
        0 => 4,
        _ => return None,
    };
    let rate = [44100, 48000, 32000].get(usize::from(h[2] >> 2 & 3))? / divider;
    let bitrates = if version == 3 {
        [
            0, 32, 40, 48, 56, 64, 80, 96, 112, 128, 160, 192, 224, 256, 320,
        ]
    } else {
        [0, 8, 16, 24, 32, 40, 48, 56, 64, 80, 96, 112, 128, 144, 160]
    };
    let kib = *bitrates.get(usize::from(h[2] >> 4))?;
    if kib == 0 {
        return None;
    }
    let size =
        (if version == 3 { 144000 } else { 72000 }) * kib / rate + usize::from(h[2] >> 1 & 1);
    let mono = h[3] >> 6 == 3;
    let side = match (version == 3, mono) {
        (true, false) => 32,
        (false, true) => 9,
        _ => 17,
    };
    let side_end = 4 + usize::from(h[1] & 1 == 0) * 2 + side;
    (size >= side_end).then_some(Header {
        size,
        side_end,
        rate,
        mono,
        version,
    })
}

pub(super) fn signature(b: &[u8]) -> bool {
    if b.starts_with(b"ID3") {
        return matches!(b.get(3), Some(2..=4)) && b.get(4).is_some_and(|v| *v != 255);
    }
    header(b).is_some_and(|h| matches!(b.get(h.side_end..h.side_end + 4), Some(b"Xing" | b"Info")))
}

fn id3_size(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(4)?;
    if !input.bytes.starts_with(b"ID3") {
        return Ok(0);
    }
    input.ensure(10)?;
    let b = &input.bytes[..10];
    let flag_mask = match b[3] {
        2 => 0xc0,
        3 => 0xe0,
        4 => 0xf0,
        _ => return Err(invalid("ID3 version")),
    };
    if b[4] == 0xff || b[5] & !flag_mask != 0 || b[6..].iter().any(|v| v & 0x80 != 0) {
        return Err(invalid("ID3 flags or synchsafe length"));
    }
    let size = b[6..]
        .iter()
        .fold(0usize, |n, v| (n << 7) | usize::from(*v));
    let footer = b[3] == 4 && b[5] & 0x10 != 0;
    let end = 10 + size + if footer { 10 } else { 0 };
    input.ensure(end)?;
    if footer
        && (&input.bytes[end - 10..end - 7] != b"3DI"
            || input.bytes[end - 7..end] != input.bytes[3..10])
    {
        return Err(invalid("ID3v2.4 footer mismatch"));
    }
    Ok(end)
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<(usize, Vec<usize>)> {
    let start = id3_size(input)?;
    input.ensure(start + 4)?;
    let first = header(&input.bytes[start..]).ok_or_else(|| invalid("Layer III header"))?;
    input.ensure(start + first.size)?;
    let tag = &input.bytes[start + first.side_end..start + first.size];
    if tag.len() < 16 || !matches!(&tag[..4], b"Info" | b"Xing") {
        return Err(Error::Unsupported(
            "MP3 carving requires Xing/Info frame and byte counts".into(),
        ));
    }
    let field = |pos| u32::from_be_bytes(tag[pos..pos + 4].try_into().unwrap());
    let flags = field(4);
    if flags & !15 != 0
        || flags & 3 != 3
        || tag.len() < 16 + usize::from(flags & 4 != 0) * 100 + usize::from(flags & 8 != 0) * 4
    {
        return Err(invalid("Xing flags or lengths"));
    }
    let count = field(8);
    let length = field(12) as usize;
    let end = start
        .checked_add(length)
        .filter(|p| *p <= input.limit())
        .ok_or_else(|| invalid("Xing size outside source"))?;
    if count == 0 || count > 1048576 || length <= first.size {
        return Err(invalid("Xing extent or frame count"));
    }
    let mut pos = start + first.size;
    let mut frames = 0u32;
    while pos < end {
        input.check()?;
        if end - pos < 4 || frames > count {
            return Err(invalid("misaligned frame sequence"));
        }
        input.ensure(pos + 4)?;
        let h = header(&input.bytes[pos..]).ok_or_else(|| invalid("invalid MPEG audio frame"))?;
        if h.version != first.version
            || h.rate != first.rate
            || h.mono != first.mono
            || h.size > end - pos
        {
            return Err(invalid("inconsistent or incomplete audio frame"));
        }
        pos += h.size;
        frames += 1;
    }
    if frames != count {
        return Err(invalid("audio frame count disagrees with Xing"));
    }
    input.ensure(end)?;
    Ok((end, if start == 0 { Vec::new() } else { vec![start] }))
}
