use crate::{Error, Result, source::Image};
use sha2::{Digest, Sha256};

fn invalid(message: &str) -> Error {
    Error::Invalid(message.into())
}
fn bytes(data: &[u8], at: usize, len: usize) -> Result<&[u8]> {
    data.get(
        at..at
            .checked_add(len)
            .ok_or_else(|| invalid("field overflow"))?,
    )
    .ok_or_else(|| invalid("truncated NTFS field"))
}
pub(crate) fn u16_at(data: &[u8], at: usize) -> Result<u16> {
    Ok(u16::from_le_bytes(bytes(data, at, 2)?.try_into().unwrap()))
}
fn u32_at(data: &[u8], at: usize) -> Result<u32> {
    Ok(u32::from_le_bytes(bytes(data, at, 4)?.try_into().unwrap()))
}
fn u64_at(data: &[u8], at: usize) -> Result<u64> {
    Ok(u64::from_le_bytes(bytes(data, at, 8)?.try_into().unwrap()))
}

#[derive(Clone, Debug)]
pub(crate) struct Run {
    pub logical: u64,
    pub physical: Option<u64>,
    pub length: u64,
}

#[derive(Clone, Debug)]
pub(crate) enum Data {
    Resident(Vec<u8>),
    NonResident {
        runs: Vec<Run>,
        size: u64,
        initialized: u64,
    },
}
impl Data {
    pub fn size(&self) -> u64 {
        match self {
            Self::Resident(v) => v.len() as u64,
            Self::NonResident { size, .. } => *size,
        }
    }
    pub fn read(&self, image: &mut Image, offset: u64, length: usize) -> Result<Vec<u8>> {
        let end = offset
            .checked_add(length as u64)
            .ok_or_else(|| invalid("stream overflow"))?;
        if end > self.size() {
            return Err(invalid("read outside stream"));
        }
        match self {
            Self::Resident(v) => Ok(bytes(v, offset as usize, length)?.to_vec()),
            Self::NonResident {
                runs, initialized, ..
            } => {
                let mut out = vec![0; length];
                let mut cursor = offset;
                while cursor < end.min(*initialized) {
                    let run = runs
                        .iter()
                        .find(|r| cursor >= r.logical && cursor - r.logical < r.length)
                        .ok_or_else(|| invalid("unmapped logical range"))?;
                    let count = (run.length - (cursor - run.logical))
                        .min(end.min(*initialized) - cursor)
                        as usize;
                    if let Some(physical) = run.physical {
                        let chunk = image.read(physical + cursor - run.logical, count)?;
                        out[(cursor - offset) as usize..(cursor - offset) as usize + count]
                            .copy_from_slice(&chunk);
                    }
                    cursor += count as u64;
                }
                Ok(out)
            }
        }
    }
}

#[derive(Clone, Debug)]
pub(crate) struct Geometry {
    pub sector: usize,
    pub cluster: u64,
    pub record: usize,
    pub mft: u64,
    pub volume_length: u64,
}
impl Geometry {
    pub fn parse(image: &mut Image) -> Result<Self> {
        let boot = image.read(0, 512)?;
        if &boot[3..11] != b"NTFS    " || boot[510..512] != [0x55, 0xaa] {
            return Err(Error::Unsupported(
                "expected an NTFS volume RAW image (partition starts at byte 0)".into(),
            ));
        }
        let sector = u16_at(&boot, 11)? as usize;
        let spc = boot[13] as u64;
        if ![512, 1024, 2048, 4096].contains(&sector) || !spc.is_power_of_two() || spc > 128 {
            return Err(invalid("unsupported NTFS sector/cluster geometry"));
        }
        let cluster = sector as u64 * spc;
        let count = u64_at(&boot, 40)?;
        // NTFS commonly records the last sector index here; tolerate that backup sector.
        let volume_length = count
            .checked_mul(sector as u64)
            .ok_or_else(|| invalid("volume overflow"))?;
        if volume_length < 512 || volume_length > image.length {
            return Err(invalid("truncated NTFS volume"));
        }
        let mft = u64_at(&boot, 48)?
            .checked_mul(cluster)
            .ok_or_else(|| invalid("MFT overflow"))?;
        let code = boot[64] as i8;
        let record = if code < 0 {
            1u64.checked_shl(-(code as i32) as u32)
                .ok_or_else(|| invalid("record size overflow"))?
        } else {
            cluster
                .checked_mul(code as u64)
                .ok_or_else(|| invalid("record size overflow"))?
        };
        if record < sector as u64 || record > 65536 || !record.is_multiple_of(sector as u64) {
            return Err(invalid("unsupported MFT record size"));
        }
        if mft
            .checked_add(record)
            .is_none_or(|end| end > volume_length)
        {
            return Err(invalid("MFT outside volume"));
        }
        Ok(Self {
            sector,
            cluster,
            record: record as usize,
            mft,
            volume_length,
        })
    }
}

fn runlist(data: &[u8], geometry: &Geometry, last_vcn: u64) -> Result<Vec<Run>> {
    let mut pos = 0;
    let mut lcn = 0i64;
    let mut logical = 0u64;
    let mut runs = Vec::new();
    loop {
        let header = *data
            .get(pos)
            .ok_or_else(|| invalid("unterminated runlist"))?;
        pos += 1;
        if header == 0 {
            break;
        }
        let length_bytes = (header & 15) as usize;
        let offset_bytes = (header >> 4) as usize;
        if length_bytes == 0 || length_bytes > 8 || offset_bytes > 8 {
            return Err(invalid("invalid runlist integer width"));
        }
        let mut length_buf = [0; 8];
        length_buf[..length_bytes].copy_from_slice(bytes(data, pos, length_bytes)?);
        pos += length_bytes;
        let clusters = u64::from_le_bytes(length_buf);
        if clusters == 0 {
            return Err(invalid("zero length run"));
        }
        let length = clusters
            .checked_mul(geometry.cluster)
            .ok_or_else(|| invalid("run length overflow"))?;
        let physical = if offset_bytes == 0 {
            None
        } else {
            let encoded = bytes(data, pos, offset_bytes)?;
            let mut offset_buf = if encoded[offset_bytes - 1] & 0x80 != 0 {
                [0xff; 8]
            } else {
                [0; 8]
            };
            offset_buf[..offset_bytes].copy_from_slice(encoded);
            pos += offset_bytes;
            lcn = lcn
                .checked_add(i64::from_le_bytes(offset_buf))
                .ok_or_else(|| invalid("LCN overflow"))?;
            let address = u64::try_from(lcn)
                .ok()
                .and_then(|v| v.checked_mul(geometry.cluster))
                .ok_or_else(|| invalid("negative/overflowing LCN"))?;
            if address
                .checked_add(length)
                .is_none_or(|end| end > geometry.volume_length)
            {
                return Err(invalid("data run outside volume"));
            }
            Some(address)
        };
        runs.push(Run {
            logical,
            physical,
            length,
        });
        logical = logical
            .checked_add(length)
            .ok_or_else(|| invalid("logical run overflow"))?;
    }
    let expected = last_vcn
        .checked_add(1)
        .and_then(|n| n.checked_mul(geometry.cluster))
        .ok_or_else(|| invalid("VCN overflow"))?;
    if logical != expected {
        return Err(invalid("runlist VCN coverage mismatch"));
    }
    Ok(runs)
}

#[derive(Clone, Debug)]
pub(crate) struct Record {
    pub sequence: u16,
    pub in_use: bool,
    pub directory: bool,
    pub base: u64,
    pub name: Option<String>,
    pub parent: u64,
    pub parent_sequence: u16,
    pub data: Option<Data>,
    pub unsupported: Vec<String>,
    pub hash: String,
}

pub(crate) fn parse_record(mut raw: Vec<u8>, geometry: &Geometry) -> Result<Record> {
    if raw.len() != geometry.record || bytes(&raw, 0, 4)? != b"FILE" {
        return Err(invalid("not an MFT record"));
    }
    let hash = format!("{:x}", Sha256::digest(&raw));
    let usa = u16_at(&raw, 4)? as usize;
    let count = u16_at(&raw, 6)? as usize;
    if count != raw.len() / geometry.sector + 1 || usa < 8 || usa + count * 2 > geometry.sector - 2
    {
        return Err(invalid("invalid update sequence array"));
    }
    let marker = u16_at(&raw, usa)?;
    for i in 1..count {
        let tail = i * geometry.sector - 2;
        if u16_at(&raw, tail)? != marker {
            return Err(invalid("MFT sector fixup mismatch"));
        }
        let value = u16_at(&raw, usa + i * 2)?.to_le_bytes();
        raw[tail..tail + 2].copy_from_slice(&value);
    }
    let flags = u16_at(&raw, 22)?;
    let used = u32_at(&raw, 24)? as usize;
    let mut at = u16_at(&raw, 20)? as usize;
    if used > raw.len() || at < 48 || at < usa + count * 2 || at >= used || !at.is_multiple_of(8) {
        return Err(invalid("invalid MFT attribute bounds"));
    }
    let mut record = Record {
        sequence: u16_at(&raw, 16)?,
        in_use: flags & 1 != 0,
        directory: flags & 2 != 0,
        base: u64_at(&raw, 32)?,
        name: None,
        parent: 0,
        parent_sequence: 0,
        data: None,
        unsupported: Vec::new(),
        hash,
    };
    let mut name_priority = 0;
    loop {
        let kind = u32_at(&raw[..used], at)?;
        if kind == 0xffff_ffff {
            break;
        }
        let length = u32_at(&raw[..used], at + 4)? as usize;
        if length < 24 || !length.is_multiple_of(8) {
            return Err(invalid("invalid attribute length"));
        }
        let attr = bytes(&raw[..used], at, length)?;
        let nonresident = attr[8];
        if nonresident > 1 {
            return Err(invalid("invalid residency flag"));
        }
        let named = attr[9] != 0;
        if named {
            let name_offset = u16_at(attr, 10)? as usize;
            bytes(attr, name_offset, attr[9] as usize * 2)?;
        }
        let attr_flags = u16_at(attr, 12)?;
        if kind == 0x20 {
            record
                .unsupported
                .push("attribute-list extents require a later release".into());
        }
        if nonresident == 0 {
            let value_len = u32_at(attr, 16)? as usize;
            let offset = u16_at(attr, 20)? as usize;
            if offset < 24 {
                return Err(invalid("resident value overlaps header"));
            }
            let value = bytes(attr, offset, value_len)?;
            if kind == 0x30 {
                if value.len() < 66 {
                    return Err(invalid("short FILE_NAME"));
                }
                let namespace = value[65];
                let priority = if namespace == 2 { 1 } else { 2 };
                if priority >= name_priority {
                    let name = bytes(value, 66, value[64] as usize * 2)?
                        .as_chunks::<2>()
                        .0
                        .iter()
                        .map(|b| u16::from_le_bytes([b[0], b[1]]))
                        .collect::<Vec<_>>();
                    record.name = Some(String::from_utf16_lossy(&name));
                    let parent = u64_at(value, 0)?;
                    record.parent = parent & 0x0000_ffff_ffff_ffff;
                    record.parent_sequence = (parent >> 48) as u16;
                    name_priority = priority;
                }
            }
            if kind == 0x80 && !named {
                if record.data.is_some() {
                    return Err(invalid("multiple unnamed DATA attributes"));
                }
                if attr_flags != 0 {
                    record
                        .unsupported
                        .push("unsupported resident DATA flags".into());
                }
                record.data = Some(Data::Resident(value.to_vec()));
            }
        } else if kind == 0x80 && !named {
            if record.data.is_some() {
                return Err(invalid("multiple DATA extents"));
            }
            if u64_at(attr, 16)? != 0 {
                record.unsupported.push("extension DATA attribute".into());
            } else if attr_flags & !0x8000 != 0 || u16_at(attr, 34)? != 0 {
                record
                    .unsupported
                    .push("compressed/encrypted DATA is not supported by this alpha".into());
            } else {
                let size = u64_at(attr, 48)?;
                let initialized = u64_at(attr, 56)?;
                let run_offset = u16_at(attr, 32)? as usize;
                if run_offset < 64 || initialized > size {
                    return Err(invalid("invalid nonresident DATA header"));
                }
                let runs = runlist(
                    attr.get(run_offset..)
                        .ok_or_else(|| invalid("runlist offset"))?,
                    geometry,
                    u64_at(attr, 24)?,
                )?;
                let covered = runs.last().map(|r| r.logical + r.length).unwrap_or(0);
                if size > covered {
                    return Err(invalid("DATA larger than runlist"));
                }
                record.data = Some(Data::NonResident {
                    runs,
                    size,
                    initialized,
                });
            }
        }
        at = at
            .checked_add(length)
            .ok_or_else(|| invalid("attribute offset overflow"))?;
    }
    Ok(record)
}

pub(crate) struct Ntfs {
    pub geometry: Geometry,
    pub mft: Data,
    pub records: u64,
}
impl Ntfs {
    pub fn open(image: &mut Image) -> Result<Self> {
        let geometry = Geometry::parse(image)?;
        let first = parse_record(image.read(geometry.mft, geometry.record)?, &geometry)?;
        if !first.unsupported.is_empty() {
            return Err(Error::Unsupported(
                "MFT attribute-list/compression is not supported by this alpha".into(),
            ));
        }
        let mft = first.data.ok_or_else(|| invalid("MFT DATA is absent"))?;
        if !matches!(mft, Data::NonResident { .. }) {
            return Err(invalid("resident MFT"));
        }
        if let Data::NonResident {
            runs,
            size,
            initialized,
        } = &mft
            && (*initialized < *size || runs.iter().any(|r| r.physical.is_none()))
        {
            return Err(invalid("MFT has uninitialized or sparse ranges"));
        }
        if !mft.size().is_multiple_of(geometry.record as u64) {
            return Err(invalid("MFT stream size is not record aligned"));
        }
        let records = mft.size() / geometry.record as u64;
        Ok(Self {
            geometry,
            mft,
            records,
        })
    }
    pub fn record(&self, image: &mut Image, index: u64) -> Result<Option<Record>> {
        if index >= self.records {
            return Err(invalid("record index outside MFT"));
        }
        let raw = self.mft.read(
            image,
            index * self.geometry.record as u64,
            self.geometry.record,
        )?;
        if raw.get(..4) != Some(b"FILE") {
            return Ok(None);
        }
        parse_record(raw, &self.geometry).map(Some)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn geometry() -> Geometry {
        Geometry {
            sector: 512,
            cluster: 512,
            record: 1024,
            mft: 2048,
            volume_length: 1024 * 1024,
        }
    }

    #[test]
    fn signed_delta_and_sparse_run_preserve_lcn() {
        let runs = runlist(
            &[0x21, 1, 0x2c, 1, 0x01, 1, 0x11, 1, 0xff, 0],
            &geometry(),
            2,
        )
        .unwrap();
        assert_eq!(runs[0].physical, Some(300 * 512));
        assert_eq!(runs[1].physical, None);
        assert_eq!(runs[2].physical, Some(299 * 512));
        assert_eq!(runs[2].logical, 1024);
    }
    #[test]
    fn malformed_runs_are_rejected() {
        for encoded in [
            vec![0x11, 0, 4, 0],          // zero length
            vec![0x11, 1, 0xff, 0],       // negative LCN
            vec![0x19, 1, 2, 3, 0],       // oversized length integer
            vec![0x11, 1],                // truncated offset
            vec![0x11, 1, 4],             // no terminator
            vec![0x21, 1, 0xff, 0x7f, 0], // outside image
        ] {
            assert!(runlist(&encoded, &geometry(), 0).is_err(), "{encoded:?}");
        }
        assert!(runlist(&[0x11, 1, 4, 0], &geometry(), 1).is_err());
    }
    #[test]
    fn truncated_fields_never_panic() {
        for len in 0..1024 {
            assert!(parse_record(vec![0; len], &geometry()).is_err());
        }
    }
    #[test]
    fn deterministic_malformed_records_do_not_panic() {
        let mut seed = 0x12345678u32;
        for _ in 0..2000 {
            let mut raw = vec![0; 1024];
            for b in &mut raw {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *b = seed as u8;
            }
            raw[..4].copy_from_slice(b"FILE");
            let _ = parse_record(raw, &geometry());
        }
    }
    #[test]
    fn uninitialized_data_is_zero_not_old_disk_content() {
        let temporary = tempfile::NamedTempFile::new().unwrap();
        std::fs::write(temporary.path(), vec![0x77; 4096]).unwrap();
        let mut image = Image::open(temporary.path()).unwrap();
        let stream = Data::NonResident {
            runs: vec![Run {
                logical: 0,
                physical: Some(1024),
                length: 1024,
            }],
            size: 1024,
            initialized: 512,
        };
        let bytes = stream.read(&mut image, 256, 768).unwrap();
        assert_eq!(&bytes[..256], &[0x77; 256]);
        assert_eq!(&bytes[256..], &[0; 512]);
        assert!(stream.read(&mut image, u64::MAX, 2).is_err());
    }
    #[test]
    fn record_fixup_and_attribute_boundaries_are_checked() {
        let mut raw = vec![0; 1024];
        raw[..4].copy_from_slice(b"FILE");
        raw[4..6].copy_from_slice(&48u16.to_le_bytes());
        raw[6..8].copy_from_slice(&3u16.to_le_bytes());
        raw[20..22].copy_from_slice(&56u16.to_le_bytes());
        raw[24..28].copy_from_slice(&64u32.to_le_bytes());
        raw[48..50].copy_from_slice(&0xa55au16.to_le_bytes());
        raw[510..512].copy_from_slice(&0xa55au16.to_le_bytes());
        raw[1022..1024].copy_from_slice(&0xa55au16.to_le_bytes());
        raw[56..60].copy_from_slice(&u32::MAX.to_le_bytes());
        assert!(parse_record(raw.clone(), &geometry()).is_ok());
        let mut torn = raw.clone();
        torn[510] = 0;
        assert!(parse_record(torn, &geometry()).is_err());
        let mut overlapping = raw.clone();
        overlapping[20..22].copy_from_slice(&48u16.to_le_bytes());
        assert!(parse_record(overlapping, &geometry()).is_err());
        raw[56..60].copy_from_slice(&0x80u32.to_le_bytes());
        raw[60..64].copy_from_slice(&0xfffffff8u32.to_le_bytes());
        assert!(parse_record(raw, &geometry()).is_err());
    }
}
