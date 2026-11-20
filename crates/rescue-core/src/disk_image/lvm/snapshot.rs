//! Linux dm-snapshot persistent exception store, version 1.
use super::{Error, Result, invalid, layout::Data, pv::Physical};
use serde::Serialize;
use std::{
    collections::{HashMap, HashSet},
    sync::atomic::{AtomicBool, Ordering},
};

/// The visible snapshot's name and UUID belong to its COW LV in the on-disk LVM layout.
#[derive(Clone, Serialize)]
pub struct SnapshotInfo {
    pub origin: String,
    pub origin_uuid: String,
    pub chunk_bytes: u64,
    pub cow_bytes: u64,
}

pub(super) struct History {
    pub cow: Data,
    pub chunk: u64,
}
pub(super) struct Exceptions {
    by_origin: HashMap<u64, u64>,
}

impl History {
    pub fn load(
        &self,
        disks: &mut [Physical],
        origin_len: u64,
        cancel: &AtomicBool,
    ) -> Result<Exceptions> {
        let mut header = [0u8; 16];
        self.cow.read(disks, 0, &mut header)?;
        let fields: [[u8; 4]; 4] = header.as_chunks::<4>().0.try_into().unwrap();
        let [magic, valid, version, sectors] = fields.map(u32::from_le_bytes);
        if magic != 0x70416e53
            || valid != 1
            || version != 1
            || u64::from(sectors) * 512 != self.chunk
        {
            return Err(invalid(
                "invalid, overflowed or unsupported persistent snapshot header",
            ));
        }
        let slots = self.chunk / 16;
        let origin_chunks = origin_len.div_ceil(self.chunk);
        let cow_chunks = self.cow.bytes / self.chunk;
        let mut by_origin = HashMap::new();
        let mut used = HashSet::new();
        let mut area = 1u64;
        let mut page = vec![0; self.chunk as usize];
        while area < cow_chunks {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            self.cow.read(disks, area * self.chunk, &mut page)?;
            for (i, pair) in page.as_chunks::<16>().0.iter().enumerate() {
                let old = u64::from_le_bytes(pair[..8].try_into().unwrap());
                let new = u64::from_le_bytes(pair[8..].try_into().unwrap());
                if new == 0 {
                    if old != 0 || page[(i + 1) * 16..].iter().any(|&b| b != 0) {
                        return Err(invalid("nonempty entries after snapshot terminator"));
                    }
                    return Ok(Exceptions { by_origin });
                }
                if old >= origin_chunks
                    || new >= cow_chunks
                    || new < 2
                    || (new - 1).is_multiple_of(slots + 1)
                {
                    return Err(invalid(
                        "snapshot exception points outside data or into its metadata",
                    ));
                }
                if by_origin.len() >= 1_048_576 {
                    return Err(Error::Limit(
                        "LVM snapshot exceeds 1,048,576 exceptions".into(),
                    ));
                }
                if by_origin.insert(old, new).is_some() || !used.insert(new) {
                    return Err(invalid("duplicate origin or storage chunk in snapshot"));
                }
            }
            area = area
                .checked_add(slots + 1)
                .ok_or_else(|| invalid("exception area overflow"))?;
        }
        Err(invalid("missing snapshot exception terminator"))
    }
}

impl Exceptions {
    pub fn read(
        &self,
        disks: &mut [Physical],
        base: &Data,
        history: &History,
        mut pos: u64,
        mut output: &mut [u8],
    ) -> Result<()> {
        while !output.is_empty() {
            let within = pos % history.chunk;
            let count = output.len().min((history.chunk - within) as usize);
            let (part, rest) = output.split_at_mut(count);
            if let Some(&stored) = self.by_origin.get(&(pos / history.chunk)) {
                history
                    .cow
                    .read(disks, stored * history.chunk + within, part)?;
            } else {
                base.read(disks, pos, part)?;
            }
            output = rest;
            pos += count as u64;
        }
        Ok(())
    }
}
