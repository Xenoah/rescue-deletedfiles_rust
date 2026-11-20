//! Expand complete same-codec compressed streams into a seekable logical disk.
use super::{ImportedFile, publish_output};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufRead, BufReader, Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_OUTPUT: u64 = 16 << 40;
const MAX_WINDOW: u64 = 64 << 20;

fn invalid(detail: &str) -> Error {
    Error::Invalid(format!("compressed RAW: {detail}"))
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Codec {
    Gzip,
    Bzip2,
    Xz,
    Zstd,
    Lz4,
}

impl Codec {
    fn detect(prefix: &[u8]) -> Option<Self> {
        match prefix {
            [0x1f, 0x8b, 8, ..] => Some(Self::Gzip),
            [b'B', b'Z', b'h', b'1'..=b'9', ..] => Some(Self::Bzip2),
            [0xfd, b'7', b'z', b'X', b'Z', 0, ..] => Some(Self::Xz),
            [0x28, 0xb5, 0x2f, 0xfd, ..] => Some(Self::Zstd),
            [0x04, 0x22, 0x4d, 0x18, ..] => Some(Self::Lz4),
            _ => None,
        }
    }
}

#[derive(Default)]
struct RawDisk {
    size: u64,
    sha: Sha256,
}

impl RawDisk {
    fn copy(
        &mut self,
        reader: &mut impl Read,
        output: &mut File,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let mut buffer = vec![0; 256 << 10];
        loop {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let n = reader.read(&mut buffer)?;
            if n == 0 {
                return Ok(());
            }
            self.size += n as u64;
            if self.size > MAX_OUTPUT {
                return Err(Error::Limit("compressed disk exceeds 16 TiB".into()));
            }
            let bytes = &buffer[..n];
            self.sha.update(bytes);
            super::write_sparse_chunk(output, bytes)?;
        }
    }
}

// FrameDecoder treats EOF in place of a block header as an ordinary end. Verify
// that one actual end marker (and its optional checksum) defines the end of each frame.
fn lz4_structure(reader: &mut BufReader<File>, length: u64, cancel: &AtomicBool) -> Result<u64> {
    let start = reader.stream_position()?;
    let mut prefix = [0u8; 7];
    reader.read_exact(&mut prefix)?;
    let flags = prefix[4];
    if flags >> 6 != 1 || flags & 3 != 0 {
        return Err(invalid("unsupported LZ4 header"));
    }
    let limit = match (prefix[5] >> 4) & 7 {
        4 => 64 << 10,
        5 => 256 << 10,
        6 => 1 << 20,
        7 => 4 << 20,
        _ => return Err(invalid("LZ4 block limit")),
    };
    let mut pos = start + 7 + if flags & 8 != 0 { 8 } else { 0 };
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        reader.seek(SeekFrom::Start(pos))?;
        let mut block = [0; 4];
        reader.read_exact(&mut block)?;
        let field = u32::from_le_bytes(block);
        if field == 0 {
            let end = pos + 4 + if flags & 4 != 0 { 4 } else { 0 };
            if end > length {
                return Err(invalid("missing LZ4 content checksum"));
            }
            reader.seek(SeekFrom::Start(start))?;
            return Ok(end);
        }
        let bytes = field & 0x7fff_ffff;
        if bytes > limit {
            return Err(invalid("oversized LZ4 block"));
        }
        pos += 4 + u64::from(bytes) + if flags & 16 != 0 { 4 } else { 0 };
        if pos > length {
            return Err(invalid("truncated LZ4 block"));
        }
    }
}

impl Codec {
    fn decode_frame(
        self,
        reader: &mut BufReader<File>,
        length: u64,
        disk: &mut RawDisk,
        out: &mut File,
        cancel: &AtomicBool,
    ) -> Result<()> {
        match self {
            Codec::Gzip => disk.copy(
                &mut flate2::bufread::GzDecoder::new(&mut *reader),
                out,
                cancel,
            )?,
            Codec::Bzip2 => disk.copy(
                &mut bzip2::bufread::BzDecoder::new(&mut *reader),
                out,
                cancel,
            )?,
            Codec::Xz => disk.copy(
                &mut lzma_rust2::XzReader::new_mem_limit(&mut *reader, false, 65536),
                out,
                cancel,
            )?,
            Codec::Zstd => {
                let mut decoder = ruzstd::decoding::StreamingDecoder::new_with_max_window_size(
                    &mut *reader,
                    MAX_WINDOW,
                )
                .map_err(|e| invalid(&e.to_string()))?;
                disk.copy(&mut decoder, out, cancel)?;
                if !decoder.decoder.is_finished()
                    || decoder
                        .decoder
                        .get_checksum_from_data()
                        .is_some_and(|value| {
                            Some(value) != decoder.decoder.get_calculated_checksum()
                        })
                {
                    return Err(invalid("Zstandard frame or checksum"));
                }
            }
            Codec::Lz4 => {
                let end = lz4_structure(reader, length, cancel)?;
                let mut decoder = lz4_flex::frame::FrameDecoder::new(&mut *reader);
                loop {
                    let before = decoder.get_mut().stream_position()?;
                    disk.copy(&mut decoder, out, cancel)?;
                    let after = decoder.get_mut().stream_position()?;
                    if after == end {
                        break;
                    }
                    if after > end || after <= before {
                        return Err(invalid("LZ4 decoder made no progress"));
                    }
                }
            }
        }
        Ok(())
    }
}

fn xz_padding(reader: &mut BufReader<File>, cancel: &AtomicBool) -> Result<()> {
    let mut count = 0u64;
    loop {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let input = reader.fill_buf()?;
        let n = input.iter().take_while(|&&b| b == 0).count();
        let more = n != 0 && n == input.len();
        reader.consume(n);
        count += n as u64;
        if !more {
            break;
        }
    }
    if count.is_multiple_of(4) {
        Ok(())
    } else {
        Err(invalid("misaligned XZ stream padding"))
    }
}

pub(super) fn import(
    path: &Path,
    kind: Option<&str>,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if kind.is_some_and(|v| v != "compressed-raw") {
        return Ok(None);
    }
    let mut reader = BufReader::new(File::open(path)?);
    let original = reader.get_ref().metadata()?;
    let Some(codec) = Codec::detect(reader.fill_buf()?) else {
        return if kind.is_some() {
            Err(invalid("expected gzip, bzip2, XZ, Zstandard or LZ4"))
        } else {
            Ok(None)
        };
    };
    let file = publish_output(output, cancel, |out| {
        let mut disk = RawDisk::default();
        let mut frames = 0usize;
        loop {
            frames += 1;
            if frames > 65536 {
                return Err(Error::Limit("more than 65536 compressed streams".into()));
            }
            codec.decode_frame(&mut reader, original.len(), &mut disk, out, cancel)?;
            if codec == Codec::Xz {
                xz_padding(&mut reader, cancel)?;
            }
            let pos = reader.stream_position()?;
            if pos == original.len() {
                break;
            }
            let remaining = original
                .len()
                .checked_sub(pos)
                .ok_or(Error::SourceChanged)?;
            let mut head = [0; 6];
            let n = remaining.min(6) as usize;
            reader.read_exact(&mut head[..n])?;
            if Codec::detect(&head[..n]) != Some(codec) {
                return Err(invalid("unexpected trailing data or mixed codecs"));
            }
            reader.seek(SeekFrom::Start(pos))?;
        }
        if disk.size == 0 {
            return Err(invalid("empty logical disk"));
        }
        let now = reader.get_ref().metadata()?;
        if now.len() != original.len() || now.modified()? != original.modified()? {
            return Err(Error::SourceChanged);
        }
        out.set_len(disk.size)?;
        Ok((disk.size, disk.sha))
    })?;
    Ok(Some(file))
}
