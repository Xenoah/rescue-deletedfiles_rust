//! Import Android sparse 1.0 into an acquisition pair. DONT_CARE is unknown media.
use super::{ImageInput, ImportedFile};
use crate::{
    AcquisitionIdentity, Error, Result, canonical_output_path,
    read_map::{AcquisitionStrategy, Header, IMAGE_NAME, MAP_NAME, ReadMap, ReadState, Segment},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{self, Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const MAGIC: u32 = 0xed26ff3a;
const BUFFER: usize = 1 << 20;
const MAX_RECORDS: usize = 262_144;
const MAX_LOGICAL: u64 = 64 << 30;

#[derive(Debug, Serialize)]
pub struct AndroidVerification {
    pub input_sha256: String,
    pub read_map: PathBuf,
    pub recorded_bytes: u64,
    pub unrecorded_bytes: u64,
    pub crc32_checks: u32,
}

fn invalid(msg: &str) -> Error {
    Error::Invalid(format!("Android sparse image: {msg}"))
}

fn u16le(b: &[u8], i: usize) -> u16 {
    u16::from_le_bytes(b[i..i + 2].try_into().unwrap())
}
fn u32le(b: &[u8], i: usize) -> u32 {
    u32::from_le_bytes(b[i..i + 4].try_into().unwrap())
}

pub(super) fn has_magic(source: &Path) -> Result<bool> {
    let mut tag = [0; 4];
    match File::open(source)?.read_exact(&mut tag) {
        Ok(()) => Ok(u32::from_le_bytes(tag) == MAGIC),
        Err(e) if e.kind() == io::ErrorKind::UnexpectedEof => Ok(false),
        Err(e) => Err(e.into()),
    }
}

struct Input {
    file: File,
    digest: Sha256,
}
impl Input {
    fn take(&mut self, into: &mut [u8]) -> Result<()> {
        self.file.read_exact(into).map_err(|e| {
            if e.kind() == io::ErrorKind::UnexpectedEof {
                invalid("truncated header or payload")
            } else {
                e.into()
            }
        })?;
        self.digest.update(into);
        Ok(())
    }
}

fn cancelled(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

#[derive(Clone, Copy)]
enum Payload {
    Raw,
    Fill([u8; 4]),
    Unknown,
}

struct Decoder {
    output: File,
    logical: Sha256,
    crc: crc32fast::Hasher,
    crc_checks: u32,
    offset: u64,
    unrecorded: u64,
    records: Vec<Segment>,
    buffer: Vec<u8>,
    timestamp: u64,
}
impl Decoder {
    fn verify_crc(&mut self, expected: u32) -> Result<()> {
        if self.crc.clone().finalize() != expected {
            return Err(invalid("logical CRC-32 mismatch"));
        }
        self.crc_checks += 1;
        Ok(())
    }
    fn expand(
        &mut self,
        source: &mut Input,
        kind: Payload,
        bytes: u64,
        cancel: &AtomicBool,
    ) -> Result<()> {
        let sparse_output = matches!(kind, Payload::Unknown | Payload::Fill([0, 0, 0, 0]));
        if sparse_output {
            self.output.seek(SeekFrom::Start(self.offset + bytes))?;
        }
        match kind {
            Payload::Fill(pattern) => {
                let needed = bytes.min(BUFFER as u64) as usize;
                self.buffer[..needed].as_chunks_mut::<4>().0.fill(pattern);
            }
            Payload::Unknown => {
                let needed = bytes.min(BUFFER as u64) as usize;
                self.buffer[..needed].fill(0);
                self.unrecorded += bytes;
            }
            Payload::Raw => {}
        }
        let end = self.offset + bytes;
        while self.offset < end {
            cancelled(cancel)?;
            let size = (end - self.offset).min(BUFFER as u64) as usize;
            let data = &mut self.buffer[..size];
            if matches!(kind, Payload::Raw) {
                source.take(data)?;
            }
            if !matches!(kind, Payload::Unknown) {
                if self.records.len() == MAX_RECORDS {
                    return Err(Error::Limit("Android sparse read-map record limit".into()));
                }
                if !sparse_output {
                    self.output.write_all(data)?;
                }
                self.records.push(Segment {
                    offset: self.offset,
                    length: size as u64,
                    state: ReadState::Copied,
                    attempts: 1,
                    sha256: Sha256::digest(&*data).into(),
                    error_code: 0,
                    read_unix_ms: self.timestamp,
                });
            }
            self.logical.update(&*data);
            self.crc.update(data);
            self.offset += size as u64;
        }
        Ok(())
    }
    fn decode(&mut self, source: &mut Input, cancel: &AtomicBool) -> Result<()> {
        let mut header = [0; 28];
        source.take(&mut header)?;
        if u32le(&header, 0) != MAGIC {
            return Err(invalid("invalid magic"));
        }
        if (u16le(&header, 4), u16le(&header, 6)) != (1, 0) {
            return Err(Error::Unsupported(
                "only Android sparse version 1.0 is supported".into(),
            ));
        }
        let file_header = usize::from(u16le(&header, 8));
        let chunk_header = usize::from(u16le(&header, 10));
        let block = u64::from(u32le(&header, 12));
        let length = block * u64::from(u32le(&header, 16));
        let chunks = u32le(&header, 20);
        let checksum = u32le(&header, 24);
        if !(28..=4096).contains(&file_header)
            || !(12..=4096).contains(&chunk_header)
            || !(512..=BUFFER as u64).contains(&block)
            || !block.is_multiple_of(512)
            || length == 0
            || chunks == 0
        {
            return Err(invalid("invalid header sizes or disk geometry"));
        }
        if length > MAX_LOGICAL || chunks as usize > MAX_RECORDS {
            return Err(Error::Limit(
                "Android sparse logical size or chunk count exceeds limit".into(),
            ));
        }
        let mut h = [0u8; 4096];
        source.take(&mut h[..file_header - 28])?;
        for _ in 0..chunks {
            cancelled(cancel)?;
            source.take(&mut h[..chunk_header])?;
            let tag = u16le(&h, 0);
            let n = block * u64::from(u32le(&h, 4));
            let payload = u64::from(u32le(&h, 8))
                .checked_sub(chunk_header as u64)
                .ok_or_else(|| invalid("chunk header exceeds stored chunk size"))?;
            if n > length - self.offset || u16le(&h, 2) != 0 {
                return Err(invalid(
                    "chunk exceeds logical length or has reserved flags",
                ));
            }
            let mut value = [0; 4];
            let kind = match tag {
                0xcac1 if n != 0 && payload == n => Payload::Raw,
                0xcac2 if n != 0 && payload == 4 => {
                    source.take(&mut value)?;
                    Payload::Fill(value)
                }
                0xcac3 if n != 0 && payload == 0 => Payload::Unknown,
                0xcac4 if n == 0 && payload == 4 => {
                    source.take(&mut value)?;
                    self.verify_crc(u32::from_le_bytes(value))?;
                    continue;
                }
                _ => {
                    return Err(invalid(
                        "unsupported chunk type or inconsistent chunk length",
                    ));
                }
            };
            self.expand(source, kind, n, cancel)?;
        }
        if self.offset != length || source.file.stream_position()? != source.file.metadata()?.len()
        {
            return Err(invalid(
                "logical block count or physical end of image does not match",
            ));
        }
        if checksum != 0 {
            self.verify_crc(checksum)?;
        }
        self.output.set_len(length)?;
        self.output.sync_all()?;
        Ok(())
    }
}

#[cfg(any(target_os = "linux", target_os = "macos"))]
fn publish_directory(staged: &Path, output: &Path) -> io::Result<()> {
    use rustix::fs::{CWD, RenameFlags, renameat_with};
    renameat_with(CWD, staged, CWD, output, RenameFlags::NOREPLACE).map_err(Into::into)
}

#[cfg(windows)]
fn publish_directory(staged: &Path, output: &Path) -> io::Result<()> {
    use std::os::windows::ffi::OsStrExt;
    use windows_sys::Win32::Storage::FileSystem::{MOVEFILE_WRITE_THROUGH, MoveFileExW};
    fn wide(path: &Path) -> io::Result<Vec<u16>> {
        let mut value: Vec<_> = path.as_os_str().encode_wide().collect();
        if value.contains(&0) {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "NUL in output path",
            ));
        }
        value.push(0);
        Ok(value)
    }
    let from = wide(staged)?;
    let to = wide(output)?;
    // Both paths are owned, NUL-terminated UTF-16. No replace or cross-volume copy flag.
    if unsafe { MoveFileExW(from.as_ptr(), to.as_ptr(), MOVEFILE_WRITE_THROUGH) } == 0 {
        return Err(io::Error::last_os_error());
    }
    Ok(())
}

#[cfg(not(any(target_os = "linux", target_os = "macos", windows)))]
fn publish_directory(_: &Path, _: &Path) -> io::Result<()> {
    Err(io::Error::new(
        io::ErrorKind::Unsupported,
        "atomic directory publication unavailable on this platform",
    ))
}

pub fn import_android(
    source: &Path,
    destination: &Path,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    let request = ImageInput {
        source: source.to_owned(),
        segment: Vec::new(),
        image_type: Some("android-sparse".into()),
        sector_size: None,
    };
    let (_, paths) = request.arguments(Some(destination))?;
    let source = PathBuf::from(&paths[0]);
    let out = canonical_output_path(destination)?;
    super::extents::guard_file_output(&source, &out)?;
    if out.try_exists()? {
        return Err(Error::Request(
            "select a new Android image directory".into(),
        ));
    }
    let parent = out
        .parent()
        .ok_or_else(|| Error::Request("expected a new output directory".into()))?;
    let mut input = Input {
        file: File::open(&source)?,
        digest: Sha256::new(),
    };
    let before = input.file.metadata()?;
    let stage = tempfile::tempdir_in(parent)?;
    let timestamp = crate::acquisition::now_ms()?;
    let mut decoder = Decoder {
        output: File::create(stage.path().join(IMAGE_NAME))?,
        logical: Sha256::new(),
        crc: crc32fast::Hasher::new(),
        crc_checks: 0,
        offset: 0,
        unrecorded: 0,
        records: Vec::new(),
        buffer: vec![0; BUFFER],
        timestamp,
    };
    decoder.decode(&mut input, cancel)?;
    let input_sha256 = format!("{:x}", input.digest.finalize());
    let mut map_file = File::create(stage.path().join(MAP_NAME))?;
    let mut map = ReadMap::create(
        &mut map_file,
        Header {
            schema_version: 2,
            id: uuid::Uuid::new_v4().to_string(),
            strategy: AcquisitionStrategy::Adaptive,
            source: AcquisitionIdentity {
                path: source,
                length: decoder.offset,
                sector_size: 512,
                identity: format!("android-sparse:{input_sha256}"),
                modified_ns: None,
            },
            block_size: BUFFER as u32,
            image_name: IMAGE_NAME.into(),
            created_unix_ms: timestamp,
        },
    )?;
    for record in decoder.records {
        cancelled(cancel)?;
        map.append(&mut map_file, record)?;
    }
    map_file.sync_all()?;
    let after = input.file.metadata()?;
    if before.len() != after.len() || before.modified()? != after.modified()? {
        return Err(Error::SourceChanged);
    }
    let report = ImportedFile {
        path: out.join(IMAGE_NAME),
        bytes: decoder.offset,
        sha256: format!("{:x}", decoder.logical.finalize()),
        ewf: None,
        android_sparse: Some(AndroidVerification {
            input_sha256,
            read_map: out.join(MAP_NAME),
            recorded_bytes: decoder.offset - decoder.unrecorded,
            unrecorded_bytes: decoder.unrecorded,
            crc32_checks: decoder.crc_checks,
        }),
    };
    drop(decoder.output);
    drop(map_file);
    cancelled(cancel)?;
    publish_directory(stage.path(), &out)?;
    // The staged tree now lives at the destination; disarm cleanup of its old path.
    let _ = stage.keep();
    Ok(report)
}
