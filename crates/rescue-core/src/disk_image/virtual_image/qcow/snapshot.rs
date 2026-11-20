//! Read the internal snapshot directory without changing the active disk.
use super::{COPIED, MAX_TABLE_BYTES, OFFSET, Regions, be32, be64, invalid, read_at};
use crate::{Error, Result};
use serde::Serialize;
use std::{
    collections::{BTreeMap, HashSet},
    fs::File,
    sync::atomic::{AtomicBool, Ordering},
};

#[derive(Debug, Clone, Serialize)]
pub struct Qcow2Snapshot {
    pub id: String,
    pub name: String,
    pub guest_bytes: u64,
    pub vm_state_bytes: u64,
    #[serde(skip)]
    pub(super) l1_offset: u64,
    #[serde(skip)]
    pub(super) l1_count: u32,
}

#[derive(Clone, Copy)]
pub enum Qcow2SnapshotSelector<'a> {
    Id(&'a str),
    Name(&'a str),
}

impl Qcow2SnapshotSelector<'_> {
    pub(super) fn find(self, snapshots: &[Qcow2Snapshot]) -> Result<&Qcow2Snapshot> {
        let mut matches = snapshots.iter().filter(|snapshot| match self {
            Self::Id(value) => snapshot.id == value,
            Self::Name(value) => snapshot.name == value,
        });
        let selected = matches
            .next()
            .ok_or_else(|| Error::Request("QCOW2 snapshot not found".into()))?;
        if matches.next().is_some() {
            return Err(Error::Request(
                "ambiguous QCOW2 snapshot name; select by ID".into(),
            ));
        }
        Ok(selected)
    }
}

pub(super) fn directory(
    source: &mut File,
    header: &[u8],
    cluster: u64,
    length: u64,
    metadata: &mut Regions,
) -> Result<Vec<Qcow2Snapshot>> {
    let count = be32(header, 60);
    let start = be64(header, 64);
    if count == 0 && start == 0 {
        return Ok(Vec::new());
    }
    if count > 256 {
        return Err(Error::Limit("QCOW2 internal snapshot count".into()));
    }
    if count == 0 || start == 0 || !start.is_multiple_of(cluster) || start >= length {
        return Err(invalid("QCOW2 snapshot directory offset or count"));
    }
    let mut offset = start;
    let mut snapshots = Vec::with_capacity(count as usize);
    let mut ids = HashSet::new();
    for _ in 0..count {
        let mut fixed = [0; 40];
        read_at(source, offset, &mut fixed)?;
        let id_len = u16::from_be_bytes([fixed[12], fixed[13]]) as usize;
        let name_len = u16::from_be_bytes([fixed[14], fixed[15]]) as usize;
        let extra = u64::from(be32(&fixed, 36));
        if id_len == 0 || (be32(header, 4) == 3 && extra < 16) {
            return Err(invalid("QCOW2 snapshot ID or extra data length"));
        }
        let used = 40 + extra + (id_len + name_len) as u64;
        let end = offset
            .checked_add(used)
            .filter(|end| *end <= length && end - start <= 16 << 20)
            .ok_or_else(|| invalid("QCOW2 snapshot directory exceeds source or limit"))?;
        let mut fields = [0; 16];
        let known = extra.min(fields.len() as u64) as usize;
        read_at(source, offset + 40, &mut fields[..known])?;
        let vm_bytes = if extra >= 8 {
            be64(&fields, 0)
        } else {
            u64::from(be32(&fixed, 32))
        };
        let guest_bytes = if extra >= 16 {
            be64(&fields, 8)
        } else {
            be64(header, 24)
        };
        let mut strings = vec![0; id_len + name_len];
        read_at(source, offset + 40 + extra, &mut strings)?;
        let decode = |bytes: &[u8]| -> Result<String> {
            std::str::from_utf8(bytes)
                .ok()
                .filter(|text| !text.chars().any(char::is_control))
                .map(str::to_owned)
                .ok_or_else(|| invalid("invalid QCOW2 snapshot ID or name"))
        };
        let id = decode(&strings[..id_len])?;
        if !ids.insert(id.clone()) {
            return Err(invalid("duplicate QCOW2 snapshot ID"));
        }
        let l1_count = be32(&fixed, 8);
        if guest_bytes == 0 || u64::from(l1_count) * 8 > MAX_TABLE_BYTES {
            return Err(invalid("QCOW2 snapshot virtual size or L1 table size"));
        }
        snapshots.push(Qcow2Snapshot {
            id,
            name: decode(&strings[id_len..])?,
            guest_bytes,
            vm_state_bytes: vm_bytes,
            l1_offset: be64(&fixed, 0),
            l1_count,
        });
        if snapshots.len() == count as usize {
            metadata.add(start, end - start, cluster, length)?;
        } else {
            offset = end
                .checked_next_multiple_of(8)
                .ok_or_else(|| invalid("QCOW2 snapshot directory overflow"))?;
        }
    }
    Ok(snapshots)
}

/// Saved machine memory begins at the first L1 slot after the disk. Its
/// physical data clusters must never be exported through a guest L2 entry.
#[derive(Default)]
pub(super) struct SavedMemory {
    // Each table starts on an L1 boundary; generations may share that table.
    tables: BTreeMap<u64, u64>,
}

impl SavedMemory {
    pub fn record(
        &mut self,
        snapshot: &Qcow2Snapshot,
        l1: &[u8],
        cluster: u64,
        entry_bytes: usize,
    ) -> Result<()> {
        let table_coverage = cluster * (cluster / entry_bytes as u64);
        let first = snapshot.guest_bytes.div_ceil(table_coverage);
        let count = snapshot.vm_state_bytes.div_ceil(table_coverage);
        if count > (l1.len() / 8) as u64 || first + count > (l1.len() / 8) as u64 {
            return Err(invalid("QCOW2 saved VM state exceeds its L1 table"));
        }
        let mut remaining = snapshot.vm_state_bytes;
        for index in first..first + count {
            let table = be64(l1, index as usize * 8) & OFFSET;
            if table == 0 {
                return Err(invalid("missing QCOW2 saved VM state L2 table"));
            }
            let bytes = remaining.min(table_coverage);
            let used = self.tables.entry(table).or_insert(0);
            *used = (*used).max(bytes);
            remaining -= bytes;
        }
        Ok(())
    }

    pub fn protect(
        &self,
        source: &mut File,
        metadata: &mut Regions,
        cluster: u64,
        entry_bytes: usize,
        length: u64,
        cancel: &AtomicBool,
    ) -> Result<()> {
        if self.tables.is_empty() {
            return Ok(());
        }
        if self.tables.len() as u64 * cluster > 128 << 20 {
            return Err(Error::Limit("QCOW2 saved VM state mapping tables".into()));
        }
        let mut table = vec![0; cluster as usize];
        let mut protected = HashSet::new();
        for (&at, &bytes) in &self.tables {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            read_at(source, at, &mut table)?;
            let mut remaining = bytes;
            for entry in table
                .chunks_exact(entry_bytes)
                .take(bytes.div_ceil(cluster) as usize)
            {
                let map = be64(entry, 0);
                let host = map & OFFSET;
                if map & !(COPIED | OFFSET) != 0 || host == 0 || !host.is_multiple_of(cluster) {
                    return Err(invalid(
                        "QCOW2 saved VM state requires allocated uncompressed clusters",
                    ));
                }
                let used = remaining.min(cluster);
                let host_bytes = if entry_bytes == 16 {
                    let bits = be64(entry, 8);
                    let allocated = bits as u32;
                    let zeros = (bits >> 32) as u32;
                    let subcluster = cluster / 32;
                    if allocated & zeros != 0
                        || u64::from(allocated.trailing_ones()) * subcluster < used
                    {
                        return Err(invalid("unallocated QCOW2 saved VM state subcluster"));
                    }
                    u64::from(32 - allocated.leading_zeros()) * subcluster
                } else {
                    cluster
                };
                // A shared RAM cluster may have already been protected in another generation.
                // Always validate the length required by this entry before reusing it.
                if host.checked_add(host_bytes).is_none_or(|end| end > length) {
                    return Err(invalid("truncated QCOW2 saved VM state cluster"));
                }
                if protected.insert(host) {
                    metadata.add(host, host_bytes, cluster, length)?;
                }
                remaining -= used;
            }
        }
        Ok(())
    }
}
