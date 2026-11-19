//! Read single-segment, XML block-map UDIF disk images with complete sector coverage.
mod codec;
use super::{ImportedFile, publish_output};
use crate::{Error, Result};
use base64::{Engine, engine::general_purpose::STANDARD};
use crc32fast::Hasher;
use roxmltree::Node;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const ADC: u32 = 0x8000_0004;
const ZLIB: u32 = 0x8000_0005;
const BZIP: u32 = 0x8000_0006;
const COMMENT: u32 = 0x7fff_fffe;
const END: u32 = u32::MAX;
const MAX_PACKED_RUN: u64 = 64 << 20;
const MAX_SECTORS: u64 = (16u64 << 40) / 512;

fn invalid(msg: &str) -> Error {
    Error::Invalid(format!("UDIF: {msg}"))
}
fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_be_bytes(b[at..at + 4].try_into().unwrap())
}
fn u64_at(b: &[u8], at: usize) -> u64 {
    u64::from_be_bytes(b[at..at + 8].try_into().unwrap())
}
fn end(start: u64, length: u64, limit: u64) -> Result<u64> {
    start
        .checked_add(length)
        .filter(|&v| v <= limit)
        .ok_or_else(|| invalid("range outside container or disk"))
}
fn check(cancel: &AtomicBool) -> Result<()> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    Ok(())
}

fn checksum(bytes: &[u8], at: usize) -> Result<Option<u32>> {
    match (u32_at(bytes, at), u32_at(bytes, at + 4)) {
        (0, 0) => Ok(None),
        (2, 32) => Ok(Some(u32_at(bytes, at + 8))),
        _ => Err(Error::Unsupported(
            "UDIF checksum algorithm or width".into(),
        )),
    }
}
fn verify(expected: Option<u32>, actual: u32) -> Result<()> {
    if expected.is_some_and(|v| v != actual) {
        return Err(invalid("CRC-32 mismatch"));
    }
    Ok(())
}

struct Run {
    kind: u32,
    logical_bytes: u64,
    offset: u64,
    packed: u64,
}
struct Table {
    start: u64,
    sectors: u64,
    crc: Option<u32>,
    runs: Vec<Run>,
}

impl Table {
    fn parse(b: &[u8], fork_start: u64, fork_size: u64, sectors: u64) -> Result<Self> {
        if b.len() < 204 || &b[..4] != b"mish" || u32_at(b, 4) != 1 {
            return Err(invalid("mish header"));
        }
        let count = u32_at(b, 200) as usize;
        if count == 0 || count > 262144 || b.len() - 204 != count * 40 {
            return Err(invalid("block run count"));
        }
        let start = u64_at(b, 8);
        let size = u64_at(b, 16);
        end(start, size, sectors)?;
        let base = u64_at(b, 24);
        let mut position = 0;
        let mut runs = Vec::new();
        for (i, row) in b[204..].as_chunks::<40>().0.iter().enumerate() {
            let kind = u32_at(row, 0);
            let at = u64_at(row, 8);
            let n = u64_at(row, 16);
            let packed = u64_at(row, 32);
            if kind == COMMENT || kind == END {
                if n != 0 || packed != 0 || (kind == END && i + 1 != count) {
                    return Err(invalid("comment or end marker"));
                }
                continue;
            }
            if i + 1 == count || n == 0 || at != position {
                return Err(invalid("gap, overlap or missing end marker"));
            }
            position = end(at, n, size)?;
            let logical_bytes = n * 512;
            let offset = end(base, u64_at(row, 24), fork_size)?;
            end(offset, packed, fork_size)?;
            match kind {
                0 | 2 if packed == 0 => {}
                1 if logical_bytes == packed => {}
                ADC | ZLIB | BZIP
                    if packed > 0
                        && packed <= MAX_PACKED_RUN
                        && logical_bytes <= MAX_PACKED_RUN => {}
                0 | 1 | 2 | ADC | ZLIB | BZIP => {
                    return Err(invalid("invalid or oversized data run"));
                }
                _ => {
                    return Err(Error::Unsupported(format!(
                        "UDIF compression type {kind:#010x}"
                    )));
                }
            }
            runs.push(Run {
                kind,
                logical_bytes,
                offset: fork_start + offset,
                packed,
            });
        }
        if position != size || size == 0 || u32_at(b, b.len() - 40) != END {
            return Err(invalid("incomplete block table"));
        }
        Ok(Self {
            start,
            sectors: size,
            crc: checksum(b, 64)?,
            runs,
        })
    }
}

fn property<'a>(dict: Node<'a, 'a>, name: &str) -> Result<Node<'a, 'a>> {
    if !dict.has_tag_name("dict") {
        return Err(invalid("plist dictionary expected"));
    }
    let mut elements = dict.children().filter(|n| n.is_element());
    let mut value = None;
    while let Some(key) = elements.next() {
        let item = elements
            .next()
            .ok_or_else(|| invalid("plist key without value"))?;
        if !key.has_tag_name("key") {
            return Err(invalid("plist key expected"));
        }
        if key.text() == Some(name) && value.replace(item).is_some() {
            return Err(invalid("duplicate plist property"));
        }
    }
    value.ok_or_else(|| invalid("missing plist property"))
}

fn tables(
    xml: &str,
    fork_start: u64,
    fork_size: u64,
    sectors: u64,
    master: Option<u32>,
) -> Result<Vec<Table>> {
    // The standard Apple external plist declaration is common. External entities
    // have no resolver, and internal declarations are unnecessary for this format.
    if xml.contains("<!ENTITY") {
        return Err(invalid("plist entity declaration"));
    }
    let doc = roxmltree::Document::parse_with_options(
        xml,
        roxmltree::ParsingOptions {
            nodes_limit: 65536,
            allow_dtd: true,
            entity_resolver: None,
        },
    )
    .map_err(|e| invalid(&e.to_string()))?;
    let plist = doc.root_element();
    if !plist.has_tag_name("plist") {
        return Err(invalid("plist root"));
    }
    let dict = plist
        .first_element_child()
        .ok_or_else(|| invalid("plist root dictionary"))?;
    let blocks = property(property(dict, "resource-fork")?, "blkx")?;
    if !blocks.has_tag_name("array") {
        return Err(invalid("blkx array"));
    }
    let mut result = Vec::new();
    let mut crc = Hasher::new();
    for item in blocks.children().filter(|n| n.is_element()) {
        let data = property(item, "Data")?;
        if !data.has_tag_name("data") || data.children().any(|n| n.is_element()) {
            return Err(invalid("block map data"));
        }
        let encoded: Vec<_> = data
            .text()
            .unwrap_or_default()
            .bytes()
            .filter(|b| !b.is_ascii_whitespace())
            .collect();
        let bytes = STANDARD
            .decode(encoded)
            .map_err(|_| invalid("base64 block map"))?;
        let table = Table::parse(&bytes, fork_start, fork_size, sectors)?;
        if let Some(v) = table.crc {
            crc.update(&v.to_be_bytes());
        }
        result.push(table);
    }
    verify(master, crc.finalize())?;
    let mut physical: Vec<_> = result
        .iter()
        .flat_map(|t| &t.runs)
        .filter(|r| r.packed > 0)
        .map(|r| (r.offset, r.offset + r.packed))
        .collect();
    physical.sort_unstable();
    if physical.windows(2).any(|r| r[0].1 > r[1].0) {
        return Err(invalid("overlapping encoded runs"));
    }
    result.sort_by_key(|t| t.start);
    let mut position = 0;
    for table in &result {
        if table.start != position {
            return Err(invalid("partition maps overlap or omit sectors"));
        }
        position += table.sectors;
    }
    if position != sectors {
        return Err(invalid("disk sector count differs from block maps"));
    }
    Ok(result)
}

fn write_tables(
    source: &mut File,
    output: &mut File,
    layout: &[Table],
    cancel: &AtomicBool,
) -> Result<(u64, Sha256)> {
    let mut digest = Sha256::new();
    let mut buffer = vec![0u8; 256 << 10];
    let zeros = vec![0u8; buffer.len()];
    let mut bytes = 0;
    for table in layout {
        let mut crc = Hasher::new();
        for run in &table.runs {
            check(cancel)?;
            source.seek(SeekFrom::Start(run.offset))?;
            if matches!(run.kind, ADC | ZLIB | BZIP) {
                let mut packed = vec![0; run.packed as usize];
                source.read_exact(&mut packed)?;
                let decoded = codec::decode(run.kind, &packed, run.logical_bytes as usize)?;
                for chunk in decoded.chunks(buffer.len()) {
                    check(cancel)?;
                    crc.update(chunk);
                    digest.update(chunk);
                    output.write_all(chunk)?;
                }
            } else {
                let mut remaining = run.logical_bytes;
                let zero = run.kind != 1;
                if zero {
                    output.seek(SeekFrom::Start(bytes + remaining))?;
                }
                while remaining > 0 {
                    check(cancel)?;
                    let n = remaining.min(buffer.len() as u64) as usize;
                    let buf = if zero {
                        &zeros[..n]
                    } else {
                        source.read_exact(&mut buffer[..n])?;
                        output.write_all(&buffer[..n])?;
                        &buffer[..n]
                    };
                    crc.update(buf);
                    digest.update(buf);
                    remaining -= n as u64;
                }
            }
            bytes += run.logical_bytes;
        }
        verify(table.crc, crc.finalize())?;
    }
    output.set_len(bytes)?;
    Ok((bytes, digest))
}

pub(super) fn import(
    path: &Path,
    requested: Option<&str>,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if requested.is_some_and(|v| !matches!(v, "udif" | "dmg")) {
        return Ok(None);
    }
    let mut file = File::open(path)?;
    let before = file.metadata()?;
    let mut footer = [0; 512];
    if before.len() >= 512 {
        file.seek(SeekFrom::End(-512))?;
        file.read_exact(&mut footer)?;
    }
    if &footer[..4] != b"koly" {
        return if requested.is_some() {
            Err(invalid("missing koly trailer"))
        } else {
            Ok(None)
        };
    }
    if u32_at(&footer, 4) != 4
        || u32_at(&footer, 8) != 512
        || u64_at(&footer, 16) != 0
        || u32_at(&footer, 56) > 1
        || u32_at(&footer, 60) > 1
        || u64_at(&footer, 48) != 0
    {
        return Err(Error::Unsupported(
            "UDIF requires a single segment and an XML block map".into(),
        ));
    }
    let sectors = u64_at(&footer, 492);
    if !(1..=MAX_SECTORS).contains(&sectors) {
        return Err(invalid("missing or excessive logical sector count"));
    }
    let fork_start = u64_at(&footer, 24);
    let fork_size = u64_at(&footer, 32);
    let xml_start = u64_at(&footer, 216);
    let xml_size = u64_at(&footer, 224);
    if !(1..=16 << 20).contains(&xml_size) {
        return Err(invalid("missing or oversized XML property list"));
    }
    let limit = before.len() - 512;
    let fork_end = end(fork_start, fork_size, limit)?;
    let xml_end = end(xml_start, xml_size, limit)?;
    if fork_size > 0 && fork_start < xml_end && xml_start < fork_end {
        return Err(invalid("data fork overlaps XML"));
    }
    file.seek(SeekFrom::Start(xml_start))?;
    let mut xml = vec![0; xml_size as usize];
    file.read_exact(&mut xml)?;
    let layout = tables(
        std::str::from_utf8(&xml).map_err(|_| invalid("plist is not UTF-8 XML"))?,
        fork_start,
        fork_size,
        sectors,
        checksum(&footer, 352)?,
    )?;
    let result = publish_output(output, cancel, |out| {
        if let Some(expected) = checksum(&footer, 80)? {
            file.seek(SeekFrom::Start(fork_start))?;
            let mut crc = Hasher::new();
            let mut buf = vec![0; 256 << 10];
            let mut left = fork_size;
            while left > 0 {
                check(cancel)?;
                let n = left.min(buf.len() as u64) as usize;
                file.read_exact(&mut buf[..n])?;
                crc.update(&buf[..n]);
                left -= n as u64;
            }
            verify(Some(expected), crc.finalize())?;
        }
        let result = write_tables(&mut file, out, &layout, cancel)?;
        let after = file.metadata()?;
        if before.len() != after.len() || before.modified()? != after.modified()? {
            return Err(Error::SourceChanged);
        }
        Ok(result)
    })?;
    Ok(Some(result))
}
