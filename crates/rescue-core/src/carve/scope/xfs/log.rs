//! Locate the head by checking the cycle stamp of every basic block. An older
//! clean marker cannot override newer recorded sectors or an unreadable gap.
use super::{be32, be64, checksum, invalid};
use crate::{Error, Image, Result, crc32c::update};
use std::sync::atomic::{AtomicBool, Ordering};

const MAGIC: u32 = 0xfeed_babe;

pub(super) fn clean_lsn(
    image: &mut Image,
    start: u64,
    bytes: u64,
    uuid: &[u8],
    cancel: &AtomicBool,
) -> Result<u64> {
    if !(512..=512 << 20).contains(&bytes) || !bytes.is_multiple_of(512) {
        return Err(Error::Unsupported("XFS internal log size".into()));
    }
    let blocks = bytes / 512;
    let mut current = None;
    let mut head = blocks;
    let mut last = None;
    let mut previous = None;
    let mut offset = 0;
    while offset < bytes {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let chunk = image.read(start + offset, (bytes - offset).min(1 << 20) as usize)?;
        for (i, data) in chunk.as_chunks::<512>().0.iter().enumerate() {
            let pos = offset / 512 + i as u64;
            let marker = be32(data, 0);
            let is_header = marker == MAGIC;
            let cycle = if is_header { be32(data, 4) } else { marker };
            let active = *current.get_or_insert(cycle);
            if active == 0 {
                return Err(invalid("empty or wrapped log cycle number"));
            }
            if cycle == active && head == blocks {
                if is_header {
                    if be64(data, 16) != (u64::from(cycle) << 32 | pos) || data[304..320] != *uuid {
                        return Err(invalid("journal record position or UUID"));
                    }
                    previous = last.replace(pos);
                }
            } else {
                if cycle != active - 1 {
                    return Err(invalid("inconsistent journal cycles"));
                }
                head = head.min(pos);
            }
        }
        offset += chunk.len() as u64;
    }
    let pos = last.ok_or_else(|| invalid("no active unmount record"))?;
    let header = image.read(start + pos * 512, 512)?;
    let data_len = be32(&header, 12) as usize;
    let buffer_size = be32(&header, 320);
    let format = be32(&header, 300);
    if be32(&header, 8) != 2
        || be32(&header, 40) != 1
        || !(1..=2).contains(&format)
        || !(16384..=262144).contains(&buffer_size)
        || !buffer_size.is_power_of_two()
        || !(512..=buffer_size as usize).contains(&data_len)
        || !data_len.is_multiple_of(512)
    {
        return Err(invalid("journal needs replay or has unsupported geometry"));
    }
    let lsn = be64(&header, 16);
    let cycle = be32(&header, 4);
    let prev = u64::from(be32(&header, 36));
    let tail = be64(&header, 24);
    let tail_pos = tail & u64::from(u32::MAX);
    let log_lsn = |block: u64| (u64::from(cycle - u32::from(block > pos)) << 32) | block;
    if prev >= blocks
        || prev == pos
        || previous.is_some_and(|p| p != prev)
        || tail_pos >= blocks
        || tail == 0
        || tail > lsn
        || tail != log_lsn(tail_pos)
    {
        return Err(invalid("journal predecessor or tail cursor"));
    }
    for cursor in [prev, tail_pos] {
        let record = image.read(start + cursor * 512, 512)?;
        if be32(&record, 0) != MAGIC
            || be32(&record, 8) != 2
            || be64(&record, 16) != log_lsn(cursor)
            || record[304..320] != *uuid
        {
            return Err(invalid("journal cursor does not identify a record"));
        }
        if cursor == prev {
            let size = be32(&record, 320);
            let len = be32(&record, 12);
            if !(16384..=262144).contains(&size)
                || !size.is_power_of_two()
                || len == 0
                || len > size
                || !len.is_multiple_of(512)
                || (prev + u64::from(size.div_ceil(32768) + len / 512)) % blocks != pos
            {
                return Err(invalid(
                    "journal predecessor is not contiguous with unmount",
                ));
            }
        }
    }
    let headers = u64::from(buffer_size.div_ceil(32768));
    if pos + headers + data_len as u64 / 512 != head {
        return Err(invalid("unmount record does not end at journal head"));
    }
    let data = image.read(start + (pos + headers) * 512, data_len)?;
    if data
        .as_chunks::<512>()
        .0
        .iter()
        .any(|r| be32(r, 0) != cycle)
        || !matches!(be32(&data, 4), 0 | 8)
        || data[8..12] != [0xaa, 0x20, 0, 0]
        || data[12..14]
            != if format == 1 {
                [0x6e, 0x55]
            } else {
                [0x55, 0x6e]
            }
    {
        return Err(invalid("journal head is not a complete unmount operation"));
    }
    // The kernel checksum includes the packed payload and the on-disk C header,
    // whose trailing alignment padding differs between 32- and 64-bit producers.
    let extra_bytes = (data_len.div_ceil(32768) - 1) * 512;
    let extra = image.read(start + (pos + 1) * 512, extra_bytes)?;
    let stored = u32::from_le_bytes(header[32..36].try_into().unwrap());
    if ![324, 328].iter().any(|&h| {
        let mut crc = checksum(&header[..h], 32);
        for block in extra.as_chunks::<512>().0 {
            crc = update(crc, &block[..260]);
        }
        !update(crc, &data) == stored
    }) {
        return Err(invalid("journal unmount checksum"));
    }
    Ok(lsn)
}
