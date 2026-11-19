//! Microsoft Cabinet 1.3: check folder/file layout, CFDATA checksums and decoder output lengths.
use super::{charge, container::Input};
use crate::{Error, Result, source::Image};
use flate2::{Decompress, FlushDecompress, Status};
use lzxd::{Lzxd, WindowSize};
use std::{
    ops::Range,
    sync::atomic::{AtomicBool, Ordering},
};

const BLOCK: usize = 32 * 1024;
const MAX_OUTPUT: usize = 128 << 20;

fn invalid(detail: &str) -> Error {
    Error::Invalid(format!("invalid or unsupported CAB: {detail}"))
}
fn u16_at(data: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([data[at], data[at + 1]])
}
fn u32_at(data: &[u8], at: usize) -> u32 {
    u32::from_le_bytes(data[at..at + 4].try_into().unwrap())
}
fn take<'a>(data: &'a [u8], pos: &mut usize, len: usize) -> Result<&'a [u8]> {
    let end = pos
        .checked_add(len)
        .ok_or_else(|| invalid("offset overflow"))?;
    let part = data
        .get(*pos..end)
        .ok_or_else(|| invalid("truncated table or block"))?;
    *pos = end;
    Ok(part)
}

fn padding(data: &[u8], start: usize, end: usize) -> Result<()> {
    let gap = data
        .get(start..end)
        .ok_or_else(|| invalid("overlapping cabinet records"))?;
    if gap.len() > 4096 || gap.iter().any(|&b| b != 0) {
        return Err(invalid("unexplained bytes between cabinet records"));
    }
    Ok(())
}

fn checksum(bytes: &[u8]) -> u32 {
    let (words, tail) = bytes.as_chunks::<4>();
    // MS-CAB puts leftover bytes in the opposite byte order to complete words.
    let remainder = tail.iter().fold(0u32, |n, &b| (n << 8) | u32::from(b));
    words
        .iter()
        .fold(remainder, |sum, &word| sum ^ u32::from_le_bytes(word))
}

struct Folder {
    start: usize,
    blocks: usize,
    compression: u16,
    files: Vec<Range<u64>>,
}

enum Decoder {
    Stored,
    MsZip(Vec<u8>),
    Lzx(Box<Lzxd>),
}
impl Decoder {
    fn new(method: u16) -> Result<Self> {
        let windows = [
            WindowSize::KB32,
            WindowSize::KB64,
            WindowSize::KB128,
            WindowSize::KB256,
            WindowSize::KB512,
            WindowSize::MB1,
            WindowSize::MB2,
        ];
        match method {
            0 => Ok(Self::Stored),
            1 => Ok(Self::MsZip(Vec::with_capacity(BLOCK))),
            n if n & 255 == 3 && (15..=21).contains(&(n >> 8)) => Ok(Self::Lzx(Box::new(
                Lzxd::new(windows[(n >> 8) as usize - 15]),
            ))),
            _ => Err(invalid("unsupported compression method or window")),
        }
    }
    fn block(&mut self, packed: &[u8], unpacked: usize) -> Result<()> {
        match self {
            Self::Stored if packed.len() == unpacked => Ok(()),
            Self::Stored => Err(invalid("stored size mismatch")),
            Self::Lzx(decoder) => {
                let result = decoder
                    .decompress_next(packed, unpacked)
                    .map_err(|_| invalid("LZX block"))?;
                if result.len() != unpacked {
                    return Err(invalid("LZX decoded size"));
                }
                Ok(())
            }
            Self::MsZip(history) => {
                let payload = packed
                    .strip_prefix(b"CK")
                    .ok_or_else(|| invalid("MSZIP block signature"))?;
                let mut decoder = Decompress::new(false);
                if !history.is_empty() {
                    decoder
                        .set_dictionary(history)
                        .map_err(|_| invalid("MSZIP history"))?;
                }
                let mut decoded = vec![0; unpacked + 1];
                let status = decoder
                    .decompress(payload, &mut decoded, FlushDecompress::Finish)
                    .map_err(|_| invalid("MSZIP deflate stream"))?;
                if status != Status::StreamEnd
                    || decoder.total_out() != unpacked as u64
                    || decoder.total_in() != payload.len() as u64
                {
                    return Err(invalid("MSZIP encoded or decoded size"));
                }
                let discard = (history.len() + unpacked).saturating_sub(BLOCK);
                history.drain(..discard);
                history.extend_from_slice(&decoded[..unpacked]);
                Ok(())
            }
        }
    }
}

fn folders(data: &[u8], pos: &mut usize, folder_reserve: usize) -> Result<Vec<Folder>> {
    let count = usize::from(u16_at(data, 26));
    if !(1..=1024).contains(&count) {
        return Err(invalid("folder count"));
    }
    let mut result = Vec::with_capacity(count);
    for _ in 0..count {
        let row = take(data, pos, 8 + folder_reserve)?;
        if row[8..].iter().any(|&b| b != 0) {
            return Err(invalid("unknown reserved folder extension"));
        }
        result.push(Folder {
            start: u32_at(row, 0) as usize,
            blocks: usize::from(u16_at(row, 4)),
            compression: u16_at(row, 6),
            files: Vec::new(),
        });
    }
    Ok(result)
}

fn file_table(data: &[u8], pos: &mut usize, folders: &mut [Folder]) -> Result<()> {
    let count = usize::from(u16_at(data, 28));
    if !(1..=16384).contains(&count) {
        return Err(invalid("file count"));
    }
    for _ in 0..count {
        let row = take(data, pos, 16)?;
        let folder = folders
            .get_mut(usize::from(u16_at(row, 8)))
            .ok_or_else(|| invalid("missing or continued folder"))?;
        let begin = u64::from(u32_at(row, 4));
        folder.files.push(begin..begin + u64::from(u32_at(row, 0)));
        let length = data[*pos..]
            .iter()
            .take(4097)
            .position(|&b| b == 0)
            .filter(|&n| n > 0)
            .ok_or_else(|| invalid("missing or oversized file name"))?;
        let name = take(data, pos, length + 1)?;
        if row[14] & 0x80 != 0 && std::str::from_utf8(&name[..length]).is_err() {
            return Err(invalid("invalid UTF-8 file name"));
        }
    }
    Ok(())
}

pub(super) fn validate(
    image: &mut Image,
    at: u64,
    boundary: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
) -> Result<Vec<u8>> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    input.ensure(36)?;
    let h = &input.bytes[..36];
    if &h[..4] != b"MSCF"
        || h[24..26] != [3, 1]
        || [4, 12, 20].into_iter().any(|p| u32_at(h, p) != 0)
        || u16_at(h, 30) & !4 != 0
        || u16_at(h, 34) != 0
    {
        return Err(invalid("signature, version or cabinet dependency"));
    }
    let size = u32_at(h, 8) as usize;
    if size < 36 {
        return Err(invalid("cabinet length"));
    }
    let files_at = u32_at(h, 16) as usize;
    let reserved = u16_at(h, 30) == 4;
    let data = input.finish(size)?;
    let mut pos = 36;
    let (mut folder_reserve, mut block_reserve) = (0, 0);
    if reserved {
        let r = take(&data, &mut pos, 4)?;
        folder_reserve = usize::from(r[2]);
        block_reserve = usize::from(r[3]);
        if take(&data, &mut pos, usize::from(u16_at(r, 0)))?
            .iter()
            .any(|&b| b != 0)
        {
            return Err(invalid("signed or unknown reserved header"));
        }
    }
    let mut folders = folders(&data, &mut pos, folder_reserve)?;
    padding(&data, pos, files_at)?;
    pos = files_at;
    file_table(&data, &mut pos, &mut folders)?;
    folders.sort_by_key(|folder| folder.start);
    let mut total = 0usize;
    for mut folder in folders {
        padding(&data, pos, folder.start)?;
        pos = folder.start;
        let mut decoder = Decoder::new(folder.compression)?;
        let mut size = 0u64;
        for _ in 0..folder.blocks {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let row = take(&data, &mut pos, 8)?;
            let compressed = usize::from(u16_at(row, 4));
            let unpacked = usize::from(u16_at(row, 6));
            if !(1..=BLOCK).contains(&unpacked) || compressed == 0 || compressed > BLOCK + 6144 {
                return Err(invalid("continued or invalid CFDATA block sizes"));
            }
            total += unpacked;
            if total > MAX_OUTPUT {
                return Err(invalid("decoded size limit"));
            }
            charge(budget, unpacked)?;
            let content = take(&data, &mut pos, block_reserve + compressed)?;
            let sum = u32_at(row, 0);
            let (reserve, packed) = content.split_at(block_reserve);
            if reserve.iter().any(|&b| b != 0) {
                return Err(invalid("unknown reserved data extension"));
            }
            if sum == 0 || (checksum(packed) ^ u32_at(row, 4)) != sum {
                return Err(invalid("missing or incorrect CFDATA checksum"));
            }
            decoder.block(packed, unpacked)?;
            size += unpacked as u64;
        }
        folder.files.sort_by_key(|range| (range.start, range.end));
        let mut next = 0;
        for range in folder.files {
            if range.start != next || range.end > size {
                return Err(invalid("file overlaps or exceeds decoded folder"));
            }
            next = range.end;
        }
        if next != size {
            return Err(invalid("unaccounted decoded folder bytes"));
        }
    }
    if pos != data.len() {
        return Err(invalid("unaccounted cabinet trailer"));
    }
    Ok(data)
}
