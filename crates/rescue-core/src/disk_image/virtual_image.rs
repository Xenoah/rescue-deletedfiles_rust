//! Read logical sectors from self-contained virtual disks. No host path stored in an image is opened.
mod qcow;
mod vhd;
mod vhdx;
mod vmdk;
use super::ImportedFile;
use crate::{Error, Result};
use flate2::{Decompress, FlushDecompress, Status};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};
pub(super) use vmdk::import_segments;

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

    fn block(&self, guest_offset: u64) -> Block {
        let slot = le32(&self.blocks, (guest_offset / self.block_size) as usize * 4);
        let physical = (slot < 0xffff_fffe).then(|| {
            self.data_offset + u64::from(slot) * (self.block_size + self.extra) + self.extra
        });
        physical.map_or(Block::Zero, Block::Data)
    }
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Compression {
    QcowDeflate,
    QcowZstd,
    VmdkZlib,
}

impl Compression {
    fn decode(self, encoded: &[u8], decoded: &mut [u8], cluster: u64, count: u64) -> Result<()> {
        if self == Self::QcowZstd {
            let mut decoder =
                ruzstd::decoding::StreamingDecoder::new_with_max_window_size(encoded, 8 << 20)
                    .map_err(|_| invalid("invalid QCOW2 Zstandard frame header or window"))?;
            decoder
                .read_exact(&mut decoded[..cluster as usize])
                .map_err(|_| invalid("invalid or short QCOW2 Zstandard frame"))?;
            let mut extra = [0];
            if decoder
                .read(&mut extra)
                .map_err(|_| invalid("invalid QCOW2 Zstandard end of frame"))?
                != 0
                || !decoder.decoder.is_finished()
                || decoder
                    .decoder
                    .get_checksum_from_data()
                    .is_some_and(|hash| Some(hash) != decoder.decoder.get_calculated_checksum())
            {
                return Err(invalid("QCOW2 Zstandard cluster size or checksum"));
            }
            // Another cluster may use the remaining bytes in the final host sector.
            return Ok(());
        }
        let wrapped = self == Self::VmdkZlib;
        let mut stream = Decompress::new(wrapped);
        let status = stream
            .decompress(encoded, decoded, FlushDecompress::Finish)
            .map_err(|_| invalid("invalid compressed virtual disk block"))?;
        let decoded_length = stream.total_out();
        if status != Status::StreamEnd
            || (decoded_length != cluster && !(wrapped && decoded_length == count))
            || (wrapped && stream.total_in() != encoded.len() as u64)
        {
            return Err(invalid("compressed virtual disk block length"));
        }
        Ok(())
    }
}

enum Block {
    Zero,
    Data(u64),
    Compressed {
        at: u64,
        bytes: usize,
        codec: Compression,
    },
}
enum Disk {
    Vdi(Vdi),
    Qcow(qcow::Qcow),
    Vhd(vhd::Vhd),
    Vhdx(vhdx::Vhdx),
    Vmdk(vmdk::Vmdk),
    Flat { size: u64, at: Option<u64> },
}
impl Disk {
    fn size(&self) -> u64 {
        match self {
            Self::Vdi(v) => v.size,
            Self::Qcow(q) => q.size,
            Self::Vhd(v) => v.size,
            Self::Vhdx(v) => v.size,
            Self::Vmdk(v) => v.size,
            Self::Flat { size, .. } => *size,
        }
    }
    fn block(&mut self, source: &mut File, offset: u64) -> Result<(Block, u64)> {
        match self {
            Self::Vdi(v) => Ok((v.block(offset), v.block_size)),
            Self::Qcow(q) => q.block(source, offset),
            Self::Vhd(v) => v.block(source, offset),
            Self::Vhdx(v) => Ok(v.block(offset)),
            Self::Vmdk(v) => Ok(v.block(offset)),
            Self::Flat { size, at } => Ok((
                at.map_or(Block::Zero, |at| Block::Data(at + offset)),
                *size - offset,
            )),
        }
    }
    fn write_raw(
        &mut self,
        source: &mut File,
        file: &mut File,
        cancel: &AtomicBool,
    ) -> Result<(u64, Sha256)> {
        let size = self.size();
        let mut digest = Sha256::new();
        self.write_at(source, file, 0, &mut digest, cancel)?;
        file.set_len(size)?;
        Ok((size, digest))
    }
    fn write_at(
        &mut self,
        source: &mut File,
        file: &mut File,
        base: u64,
        digest: &mut Sha256,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let size = self.size();
        let mut buffer = vec![0; 256 * 1024];
        let zeros = vec![0; buffer.len()];
        let mut encoded = Vec::new();
        let mut decoded = Vec::new();
        let mut offset = 0;
        while offset < size {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let (block, cluster) = self.block(source, offset)?;
            let count = cluster.min(size - offset);
            match block {
                Block::Compressed { at, bytes, codec } => {
                    encoded.resize(bytes, 0);
                    read_at(source, at, &mut encoded)?;
                    decoded.resize(cluster as usize + 1, 0);
                    codec.decode(&encoded, &mut decoded, cluster, count)?;
                    file.seek(SeekFrom::Start(base + offset))?;
                    file.write_all(&decoded[..count as usize])?;
                    digest.update(&decoded[..count as usize]);
                }
                block => {
                    let data = if let Block::Data(at) = block {
                        source.seek(SeekFrom::Start(at))?;
                        file.seek(SeekFrom::Start(base + offset))?;
                        true
                    } else {
                        false
                    };
                    let mut remaining = count;
                    while remaining > 0 {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(Error::Cancelled);
                        }
                        let n = remaining.min(buffer.len() as u64) as usize;
                        if data {
                            source.read_exact(&mut buffer[..n])?;
                            if buffer[..n].iter().all(|&b| b == 0) {
                                file.seek_relative(n as i64)?;
                            } else {
                                file.write_all(&buffer[..n])?;
                            }
                            digest.update(&buffer[..n]);
                        } else {
                            digest.update(&zeros[..n]);
                        }
                        remaining -= n as u64;
                    }
                }
            }
            offset += count;
        }
        Ok(())
    }
}

pub(super) fn import(
    path: &Path,
    requested: Option<&str>,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if requested.is_some_and(|v| !matches!(v, "vdi" | "qcow2" | "vhd" | "vhdx" | "vmdk")) {
        return Ok(None);
    }
    let mut source = File::open(path)?;
    let before = source.metadata()?;
    let mut head = [0; 68];
    let n = before.len().min(head.len() as u64) as usize;
    read_at(&mut source, 0, &mut head[..n])?;
    // A fixed VHD starts directly with guest bytes, which may themselves look
    // like another container. Its identifying footer is outside the guest.
    let mut tail = [0; 8];
    if before.len() >= 512 {
        read_at(&mut source, before.len() - 512, &mut tail)?;
    }
    let kind = if tail == *b"conectix" || head[..8] == *b"conectix" {
        Some("vhd")
    } else if head[..4] == *b"KDMV" {
        Some("vmdk")
    } else if head[..8] == *b"vhdxfile" {
        Some("vhdx")
    } else if head[..4] == *b"QFI\xfb" {
        Some("qcow2")
    } else if head[64..68] == *b"\x7f\x10\xda\xbe" {
        Some("vdi")
    } else {
        None
    };
    if requested == Some("vmdk") && kind.is_none() {
        return Ok(None); // Other VMDK variants can use the external driver.
    }
    // Historically the external VHD reader also accepted VHDX under the vhd alias.
    if requested.is_some()
        && kind != requested
        && !(kind == Some("vhdx") && requested == Some("vhd"))
    {
        return Err(invalid("signature differs from requested image type"));
    }
    let mut disk = match kind {
        Some("vmdk") => Disk::Vmdk(vmdk::Vmdk::open(&mut source, before.len(), None, cancel)?),
        Some("vhdx") => Disk::Vhdx(vhdx::Vhdx::open(&mut source, before.len(), cancel)?),
        Some("vhd") => Disk::Vhd(vhd::Vhd::open(&mut source, before.len(), cancel)?),
        Some("qcow2") => Disk::Qcow(qcow::Qcow::open(&mut source, before.len(), cancel)?),
        Some("vdi") => Disk::Vdi(Vdi::open(&mut source, before.len())?),
        _ => return Ok(None),
    };
    let result = super::publish_output(output, cancel, |file| {
        let summary = disk.write_raw(&mut source, file, cancel)?;
        let after = source.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(Error::SourceChanged);
        }
        Ok(summary)
    })?;
    Ok(Some(result))
}
