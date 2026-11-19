//! Read logical sectors from self-contained virtual disks. No host path stored in an image is opened.
use rescue_core::{Error, Result};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("virtual disk image: {reason}"))
}

fn read_at(source: &mut File, offset: u64, bytes: &mut [u8]) -> Result<()> {
    source.seek(SeekFrom::Start(offset))?;
    source.read_exact(bytes).map_err(|e| {
        if e.kind() == std::io::ErrorKind::UnexpectedEof {
            invalid("truncated structure or data block")
        } else {
            e.into()
        }
    })
}

fn le32(bytes: &[u8], pos: usize) -> u32 {
    u32::from_le_bytes(bytes[pos..pos + 4].try_into().unwrap())
}

struct Vdi {
    size: u64,
    block_size: u64,
    data_offset: u64,
    extra: u64,
    blocks: Vec<u8>,
}

impl Vdi {
    fn open(source: &mut File, length: u64) -> Result<Self> {
        let mut h = [0; 456];
        read_at(source, 0, &mut h)?;
        if &h[64..68] != b"\x7f\x10\xda\xbe"
            || le32(&h, 68) != 0x10001
            || le32(&h, 72) < 384
            || !matches!(le32(&h, 76), 1 | 2)
            || le32(&h, 80) != 0
            || h[424..456].iter().any(|v| *v != 0)
        {
            return Err(Error::Unsupported(
                "VDI requires a standalone fixed or dynamic v1.1 base image".into(),
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
        let mut blocks = vec![0; count as usize * 4];
        read_at(source, map_offset, &mut blocks)?;
        let mut seen = vec![false; used as usize];
        for &bytes in blocks.as_chunks::<4>().0 {
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
            block_size,
            data_offset,
            extra,
            blocks,
        })
    }

    fn block(&self, guest_offset: u64) -> (u64, Option<u64>) {
        let slot = le32(&self.blocks, (guest_offset / self.block_size) as usize * 4);
        let available = self.block_size.min(self.size - guest_offset);
        let physical = (slot < 0xffff_fffe).then(|| {
            self.data_offset + u64::from(slot) * (self.block_size + self.extra) + self.extra
        });
        (available, physical)
    }
}

pub(super) fn import(
    path: &Path,
    requested: Option<&str>,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<Option<Value>> {
    if requested.is_some_and(|v| v != "vdi") {
        return Ok(None);
    }
    let mut source = File::open(path)?;
    let before = source.metadata()?;
    let mut signature = [0; 4];
    let detected = before.len() >= 68 && {
        read_at(&mut source, 64, &mut signature)?;
        signature == *b"\x7f\x10\xda\xbe"
    };
    if !detected {
        return if requested.is_some() {
            Err(invalid("expected VDI signature"))
        } else {
            Ok(None)
        };
    }
    let image = Vdi::open(&mut source, before.len())?;
    let result = super::sleuthkit::publish_output(output, cancel, |file| {
        let mut digest = Sha256::new();
        let mut buffer = vec![0; 256 * 1024];
        let mut offset = 0;
        while offset < image.size {
            let (count, physical) = image.block(offset);
            if let Some(at) = physical {
                source.seek(SeekFrom::Start(at))?;
                file.seek(SeekFrom::Start(offset))?;
            } else {
                buffer.fill(0);
            }
            let mut remaining = count;
            while remaining > 0 {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let n = remaining.min(buffer.len() as u64) as usize;
                if physical.is_some() {
                    source.read_exact(&mut buffer[..n])?;
                    file.write_all(&buffer[..n])?;
                }
                digest.update(&buffer[..n]);
                remaining -= n as u64;
            }
            offset += count;
        }
        file.set_len(image.size)?;
        let after = source.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(Error::SourceChanged);
        }
        Ok((offset, digest))
    })?;
    Ok(Some(result))
}
