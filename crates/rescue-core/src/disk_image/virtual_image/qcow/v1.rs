//! Original (version 1) QCOW images with no encryption or backing file.
use super::{
    Block, Compression, Error, File, MAX_METADATA_REGIONS, MAX_TABLE_BYTES, Regions, Result, be32,
    be64, invalid, read_at,
};
use std::sync::atomic::{AtomicBool, Ordering};

const HEADER: usize = 48;
const COMPRESSED: u64 = 1 << 63;

pub(in crate::disk_image::virtual_image) struct Legacy {
    pub size: u64,
    cluster: u64,
    length: u64,
    l1: Vec<u8>,
    l2: Vec<u8>,
    loaded: Option<u64>,
    occupied: Regions,
}

impl Regions {
    // QCOW v1 has no refcounts. Register each physical range once during sequential export.
    // Adjacent ranges can share a map entry, including tightly packed compressed streams.
    fn claim_v1(&mut self, start: u64, bytes: u64, length: u64) -> Result<()> {
        let end = start
            .checked_add(bytes)
            .filter(|end| *end <= length)
            .ok_or_else(|| invalid("QCOW v1 range outside host file"))?;
        if bytes == 0 || self.overlaps(start, end) {
            return Err(invalid("empty or overlapping QCOW v1 allocation"));
        }
        let previous = self
            .0
            .range(..start)
            .next_back()
            .and_then(|(&a, &b)| (b == start).then_some(a));
        let merged_end = self.0.remove(&end).unwrap_or(end);
        if let Some(a) = previous {
            self.0.insert(a, merged_end);
        } else {
            if self.0.len() >= MAX_METADATA_REGIONS {
                return Err(Error::Limit(
                    "too many disjoint QCOW v1 physical ranges".into(),
                ));
            }
            self.0.insert(start, merged_end);
        }
        Ok(())
    }
}

impl Legacy {
    pub fn open(source: &mut File, length: u64, cancel: &AtomicBool) -> Result<Self> {
        let mut header = [0; HEADER];
        read_at(source, 0, &mut header)?;
        if &header[..4] != b"QFI\xfb" || be32(&header, 4) != 1 {
            return Err(invalid("QCOW v1 header"));
        }
        if be64(&header, 8) != 0 || be32(&header, 16) != 0 || be32(&header, 36) != 0 {
            return Err(Error::Unsupported(
                "QCOW v1 requires an unencrypted image with no backing file".into(),
            ));
        }
        let size = be64(&header, 24);
        let bits = header[32];
        let l2_bits = header[33];
        if size == 0
            || !size.is_multiple_of(512)
            || !(9..=16).contains(&bits)
            || !(6..=13).contains(&l2_bits)
            || header[34..36] != [0; 2]
        {
            return Err(invalid("QCOW v1 virtual size or table geometry"));
        }
        let cluster = 1u64 << bits;
        let per_table = 1u64 << l2_bits;
        let l1_bytes = size.div_ceil(cluster * per_table) * 8;
        if l1_bytes > MAX_TABLE_BYTES {
            return Err(Error::Limit("QCOW v1 L1 table exceeds 32 MiB".into()));
        }
        let l1_at = be64(&header, 40);
        if !l1_at.is_multiple_of(8) {
            return Err(invalid("unaligned QCOW v1 L1 table"));
        }
        let mut occupied = Regions::default();
        occupied.claim_v1(0, HEADER as u64, length)?;
        occupied.claim_v1(l1_at, l1_bytes, length)?;
        let mut l1 = vec![0; l1_bytes as usize];
        read_at(source, l1_at, &mut l1)?;
        for (index, entry) in l1.as_chunks::<8>().0.iter().enumerate() {
            if index.is_multiple_of(8192) && cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let at = u64::from_be_bytes(*entry);
            if at != 0 {
                if !at.is_multiple_of(512) {
                    return Err(invalid("unaligned QCOW v1 L2 table"));
                }
                occupied.claim_v1(at, per_table * 8, length)?;
            }
        }
        Ok(Self {
            size,
            cluster,
            length,
            l1,
            l2: vec![0; per_table as usize * 8],
            loaded: None,
            occupied,
        })
    }

    pub fn block(&mut self, source: &mut File, guest: u64) -> Result<(Block, u64)> {
        let per_table = self.l2.len() as u64 / 8;
        let index = guest / self.cluster;
        let l1_index = index / per_table;
        let table = be64(&self.l1, l1_index as usize * 8);
        if table == 0 {
            return Ok((Block::Zero, self.cluster * (per_table - index % per_table)));
        }
        if self.loaded != Some(table) {
            read_at(source, table, &mut self.l2)?;
            self.loaded = Some(table);
            let used = (self.size.div_ceil(self.cluster) - l1_index * per_table).min(per_table);
            if self.l2[used as usize * 8..].iter().any(|&b| b != 0) {
                return Err(invalid("QCOW v1 mapping beyond virtual disk"));
            }
        }
        let entry = be64(&self.l2, (index % per_table) as usize * 8);
        let block = if entry == 0 {
            Block::Zero
        } else if entry & COMPRESSED != 0 {
            // Unlike QCOW2, v1 stores the exact compressed byte length in the descriptor.
            let shift = 63 - self.cluster.trailing_zeros();
            let at = entry & ((1u64 << shift) - 1);
            let bytes = (entry >> shift) & (self.cluster - 1);
            self.occupied.claim_v1(at, bytes, self.length)?;
            Block::Compressed {
                at,
                bytes: bytes as usize,
                codec: Compression::QcowV1Deflate,
            }
        } else {
            if !entry.is_multiple_of(512) {
                return Err(invalid("unaligned QCOW v1 data cluster"));
            }
            self.occupied.claim_v1(entry, self.cluster, self.length)?;
            Block::Data(entry)
        };
        Ok((block, self.cluster))
    }
}
