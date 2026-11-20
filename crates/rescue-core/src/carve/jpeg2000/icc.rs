//! Restricted JP2 colour profiles: monochrome curves and three-component matrix/TRC ICC v2.
use super::{be32, invalid};
use crate::Result;
use std::collections::BTreeMap;

pub(super) fn validate(b: &[u8], components: u16) -> Result<()> {
    if !(132..=1024 * 1024).contains(&b.len())
        || be32(b, 0) as usize != b.len()
        || b[8] != 2
        || !matches!(&b[12..16], b"mntr" | b"scnr")
        || &b[20..24] != b"XYZ "
        || &b[36..40] != b"acsp"
        || be32(b, 64) > 3
        || b[100..128] != [0; 28]
    {
        return Err(invalid("restricted ICC profile header"));
    }
    let gray = components <= 2;
    if &b[16..20] != if gray { b"GRAY" } else { b"RGB " } {
        return Err(invalid("ICC color space disagrees with component count"));
    }
    let count = be32(b, 128) as usize;
    if !(1..=256).contains(&count) || 132 + 12 * count > b.len() {
        return Err(invalid("ICC tag count"));
    }
    let first = 132 + 12 * count;
    let mut tags = BTreeMap::new();
    let mut ranges = Vec::with_capacity(count);
    for entry in b[132..first].as_chunks::<12>().0 {
        let name: [u8; 4] = entry[..4].try_into().unwrap();
        let offset = be32(entry, 4) as usize;
        let len = be32(entry, 8) as usize;
        let end = offset
            .checked_add(len)
            .filter(|&e| e <= b.len())
            .ok_or_else(|| invalid("ICC tag bounds"))?;
        if offset < first
            || !offset.is_multiple_of(4)
            || len < 8
            || b[offset + 4..offset + 8] != [0; 4]
            || tags.insert(name, &b[offset..end]).is_some()
            || matches!(&name[..3], b"A2B" | b"B2A" | b"D2B" | b"B2D")
        {
            return Err(invalid(
                "ICC tag location, duplicate or unsupported color lookup table",
            ));
        }
        ranges.push((offset, end));
    }
    // ICC permits several signatures to reference the very same data element.
    ranges.sort_unstable();
    ranges.dedup();
    if ranges.windows(2).any(|r| r[0].1 > r[1].0) {
        return Err(invalid("overlapping ICC tag data"));
    }
    let xyz = |name: &[u8; 4]| {
        tags.get(name)
            .is_some_and(|d| d.len() == 20 && &d[..4] == b"XYZ ")
    };
    let curve = |name: &[u8; 4]| {
        tags.get(name).is_some_and(|d| {
            if d.len() < 12 || &d[..4] != b"curv" {
                return false;
            }
            let count = be32(d, 8) as usize;
            count <= 65_536 && d.len() == 12 + 2 * count
        })
    };
    if !xyz(b"wtpt")
        || if gray {
            !curve(b"kTRC")
        } else {
            ![b"rXYZ", b"gXYZ", b"bXYZ"].into_iter().all(xyz)
                || ![b"rTRC", b"gTRC", b"bTRC"].into_iter().all(curve)
        }
    {
        return Err(invalid("incomplete ICC matrix or tone curves"));
    }
    Ok(())
}
