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
    Compressed {
        runs: Vec<Run>,
        size: u64,
        initialized: u64,
        unit: u64,
    },
}
impl Data {
    pub fn size(&self) -> u64 {
        match self {
            Self::Resident(v) => v.len() as u64,
            Self::NonResident { size, .. } | Self::Compressed { size, .. } => *size,
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
                    let index = runs
                        .partition_point(|r| r.logical <= cursor)
                        .checked_sub(1)
                        .ok_or_else(|| invalid("unmapped logical range"))?;
                    let run = &runs[index];
                    if cursor - run.logical >= run.length {
                        return Err(invalid("unmapped logical range"));
                    }
                    let count = (run.length - (cursor - run.logical))
                        .min(end.min(*initialized) - cursor)
                        as usize;
                    if let Some(physical) = run.physical {
                        let chunk = image.read(physical + (cursor - run.logical), count)?;
                        out[(cursor - offset) as usize..(cursor - offset) as usize + count]
                            .copy_from_slice(&chunk);
                    }
                    cursor += count as u64;
                }
                Ok(out)
            }
            Self::Compressed {
                runs,
                initialized,
                unit,
                ..
            } => {
                let mut output = vec![0; length];
                let mut cursor = offset;
                while cursor < end.min(*initialized) {
                    let start = cursor / unit * unit;
                    let required = (initialized - start).min(*unit);
                    let decoded = compression_unit(image, runs, start, *unit, required)?;
                    let count = (start + required - cursor).min(end - cursor) as usize;
                    let within = (cursor - start) as usize;
                    let into = (cursor - offset) as usize;
                    output[into..into + count].copy_from_slice(&decoded[within..within + count]);
                    cursor += count as u64;
                }
                Ok(output)
            }
        }
    }
}

fn compression_unit(
    image: &mut Image,
    runs: &[Run],
    start: u64,
    unit: u64,
    required: u64,
) -> Result<Vec<u8>> {
    let covered = runs.last().map(|r| r.logical + r.length).unwrap_or(0);
    let end = start.saturating_add(unit).min(covered);
    let mut encoded = Vec::with_capacity(unit as usize);
    let mut cursor = start;
    let mut sparse = false;
    for run in runs
        .iter()
        .filter(|r| r.logical < end && r.logical + r.length > start)
    {
        if run.logical > cursor {
            return Err(invalid("unmapped compression unit"));
        }
        let count = (run.logical + run.length).min(end) - cursor;
        if let Some(address) = run.physical {
            if sparse {
                return Err(invalid(
                    "allocated clusters after sparse compression-unit suffix",
                ));
            }
            encoded.extend(image.read(address + (cursor - run.logical), count as usize)?);
        } else {
            sparse = true;
        }
        cursor += count;
    }
    if cursor < start + required {
        return Err(invalid("incomplete compression unit"));
    }
    if encoded.is_empty() {
        Ok(vec![0; required as usize])
    } else if !sparse {
        encoded.truncate(required as usize);
        Ok(encoded)
    } else {
        crate::lznt1::decompress(&encoded, required as usize)
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
    // NTFS uses signed -1 as the highest VCN of an empty nonresident stream.
    let expected = if last_vcn == u64::MAX && runs.is_empty() {
        0
    } else {
        last_vcn
            .checked_add(1)
            .and_then(|n| n.checked_mul(geometry.cluster))
            .ok_or_else(|| invalid("VCN overflow"))?
    };
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
    pub streams: Vec<Stream>,
    pub unsupported: Vec<String>,
    pub hash: String,
    pub created: Option<crate::FileTimestamp>,
    pub modified: Option<crate::FileTimestamp>,
}

#[derive(Clone, Debug)]
pub(crate) struct Stream {
    pub name: Option<String>,
    pub data: Option<Data>,
    pub unsupported: Vec<String>,
}

pub(crate) fn candidate_id(index: u64, sequence: u16, stream: Option<&str>) -> String {
    let mut id = format!("mft-{index}-{sequence}");
    if let Some(name) = stream {
        id.push_str("-ads-");
        for code in name.encode_utf16() {
            use std::fmt::Write;
            let _ = write!(&mut id, "{code:04x}");
        }
    }
    id
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
        streams: Vec::new(),
        unsupported: Vec::new(),
        hash,
        created: None,
        modified: None,
    };
    let mut name_priority = 0;
    let mut standard_times = false;
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
        let stream_name = if attr[9] != 0 {
            let name_offset = u16_at(attr, 10)? as usize;
            let header = if nonresident == 0 {
                24
            } else if kind == 0x80 && u16_at(attr, 12)? & 1 != 0 {
                72
            } else {
                64
            };
            if name_offset < header {
                return Err(invalid("attribute name overlaps header"));
            }
            let codes = bytes(attr, name_offset, attr[9] as usize * 2)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes([b[0], b[1]]))
                .collect::<Vec<_>>();
            Some(String::from_utf16(&codes).map_err(|_| invalid("invalid stream name UTF-16"))?)
        } else {
            None
        };
        let attr_flags = u16_at(attr, 12)?;
        if kind == 0x20 {
            record
                .unsupported
                .push("attribute-list extents require a later release".into());
        }
        if nonresident == 0 {
            let value_len = u32_at(attr, 16)? as usize;
            let offset = u16_at(attr, 20)? as usize;
            if offset < 24
                || (stream_name.is_some()
                    && offset < u16_at(attr, 10)? as usize + attr[9] as usize * 2)
            {
                return Err(invalid("resident value overlaps header"));
            }
            let value = bytes(attr, offset, value_len)?;
            if kind == 0x10 && value.len() >= 32 {
                record.created = crate::timestamps::filetime(u64_at(value, 0)?);
                record.modified = crate::timestamps::filetime(u64_at(value, 8)?);
                standard_times = true;
            }
            if kind == 0x30 {
                if value.len() < 66 {
                    return Err(invalid("short FILE_NAME"));
                }
                let namespace = value[65];
                let priority = if namespace == 2 { 1 } else { 2 };
                if priority >= name_priority {
                    if !standard_times {
                        record.created = crate::timestamps::filetime(u64_at(value, 8)?);
                        record.modified = crate::timestamps::filetime(u64_at(value, 16)?);
                    }
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
        }
        if kind == 0x80 {
            if record.streams.iter().any(|s| s.name == stream_name) {
                return Err(invalid("duplicate DATA stream or unsupported extent"));
            }
            let stream = parse_stream(attr, stream_name, attr_flags, geometry)?;
            record.streams.push(stream);
        }
        at = at
            .checked_add(length)
            .ok_or_else(|| invalid("attribute offset overflow"))?;
    }
    Ok(record)
}

fn parse_stream(
    attr: &[u8],
    name: Option<String>,
    flags: u16,
    geometry: &Geometry,
) -> Result<Stream> {
    let mut stream = Stream {
        name,
        data: None,
        unsupported: Vec::new(),
    };
    if attr[8] == 0 {
        if flags != 0 {
            stream
                .unsupported
                .push("unsupported resident DATA flags".into());
        } else {
            stream.data = Some(Data::Resident(
                bytes(attr, u16_at(attr, 20)? as usize, u32_at(attr, 16)? as usize)?.to_vec(),
            ));
        }
        return Ok(stream);
    }
    let compression = u16_at(attr, 34)?;
    let compressed = flags & 1 != 0;
    if u64_at(attr, 16)? != 0 {
        stream.unsupported.push("extension DATA attribute".into());
    } else if flags & !0x8001 != 0 {
        stream
            .unsupported
            .push("encrypted or unknown DATA flags".into());
    } else if (compressed && (compression != 4 || geometry.cluster > 4096))
        || (!compressed && compression != 0)
    {
        stream
            .unsupported
            .push("unsupported compression-unit geometry".into());
    } else {
        let size = u64_at(attr, 48)?;
        let initialized = u64_at(attr, 56)?;
        let run_offset = u16_at(attr, 32)? as usize;
        let header_end = if compressed { 72 } else { 64 };
        let name_end = if stream.name.is_some() {
            u16_at(attr, 10)? as usize + attr[9] as usize * 2
        } else {
            0
        };
        if run_offset < header_end.max(name_end) || initialized > size {
            return Err(invalid("invalid nonresident DATA header"));
        }
        if u64_at(attr, 24)? == u64::MAX
            && (size != 0 || initialized != 0 || u64_at(attr, 40)? != 0)
        {
            return Err(invalid("nonempty DATA with empty VCN sentinel"));
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
        stream.data = Some(if compressed {
            Data::Compressed {
                runs,
                size,
                initialized,
                unit: geometry.cluster * 16,
            }
        } else {
            Data::NonResident {
                runs,
                size,
                initialized,
            }
        });
    }
    Ok(stream)
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
        let stream = first
            .streams
            .into_iter()
            .find(|s| s.name.is_none())
            .ok_or_else(|| invalid("MFT DATA is absent"))?;
        if !stream.unsupported.is_empty() {
            return Err(Error::Unsupported(stream.unsupported.join("; ")));
        }
        let mft = stream.data.ok_or_else(|| invalid("MFT DATA is absent"))?;
        if !matches!(mft, Data::NonResident { .. }) {
            return Err(invalid("MFT must be nonresident and uncompressed"));
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
        if mft.size() < geometry.record as u64 || !mft.size().is_multiple_of(geometry.record as u64)
        {
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
    fn empty_nonresident_stream_accepts_only_the_empty_vcn_sentinel() {
        let mut attr = vec![0u8; 72];
        attr[8] = 1;
        attr[24..32].copy_from_slice(&u64::MAX.to_le_bytes());
        attr[32..34].copy_from_slice(&64u16.to_le_bytes());
        let stream = parse_stream(&attr, None, 0, &geometry()).unwrap();
        assert_eq!(stream.data.unwrap().size(), 0);
        for field in [16, 40, 48, 56] {
            let mut malformed = attr.clone();
            malformed[field..field + 8].copy_from_slice(&1u64.to_le_bytes());
            if field == 16 {
                assert!(
                    parse_stream(&malformed, None, 0, &geometry())
                        .unwrap()
                        .data
                        .is_none()
                );
            } else {
                assert!(parse_stream(&malformed, None, 0, &geometry()).is_err());
            }
        }
        assert!(runlist(&[0x11, 1, 4, 0], &geometry(), u64::MAX).is_err());
        assert!(runlist(&[0], &geometry(), 0).is_err());
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
    fn compressed_4k_clusters_allow_random_reads_across_units() {
        let temporary = tempfile::NamedTempFile::new().unwrap();
        let mut raw = vec![b'B'; 8192 + 65536];
        let encoded = [3, 0xb0, 2, b'A', 0xfc, 0x0f].repeat(16);
        raw[4096..4096 + encoded.len()].copy_from_slice(&encoded);
        std::fs::write(temporary.path(), raw).unwrap();
        let mut image = Image::open(temporary.path()).unwrap();
        let stream = Data::Compressed {
            runs: vec![
                Run {
                    logical: 0,
                    physical: Some(4096),
                    length: 4096,
                },
                Run {
                    logical: 4096,
                    physical: None,
                    length: 61440,
                },
                Run {
                    logical: 65536,
                    physical: Some(8192),
                    length: 65536,
                },
            ],
            size: 131072,
            initialized: 130972,
            unit: 65536,
        };
        let crossing = stream.read(&mut image, 65000, 1000).unwrap();
        assert_eq!(&crossing[..536], &[b'A'; 536]);
        assert_eq!(&crossing[536..], &[b'B'; 464]);
        let tail = stream.read(&mut image, 130900, 172).unwrap();
        assert_eq!(&tail[..72], &[b'B'; 72]);
        assert_eq!(&tail[72..], &[0; 100]);
        let malformed = [
            Run {
                logical: 0,
                physical: Some(4096),
                length: 4096,
            },
            Run {
                logical: 4096,
                physical: None,
                length: 4096,
            },
            Run {
                logical: 8192,
                physical: Some(8192),
                length: 57344,
            },
        ];
        assert!(compression_unit(&mut image, &malformed, 0, 65536, 65536).is_err());
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
