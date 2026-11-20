//! VirtualBox VDI v1.1 base images and explicit differencing chains.
use super::{
    Block,
    chain::{self, Layer},
    invalid, le32, read_at,
};
use crate::{
    Error, Result,
    disk_image::{ImportedFile, RawExpectation},
};
use std::{
    collections::HashSet,
    ffi::OsString,
    fs::File,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) const MAX_TABLE: usize = 64 << 20;
#[derive(PartialEq, Eq)]
struct Identity {
    image: [u8; 16],
    modification: [u8; 16],
}
impl Identity {
    fn valid(&self) -> bool {
        self.image != [0; 16] && self.modification != [0; 16]
    }
}
enum Allocation {
    Inherit,
    Zero,
    Data(u64),
}

pub(super) struct Vdi {
    pub(super) size: u64,
    sector: u32,
    identity: Identity,
    parent: Option<Identity>,
    block_size: u64,
    data_offset: u64,
    extra: u64,
    blocks: Vec<u8>,
}

impl Vdi {
    pub(super) fn open(
        source: &mut File,
        length: u64,
        chain: bool,
        table_budget: usize,
        cancel: &AtomicBool,
    ) -> Result<Self> {
        let mut h = [0; 456];
        read_at(source, 0, &mut h)?;
        if &h[64..68] != b"\x7f\x10\xda\xbe"
            || le32(&h, 68) != 0x10001
            || le32(&h, 72) < 384
            || !matches!(le32(&h, 76), 1 | 2 | 4)
            || le32(&h, 80) != 0
        {
            return Err(Error::Unsupported(
                "unsupported VDI header version, image type or flags".into(),
            ));
        }
        let id = |start: usize| -> [u8; 16] { h[start..start + 16].try_into().unwrap() };
        let identity = Identity {
            image: id(392),
            modification: id(408),
        };
        let parent = Identity {
            image: id(424),
            modification: id(440),
        };
        let parent = if le32(&h, 76) == 4 {
            if !chain || !parent.valid() {
                return Err(Error::Unsupported(
                    "differencing VDI requires all identified parent files, nearest parent first"
                        .into(),
                ));
            }
            Some(parent)
        } else if parent.image != [0; 16] || parent.modification != [0; 16] {
            return Err(invalid("base VDI contains unexpected parent IDs"));
        } else {
            None
        };
        if chain && !identity.valid() {
            return Err(invalid(
                "VDI chain member has a missing image or modification UUID",
            ));
        }
        let map_offset = u64::from(le32(&h, 340));
        let data_offset = u64::from(le32(&h, 344));
        let sector_size = le32(&h, 360);
        let size = u64::from_le_bytes(h[368..376].try_into().unwrap());
        let block_size = u64::from(le32(&h, 376));
        let extra = u64::from(le32(&h, 380));
        let count = u64::from(le32(&h, 384));
        let used = le32(&h, 388);
        if !matches!(sector_size, 512 | 4096)
            || size == 0
            || !size.is_multiple_of(u64::from(sector_size))
            || !(512..=64 * 1024 * 1024).contains(&block_size)
            || !block_size.is_power_of_two()
            || extra > block_size
            || !extra.is_multiple_of(u64::from(sector_size))
            || count != size.div_ceil(block_size)
            || count > 16 * 1024 * 1024
            || u64::from(used) > count
            || (le32(&h, 76) == 2 && u64::from(used) != count)
            || map_offset < 72 + u64::from(le32(&h, 72))
            || data_offset < map_offset + count * 4
            || data_offset + u64::from(used) * (block_size + extra) > length
        {
            return Err(invalid(
                "inconsistent VDI header, allocation table or backing length",
            ));
        }
        if count * 4 > table_budget as u64 {
            return Err(Error::Limit(
                "VDI allocation tables exceed the memory limit".into(),
            ));
        }
        let mut blocks = vec![0; count as usize * 4];
        read_at(source, map_offset, &mut blocks)?;
        let mut seen = vec![false; used as usize];
        for (index, &bytes) in blocks.as_chunks::<4>().0.iter().enumerate() {
            if index.is_multiple_of(65536) && cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let slot = u32::from_le_bytes(bytes);
            if matches!(slot, 0xffff_fffe | 0xffff_ffff) {
                if le32(&h, 76) == 2 {
                    return Err(invalid("unallocated fixed VDI block"));
                }
            } else if slot >= used || std::mem::replace(&mut seen[slot as usize], true) {
                return Err(invalid("invalid or duplicate VDI block mapping"));
            }
        }
        if seen.contains(&false) {
            return Err(invalid("unreferenced allocated VDI block"));
        }
        Ok(Self {
            size,
            sector: sector_size,
            identity,
            parent,
            block_size,
            data_offset,
            extra,
            blocks,
        })
    }

    // Free slots in a child inherit their parent. A zero marker explicitly hides parent data.
    fn allocation(&self, offset: u64) -> (Allocation, u64) {
        let within = offset % self.block_size;
        let slot = le32(&self.blocks, (offset / self.block_size) as usize * 4);
        let allocation = match slot {
            0xffff_ffff => Allocation::Inherit,
            0xffff_fffe => Allocation::Zero,
            _ => Allocation::Data(
                self.data_offset
                    + u64::from(slot) * (self.block_size + self.extra)
                    + self.extra
                    + within,
            ),
        };
        (allocation, self.block_size - within)
    }

    pub(super) fn standalone_block(&self, offset: u64) -> (Block, u64) {
        let (slot, span) = self.allocation(offset);
        (
            match slot {
                Allocation::Data(at) => Block::Data(at),
                Allocation::Inherit | Allocation::Zero => Block::Zero,
            },
            span,
        )
    }
}

pub(in crate::disk_image) fn import_vdi_chain(
    paths: &[OsString],
    requested: Option<&str>,
    output: &Path,
    expected: RawExpectation,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if requested.is_some_and(|kind| kind != "vdi") {
        return Ok(None);
    }
    let mut first = File::open(&paths[0])?;
    let mut signature = [0; 4];
    if first.metadata()?.len() < 68 {
        return Ok(None);
    }
    read_at(&mut first, 64, &mut signature)?;
    if signature != *b"\x7f\x10\xda\xbe" {
        return Ok(None);
    }
    if paths.len() > 8 {
        return Err(Error::Limit("VDI chain exceeds eight files".into()));
    }
    let mut first = Some(first);
    let mut members: Vec<Layer<Vdi>> = Vec::new();
    let mut handles = HashSet::new();
    let mut uuids = HashSet::new();
    let mut budget = 128 << 20;
    for path in paths {
        let mut file = match first.take() {
            Some(file) => file,
            None => File::open(path)?,
        };
        if !handles.insert(same_file::Handle::from_file(file.try_clone()?)?) {
            return Err(invalid("repeated VDI chain file"));
        }
        let before = file.metadata()?;
        let disk = Vdi::open(&mut file, before.len(), true, budget, cancel)?;
        if !uuids.insert(disk.identity.image) {
            return Err(invalid("repeated image UUID in VDI chain"));
        }
        if let Some(child) = members.last()
            && (child.layout.parent.as_ref() != Some(&disk.identity)
                || disk.size != child.layout.size
                || disk.sector != child.layout.sector)
        {
            return Err(invalid(
                "selected VDI parent identity, modification UUID or logical geometry differs from the child",
            ));
        }
        budget -= disk.blocks.len();
        members.push(Layer {
            file,
            before,
            layout: disk,
        });
    }
    if members.last().is_some_and(|m| m.layout.parent.is_some()) {
        return Err(Error::Request(
            "select every VDI parent through the fixed or dynamic base".into(),
        ));
    }
    let size = members[0].layout.size;
    chain::flatten(
        &mut members,
        size,
        output,
        expected,
        cancel,
        |layer, offset, buf, limit| {
            let (allocation, span) = layer.layout.allocation(offset);
            *limit = span.min(*limit as u64) as usize;
            match allocation {
                Allocation::Inherit => return Ok(false),
                Allocation::Zero => buf[..*limit].fill(0),
                Allocation::Data(at) => read_at(&mut layer.file, at, &mut buf[..*limit])?,
            }
            Ok(true)
        },
    )
    .map(Some)
}
