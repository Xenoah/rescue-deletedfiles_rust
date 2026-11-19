use super::{
    Input, Result, be16, be32, flavor, invalid, search_fields, sum_words, tag_valid, validate_face,
};
use crate::carve::container::MAX_ENCODED;
use flate2::{Decompress, FlushDecompress, Status};

fn inflate(from: &[u8], length: usize) -> Result<Vec<u8>> {
    let mut output = vec![0; length + 1];
    let mut stream = Decompress::new(true);
    let status = stream
        .decompress(from, &mut output, FlushDecompress::Finish)
        .map_err(|_| invalid())?;
    if status != Status::StreamEnd
        || stream.total_in() != from.len() as u64
        || stream.total_out() != length as u64
    {
        return Err(invalid());
    }
    output.truncate(length);
    Ok(output)
}

struct Table {
    tag: [u8; 4],
    at: usize,
    packed: usize,
    original: usize,
    checksum: u32,
}

pub(super) fn parse(input: &mut Input) -> Result<usize> {
    input.ensure(44)?;
    let b = &input.bytes;
    if &b[..4] != b"wOFF" || be16(b, 14) != 0 {
        return Err(invalid());
    }
    let format = flavor(&b[4..8]).ok_or_else(invalid)?;
    let end = be32(b, 8) as usize;
    let count = be16(b, 12);
    let fields = search_fields(count).ok_or_else(invalid)?;
    let directory = 44 + usize::from(count) * 20;
    let sfnt_size = be32(b, 16) as usize;
    let meta = (
        be32(b, 24) as usize,
        be32(b, 28) as usize,
        be32(b, 32) as usize,
    );
    let private = (be32(b, 36) as usize, be32(b, 40) as usize);
    if end < directory || end > input.limit() || sfnt_size > MAX_ENCODED || meta.2 > 4 * 1024 * 1024
    {
        return Err(invalid());
    }
    input.ensure(directory)?;
    let mut written = 12 + usize::from(count) * 16;
    let mut tables = Vec::with_capacity(count.into());
    let mut previous = None;
    for row in input.bytes[44..directory].as_chunks::<20>().0 {
        let tag: [u8; 4] = row[..4].try_into().unwrap();
        let at = u64::from(be32(row, 4));
        let packed = u64::from(be32(row, 8));
        let original = u64::from(be32(row, 12));
        if !tag_valid(&tag)
            || previous.is_some_and(|v| v >= tag)
            || packed > original
            || at < directory as u64
            || !at.is_multiple_of(4)
            || at + packed > end as u64
            || written as u64 + original.next_multiple_of(4) > sfnt_size as u64
        {
            return Err(invalid());
        }
        written += original.next_multiple_of(4) as usize;
        tables.push(Table {
            tag,
            at: at as usize,
            packed: packed as usize,
            original: original as usize,
            checksum: be32(row, 16),
        });
        previous = Some(tag);
    }
    if written != sfnt_size {
        return Err(invalid());
    }
    input.ensure(end)?;
    input.work(sfnt_size * 2 + meta.2)?;
    let b = &input.bytes[..end];
    let mut positions: Vec<_> = tables.iter().map(|t| (t.at, t.packed)).collect();
    positions.sort_unstable();
    let mut pos = directory;
    for (at, n) in positions {
        if at != pos.next_multiple_of(4) || b[pos..at].iter().any(|&v| v != 0) {
            return Err(invalid());
        }
        pos = at + n;
    }
    for (start, length, metadata) in [(meta.0, meta.1, true), (private.0, private.1, false)] {
        if start == 0 && length == 0 && (!metadata || meta.2 == 0) {
            continue;
        }
        if length == 0
            || start != pos.next_multiple_of(4)
            || !matches!(start.checked_add(length), Some(v) if v <= end)
            || b[pos..start].iter().any(|&v| v != 0)
        {
            return Err(invalid());
        }
        if metadata {
            if meta.2 == 0 {
                return Err(invalid());
            }
            let xml = inflate(&b[start..start + length], meta.2)?;
            let xml = std::str::from_utf8(&xml).map_err(|_| invalid())?;
            let options = roxmltree::ParsingOptions {
                nodes_limit: 65_536,
                ..Default::default()
            };
            let doc =
                roxmltree::Document::parse_with_options(xml, options).map_err(|_| invalid())?;
            let root = doc.root_element();
            if !root.has_tag_name("metadata") || root.attribute("version") != Some("1.0") {
                return Err(invalid());
            }
        }
        pos = start + length;
    }
    if end != pos && (end != pos.next_multiple_of(4) || b[pos..end].iter().any(|&v| v != 0)) {
        return Err(invalid());
    }
    let mut sfnt = vec![0; sfnt_size];
    sfnt[..4].copy_from_slice(&b[4..8]);
    for (i, v) in std::iter::once(count).chain(fields).enumerate() {
        sfnt[4 + i * 2..6 + i * 2].copy_from_slice(&v.to_be_bytes());
    }
    let mut at = 12 + usize::from(count) * 16;
    for (i, t) in tables.iter().enumerate() {
        let data = &b[t.at..t.at + t.packed];
        let target = &mut sfnt[at..at + t.original];
        if t.packed < t.original {
            target.copy_from_slice(&inflate(data, t.original)?);
        } else {
            target.copy_from_slice(data);
        }
        if sum_words(target, t.tag == *b"head") != t.checksum {
            return Err(invalid());
        }
        let row = &mut sfnt[12 + i * 16..28 + i * 16];
        row[..4].copy_from_slice(&t.tag);
        row[4..8].copy_from_slice(&t.checksum.to_be_bytes());
        row[8..12].copy_from_slice(&(at as u32).to_be_bytes());
        row[12..16].copy_from_slice(&(t.original as u32).to_be_bytes());
        at += t.original.next_multiple_of(4);
    }
    validate_face(&sfnt, format)?;
    Ok(end)
}
