//! Bounded MBR/EBR and CRC-checked GPT interpretation. No disk writes or repairs.
use crate::{Error, FileSystem, Result, source::Image};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceRegion {
    pub index: u32,
    pub name: String,
    pub offset: u64,
    pub length: u64,
    pub filesystem: Option<FileSystem>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct SourceLayout {
    pub path: PathBuf,
    pub device: bool,
    pub bytes: u64,
    pub sector_size: u32,
    pub scheme: String,
    pub regions: Vec<SourceRegion>,
    pub warnings: Vec<String>,
}
fn invalid(s: &str) -> Error {
    Error::Invalid(format!("partition table: {s}"))
}
fn u32_at(b: &[u8], p: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(
        b.get(p..p + 4)
            .ok_or_else(|| invalid("short field"))?
            .try_into()
            .unwrap(),
    ))
}
fn u64_at(b: &[u8], p: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(
        b.get(p..p + 8)
            .ok_or_else(|| invalid("short field"))?
            .try_into()
            .unwrap(),
    ))
}
fn filesystem(image: &mut Image, offset: u64, length: u64) -> Result<Option<FileSystem>> {
    if length < 512 {
        return Ok(None);
    }
    let boot = image.read(offset, 512)?;
    Ok(if &boot[3..11] == b"NTFS    " {
        Some(FileSystem::Ntfs)
    } else if &boot[3..11] == b"EXFAT   " {
        Some(FileSystem::Exfat)
    } else if &boot[82..90] == b"FAT32   " {
        Some(FileSystem::Fat32)
    } else {
        None
    })
}
fn region(
    image: &mut Image,
    index: u32,
    name: String,
    start: u64,
    blocks: u64,
    sector: u64,
) -> Result<SourceRegion> {
    let offset = start
        .checked_mul(sector)
        .ok_or_else(|| invalid("offset overflow"))?;
    let length = blocks
        .checked_mul(sector)
        .ok_or_else(|| invalid("length overflow"))?;
    if offset == 0 || length == 0 || offset.checked_add(length).is_none_or(|n| n > image.length) {
        return Err(invalid("partition beyond disk"));
    }
    Ok(SourceRegion {
        index,
        name,
        offset,
        length,
        filesystem: filesystem(image, offset, length)?,
    })
}
fn validate_regions(regions: &[SourceRegion]) -> Result<()> {
    if regions.len() > 256 {
        return Err(Error::Limit("more than 256 partitions".into()));
    }
    let mut ordered = regions.iter().collect::<Vec<_>>();
    ordered.sort_by_key(|r| r.offset);
    for pair in ordered.windows(2) {
        if pair[0].offset + pair[0].length > pair[1].offset {
            return Err(invalid("overlapping partitions"));
        }
    }
    Ok(())
}
#[derive(Clone)]
struct Gpt {
    current: u64,
    alternate: u64,
    first: u64,
    last: u64,
    guid: [u8; 16],
    entries: Vec<u8>,
    count: u32,
    stride: u32,
}
fn gpt_header(image: &mut Image, sector: u64, lba: u64) -> Result<Gpt> {
    let bytes = image.read(
        lba.checked_mul(sector)
            .ok_or_else(|| invalid("header offset"))?,
        sector as usize,
    )?;
    if &bytes[..8] != b"EFI PART" || u32_at(&bytes, 8)? != 0x10000 {
        return Err(invalid("GPT signature/revision"));
    }
    let size = u32_at(&bytes, 12)? as usize;
    if !(92..=sector as usize).contains(&size) || u32_at(&bytes, 20)? != 0 {
        return Err(invalid("GPT header length/reserved field"));
    }
    let expected = u32_at(&bytes, 16)?;
    let mut header = bytes[..size].to_vec();
    header[16..20].fill(0);
    if crc32fast::hash(&header) != expected {
        return Err(invalid("GPT header CRC"));
    }
    let current = u64_at(&bytes, 24)?;
    let alternate = u64_at(&bytes, 32)?;
    let first = u64_at(&bytes, 40)?;
    let last = u64_at(&bytes, 48)?;
    let blocks = image.length / sector;
    if current != lba
        || alternate >= blocks
        || alternate == current
        || first < 2
        || first > last
        || last >= blocks - 1
        || !(current == 1 || current == blocks - 1)
        || !(alternate == 1 || alternate == blocks - 1)
    {
        return Err(invalid("GPT header bounds"));
    }
    let mut guid = [0; 16];
    guid.copy_from_slice(&bytes[56..72]);
    if guid == [0; 16] {
        return Err(invalid("zero GPT disk GUID"));
    }
    let at = u64_at(&bytes, 72)?;
    let count = u32_at(&bytes, 80)?;
    let stride = u32_at(&bytes, 84)?;
    if count == 0 || count > 4096 || !(128..=4096).contains(&stride) || !stride.is_multiple_of(8) {
        return Err(invalid("GPT entry geometry"));
    }
    let length = u64::from(count) * u64::from(stride);
    if length > 16 * 1024 * 1024 {
        return Err(Error::Limit("GPT entry table exceeds 16 MiB".into()));
    }
    let end = at
        .checked_mul(sector)
        .and_then(|n| n.checked_add(length))
        .ok_or_else(|| invalid("GPT table overflow"))?;
    if at < 2
        || end > image.length
        || (current == 1 && end > first * sector)
        || (current != 1 && (at <= last || end > current * sector))
    {
        return Err(invalid("GPT entry table overlaps data/header"));
    }
    let entries = image.read(at * sector, length as usize)?;
    if crc32fast::hash(&entries) != u32_at(&bytes, 88)? {
        return Err(invalid("GPT entry table CRC"));
    }
    Ok(Gpt {
        current,
        alternate,
        first,
        last,
        guid,
        entries,
        count,
        stride,
    })
}
fn gpt(image: &mut Image, sector: u64, warnings: &mut Vec<String>) -> Result<Vec<SourceRegion>> {
    if !image.length.is_multiple_of(sector) {
        return Err(invalid("GPT disk size is not sector aligned"));
    }
    let primary = gpt_header(image, sector, 1);
    let backup = gpt_header(image, sector, image.length / sector - 1);
    let table = match (primary, backup) {
        (Ok(p), Ok(b)) => {
            if p.current != b.alternate
                || p.alternate != b.current
                || p.first != b.first
                || p.last != b.last
                || p.guid != b.guid
                || p.count != b.count
                || p.stride != b.stride
                || p.entries != b.entries
            {
                return Err(invalid("primary and backup GPT disagree"));
            }
            p
        }
        (Ok(p), Err(e)) => {
            warnings.push(format!("Backup GPT unavailable: {e}"));
            p
        }
        (Err(e), Ok(b)) => {
            warnings.push(format!(
                "Primary GPT unavailable; using CRC-checked backup: {e}"
            ));
            b
        }
        (Err(e), Err(_)) => return Err(e),
    };
    let mut regions = Vec::new();
    let mut guids = BTreeSet::new();
    for i in 0..table.count {
        let entry = &table.entries
            [i as usize * table.stride as usize..(i as usize + 1) * table.stride as usize];
        if entry[..16].iter().all(|b| *b == 0) {
            continue;
        }
        if entry[16..32].iter().all(|b| *b == 0) || !guids.insert(entry[16..32].to_vec()) {
            return Err(invalid("missing/duplicate partition GUID"));
        }
        let start = u64_at(entry, 32)?;
        let end = u64_at(entry, 40)?;
        if start < table.first || end > table.last || end < start {
            return Err(invalid("GPT partition bounds"));
        }
        let codes = entry[56..128]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|v| u16::from_le_bytes([v[0], v[1]]))
            .take_while(|v| *v != 0)
            .collect::<Vec<_>>();
        let name = String::from_utf16_lossy(&codes);
        regions.push(region(
            image,
            i + 1,
            if name.is_empty() {
                format!("Partition {}", i + 1)
            } else {
                name
            },
            start,
            end - start + 1,
            sector,
        )?);
    }
    validate_regions(&regions)?;
    Ok(regions)
}
fn mbr_entries(bytes: &[u8]) -> Result<Vec<(u8, u64, u64)>> {
    if bytes.len() < 512 || bytes[510..512] != [0x55, 0xaa] {
        return Err(invalid("MBR/EBR signature"));
    }
    let mut entries = Vec::new();
    for i in 0..4 {
        let e = &bytes[446 + i * 16..462 + i * 16];
        if e[0] != 0 && e[0] != 0x80 {
            return Err(invalid("invalid boot flag"));
        }
        let start = u64::from(u32_at(e, 8)?);
        let size = u64::from(u32_at(e, 12)?);
        if e[4] != 0 && (start == 0 || size == 0) {
            return Err(invalid("empty nonzero partition"));
        }
        entries.push((e[4], start, size));
    }
    Ok(entries)
}
fn extended(kind: u8) -> bool {
    matches!(kind, 0x05 | 0x0f | 0x85)
}
fn mbr(image: &mut Image, boot: &[u8], sector: u64) -> Result<Vec<SourceRegion>> {
    let entries = mbr_entries(boot)?;
    let mut out = Vec::new();
    let mut logical = 5;
    for (index, (kind, start, size)) in entries.into_iter().enumerate() {
        if kind == 0 {
            continue;
        }
        if extended(kind) {
            let end = start
                .checked_add(size)
                .ok_or_else(|| invalid("extended overflow"))?;
            if end.checked_mul(sector).is_none_or(|n| n > image.length) {
                return Err(invalid("extended partition bounds"));
            }
            let mut next = start;
            let mut seen = BTreeSet::new();
            loop {
                if seen.len() >= 128 || !seen.insert(next) {
                    return Err(invalid("cyclic/oversize EBR chain"));
                }
                if next < start || next >= end {
                    return Err(invalid("EBR outside extended container"));
                }
                let ebr = image.read(next * sector, 512)?;
                let e = mbr_entries(&ebr)?;
                if e[2].0 != 0 || e[3].0 != 0 || extended(e[0].0) {
                    return Err(invalid("unsupported EBR entries"));
                }
                if e[0].0 != 0 {
                    let at = next
                        .checked_add(e[0].1)
                        .ok_or_else(|| invalid("logical offset overflow"))?;
                    if at.checked_add(e[0].2).is_none_or(|n| n > end) {
                        return Err(invalid("logical partition outside extended container"));
                    }
                    out.push(region(
                        image,
                        logical,
                        format!("Logical partition {logical}"),
                        at,
                        e[0].2,
                        sector,
                    )?);
                    logical += 1;
                }
                if e[1].0 == 0 {
                    break;
                }
                if !extended(e[1].0) {
                    return Err(invalid("invalid EBR next link"));
                }
                next = start
                    .checked_add(e[1].1)
                    .ok_or_else(|| invalid("EBR offset overflow"))?;
            }
        } else {
            out.push(region(
                image,
                index as u32 + 1,
                format!("Partition {}", index + 1),
                start,
                size,
                sector,
            )?);
        }
    }
    validate_regions(&out)?;
    Ok(out)
}
pub fn inspect_source(path: &Path) -> Result<SourceLayout> {
    let mut image = Image::open(path)?;
    if image.length < 512 {
        return Err(invalid("source shorter than a sector"));
    }
    let mut layout = SourceLayout {
        path: image.path.clone(),
        device: image.is_device(),
        bytes: image.length,
        sector_size: image.sector_size(),
        scheme: "volume".into(),
        regions: Vec::new(),
        warnings: Vec::new(),
    };
    if let Some(fs) = filesystem(&mut image, 0, layout.bytes)? {
        layout.regions.push(SourceRegion {
            index: 0,
            name: "Volume".into(),
            offset: 0,
            length: layout.bytes,
            filesystem: Some(fs),
        });
        return Ok(layout);
    }
    let boot = image.read(0, 512)?;
    let entries = mbr_entries(&boot)?;
    if entries.iter().any(|e| e.0 == 0xee) {
        if entries.iter().any(|e| e.0 != 0 && e.0 != 0xee) {
            return Err(invalid("hybrid GPT/MBR is ambiguous"));
        }
        let sectors = if image.is_device() {
            vec![u64::from(image.sector_size())]
        } else {
            vec![512, 4096]
        };
        let mut error = invalid("no valid GPT");
        for sector in sectors {
            let mut warnings = Vec::new();
            match gpt(&mut image, sector, &mut warnings) {
                Ok(regions) => {
                    layout.scheme = "gpt".into();
                    layout.sector_size = sector as u32;
                    layout.regions = regions;
                    layout.warnings = warnings;
                    return Ok(layout);
                }
                Err(e) => error = e,
            }
        }
        return Err(error);
    }
    layout.scheme = "mbr".into();
    layout.regions = mbr(&mut image, &boot, u64::from(layout.sector_size))?;
    if layout.regions.is_empty() {
        return Err(Error::Unsupported(
            "no supported volume or partition table; select carving for raw PNG search".into(),
        ));
    }
    Ok(layout)
}
