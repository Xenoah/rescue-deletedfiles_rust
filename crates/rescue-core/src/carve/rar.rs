//! Determine the end of one contiguous archive and test decoded members without extracting paths.
use super::container::Input;
use crate::{Error, Result, source::Image};
use rars::{ArchiveMemberDetail, ArchiveMemberHash, ArchiveReadOptions, ArchiveReader};
use std::sync::atomic::{AtomicBool, Ordering};

const RAR4: &[u8] = b"Rar!\x1a\x07\x00";
const RAR5: &[u8] = b"Rar!\x1a\x07\x01\x00";
const MAX_HEADERS: usize = 4096;
const MAX_DECODED: u64 = 64 << 20;

pub(super) fn signature(bytes: &[u8]) -> bool {
    bytes.starts_with(RAR4) || bytes.starts_with(RAR5)
}
fn invalid() -> Error {
    Error::Invalid("incomplete, unsupported or inconsistent RAR archive".into())
}

fn vint(header: &[u8], pos: &mut usize) -> Result<u64> {
    let mut value = 0;
    for shift in (0..64).step_by(7) {
        let byte = *header.get(*pos).ok_or_else(invalid)?;
        *pos += 1;
        if shift == 63 && byte > 1 {
            return Err(invalid());
        }
        value |= u64::from(byte & 127) << shift;
        if byte < 128 {
            return Ok(value);
        }
    }
    Err(invalid())
}

// RAR 4 carries a CRC16 (the low 16 bits of CRC32) and an optional packed
// data length after its fixed prefix. Locate the end record without searching
// through compressed payloads that may contain another signature or end marker.
fn end4(input: &mut Input<'_>, comment: &mut Option<(u32, u32)>) -> Result<usize> {
    let mut pos = RAR4.len();
    for i in 0..MAX_HEADERS {
        input.ensure(pos + 7)?;
        let fixed = &input.bytes[pos..pos + 7];
        let crc = u16::from_le_bytes([fixed[0], fixed[1]]);
        let kind = fixed[2];
        let flags = u16::from_le_bytes([fixed[3], fixed[4]]);
        let size = usize::from(u16::from_le_bytes([fixed[5], fixed[6]]));
        if size < 7 || (i == 0) != (kind == 0x73) {
            return Err(invalid());
        }
        let end = pos + size;
        input.ensure(end)?;
        let hdr = &input.bytes[pos..end];
        if crc32fast::hash(&hdr[2..]) as u16 != crc {
            return Err(invalid());
        }
        pos = match kind {
            // Accept single-volume archives, including solid and locked sets.
            0x73 if flags & !0x000c == 0 => end,
            // Large files, split members, encrypted data and comments require
            // unsupported layouts or exceed our encoded/decoded byte limits.
            0x74 if flags & 0x8000 != 0 && flags & 0x010f == 0 && size >= 32 => {
                let packed = u32::from_le_bytes(hdr[7..11].try_into().unwrap());
                end.checked_add(packed as usize)
                    .filter(|&p| p <= input.limit())
                    .ok_or_else(invalid)?
            }
            0x7a if i == 1 && flags == 0x8000 && size == 35 && &hdr[32..35] == b"CMT" => {
                let packed = u32::from_le_bytes(hdr[7..11].try_into().unwrap());
                let unpacked = u32::from_le_bytes(hdr[11..15].try_into().unwrap());
                let crc = u32::from_le_bytes(hdr[16..20].try_into().unwrap());
                if unpacked > 1 << 20 || packed > 1 << 20 || hdr[26..28] != [3, 0] {
                    return Err(invalid());
                }
                *comment = Some((unpacked, crc));
                end.checked_add(packed as usize)
                    .filter(|&p| p <= input.limit())
                    .ok_or_else(invalid)?
            }
            0x7b if flags & !0x4000 == 0 && size == 7 => return Ok(end),
            _ => return Err(invalid()),
        };
    }
    Err(invalid())
}

// Quick-open stores exact copies of earlier headers. RAR5 permits these records
// without a file-level checksum, so compare each cache record to the bytes it names.
fn quick_open(
    input: &mut Input<'_>,
    start: usize,
    data: std::ops::Range<usize>,
    headers: &[(usize, usize)],
) -> Result<()> {
    input.ensure(data.end)?;
    let bytes = &input.bytes[..data.end];
    let mut pos = data.start;
    for _ in 0..MAX_HEADERS {
        if pos == data.end {
            return Ok(());
        }
        let crc = bytes.get(pos..pos + 4).ok_or_else(invalid)?;
        pos += 4;
        let from = pos;
        let size = vint(bytes, &mut pos)?;
        if pos - from > 3 {
            return Err(invalid());
        }
        let end = pos
            .checked_add(usize::try_from(size).map_err(|_| invalid())?)
            .filter(|&e| e <= data.end)
            .ok_or_else(invalid)?;
        let record = &bytes[..end];
        if crc32fast::hash(&record[from..]).to_le_bytes() != crc || vint(record, &mut pos)? != 0 {
            return Err(invalid());
        }
        let offset = usize::try_from(vint(record, &mut pos)?).map_err(|_| invalid())?;
        let reference = start.checked_sub(offset).ok_or_else(invalid)?;
        let cache_size = usize::try_from(vint(record, &mut pos)?).map_err(|_| invalid())?;
        let original = headers
            .binary_search_by_key(&reference, |&(at, _)| at)
            .map_err(|_| invalid())?;
        let (at, until) = headers[original];
        if cache_size != end - pos || record[at..until] != record[pos..end] {
            return Err(invalid());
        }
        pos = end;
    }
    Err(invalid())
}

fn end5(input: &mut Input<'_>, comment: &mut Option<(u32, u32)>) -> Result<usize> {
    let mut pos = RAR5.len();
    let mut headers = Vec::new();
    let mut seen_quick_open = false;
    for i in 0..MAX_HEADERS {
        // Read only the available length bytes so an end header at the last
        // readable sector can still be verified without reading beyond it.
        input.ensure(pos + 5)?;
        let mut size_pos = pos + 4;
        while input.bytes[size_pos] & 128 != 0 {
            size_pos += 1;
            if size_pos >= pos + 7 {
                return Err(invalid());
            }
            input.ensure(size_pos + 1)?;
        }
        let mut body = pos + 4;
        let size = vint(&input.bytes, &mut body)? as usize;
        if !(3..=1_048_576).contains(&size) {
            return Err(invalid());
        }
        let end = body + size;
        input.ensure(end)?;
        let header = &input.bytes[..end];
        let crc = u32::from_le_bytes(header[pos..pos + 4].try_into().unwrap());
        if crc != crc32fast::hash(&header[pos + 4..]) {
            return Err(invalid());
        }
        let kind = vint(header, &mut body)?;
        let flags = vint(header, &mut body)?;
        if (i == 0) != (kind == 1) || flags & !7 != 0 {
            return Err(invalid());
        }
        let extra = if flags & 1 != 0 {
            vint(header, &mut body)?
        } else {
            0
        };
        let packed = if flags & 2 != 0 {
            vint(header, &mut body)?
        } else {
            0
        };
        if extra > (end - body) as u64 {
            return Err(invalid());
        }
        let next = end
            .checked_add(usize::try_from(packed).map_err(|_| invalid())?)
            .filter(|&p| p <= input.limit())
            .ok_or_else(invalid)?;
        match kind {
            1 if packed == 0 => (),
            2 if !seen_quick_open => headers.push((pos, end)),
            3 if i == 1 && flags == 2 && extra == 0 && packed <= 1 << 20 => {
                if vint(header, &mut body)? != 4
                    || vint(header, &mut body)? != packed
                    || vint(header, &mut body)? != 0
                {
                    return Err(invalid());
                }
                let crc = u32::from_le_bytes(
                    header
                        .get(body..body + 4)
                        .ok_or_else(invalid)?
                        .try_into()
                        .unwrap(),
                );
                body += 4;
                if vint(header, &mut body)? != 0
                    || vint(header, &mut body)? > 1
                    || vint(header, &mut body)? != 3
                    || &header[body..] != b"CMT"
                {
                    return Err(invalid());
                }
                *comment = Some((packed as u32, crc));
            }
            3 if !seen_quick_open && flags == 6 && extra == 0 && packed > 0 => {
                if vint(header, &mut body)? != 0
                    || vint(header, &mut body)? != packed
                    || vint(header, &mut body)? != 0
                    || vint(header, &mut body)? != 0
                    || vint(header, &mut body)? > 1
                    || vint(header, &mut body)? != 2
                    || &header[body..] != b"QO"
                {
                    return Err(invalid());
                }
                quick_open(input, pos, end..next, &headers)?;
                seen_quick_open = true;
            }
            5 if extra == 0 && packed == 0 && vint(header, &mut body)? == 0 && body == end => {
                return Ok(end);
            }
            _ => return Err(invalid()),
        }
        pos = next;
    }
    Err(invalid())
}

pub(super) fn validate(
    image: &mut Image,
    at: u64,
    boundary: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
) -> Result<Vec<u8>> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    input.ensure(8)?;
    let mut comment = None;
    let end = if input.bytes.starts_with(RAR4) {
        end4(&mut input, &mut comment)?
    } else if input.bytes.starts_with(RAR5) {
        end5(&mut input, &mut comment)?
    } else {
        return Err(invalid());
    };
    let options = ArchiveReadOptions::new()
        .with_max_header_count(MAX_HEADERS as u64)
        .with_max_header_bytes(4 << 20)
        .with_max_member_output_bytes(MAX_DECODED)
        .with_max_total_output_bytes(MAX_DECODED)
        .with_max_reader_workspace_bytes(128 << 20)
        .with_rar50_dictionary_size_limit(32 << 20)
        .with_rar50_buffered_decode_limit(4 << 20);
    let archive =
        ArchiveReader::read_with_options(&input.bytes[..end], options).map_err(|_| invalid())?;
    if archive.is_volume() {
        return Err(invalid());
    }
    let mut files = 0;
    let mut total = 0u64;
    for member in archive.members() {
        let m = &member.meta;
        if m.is_redirection || m.is_split_before || m.is_split_after || m.is_encrypted {
            return Err(invalid());
        }
        if m.is_directory {
            if m.unpacked_size != 0 || m.packed_size != 0 {
                return Err(invalid());
            }
            continue;
        }
        if !matches!(
            member.detail,
            ArchiveMemberDetail::Rar15To40 { .. }
                | ArchiveMemberDetail::Rar50Plus { crc32: Some(_), .. }
                | ArchiveMemberDetail::Rar50Plus {
                    hash: Some(ArchiveMemberHash::Blake2sp(_)),
                    ..
                }
        ) {
            return Err(invalid());
        }
        files += 1;
        total = total
            .checked_add(m.unpacked_size)
            .filter(|&s| s <= MAX_DECODED)
            .ok_or_else(invalid)?;
    }
    if files == 0 {
        return Err(invalid());
    }
    if let Some((len, expected)) = comment {
        input.work(len as usize)?;
        let decoded = archive
            .comment_with_options(options)
            .map_err(|_| invalid())?
            .ok_or_else(invalid)?;
        if decoded.len() != len as usize || crc32fast::hash(&decoded) != expected {
            return Err(invalid());
        }
    }
    input.work(total as usize)?;
    let tested = archive.test_with_options(options);
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    tested.map_err(|_| invalid())?;
    input.finish(end)
}
