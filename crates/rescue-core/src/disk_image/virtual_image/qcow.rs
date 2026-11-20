//! Read-only QCOW2 v2/v3 metadata and allocation maps.
pub(in crate::disk_image) mod backing;
mod snapshot;
pub(super) mod v1;
use super::{Block, Compression, invalid, read_at};
use crate::{Error, Result};
pub use snapshot::{Qcow2Snapshot, Qcow2SnapshotSelector};
use std::{
    collections::{BTreeMap, HashMap},
    fs::File,
    sync::atomic::{AtomicBool, Ordering},
};

const OFFSET: u64 = 0x00ff_ffff_ffff_fe00;
const COPIED: u64 = 1 << 63;
const COMPRESSED: u64 = 1 << 62;
const COMPRESSION_TYPE: u64 = 1 << 3;
const EXTENDED_L2: u64 = 1 << 4;
const MAX_TABLE_BYTES: u64 = 32 * 1024 * 1024;
const MAX_METADATA_REGIONS: usize = 524288;

fn be32(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
fn be64(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}

#[derive(Default)]
struct Regions(BTreeMap<u64, u64>);
impl Regions {
    fn overlaps(&self, start: u64, end: u64) -> bool {
        self.0
            .range(..end)
            .next_back()
            .is_some_and(|(_, previous_end)| *previous_end > start)
    }
    fn add(&mut self, start: u64, size: u64, cluster: u64, length: u64) -> Result<()> {
        let end = start
            .checked_add(size)
            .ok_or_else(|| invalid("QCOW2 metadata overflow"))?;
        if size == 0 || !start.is_multiple_of(cluster) || end > length || self.overlaps(start, end)
        {
            return Err(invalid(
                "overlapping, unaligned or truncated QCOW2 metadata",
            ));
        }
        if self.0.len() >= MAX_METADATA_REGIONS {
            return Err(Error::Limit("QCOW2 metadata region count".into()));
        }
        self.0.insert(start, end.next_multiple_of(cluster));
        Ok(())
    }
}

pub(super) struct Qcow {
    pub size: u64,
    pub(super) snapshots: Vec<Qcow2Snapshot>,
    backing: Option<backing::Reference>,
    cluster: u64,
    entry_bytes: usize,
    compression: Compression,
    length: u64,
    version: u32,
    l1: Vec<u8>,
    metadata: Regions,
    cached_l2: Option<u64>,
    l2: Vec<u8>,
}

impl Qcow {
    pub fn open(source: &mut File, length: u64, cancel: &AtomicBool) -> Result<Self> {
        Self::open_member(source, length, false, None, cancel)
    }

    pub(super) fn open_member(
        source: &mut File,
        length: u64,
        allow_backing: bool,
        selector: Option<Qcow2SnapshotSelector>,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let mut header = [0; 104];
        read_at(source, 0, &mut header[..72])?;
        let version = be32(&header, 4);
        if !matches!(version, 2 | 3) || header[..4] != *b"QFI\xfb" || be32(&header, 32) != 0 {
            return Err(Error::Unsupported(
                "QCOW2 requires an unencrypted v2/v3 image".into(),
            ));
        }
        let bits = be32(&header, 20);
        if !(9..=21).contains(&bits) {
            return Err(invalid("QCOW2 cluster size"));
        }
        let cluster = 1u64 << bits;
        let mut compression = Compression::QcowDeflate;
        let mut entry_bytes = 8;
        if version == 3 {
            read_at(source, 72, &mut header[72..])?;
            let flags = be64(&header, 72);
            let header_len = u64::from(be32(&header, 100));
            if flags & !(COMPRESSION_TYPE | EXTENDED_L2) != 0
                || !(104..=cluster).contains(&header_len)
                || !header_len.is_multiple_of(8)
                || be32(&header, 96) > 6
            {
                return Err(Error::Unsupported(
                    "QCOW2 incompatible features, dirty/corrupt flag or unsupported header".into(),
                ));
            }
            let mut kind = [0; 1];
            if header_len > 104 {
                read_at(source, 104, &mut kind)?;
            }
            match (flags & COMPRESSION_TYPE != 0, kind[0]) {
                (false, 0) => {}
                (true, 1) => compression = Compression::QcowZstd,
                _ => {
                    return Err(Error::Unsupported(
                        "unknown or inconsistent QCOW2 compression type".into(),
                    ));
                }
            }
            if flags & EXTENDED_L2 != 0 {
                if cluster < 16 * 1024 {
                    return Err(invalid("QCOW2 subclusters smaller than one sector"));
                }
                entry_bytes = 16;
            }
        }
        let backing = backing::reference(source, length, &header, cluster, allow_backing)?;
        let size = be64(&header, 24);
        let l1_count = u64::from(be32(&header, 36));
        let l1_at = be64(&header, 40);
        let ref_at = be64(&header, 48);
        let ref_bytes = u64::from(be32(&header, 56)) * cluster;
        let required = size.div_ceil(cluster * (cluster / entry_bytes as u64));
        if size == 0
            || l1_count < required
            || l1_count * 8 > MAX_TABLE_BYTES
            || ref_bytes == 0
            || ref_bytes > MAX_TABLE_BYTES
        {
            return Err(invalid("QCOW2 virtual size or table size"));
        }
        let mut metadata = Regions::default();
        metadata.add(0, cluster, cluster, length)?;
        let snapshots = snapshot::directory(source, &header, cluster, length, &mut metadata)?;
        if !snapshots.is_empty() && allow_backing {
            return Err(Error::Unsupported(
                "QCOW2 internal snapshots on an external backing chain".into(),
            ));
        }
        let selected = selector.map(|s| s.find(&snapshots)).transpose()?;
        let selected_at = selected.map_or(l1_at, |s| s.l1_offset);
        let size = selected.map_or(size, |s| s.guest_bytes);
        let views = std::iter::once((l1_at, l1_count)).chain(
            snapshots
                .iter()
                .map(|s| (s.l1_offset, u64::from(s.l1_count))),
        );
        let mut total = 0;
        for (at, count) in views.clone() {
            total += count * 8;
            if total > 2 * MAX_TABLE_BYTES {
                return Err(Error::Limit("QCOW2 aggregate L1 table size".into()));
            }
            metadata.add(at, count * 8, cluster, length)?;
        }
        if snapshots.iter().any(|s| {
            u64::from(s.l1_count)
                < s.guest_bytes
                    .div_ceil(cluster * (cluster / entry_bytes as u64))
        }) {
            return Err(invalid("QCOW2 snapshot L1 table does not cover its disk"));
        }
        metadata.add(ref_at, ref_bytes, cluster, length)?;
        let mut refs = vec![0; ref_bytes as usize];
        read_at(source, ref_at, &mut refs)?;
        for (index, &raw) in refs.as_chunks::<8>().0.iter().enumerate() {
            if index.is_multiple_of(8192) && cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let at = u64::from_be_bytes(raw);
            if at != 0 {
                metadata.add(at, cluster, cluster, length)?;
            }
        }
        let mut l1 = Vec::new();
        // Copy-on-write snapshots can share an L2 table at the same guest position.
        // Reserve every view's metadata even when exporting only one generation.
        let mut l2_positions = HashMap::new();
        let mut vm_state = snapshot::SavedMemory::default();
        for (view_index, (at, count)) in views.enumerate() {
            let mut table = vec![0; (count * 8) as usize];
            read_at(source, at, &mut table)?;
            for (index, &raw) in table.as_chunks::<8>().0.iter().enumerate() {
                if index.is_multiple_of(8192) && cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let entry = u64::from_be_bytes(raw);
                if entry == 0 {
                    continue;
                }
                if entry & !(OFFSET | COPIED) != 0 || entry & OFFSET == 0 {
                    return Err(invalid("QCOW2 reserved L1 flags or missing L2 offset"));
                }
                let address = entry & OFFSET;
                match l2_positions.entry(address) {
                    std::collections::hash_map::Entry::Vacant(position) => {
                        metadata.add(address, cluster, cluster, length)?;
                        position.insert(index);
                    }
                    std::collections::hash_map::Entry::Occupied(position)
                        if *position.get() != index =>
                    {
                        return Err(invalid("QCOW2 L2 table reused at different guest offsets"));
                    }
                    _ => {}
                }
            }
            if view_index > 0 && snapshots[view_index - 1].vm_state_bytes != 0 {
                vm_state.record(&snapshots[view_index - 1], &table, cluster, entry_bytes)?;
            }
            if at == selected_at {
                l1 = table;
            }
        }
        vm_state.protect(source, &mut metadata, cluster, entry_bytes, length, cancel)?;
        Ok(Self {
            snapshots,
            backing,
            size,
            cluster,
            length,
            version,
            compression,
            entry_bytes,
            l1,
            metadata,
            cached_l2: None,
            l2: vec![0; cluster as usize],
        })
    }
    pub fn block(&mut self, source: &mut File, guest: u64) -> Result<(Block, u64)> {
        let (block, length) = self.mapping(source, guest)?;
        Ok((block.unwrap_or(Block::Zero), length))
    }

    // The caller supplies a cluster boundary, or a subcluster boundary for extended L2.
    // None inherits from a parent; an explicit zero always masks older data.
    fn mapping(&mut self, source: &mut File, guest: u64) -> Result<(Option<Block>, u64)> {
        let index = guest / self.cluster;
        let per_l2 = self.cluster / self.entry_bytes as u64;
        let table = be64(&self.l1, (index / per_l2) as usize * 8) & OFFSET;
        if table == 0 {
            return Ok((None, self.cluster - guest % self.cluster));
        }
        if self.cached_l2 != Some(table) {
            read_at(source, table, &mut self.l2)?;
            self.cached_l2 = Some(table);
        }
        let position = (index % per_l2) as usize * self.entry_bytes;
        let entry = be64(&self.l2, position);
        let extended = self.entry_bytes == 16;
        let bitmap = if extended {
            be64(&self.l2, position + 8)
        } else {
            0
        };
        if entry & COMPRESSED != 0 {
            if entry & COPIED != 0 || bitmap != 0 {
                return Err(invalid("QCOW2 reserved flags on compressed cluster"));
            }
            let size_bits = self.cluster.trailing_zeros() - 8;
            let shift = 62 - size_bits;
            let at = entry & ((1u64 << shift) - 1);
            let sectors = ((entry >> shift) & ((1u64 << size_bits) - 1)) + 1;
            let bytes = sectors * 512 - (at % 512);
            self.check_data(at, bytes)?;
            return Ok((
                Some(Block::Compressed {
                    codec: self.compression,
                    at,
                    bytes: bytes as usize,
                }),
                self.cluster,
            ));
        }
        let zero = entry & 1 != 0;
        let at = entry & OFFSET;
        if entry & !(COPIED | OFFSET | 1) != 0
            || (zero && (self.version == 2 || extended))
            || !at.is_multiple_of(self.cluster)
            || (at == 0 && entry & COPIED != 0)
        {
            return Err(invalid("QCOW2 reserved data flags or alignment"));
        }
        if extended {
            let allocated = bitmap as u32;
            let zeros = (bitmap >> 32) as u32;
            if allocated & zeros != 0 || (allocated != 0 && at == 0) {
                return Err(invalid(
                    "QCOW2 subcluster bitmap conflicts with cluster allocation",
                ));
            }
            let subcluster = self.cluster / 32;
            if at != 0 {
                // A sparse host file can end immediately after its last allocated subcluster.
                // The entire reserved cluster must still be clear of metadata.
                let reserved_end = at
                    .checked_add(self.cluster)
                    .ok_or_else(|| invalid("QCOW2 cluster overflow"))?;
                if self.metadata.overlaps(at, reserved_end) {
                    return Err(invalid("QCOW2 subclusters overlap metadata"));
                }
                self.check_data(at, u64::from(32 - allocated.leading_zeros()) * subcluster)?;
            }
            let within = guest % self.cluster;
            let sub = (within / subcluster) as u32;
            let bit = 1 << sub;
            let (block, bitmap) = if allocated & bit != 0 {
                (Some(Block::Data(at + within)), allocated)
            } else if zeros & bit != 0 {
                (Some(Block::Zero), zeros)
            } else {
                (None, !(allocated | zeros))
            };
            let count = subcluster * u64::from((bitmap >> sub).trailing_ones());
            return Ok((block, count));
        }
        if at != 0 {
            self.check_data(at, self.cluster)?;
        }
        let block = if zero {
            Some(Block::Zero)
        } else if at != 0 {
            Some(Block::Data(at))
        } else {
            None
        };
        Ok((block, self.cluster))
    }
    fn check_data(&self, at: u64, bytes: u64) -> Result<()> {
        let end = at
            .checked_add(bytes)
            .ok_or_else(|| invalid("QCOW2 offset overflow"))?;
        if end > self.length || self.metadata.overlaps(at, end) {
            return Err(invalid("QCOW2 data outside source or overlapping metadata"));
        }
        Ok(())
    }
}
