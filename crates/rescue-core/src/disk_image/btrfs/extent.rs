//! Reconstruct snapshot file offsets, validating physical file data against the checksum tree.
use super::tree::{Item, Key, Node, Volume};
use super::{FileInfo, bad, u32le, u64le};
use crate::{Error, Result};
use flate2::{Decompress, FlushDecompress, Status};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
};

pub(super) struct Record {
    pub start: u64,
    pub kind: u8,
    pub item: Item,
}
const MAX_COMPRESSED: usize = 128 << 10;
const MAX_OUTPUT: u64 = 16 << 40;

#[derive(Default)]
pub(super) struct Checksums(BTreeMap<u64, Vec<u8>>);
impl Checksums {
    pub fn load(volume: &mut Volume, root: Node) -> Result<Self> {
        let sector = volume.sector as u64;
        let width = volume.checksum.size();
        let mut index = BTreeMap::new();
        let mut bytes = 0usize;
        let mut end = 0;
        volume.walk(root, |Key(object, ty, logical), _, payload| {
            if object != u64::MAX - 9 || ty != 128 {
                return Err(bad("unsupported checksum-tree item"));
            }
            if payload.is_empty()
                || !payload.len().is_multiple_of(width)
                || !logical.is_multiple_of(sector)
                || logical < end
            {
                return Err(bad("invalid extent checksum range"));
            }
            end = logical
                .checked_add((payload.len() / width) as u64 * sector)
                .ok_or_else(|| bad("checksum range overflow"))?;
            bytes += payload.len();
            if bytes > 64 << 20 {
                return Err(Error::Limit("Btrfs checksum index exceeds 64 MiB".into()));
            }
            index.insert(logical, payload.to_vec());
            Ok(())
        })?;
        Ok(Self(index))
    }
    fn sector(&self, volume: &mut Volume, logical: u64, bytes: &mut [u8]) -> Result<()> {
        let width = volume.checksum.size();
        let sum = self
            .0
            .range(..=logical)
            .next_back()
            .and_then(|(&start, values)| {
                usize::try_from((logical - start) / volume.sector as u64)
                    .ok()
                    .and_then(|i| i.checked_mul(width))
                    .and_then(|start| values.get(start..)?.get(..width))
            })
            .ok_or_else(|| bad("missing data sector checksum"))?;
        volume.data_sector(logical, sum, bytes)
    }
}

// Each LZO segment describes at most one filesystem sector. A segment's
// four-byte header cannot cross a sector boundary; the kernel pads that gap.
fn lzo_segments(
    input: &[u8],
    sector: usize,
    padded: usize,
    inline: bool,
) -> Result<(Vec<u8>, &[u8])> {
    if input.len() < 8 {
        return Err(bad("truncated LZO extent header"));
    }
    let total = u32le(input, 0) as usize;
    let data = input
        .get(..total)
        .filter(|s| s.len() >= 8)
        .ok_or_else(|| bad("LZO extent length exceeds stored data"))?;
    let mut plain = Vec::with_capacity(padded);
    let mut segment = vec![0; sector];
    let mut at = 4;
    while at < data.len() {
        if data.len() - at < 4 || (inline && at != 4) {
            return Err(bad("invalid LZO segment header"));
        }
        let bytes = u32le(data, at) as usize;
        at += 4;
        if bytes == 0 || bytes > data.len() - at {
            return Err(bad("invalid LZO segment length"));
        }
        let decoded = lzo::decompress_into(&data[at..at + bytes], &mut segment)
            .map_err(|_| bad("invalid LZO segment stream"))?;
        if decoded == 0 || plain.len() + decoded > padded {
            return Err(bad("oversized LZO extent"));
        }
        plain.extend_from_slice(&segment[..decoded]);
        at += bytes;
        let until_boundary = at.next_multiple_of(sector) - at;
        if (1..4).contains(&until_boundary) && at < data.len() {
            let padding = data
                .get(at..at + until_boundary)
                .ok_or_else(|| bad("truncated LZO alignment"))?;
            if padding.iter().any(|&b| b != 0) {
                return Err(bad("invalid LZO segment alignment"));
            }
            at += until_boundary;
        }
        if at < data.len() && decoded != sector {
            return Err(bad("short intermediate LZO segment"));
        }
    }
    Ok((plain, &input[total..]))
}

fn decode(codec: u8, input: &[u8], length: usize, sector: usize, inline: bool) -> Result<Vec<u8>> {
    let padded = length.div_ceil(sector) * sector;
    if padded > MAX_COMPRESSED {
        return Err(bad("oversized compressed extent"));
    }
    let (mut decoded, remaining) = match codec {
        1 => {
            let mut stream = Decompress::new(true);
            let mut data = vec![0; padded + 1];
            if stream
                .decompress(input, &mut data, FlushDecompress::Finish)
                .map_err(|_| bad("invalid zlib stream"))?
                != Status::StreamEnd
            {
                return Err(bad("incomplete or oversized zlib extent"));
            }
            data.truncate(stream.total_out() as usize);
            (data, &input[stream.total_in() as usize..])
        }
        2 => lzo_segments(input, sector, padded, inline)?,
        3 => {
            let mut rest = input;
            let mut decoder = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
                &mut rest,
                MAX_COMPRESSED as u64,
            )
            .map_err(|_| bad("invalid Zstandard header"))?;
            let mut data = Vec::with_capacity(padded);
            (&mut decoder)
                .take(padded as u64 + 1)
                .read_to_end(&mut data)?;
            if !decoder.decoder.is_finished()
                || decoder
                    .decoder
                    .get_checksum_from_data()
                    .is_some_and(|sum| Some(sum) != decoder.decoder.get_calculated_checksum())
            {
                return Err(bad("incomplete Zstandard frame or checksum mismatch"));
            }
            (data, rest)
        }
        _ => {
            return Err(Error::Unsupported(
                "unsupported Btrfs compression codec".into(),
            ));
        }
    };
    if (inline && !remaining.is_empty())
        || remaining.iter().any(|&b| b != 0)
        || !(decoded.len() == length || decoded.len() == padded)
        || decoded[length..].iter().any(|&b| b != 0)
    {
        return Err(bad("invalid compressed extent length or padding"));
    }
    decoded.truncate(length);
    Ok(decoded)
}

struct Output<'a> {
    file: &'a mut File,
    digest: Sha256,
    written: u64,
}
impl Output<'_> {
    fn write(&mut self, bytes: &[u8]) -> Result<()> {
        self.file.write_all(bytes)?;
        self.digest.update(bytes);
        self.written += bytes.len() as u64;
        Ok(())
    }
    fn gap(&mut self, volume: &Volume, end: u64) -> Result<()> {
        if end < self.written {
            return Err(bad("overlapping file extents"));
        }
        self.file.seek(SeekFrom::Start(end))?;
        static ZERO: [u8; 65536] = [0; 65536];
        while self.written < end {
            volume.unchanged()?;
            let n = (end - self.written).min(ZERO.len() as u64) as usize;
            self.digest.update(&ZERO[..n]);
            self.written += n as u64;
        }
        Ok(())
    }
}

pub(super) fn write_file(
    volume: &mut Volume,
    inode: &FileInfo,
    records: &[Record],
    sums: &Checksums,
    file: &mut File,
) -> Result<(u64, Sha256)> {
    let size = inode.size;
    if size > MAX_OUTPUT {
        return Err(Error::Limit("Btrfs file exceeds 16 TiB".into()));
    }
    if records.is_empty() && inode.allocated != 0 {
        return Err(bad("allocated inode has no extent items"));
    }
    let mut out = Output {
        file,
        digest: Sha256::new(),
        written: 0,
    };
    let sector = volume.sector as u64;
    for record in records {
        let raw = volume.item(record.item)?;
        if raw.len() < 21 || raw[17..20] != [0; 3] {
            return Err(bad("incomplete or encoded extent"));
        }
        if record.kind == 0 {
            if records.len() != 1 || record.start != 0 || u64le(&raw, 8) != size || size > 65536 {
                return Err(bad("inconsistent inline file"));
            }
            let stored = &raw[21..];
            match raw[16] {
                0 if stored.len() == size as usize => out.write(stored)?,
                0 => return Err(bad("short inline data")),
                codec => out.write(&decode(codec, stored, size as usize, volume.sector, true)?)?,
            }
            continue;
        }
        if raw.len() != 53 || !matches!(record.kind, 1 | 2) || !record.start.is_multiple_of(sector)
        {
            return Err(bad("invalid regular extent record"));
        }
        let ram = u64le(&raw, 8);
        let disk = u64le(&raw, 21);
        let allocated = u64le(&raw, 29);
        let offset = u64le(&raw, 37);
        let count = u64le(&raw, 45);
        if count == 0
            || !count.is_multiple_of(sector)
            || !offset.is_multiple_of(sector)
            || record.start.checked_add(count).is_none()
        {
            return Err(bad("invalid file extent range"));
        }
        // KEEP_SIZE preallocation may begin beyond EOF and contributes no readable bytes.
        if record.start >= size && record.kind == 2 {
            continue;
        }
        out.gap(volume, record.start.min(size))?;
        if record.start >= size {
            return Err(bad("data extent begins beyond inode size"));
        }
        let needed = count.min(size - record.start);
        if disk == 0 {
            if allocated != 0 || offset != 0 || raw[16] != 0 || record.kind != 1 {
                return Err(bad("invalid explicit hole"));
            }
            out.gap(volume, record.start + needed)?;
            continue;
        }
        if !disk.is_multiple_of(sector)
            || allocated == 0
            || !allocated.is_multiple_of(sector)
            || disk.checked_add(allocated).is_none()
            || offset.checked_add(count).is_none_or(|n| n > ram)
        {
            return Err(bad("invalid allocated extent geometry"));
        }
        if record.kind == 2 {
            if raw[16] != 0 || ram != allocated {
                return Err(bad("invalid preallocation"));
            }
            out.gap(volume, record.start + needed)?;
        } else if inode.flags & 1 != 0 {
            return Err(Error::Unsupported(
                "NODATASUM file has no verifiable sector checksums".into(),
            ));
        } else if raw[16] == 0 {
            if ram != allocated {
                return Err(bad("uncompressed disk and RAM lengths differ"));
            }
            let mut buf = vec![0; volume.sector];
            let mut done = 0;
            while done < needed {
                volume.unchanged()?;
                sums.sector(volume, disk + offset + done, &mut buf)?;
                let take = (needed - done).min(sector) as usize;
                out.write(&buf[..take])?;
                done += take as u64;
            }
        } else {
            if allocated > MAX_COMPRESSED as u64 || ram > MAX_COMPRESSED as u64 {
                return Err(bad("oversized compressed disk extent"));
            }
            let mut stored = vec![0; allocated as usize];
            for (i, buf) in stored.chunks_mut(volume.sector).enumerate() {
                sums.sector(volume, disk + i as u64 * sector, buf)?;
            }
            let plain = decode(raw[16], &stored, ram as usize, volume.sector, false)?;
            out.write(&plain[offset as usize..(offset + needed) as usize])?;
        }
    }
    out.gap(volume, size)?;
    out.file.set_len(size)?;
    volume.unchanged()?;
    Ok((size, out.digest))
}
