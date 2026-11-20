//! Read the internal snapshot directory without changing the active disk.
use super::{MAX_TABLE_BYTES, Regions, be32, be64, invalid, read_at};
use crate::{Error, Result};
use serde::Serialize;
use std::{collections::HashSet, fs::File};

#[derive(Debug, Clone, Serialize)]
pub struct Qcow2Snapshot {
    pub id: String,
    pub name: String,
    pub guest_bytes: u64,
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
        if vm_bytes != 0 {
            return Err(Error::Unsupported(
                "QCOW2 snapshots with saved VM memory".into(),
            ));
        }
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
