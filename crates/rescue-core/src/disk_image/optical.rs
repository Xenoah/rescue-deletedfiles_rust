//! Extract 2048-byte user data from a single checked CD-ROM data track.
use super::{ImportedFile, publish_output};
use crate::{Error, Result};
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{BufReader, BufWriter, Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const SYNC: [u8; 12] = [0, 255, 255, 255, 255, 255, 255, 255, 255, 255, 255, 0];
const PACKED: usize = 2352;
const WITH_SUBCHANNEL: usize = 2448;
const MAX_FRAMES: u64 = 100 * 60 * 75;

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("CD-ROM track: {reason}"))
}

fn frame_number(header: &[u8]) -> Result<u64> {
    let decimal = |b: u8, limit| {
        let n = (b >> 4) * 10 + (b & 15);
        if (b >> 4) > 9 || (b & 15) > 9 || n >= limit {
            return Err(invalid("invalid minute, second or frame address"));
        }
        Ok(u64::from(n))
    };
    Ok((decimal(header[0], 100)? * 60 + decimal(header[1], 60)?) * 75 + decimal(header[2], 75)?)
}

struct Tables {
    edc: [u32; 256],
    double: [u8; 256],
    inverse: [u8; 256],
}

impl Tables {
    fn new() -> Self {
        let mut tables = Self {
            edc: [0; 256],
            double: [0; 256],
            inverse: [0; 256],
        };
        for b in 0..=255u8 {
            let mut crc = u32::from(b);
            for _ in 0..8 {
                crc = (crc >> 1) ^ if crc & 1 != 0 { 0xd8018001 } else { 0 };
            }
            tables.edc[usize::from(b)] = crc;
            let doubled = (b << 1) ^ if b & 0x80 != 0 { 0x1d } else { 0 };
            tables.double[usize::from(b)] = doubled;
            tables.inverse[usize::from(b ^ doubled)] = b;
        }
        tables
    }

    // ECMA-130 interleaves 86 P words of 24 symbols, then 52 Q words of 43.
    fn parity(
        &self,
        data: &[u8],
        expected: &[u8],
        words: usize,
        symbols: usize,
        step: usize,
        advance: usize,
    ) -> bool {
        for word in 0..words {
            let mut at = (word / 2) * step + word % 2;
            let (mut a, mut b) = (0, 0);
            for _ in 0..symbols {
                let v = data[at];
                a = self.double[usize::from(a ^ v)];
                b ^= v;
                at += advance;
                if at >= data.len() {
                    at -= data.len();
                }
            }
            let first = self.inverse[usize::from(self.double[usize::from(a)] ^ b)];
            if expected[word] != first || expected[word + words] != first ^ b {
                return false;
            }
        }
        true
    }

    fn payload<'a>(&self, frame: &'a mut [u8], expected_frame: u64, mode: u8) -> Result<&'a [u8]> {
        if frame[..12] != SYNC
            || frame_number(&frame[12..15])? != expected_frame
            || frame[15] != mode
        {
            return Err(invalid("discontinuous frame address or mixed track types"));
        }
        let (start, edc_start, edc_at) = match mode {
            1 if frame[2068..2076] == [0; 8] => (16, 0, 2064),
            1 => return Err(invalid("nonzero reserved Mode 1 bytes")),
            2 if frame[16..20] != frame[20..24] => {
                return Err(invalid("XA subheader copies differ"));
            }
            2 if frame[18] & 0x20 == 0 => (24, 16, 2072),
            _ => {
                return Err(Error::Unsupported(
                    "requires a Mode 1 or Mode 2 XA Form 1 data track".into(),
                ));
            }
        };
        let crc = frame[edc_start..edc_at].iter().fold(0u32, |crc, &byte| {
            (crc >> 8) ^ self.edc[((crc as u8) ^ byte) as usize]
        });
        if frame[edc_at..edc_at + 4] != crc.to_le_bytes() {
            return Err(invalid("sector EDC mismatch"));
        }
        // XA excludes its address and mode from the Reed–Solomon parity.
        if mode == 2 {
            frame[12..16].fill(0);
        }
        if !self.parity(&frame[12..2076], &frame[2076..2248], 86, 24, 2, 86)
            || !self.parity(&frame[12..2248], &frame[2248..2352], 52, 43, 86, 88)
        {
            return Err(invalid("sector P/Q parity mismatch"));
        }
        Ok(&frame[start..start + 2048])
    }
}

fn stride(file: &mut File, length: u64) -> Result<usize> {
    let mut chosen = None;
    for stride in [PACKED, WITH_SUBCHANNEL] {
        let frames = length / stride as u64;
        if !length.is_multiple_of(stride as u64) || !(1..=MAX_FRAMES).contains(&frames) {
            continue;
        }
        if frames > 1 {
            let mut next = [0; 12];
            file.seek(SeekFrom::Start(stride as u64))?;
            file.read_exact(&mut next)?;
            if next != SYNC {
                continue;
            }
        }
        if chosen.replace(stride).is_some() {
            return Err(invalid("ambiguous sector stride"));
        }
    }
    chosen.ok_or_else(|| invalid("incomplete or unrecognized 2352/2448-byte sector layout"))
}

pub(super) fn import(
    path: &Path,
    kind: Option<&str>,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if kind.is_some_and(|k| k != "cd-raw") {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    let mut head = [0; 16];
    if before.len() < 16 || {
        file.read_exact(&mut head)?;
        head[..12] != SYNC
    } {
        return if kind.is_some() {
            Err(invalid("missing CD-ROM sector header"))
        } else {
            Ok(None)
        };
    }
    let stride = stride(&mut file, before.len())?;
    let count = before.len() / stride as u64;
    let first = frame_number(&head[12..])?;
    if first + count > MAX_FRAMES {
        return Err(invalid("track extends past the address space"));
    }
    let tables = Tables::new();
    file.rewind()?;
    let result = publish_output(output, cancel, |out| {
        let mut reader = BufReader::with_capacity(256 << 10, &file);
        let mut writer = BufWriter::with_capacity(256 << 10, out);
        let mut frame = [0; WITH_SUBCHANNEL];
        let mut digest = Sha256::new();
        for i in 0..count {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            reader.read_exact(&mut frame[..stride])?;
            let user = tables
                .payload(&mut frame[..PACKED], first + i, head[15])
                .map_err(|e| match e {
                    Error::Invalid(why) => Error::Invalid(format!("{why} at sector {i}")),
                    other => other,
                })?;
            writer.write_all(user)?;
            digest.update(user);
        }
        writer.flush()?;
        let after = file.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(Error::SourceChanged);
        }
        Ok((count * 2048, digest))
    })?;
    Ok(Some(result))
}
