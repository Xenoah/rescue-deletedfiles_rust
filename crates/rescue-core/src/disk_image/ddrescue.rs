//! Import GNU ddrescue data blocks in original input coordinates.
//! Only finished blocks may supply bytes to a later recovery.
use super::{ImageInput, ImportedFile};
use crate::{
    AcquisitionIdentity, Error, Result, canonical_output_path,
    read_map::{AcquisitionStrategy, Header, IMAGE_NAME, MAP_NAME, ReadMap, ReadState, Segment},
};
use serde::Serialize;
use sha2::{Digest, Sha256};
use std::{
    fs::{self, File},
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

const MAX_MAP_BYTES: u64 = 32 << 20;
const MAX_ROWS: usize = 1_000_000;
const MAX_LOGICAL: u64 = 16 << 40;

#[derive(Debug, Default, Clone, Copy)]
pub struct DdrescueLayout {
    pub logical_bytes: Option<u64>,
    pub input_position: u64,
    pub output_position: u64,
}

#[derive(Debug, Serialize)]
pub struct DdrescueVerification {
    pub read_map: PathBuf,
    pub map_sha256: String,
    pub input_position: u64,
    pub output_position: u64,
    pub finished_bytes: u64,
    pub nontrimmed_bytes: u64,
    pub nonscraped_bytes: u64,
    pub bad_sector_bytes: u64,
    pub untried_bytes: u64,
    pub outside_map_bytes: u64,
}

struct Block {
    start: u64,
    end: u64,
    status: u8,
}

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("GNU ddrescue map: {reason}"))
}

fn number(field: &str) -> Result<u64> {
    let field = field.strip_prefix('+').unwrap_or(field);
    let (digits, radix) = if let Some(hex) = field
        .strip_prefix("0x")
        .or_else(|| field.strip_prefix("0X"))
    {
        (hex, 16)
    } else if field.starts_with('0') {
        (field, 8)
    } else {
        (field, 10)
    };
    if digits.starts_with('+') {
        return Err(invalid("invalid byte position or size"));
    }
    u64::from_str_radix(digits, radix).map_err(|_| invalid("invalid byte position or size"))
}

fn parse(bytes: &[u8], length: u64) -> Result<Vec<Block>> {
    let mut blocks: Vec<Block> = Vec::new();
    let mut header = false;
    for line in bytes.split(|&b| b == b'\n') {
        let comment = line
            .iter()
            .enumerate()
            .find(|&(i, b)| *b == b'#' && (i == 0 || line[i - 1].is_ascii_whitespace()))
            .map_or(line.len(), |(i, _)| i);
        let text =
            std::str::from_utf8(&line[..comment]).map_err(|_| invalid("invalid data line"))?;
        let mut fields = text.split_ascii_whitespace();
        let Some(first) = fields.next() else {
            continue;
        };
        let start = number(first)?;
        let second = fields
            .next()
            .ok_or_else(|| invalid("missing status or size"))?;
        let third = fields.next();
        if fields.next().is_some() {
            return Err(invalid("unexpected map column"));
        }
        if !header {
            if !matches!(second, "?" | "*" | "/" | "-" | "+")
                || third.is_some_and(|s| {
                    !s.bytes().all(|b| b.is_ascii_digit()) || !s.parse::<u64>().is_ok_and(|v| v > 0)
                })
            {
                return Err(invalid(
                    "expected an acquisition status line and a positive pass number; fill and generate phases are unsupported",
                ));
            }
            header = true;
            continue;
        }
        let status = match third {
            Some(s @ ("+" | "?" | "*" | "/" | "-")) => s.as_bytes()[0],
            _ => return Err(invalid("unknown block status")),
        };
        let size = number(second)?;
        let end = start
            .checked_add(size)
            .filter(|&end| end <= length)
            .ok_or_else(|| invalid("block exceeds logical length"))?;
        if size == 0 || blocks.last().is_some_and(|b| b.end != start) {
            return Err(invalid(
                "expected positive contiguous non-overlapping blocks",
            ));
        }
        if !start.is_multiple_of(512) || (end != length && !end.is_multiple_of(512)) {
            return Err(invalid(
                "interior block boundaries must align to 512-byte sectors",
            ));
        }
        if blocks.len() >= MAX_ROWS {
            return Err(Error::Limit("ddrescue block count".into()));
        }
        blocks.push(Block { start, end, status });
    }
    if blocks.is_empty() {
        return Err(invalid("no data blocks"));
    }
    Ok(blocks)
}

fn check_cancel(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        Err(Error::Cancelled)
    } else {
        Ok(())
    }
}

fn physical(pos: u64, layout: DdrescueLayout) -> Result<u64> {
    u64::try_from(
        i128::from(pos) + i128::from(layout.output_position) - i128::from(layout.input_position),
    )
    .map_err(|_| invalid("finished range is outside the capture offset mapping"))
}

pub fn import_ddrescue(
    source: &Path,
    map_path: &Path,
    destination: &Path,
    layout: DdrescueLayout,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    check_cancel(cancel)?;
    let out = canonical_output_path(destination)?;
    let selection = ImageInput {
        source: source.to_owned(),
        segment: vec![map_path.to_owned()],
        image_type: Some("ddrescue".into()),
        sector_size: None,
    };
    let (_, paths) = selection.arguments(Some(&out))?;
    let source = Path::new(&paths[0]);
    let map_path = Path::new(&paths[1]);
    for path in [source, map_path] {
        super::extents::guard_file_output(path, &out)?;
    }
    if same_file::is_same_file(source, map_path)? {
        return Err(Error::Request(
            "capture and ddrescue map must be distinct files".into(),
        ));
    }
    if out.try_exists()? {
        return Err(Error::Request("select a new destination directory".into()));
    }
    let mut capture = File::open(source)?;
    let mut input_map = File::open(map_path)?;
    let capture_before = capture.metadata()?;
    let map_before = input_map.metadata()?;
    if layout.input_position != layout.output_position && layout.logical_bytes.is_none() {
        return Err(Error::Request(
            "supply the original logical byte length for a shifted capture".into(),
        ));
    }
    let length = layout.logical_bytes.unwrap_or(capture_before.len());
    if length == 0 || length > MAX_LOGICAL {
        return Err(invalid(
            "logical disk length must be positive and at most 16 TiB",
        ));
    }
    if map_before.len() > MAX_MAP_BYTES {
        return Err(Error::Limit("ddrescue map exceeds 32 MiB".into()));
    }
    let mut bytes = Vec::new();
    (&mut input_map)
        .take(MAX_MAP_BYTES + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 != map_before.len() {
        return Err(Error::SourceChanged);
    }
    let mut blocks = parse(&bytes, length)?;
    for block in blocks.iter().filter(|b| b.status == b'+') {
        physical(block.start, layout)?;
        if physical(block.end, layout)? > capture_before.len() {
            return Err(invalid("finished block exceeds capture length"));
        }
    }
    let map_sha256 = format!("{:x}", Sha256::digest(&bytes));
    let first = blocks[0].start;
    let last = blocks.last().unwrap().end;
    let mut proof = DdrescueVerification {
        read_map: out.join(MAP_NAME),
        map_sha256,
        input_position: layout.input_position,
        output_position: layout.output_position,
        finished_bytes: 0,
        nontrimmed_bytes: 0,
        nonscraped_bytes: 0,
        bad_sector_bytes: 0,
        untried_bytes: 0,
        outside_map_bytes: first + (length - last),
    };
    if first != 0 {
        blocks.insert(
            0,
            Block {
                start: 0,
                end: first,
                status: b' ',
            },
        );
    }
    if last < length {
        blocks.push(Block {
            start: last,
            end: length,
            status: b' ',
        });
    }
    let parent = out
        .parent()
        .ok_or_else(|| Error::Request("expected a new directory".into()))?;
    let stage = tempfile::tempdir_in(parent)?;
    let mut saved_map = File::create(stage.path().join("ddrescue.map"))?;
    saved_map.write_all(&bytes)?;
    saved_map.sync_all()?;
    drop(saved_map);
    let mut output = File::create(stage.path().join(IMAGE_NAME))?;
    let mut map_file = File::create(stage.path().join(MAP_NAME))?;
    let timestamp = crate::acquisition::now_ms()?;
    let block_size: usize = if length > 128 << 30 {
        16 << 20
    } else {
        1 << 20
    };
    let mut map = ReadMap::create(
        &mut map_file,
        Header {
            schema_version: 2,
            id: uuid::Uuid::new_v4().to_string(),
            strategy: AcquisitionStrategy::Adaptive,
            source: AcquisitionIdentity {
                path: source.to_owned(),
                length,
                sector_size: 512,
                identity: format!(
                    "gnu-ddrescue:{}:{}:{}",
                    proof.map_sha256, layout.input_position, layout.output_position
                ),
                modified_ns: None,
            },
            block_size: block_size as u32,
            image_name: IMAGE_NAME.into(),
            created_unix_ms: timestamp,
        },
    )?;
    let mut records = Vec::new();
    let mut buffer = vec![0u8; block_size];
    let mut logical = Sha256::new();
    for block in blocks {
        let size = block.end - block.start;
        let state = match block.status {
            b'+' => {
                proof.finished_bytes += size;
                capture.seek(SeekFrom::Start(physical(block.start, layout)?))?;
                output.seek(SeekFrom::Start(block.start))?;
                ReadState::Copied
            }
            b'*' => {
                proof.nontrimmed_bytes += size;
                ReadState::Failed
            }
            b'/' => {
                proof.nonscraped_bytes += size;
                ReadState::Failed
            }
            b'-' => {
                proof.bad_sector_bytes += size;
                ReadState::Failed
            }
            b'?' => {
                proof.untried_bytes += size;
                ReadState::Unattempted
            }
            _ => ReadState::Unattempted,
        };
        if state != ReadState::Copied {
            buffer[..size.min(block_size as u64) as usize].fill(0);
        }
        let mut offset = block.start;
        while offset < block.end {
            check_cancel(cancel)?;
            let n = (block.end - offset).min(block_size as u64) as usize;
            let chunk = &mut buffer[..n];
            let mut sha256 = [0; 32];
            if state == ReadState::Copied {
                capture.read_exact(chunk)?;
                super::write_sparse_chunk(&mut output, chunk)?;
                sha256 = Sha256::digest(&*chunk).into();
            }
            if state != ReadState::Unattempted {
                if records.len() >= MAX_ROWS {
                    return Err(Error::Limit("ddrescue read-map record count".into()));
                }
                records.push(Segment {
                    offset,
                    length: n as u64,
                    state,
                    attempts: 1,
                    sha256,
                    error_code: 0,
                    read_unix_ms: timestamp,
                });
            }
            logical.update(&*chunk);
            offset += n as u64;
        }
    }
    output.set_len(length)?;
    output.sync_all()?;
    for record in records {
        check_cancel(cancel)?;
        map.append(&mut map_file, record)?;
    }
    for (path, before) in [(source, capture_before), (map_path, map_before)] {
        let after = fs::metadata(path)?;
        if after.len() != before.len() || before.modified()? != after.modified()? {
            return Err(Error::SourceChanged);
        }
    }
    drop(map_file);
    drop(output);
    check_cancel(cancel)?;
    super::publish_directory(stage.path(), &out)?;
    let _ = stage.keep();
    Ok(ImportedFile {
        path: out.join(IMAGE_NAME),
        bytes: length,
        sha256: format!("{:x}", logical.finalize()),
        ewf: None,
        raw: None,
        android_sparse: None,
        ddrescue: Some(proof),
    })
}
