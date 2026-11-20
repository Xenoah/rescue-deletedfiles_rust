//! Check LVM2 labels, PV headers, metadata area headers and current raw metadata locations.
use super::{Error, Result, invalid};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom},
    path::{Path, PathBuf},
    time::SystemTime,
};

const MDA_MAGIC: &[u8] = b" LVM2 x[5A%r0N*>";
const SECTOR: u64 = 512;
const MAX_TEXT: u64 = 4 << 20;

pub(super) struct Physical {
    pub file: File,
    pub id: String,
    pub size: u64,
    pub data: (u64, u64),
    pub areas: Vec<(u64, u64)>,
    path: PathBuf,
    modified: SystemTime,
}

fn u32le(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
fn u64le(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}

fn checksum(b: &[u8]) -> u32 {
    let mut h = crc32fast::Hasher::new_with_initial(!0xf597a6cfu32);
    h.update(b);
    !h.finalize()
}

fn read(file: &mut File, offset: u64, bytes: &mut [u8]) -> Result<()> {
    file.seek(SeekFrom::Start(offset))?;
    file.read_exact(bytes)?;
    Ok(())
}

fn locations(sector: &[u8], cursor: &mut usize) -> Result<Vec<(u64, u64)>> {
    let mut areas = Vec::new();
    while *cursor + 16 <= sector.len() {
        let pair = (u64le(sector, *cursor), u64le(sector, *cursor + 8));
        *cursor += 16;
        if pair == (0, 0) {
            return Ok(areas);
        }
        if pair.0 == 0 || !pair.0.is_multiple_of(SECTOR) || !pair.1.is_multiple_of(SECTOR) {
            return Err(invalid("unaligned physical area"));
        }
        areas.push(pair);
    }
    Err(invalid("physical area list has no terminator"))
}

fn metadata(file: &mut File, start: u64, size: u64) -> Result<Option<Vec<u8>>> {
    let mut h = [0; 512];
    read(file, start, &mut h)?;
    if u32le(&h, 0) != checksum(&h[4..])
        || &h[4..20] != MDA_MAGIC
        || u32le(&h, 20) != 1
        || (u64le(&h, 24), u64le(&h, 32)) != (start, size)
    {
        return Err(invalid("metadata header, checksum or area geometry"));
    }
    let offset = u64le(&h, 40);
    let len = u64le(&h, 48);
    let flags = u32le(&h, 60);
    if h[64..88] != [0; 24] || flags & !1 != 0 {
        return Err(Error::Unsupported(
            "LVM precommitted or unknown metadata locations".into(),
        ));
    }
    if flags == 1 {
        return Ok(None);
    }
    if offset < SECTOR
        || offset >= size
        || !offset.is_multiple_of(SECTOR)
        || len == 0
        || len > size - SECTOR
        || len > MAX_TEXT
    {
        return Err(invalid("metadata text outside the bounded circular area"));
    }
    let mut text = vec![0; len as usize];
    let first = len.min(size - offset) as usize;
    read(file, start + offset, &mut text[..first])?;
    if first < text.len() {
        read(file, start + SECTOR, &mut text[first..])?;
    }
    if checksum(&text) != u32le(&h, 56) {
        return Err(invalid("metadata text checksum"));
    }
    Ok(Some(text))
}

pub(super) fn equal_metadata(authority: &mut Option<Vec<u8>>, next: Vec<u8>) -> Result<()> {
    if let Some(old) = authority {
        if *old != next {
            return Err(invalid(
                "metadata copies disagree on the current volume group",
            ));
        }
    } else {
        *authority = Some(next);
    }
    Ok(())
}

impl Physical {
    pub fn open(path: &Path, authority: &mut Option<Vec<u8>>) -> Result<Self> {
        let mut file = File::open(path)?;
        let stat = file.metadata()?;
        let size = stat.len();
        let mut sectors = [0u8; 2048];
        read(&mut file, 0, &mut sectors)?;
        let mut labels = sectors
            .as_chunks::<512>()
            .0
            .iter()
            .enumerate()
            .filter(|(_, b)| &b[..8] == b"LABELONE");
        let (index, b) = labels
            .next()
            .ok_or_else(|| invalid("no physical volume label in the first four sectors"))?;
        if labels.next().is_some()
            || u64le(b, 8) != index as u64
            || u32le(b, 16) != checksum(&b[20..])
            || &b[24..32] != b"LVM2 001"
        {
            return Err(invalid("label checksum, location or format"));
        }
        let offset = u32le(b, 20) as usize;
        if !(32..=512 - 40).contains(&offset) {
            return Err(invalid("invalid PV header offset"));
        }
        let id_data = &b[offset..offset + 32];
        if !id_data.iter().all(u8::is_ascii_alphanumeric)
            || u64le(b, offset + 32) != size
            || !size.is_multiple_of(SECTOR)
        {
            return Err(invalid("physical volume UUID or image size"));
        }
        let id = std::str::from_utf8(id_data).unwrap().to_owned();
        let mut cur = offset + 40;
        let data = locations(b, &mut cur)?;
        let &[data] = data.as_slice() else {
            return Err(Error::Unsupported("choose a PV with one data area".into()));
        };
        if data.0 < 2048 || data.0 >= size || data.1 > size - data.0 {
            return Err(invalid("invalid physical data area"));
        }
        let areas = locations(b, &mut cur)?;
        if cur + 8 > b.len() {
            return Err(invalid("PV extension header outside label"));
        }
        match (u32le(b, cur), u32le(b, cur + 4)) {
            (0, 0) => {}
            (1, 0) | (2, 0 | 1) => {
                cur += 8;
                if !locations(b, &mut cur)?.is_empty() {
                    return Err(Error::Unsupported("LVM bootloader areas".into()));
                }
            }
            _ => {
                return Err(Error::Unsupported(
                    "unsupported LVM PV extension flags or version".into(),
                ));
            }
        }
        for (i, &(start, len)) in areas.iter().enumerate() {
            if start < 2048
                || len < 1024
                || start > size
                || len > size - start
                || areas[..i]
                    .iter()
                    .any(|&(p, n)| start < p + n && p < start + len)
            {
                return Err(invalid("overlapping or invalid metadata area"));
            }
            if let Some(text) = metadata(&mut file, start, len)? {
                equal_metadata(authority, text)?;
            }
        }
        Ok(Self {
            file,
            id,
            size,
            data,
            areas,
            path: path.to_owned(),
            modified: stat.modified()?,
        })
    }
    pub fn unchanged(&self) -> Result<()> {
        for stat in [self.file.metadata()?, self.path.metadata()?] {
            if stat.len() != self.size || stat.modified()? != self.modified {
                return Err(invalid("source changed during LVM recovery"));
            }
        }
        Ok(())
    }
}
