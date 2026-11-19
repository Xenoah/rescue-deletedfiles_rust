//! Length-bounded standalone SFNT and WOFF fonts.
mod woff;
use super::{Format, container::Input};
use crate::{Error, Result};

fn invalid() -> Error {
    Error::Invalid("invalid or unsupported font structure".into())
}
fn be16(b: &[u8], at: usize) -> u16 {
    u16::from_be_bytes(b[at..at + 2].try_into().unwrap())
}
fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn flavor(magic: &[u8]) -> Option<Format<'static>> {
    match magic {
        b"\0\x01\0\0" | b"true" => Some(Format::Ttf),
        b"OTTO" => Some(Format::Otf),
        _ => None,
    }
}

fn search_fields(count: u16) -> Option<[u16; 3]> {
    if !(1..=512).contains(&count) {
        return None;
    }
    let exponent = count.ilog2() as u16;
    let largest = 16u16 << exponent;
    Some([largest, exponent, count * 16 - largest])
}

pub(super) fn signature(b: &[u8]) -> Option<Format<'static>> {
    let magic = b.get(..4)?;
    if magic == b"wOFF" {
        return flavor(b.get(4..8)?).map(|_| Format::Woff);
    }
    let format = flavor(magic)?;
    // The TrueType magic is also an everyday integer on raw media.
    let header = b.get(..16)?;
    let fields = search_fields(be16(header, 4))?;
    (fields == [be16(b, 6), be16(b, 8), be16(b, 10)] && tag_valid(&header[12..16]))
        .then_some(format)
}

fn tag_valid(tag: &[u8]) -> bool {
    tag.iter().all(|b| (0x20..=0x7e).contains(b))
}

fn sum_words(bytes: &[u8], is_head: bool) -> u32 {
    let mut total = 0u32;
    for (index, word) in bytes.chunks(4).enumerate() {
        if is_head && index == 2 {
            continue; // head.checkSumAdjustment is zero for the table checksum.
        }
        let mut padded = [0u8; 4];
        padded[..word.len()].copy_from_slice(word);
        total = total.wrapping_add(u32::from_be_bytes(padded));
    }
    total
}

fn validate_face(bytes: &[u8], format: Format) -> Result<()> {
    let face = ttf_parser::Face::parse(bytes, 0).map_err(|_| invalid())?;
    let tables = face.tables();
    let head = face
        .raw_face()
        .table(ttf_parser::Tag::from_bytes(b"head"))
        .ok_or_else(invalid)?;
    if head.len() < 54
        || be32(head, 0) != 0x10000
        || be32(head, 12) != 0x5f0f3cf5
        || face.number_of_glyphs() == 0
        || tables.cmap.is_none()
        || tables.hmtx.is_none()
        || match format {
            Format::Ttf => tables.glyf.is_none(),
            Format::Otf => tables.cff.is_none() && tables.cff2.is_none(),
            _ => true,
        }
    {
        return Err(invalid());
    }
    Ok(())
}

struct Entry {
    tag: [u8; 4],
    at: usize,
    length: usize,
    sum: u32,
}

pub(super) fn parse(input: &mut Input, format: Format) -> Result<usize> {
    if format == Format::Woff {
        return woff::parse(input);
    }
    input.ensure(16)?;
    if signature(&input.bytes) != Some(format) {
        return Err(invalid());
    }
    let count = usize::from(be16(&input.bytes, 4));
    let directory = 12 + count * 16;
    input.ensure(directory)?;
    let mut table_end = directory;
    let mut entries = Vec::with_capacity(count);
    let mut previous = None;
    for row in input.bytes[12..directory].as_chunks::<16>().0 {
        let tag: [u8; 4] = row[..4].try_into().unwrap();
        let at = u64::from(be32(row, 8));
        let length = u64::from(be32(row, 12));
        let end = at + length.next_multiple_of(4);
        if !tag_valid(&tag)
            || previous.is_some_and(|last| last >= tag)
            || !at.is_multiple_of(4)
            || at < directory as u64
            || end > input.limit() as u64
        {
            return Err(invalid());
        }
        table_end = table_end.max(end as usize);
        entries.push(Entry {
            tag,
            at: at as usize,
            length: length as usize,
            sum: be32(row, 4),
        });
        previous = Some(tag);
    }
    entries.sort_unstable_by_key(|r| (r.at, r.length));
    let mut last = directory;
    for r in &entries {
        if r.at < last {
            return Err(invalid());
        }
        last = r.at + r.length.next_multiple_of(4);
    }
    input.ensure(table_end)?;
    input.work(table_end * 2)?;
    for r in entries {
        let end = r.at + r.length;
        if sum_words(&input.bytes[r.at..end], r.tag == *b"head") != r.sum
            || input.bytes[end..end.next_multiple_of(4)]
                .iter()
                .any(|&v| v != 0)
        {
            return Err(invalid());
        }
    }
    if sum_words(&input.bytes[..table_end], false) != 0xb1b0afba {
        return Err(invalid());
    }
    validate_face(&input.bytes[..table_end], format)?;
    Ok(table_end)
}
