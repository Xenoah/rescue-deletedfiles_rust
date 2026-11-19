//! Read checksummed Btrfs tree blocks from a single-device volume.
//! Resolve metadata through the validated system and chunk trees, checking each mirror.
use super::chunks::{Chunks, Device};
use super::{bad, u32le, u64le};
use crate::{Error, Result};
use std::{
    collections::BTreeSet,
    fs::{self, File},
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const HEADER: usize = 101;
const MAX_BLOCKS: usize = 8192;
const fn crc_table() -> [u32; 256] {
    let mut a = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut v = i as u32;
        let mut j = 0;
        while j < 8 {
            v = (v >> 1) ^ if v & 1 != 0 { 0x82f63b78 } else { 0 };
            j += 1;
        }
        a[i] = v;
        i += 1;
    }
    a
}
const CRC: [u32; 256] = crc_table();
fn valid_crc(data: &[u8]) -> bool {
    let mut c = u32::MAX;
    for &b in &data[32..] {
        c = (c >> 8) ^ CRC[usize::from(c as u8 ^ b)];
    }
    u32le(data, 0) == !c
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, PartialOrd, Ord)]
pub(super) struct Key(pub u64, pub u8, pub u64);
fn key(b: &[u8]) -> Key {
    Key(u64le(b, 0), b[8], u64le(b, 9))
}
#[derive(Clone, Copy)]
pub(super) struct Node {
    pub address: u64,
    pub generation: u64,
    pub level: u8,
}
#[derive(Clone, Copy)]
pub(super) struct Item {
    pub node: Node,
    pub offset: usize,
    pub length: usize,
}
pub(super) struct Volume<'a> {
    pub root: Node,
    pub sector: usize,
    file: File,
    path: &'a Path,
    chunks: Chunks,
    node_size: usize,
    fsid: [u8; 16],
    initial: fs::Metadata,
    cancel: &'a AtomicBool,
}
impl<'a> Volume<'a> {
    pub fn open(path: &'a Path, cancel: &'a AtomicBool) -> Result<Self> {
        let mut file = File::open(path)?;
        let initial = file.metadata()?;
        if !initial.is_file() {
            return Err(Error::Request(
                "select a regular single-device Btrfs volume image".into(),
            ));
        }
        file.seek(SeekFrom::Start(65536))?;
        let mut sb = [0; 4096];
        file.read_exact(&mut sb)?;
        if &sb[64..72] != b"_BHRfS_M" || u64le(&sb, 48) != 65536 {
            return Err(bad("missing primary superblock"));
        }
        if sb[196..198] != [0, 0] || u64le(&sb, 136) != 1 {
            return Err(Error::Unsupported(
                "Btrfs requires a single-device CRC32C volume".into(),
            ));
        }
        if !valid_crc(&sb) {
            return Err(bad("superblock CRC32C mismatch"));
        }
        let sector = u32le(&sb, 144) as usize;
        let node_size = u32le(&sb, 148) as usize;
        let length = u64le(&sb, 112);
        if !(4096..=65536).contains(&sector)
            || !sector.is_power_of_two()
            || !(sector..=65536).contains(&node_size)
            || !node_size.is_power_of_two()
            || length > initial.len()
            || length < 69632
        {
            return Err(bad("invalid superblock geometry"));
        }
        let offset = if u64le(&sb, 188) & (1 << 10) != 0 {
            571
        } else {
            32
        };
        let device = Device {
            id: u64le(&sb, 201),
            uuid: sb[267..283].try_into().unwrap(),
            length,
            sector: sector as u64,
        };
        let bootstrap = Chunks::bootstrap(&sb, &device)?;
        let mut volume = Self {
            file,
            path,
            initial,
            chunks: bootstrap,
            node_size,
            sector,
            fsid: sb[offset..offset + 16].try_into().unwrap(),
            root: Node {
                address: u64le(&sb, 80),
                generation: u64le(&sb, 72),
                level: sb[198],
            },
            cancel,
        };
        let chunk_root = Node {
            address: u64le(&sb, 88),
            generation: u64le(&sb, 164),
            level: sb[199],
        };
        let mut full = Chunks::default();
        volume.walk(chunk_root, |Key(id, ty, start), _, b| {
            if ty == 228 {
                if id != 256 {
                    return Err(bad("invalid chunk-tree key"));
                }
                full.add(start, b, &device)?;
            }
            Ok(())
        })?;
        full.validate()?;
        if !full.includes(&volume.chunks) {
            return Err(bad("system chunk array disagrees with chunk tree"));
        }
        volume.chunks = full;
        Ok(volume)
    }
    pub fn unchanged(&self) -> Result<()> {
        let m = fs::metadata(self.path)?;
        if m.len() != self.initial.len() || m.modified()? != self.initial.modified()? {
            return Err(Error::SourceChanged);
        }
        if self.cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        Ok(())
    }
    fn block(&mut self, node: Node) -> Result<Vec<u8>> {
        self.unchanged()?;
        if node.level > 8 || node.address == 0 || !node.address.is_multiple_of(self.sector as u64) {
            return Err(bad("invalid tree pointer"));
        }
        let mut bytes = vec![0; self.node_size];
        for at in self.chunks.addresses(node.address, self.node_size, 2 | 4)? {
            if self
                .file
                .seek(SeekFrom::Start(at))
                .and_then(|_| self.file.read_exact(&mut bytes))
                .is_err()
            {
                continue;
            }
            if bytes[32..48] == self.fsid
                && u64le(&bytes, 48) == node.address
                && u64le(&bytes, 80) == node.generation
                && bytes[100] == node.level
                && valid_crc(&bytes)
            {
                return Ok(bytes);
            }
        }
        Err(bad("no matching checksummed tree-block mirror"))
    }
    pub fn item(&mut self, item: Item) -> Result<Vec<u8>> {
        let b = self.block(item.node)?;
        Ok(b[item.offset..item.offset + item.length].to_vec())
    }
    pub fn walk(
        &mut self,
        root: Node,
        mut visit: impl FnMut(Key, Item, &[u8]) -> Result<()>,
    ) -> Result<()> {
        let mut todo = vec![(root, None, None)];
        let mut seen = BTreeSet::new();
        while let Some((node, first, end)) = todo.pop() {
            if seen.len() >= MAX_BLOCKS {
                return Err(Error::Limit(
                    "Btrfs selection exceeds 8192 tree blocks".into(),
                ));
            }
            if !seen.insert(node.address) {
                return Err(bad("duplicate tree block"));
            }
            let b = self.block(node)?;
            let count = u32le(&b, 96) as usize;
            let width = if node.level == 0 { 25 } else { 33 };
            if count > (b.len() - HEADER) / width || count == 0 && node.level != 0 {
                return Err(bad("inconsistent item count"));
            }
            let mut previous = None;
            let mut upper = b.len();
            let mut children = Vec::new();
            for i in 0..count {
                let at = HEADER + i * width;
                let k = key(&b[at..]);
                if previous.is_some_and(|p| p >= k)
                    || end.is_some_and(|e| k >= e)
                    || (i == 0 && first.is_some_and(|p| p != k))
                {
                    return Err(bad("inconsistent tree key range"));
                }
                previous = Some(k);
                if node.level > 0 {
                    let high = if i + 1 < count {
                        Some(key(&b[at + width..]))
                    } else {
                        end
                    };
                    children.push((
                        Node {
                            address: u64le(&b, at + 17),
                            generation: u64le(&b, at + 25),
                            level: node.level - 1,
                        },
                        Some(k),
                        high,
                    ));
                } else {
                    let offset = HEADER
                        .checked_add(u32le(&b, at + 17) as usize)
                        .ok_or_else(|| bad("item offset overflow"))?;
                    let length = u32le(&b, at + 21) as usize;
                    if offset < HEADER + count * width
                        || offset.checked_add(length).is_none_or(|n| n > upper)
                    {
                        return Err(bad("overlapping or out-of-range items"));
                    }
                    upper = offset;
                    visit(
                        k,
                        Item {
                            node,
                            offset,
                            length,
                        },
                        &b[offset..offset + length],
                    )?;
                }
            }
            todo.extend(children.into_iter().rev());
        }
        self.unchanged()
    }
}
