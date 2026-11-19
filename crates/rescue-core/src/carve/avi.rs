//! Classic single-RIFF AVI with declared stream headers and bounded media chunks.
use super::container::{Input, u32le, usize64};
use crate::{Error, Result};
use std::ops::Range;

fn invalid() -> Error {
    Error::Invalid("invalid or unsupported classic AVI structure".into())
}

struct Chunk {
    id: [u8; 4],
    data: Range<usize>,
}

fn chunks(
    input: &mut Input<'_>,
    list: Range<usize>,
    remaining: &mut usize,
    mut visit: impl FnMut(&mut Input<'_>, Chunk, &mut usize) -> Result<()>,
) -> Result<()> {
    let mut pos = list.start;
    while pos < list.end {
        if *remaining == 0 {
            return Err(Error::Limit("AVI chunk count".into()));
        }
        *remaining -= 1;
        input.check()?;
        if pos + 8 > list.end {
            return Err(invalid());
        }
        input.ensure(pos + 8)?;
        let id = input.bytes[pos..pos + 4].try_into().unwrap();
        let len = usize64(u64::from(u32le(&input.bytes, pos + 4)))?;
        let data = pos + 8..pos + 8 + len;
        pos = data.end + len % 2;
        if pos > list.end {
            return Err(invalid());
        }
        visit(input, Chunk { id, data }, remaining)?;
    }
    Ok(())
}

fn list_type(input: &mut Input<'_>, chunk: &Chunk) -> Result<[u8; 4]> {
    if chunk.data.len() < 4 {
        return Err(invalid());
    }
    let start = chunk.data.start;
    input.ensure(start + 4)?;
    Ok(input.bytes[start..start + 4].try_into().unwrap())
}

fn header(input: &mut Input<'_>, list: Range<usize>, remaining: &mut usize) -> Result<usize> {
    let mut declared = None;
    let mut streams = 0;
    chunks(input, list, remaining, |input, chunk, remaining| {
        match &chunk.id {
            b"avih" if chunk.data.len() == 56 && declared.is_none() => {
                input.ensure(chunk.data.end)?;
                let count = u32le(&input.bytes, chunk.data.start + 24);
                if !(1..=100).contains(&count) {
                    return Err(invalid());
                }
                declared = Some(count as usize);
            }
            b"avih" => return Err(invalid()),
            b"LIST" => match &list_type(input, &chunk)? {
                b"strl" => {
                    let mut strh = false;
                    let mut strf = false;
                    chunks(
                        input,
                        chunk.data.start + 4..chunk.data.end,
                        remaining,
                        |input, child, _| {
                            match &child.id {
                                b"strh" if !strh && child.data.len() >= 48 => {
                                    let start = child.data.start;
                                    input.ensure(start + 28)?;
                                    if u32le(&input.bytes, start + 20) == 0
                                        || u32le(&input.bytes, start + 24) == 0
                                    {
                                        return Err(invalid());
                                    }
                                    strh = true;
                                }
                                b"strf" if !strf && !child.data.is_empty() => strf = true,
                                b"strh" | b"strf" | b"indx" => return Err(invalid()),
                                _ => {}
                            }
                            Ok(())
                        },
                    )?;
                    if !strh || !strf {
                        return Err(invalid());
                    }
                    streams += 1;
                }
                b"odml" => return Err(invalid()),
                _ => {}
            },
            _ => {}
        }
        Ok(())
    })?;
    if declared == Some(streams) {
        Ok(streams)
    } else {
        Err(invalid())
    }
}

fn movie(
    input: &mut Input<'_>,
    list: Range<usize>,
    remaining: &mut usize,
    streams: usize,
    allow_record: bool,
) -> Result<bool> {
    let mut media = false;
    chunks(input, list, remaining, |input, chunk, remaining| {
        if chunk.id == *b"LIST" {
            if !allow_record || list_type(input, &chunk)? != *b"rec " {
                return Err(invalid());
            }
            media |= movie(
                input,
                chunk.data.start + 4..chunk.data.end,
                remaining,
                streams,
                false,
            )?;
        } else if chunk.id != *b"JUNK" {
            let id = &chunk.id;
            if !id[..2].iter().all(u8::is_ascii_digit)
                || usize::from(id[0] - b'0') * 10 + usize::from(id[1] - b'0') >= streams
                || !matches!(&id[2..], b"db" | b"dc" | b"wb" | b"pc" | b"tx")
            {
                return Err(invalid());
            }
            media |= !chunk.data.is_empty();
        }
        Ok(())
    })?;
    Ok(media)
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(12)?;
    if &input.bytes[..4] != b"RIFF" || &input.bytes[8..12] != b"AVI " {
        return Err(invalid());
    }
    let end = usize64(8 + u64::from(u32le(&input.bytes, 4)))?;
    if !(12..=input.limit()).contains(&end) {
        return Err(invalid());
    }
    let mut remaining = 262144;
    let mut streams = None;
    let mut media = false;
    chunks(input, 12..end, &mut remaining, |input, chunk, remaining| {
        match &chunk.id {
            b"LIST" => match &list_type(input, &chunk)? {
                b"hdrl" if streams.is_none() => {
                    streams = Some(header(
                        input,
                        chunk.data.start + 4..chunk.data.end,
                        remaining,
                    )?);
                }
                b"movi" if !media => {
                    media = movie(
                        input,
                        chunk.data.start + 4..chunk.data.end,
                        remaining,
                        streams.ok_or_else(invalid)?,
                        true,
                    )?;
                    if !media {
                        return Err(invalid());
                    }
                }
                b"movi" | b"hdrl" | b"odml" => return Err(invalid()),
                _ => {}
            },
            b"idx1" if !media || !chunk.data.len().is_multiple_of(16) => return Err(invalid()),
            _ => {}
        }
        Ok(())
    })?;
    if media { Ok(end) } else { Err(invalid()) }
}
