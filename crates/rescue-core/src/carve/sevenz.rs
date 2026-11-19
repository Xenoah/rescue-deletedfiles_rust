//! Bounded validation of contiguous, standalone 7z archives without extracting any paths.
use super::{charge, container::Input};
use crate::{Error, Result, source::Image};
use sevenz_rust2::{ArchiveReader, Password};
use std::{
    io::Cursor,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) const MAGIC: &[u8] = b"7z\xbc\xaf'\x1c";
const MAX_DECODED: u64 = 128 << 20;
const MAX_HEADER: u64 = 1 << 20;
const MAX_ENTRIES: usize = 4096;

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("invalid or unsupported 7z archive: {reason}"))
}

pub(super) fn validate(
    image: &mut Image,
    at: u64,
    boundary: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
) -> Result<Vec<u8>> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    input.ensure(32)?;
    let header = &input.bytes[..32];
    if !header.starts_with(MAGIC) || header[6] != 0 || header[7] > 4 {
        return Err(invalid("signature or format version"));
    }
    let crc = |bytes: &[u8]| u32::from_le_bytes(bytes.try_into().expect("fixed CRC field"));
    if crc32fast::hash(&header[12..]) != crc(&header[8..12]) {
        return Err(invalid("start header CRC"));
    }
    let offset = u64::from_le_bytes(header[12..20].try_into().unwrap());
    let size = u64::from_le_bytes(header[20..28].try_into().unwrap());
    let next_crc = crc(&header[28..32]);
    let end = 32u64
        .checked_add(offset)
        .and_then(|v| v.checked_add(size))
        .filter(|&v| (1..=MAX_HEADER).contains(&size) && v <= input.limit() as u64)
        .ok_or_else(|| invalid("next header extent"))? as usize;
    let bytes = input.finish(end)?;
    let next = &bytes[end - size as usize..];
    if !matches!(next.first(), Some(1 | 0x17)) || crc32fast::hash(next) != next_crc {
        return Err(invalid("next header type or CRC"));
    }

    let mut reader =
        ArchiveReader::new_with_memory_limit(Cursor::new(&bytes), Password::empty(), 32 * 1024)
            .map_err(|_| invalid("header structure, encoding or decoder memory limit"))?;
    let archive = reader.archive();
    if archive.files.is_empty()
        || archive.files.len() > MAX_ENTRIES
        || archive.blocks.len() > MAX_ENTRIES
    {
        return Err(invalid("entry count"));
    }
    let packed_end = archive
        .pack_sizes()
        .iter()
        .try_fold(archive.pack_pos(), |pos, &size| pos.checked_add(size));
    if packed_end.is_none_or(|pos| pos > offset) {
        return Err(invalid("packed streams overlap the next header"));
    }
    let mut decoded_total = 0u64;
    for (i, file) in archive.files.iter().enumerate() {
        if file.is_anti_item
            || archive.is_file_encrypted(i)
            || (file.has_stream && !file.has_crc)
            || (!file.has_stream && file.size != 0)
        {
            return Err(invalid("dependent, encrypted or unchecked entry"));
        }
        decoded_total = decoded_total
            .checked_add(file.size)
            .filter(|&n| n <= MAX_DECODED)
            .ok_or_else(|| invalid("decoded size limit"))?;
    }
    let block_total = archive
        .blocks
        .iter()
        .try_fold(0u64, |n, b| n.checked_add(b.get_unpack_size()));
    if block_total != Some(decoded_total) {
        return Err(invalid("decoded block and file lengths disagree"));
    }
    charge(budget, decoded_total as usize)?;
    let mut count = 0;
    let expected_count = archive.files.len();
    let mut scratch = [0u8; 32 * 1024];
    let result = reader.for_each_entries(|entry, stream| {
        let mut digest = crc32fast::Hasher::new();
        let mut size = 0u64;
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(std::io::Error::other("7z validation cancelled").into());
            }
            let n = stream.read(&mut scratch)?;
            if n == 0 {
                break;
            }
            size += n as u64;
            if size > entry.size {
                return Err(sevenz_rust2::Error::ChecksumVerificationFailed);
            }
            digest.update(&scratch[..n]);
        }
        if size != entry.size || (entry.has_crc && u64::from(digest.finalize()) != entry.crc) {
            return Err(sevenz_rust2::Error::ChecksumVerificationFailed);
        }
        count += 1;
        Ok(true)
    });
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if result.is_err() || count != expected_count {
        return Err(invalid("entry data, CRC or unsupported compression"));
    }
    Ok(bytes)
}
