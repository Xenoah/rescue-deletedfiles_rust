//! Read-only QCOW2 v2/v3 base images. Only metadata inside the selected file is read.
use super::{Block, invalid, read_at};
use rescue_core::{Error, Result};
use std::{
    collections::BTreeMap,
    fs::File,
    sync::atomic::{AtomicBool, Ordering},
};

const OFFSET: u64 = 0x00ff_ffff_ffff_fe00;
const COPIED: u64 = 1 << 63;
const COMPRESSED: u64 = 1 << 62;
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
    pub cluster: u64,
    length: u64,
    version: u32,
    l1: Vec<u8>,
    metadata: Regions,
    cached_l2: Option<u64>,
    l2: Vec<u8>,
}

impl Qcow {
    pub fn open(source: &mut File, length: u64, cancel: &AtomicBool) -> Result<Self> {
        let mut header = [0; 104];
        read_at(source, 0, &mut header[..72])?;
        let version = be32(&header, 4);
        if !matches!(version, 2 | 3)
            || header[..4] != *b"QFI\xfb"
            || be64(&header, 8) != 0
            || be32(&header, 16) != 0
            || be32(&header, 32) != 0
            || be32(&header, 60) != 0
            || be64(&header, 64) != 0
        {
            return Err(Error::Unsupported("QCOW2 requires an unencrypted v2/v3 base image without a backing file or internal snapshots".into()));
        }
        let bits = be32(&header, 20);
        if !(9..=21).contains(&bits) {
            return Err(invalid("QCOW2 cluster size"));
        }
        let cluster = 1u64 << bits;
        if version == 3 {
            read_at(source, 72, &mut header[72..])?;
            if be64(&header, 72) != 0
                || !(104..=cluster).contains(&u64::from(be32(&header, 100)))
                || be32(&header, 96) > 6
            {
                return Err(Error::Unsupported(
                    "QCOW2 incompatible features, dirty/corrupt flag or unsupported header".into(),
                ));
            }
        }
        let size = be64(&header, 24);
        let l1_count = u64::from(be32(&header, 36));
        let l1_at = be64(&header, 40);
        let ref_at = be64(&header, 48);
        let ref_bytes = u64::from(be32(&header, 56)) * cluster;
        let required = size.div_ceil(cluster * (cluster / 8));
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
        metadata.add(l1_at, l1_count * 8, cluster, length)?;
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
        let mut l1 = vec![0; (l1_count * 8) as usize];
        read_at(source, l1_at, &mut l1)?;
        for (index, &raw) in l1.as_chunks::<8>().0.iter().enumerate() {
            if index.is_multiple_of(8192) && cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let entry = u64::from_be_bytes(raw);
            if entry != 0 {
                if entry & !(OFFSET | COPIED) != 0 {
                    return Err(invalid("QCOW2 reserved L1 flags"));
                }
                metadata.add(entry & OFFSET, cluster, cluster, length)?;
            }
        }
        Ok(Self {
            size,
            cluster,
            length,
            version,
            l1,
            metadata,
            cached_l2: None,
            l2: vec![0; cluster as usize],
        })
    }
    pub fn block(&mut self, source: &mut File, guest: u64) -> Result<Block> {
        let index = guest / self.cluster;
        let per_l2 = self.cluster / 8;
        let table = be64(&self.l1, (index / per_l2) as usize * 8) & OFFSET;
        if table == 0 {
            return Ok(Block::Zero);
        }
        if self.cached_l2 != Some(table) {
            read_at(source, table, &mut self.l2)?;
            self.cached_l2 = Some(table);
        }
        let entry = be64(&self.l2, (index % per_l2) as usize * 8);
        if entry & COMPRESSED != 0 {
            if entry & COPIED != 0 {
                return Err(invalid("QCOW2 copied bit on compressed cluster"));
            }
            let size_bits = self.cluster.trailing_zeros() - 8;
            let shift = 62 - size_bits;
            let at = entry & ((1u64 << shift) - 1);
            let sectors = ((entry >> shift) & ((1u64 << size_bits) - 1)) + 1;
            let bytes = sectors * 512 - (at % 512);
            self.check_data(at, bytes)?;
            return Ok(Block::Deflate {
                at,
                bytes: bytes as usize,
            });
        }
        let zero = entry & 1 != 0;
        let at = entry & OFFSET;
        if entry & !(COPIED | OFFSET | 1) != 0
            || (zero && self.version == 2)
            || !at.is_multiple_of(self.cluster)
            || (at == 0 && entry & COPIED != 0)
        {
            return Err(invalid("QCOW2 reserved data flags or alignment"));
        }
        if at != 0 {
            self.check_data(at, self.cluster)?;
        }
        if zero || at == 0 {
            Ok(Block::Zero)
        } else {
            Ok(Block::Data(at))
        }
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
