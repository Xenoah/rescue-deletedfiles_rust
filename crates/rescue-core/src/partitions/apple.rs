//! Apple Driver Descriptor Record and New World ("PM") partition map.
use super::{SourceRegion, invalid, region};
use crate::{Error, Result, source::Image};

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}

fn terminated(field: &[u8]) -> &[u8] {
    field.split(|v| *v == 0).next().unwrap_or_default()
}

pub(super) fn read(image: &mut Image, boot: &[u8]) -> Result<(u32, Vec<SourceRegion>)> {
    let block = u32::from(u16::from_be_bytes([boot[2], boot[3]]));
    let disk_blocks = be32(boot, 4);
    if !matches!(block, 512 | 1024 | 2048 | 4096)
        || disk_blocks < 2
        || u64::from(disk_blocks) * u64::from(block) > image.length
    {
        return Err(invalid("Apple driver descriptor geometry"));
    }
    let first = image.read(u64::from(block), 512)?;
    if &first[..2] != b"PM" {
        return Err(invalid("Apple partition map signature"));
    }
    let count = be32(&first, 4);
    if count > 256 {
        return Err(Error::Limit(
            "Apple partition map exceeds 256 entries".into(),
        ));
    }
    if count == 0 || count >= disk_blocks {
        return Err(invalid("Apple partition map count"));
    }

    let mut spans = Vec::new();
    let mut partitions = Vec::new();
    let mut map_found = false;
    for index in 1..=count {
        let row = image.read(u64::from(index) * u64::from(block), 512)?;
        let start = be32(&row, 8);
        let size = be32(&row, 12);
        let end = start.checked_add(size).filter(|n| *n <= disk_blocks);
        if &row[..2] != b"PM" || be32(&row, 4) != count || start == 0 || size == 0 {
            return Err(invalid("Apple partition map entry"));
        }
        spans.push((
            start,
            end.ok_or_else(|| invalid("Apple partition beyond disk"))?,
        ));
        let kind = terminated(&row[48..80]);
        if kind.is_empty() || !kind.iter().all(|b| (0x20..=0x7e).contains(b)) {
            return Err(invalid("Apple partition type"));
        }
        if kind.eq_ignore_ascii_case(b"Apple_partition_map") {
            if map_found || start != 1 || size < count {
                return Err(invalid("Apple map does not uniquely cover its own entries"));
            }
            map_found = true;
        } else if ![b"Apple_Free".as_slice(), b"Apple_Void", b"Apple_Scratch"]
            .iter()
            .any(|ignored| kind.eq_ignore_ascii_case(ignored))
        {
            let name = encoding_rs::MACINTOSH
                .decode_without_bom_handling(terminated(&row[16..48]))
                .0;
            let label = if name.is_empty() {
                String::from_utf8_lossy(kind).into_owned()
            } else {
                name.into_owned()
            };
            partitions.push((index, label, start, size));
        }
    }
    spans.sort_unstable();
    if !map_found
        || spans
            .windows(2)
            .any(|adjacent| adjacent[0].1 > adjacent[1].0)
    {
        return Err(invalid(
            "missing or overlapping Apple partition map extents",
        ));
    }
    let regions = partitions
        .into_iter()
        .map(|(index, name, start, size)| {
            region(
                image,
                index,
                name,
                u64::from(start),
                u64::from(size),
                u64::from(block),
            )
        })
        .collect::<Result<_>>()?;
    Ok((block, regions))
}
