//! ZIP carving validates central/local records and every decoded entry CRC.
//! ZIP64, stored and DEFLATE are supported; encryption and split archives are not.
use super::{Format, container::*};
use crate::{Error, Result, source::Image};
use flate2::{Decompress, FlushDecompress, Status};
use std::{collections::BTreeSet, sync::atomic::AtomicBool};

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("ZIP: {reason}"))
}

#[derive(Debug)]
struct Entry {
    name: Vec<u8>,
    flags: u16,
    method: u16,
    crc: u32,
    compressed: usize,
    decoded: usize,
    local: usize,
}

/// Read ZIP64 fields in sentinel order; reject duplicate/truncated extra records.
fn extra64(extra: &[u8], values: &mut [u64], sentinels: &[u64]) -> Result<()> {
    let mut p = 0;
    let mut found = false;
    while p < extra.len() {
        if p + 4 > extra.len() {
            return Err(invalid("truncated extra field"));
        }
        let tag = u16le(extra, p);
        let length = usize::from(u16le(extra, p + 2));
        p += 4;
        if p + length > extra.len() {
            return Err(invalid("extra field length"));
        }
        if tag == 1 {
            if found {
                return Err(invalid("duplicate ZIP64 extra field"));
            }
            found = true;
            let mut q = p;
            for (v, sentinel) in values.iter_mut().zip(sentinels) {
                if *v == *sentinel {
                    let width = if *sentinel == 65535 { 4 } else { 8 };
                    if q + width > p + length {
                        return Err(invalid("missing ZIP64 value"));
                    }
                    *v = if width == 4 {
                        u64::from(u32le(extra, q))
                    } else {
                        u64le(extra, q)
                    };
                    q += width;
                }
            }
        }
        p += length;
    }
    if !found && values.iter().zip(sentinels).any(|(v, s)| v == s) {
        return Err(invalid("missing ZIP64 extra field"));
    }
    Ok(())
}

fn directory(input: &mut Input<'_>, eocd: usize) -> Result<(Vec<Entry>, usize, usize)> {
    input.ensure(eocd + 22)?;
    let b = &input.bytes;
    if u16le(b, eocd + 4) != 0 || u16le(b, eocd + 6) != 0 {
        return Err(Error::Unsupported("split ZIP archive".into()));
    }
    if u16le(b, eocd + 8) != u16le(b, eocd + 10) {
        return Err(invalid("entry counts disagree"));
    }
    let mut count = u64::from(u16le(b, eocd + 10));
    let mut size = u64::from(u32le(b, eocd + 12));
    let mut start = u64::from(u32le(b, eocd + 16));
    let end = eocd + 22 + usize::from(u16le(b, eocd + 20));
    let mut directory_end = eocd;
    if count == 65535 || size == u64::from(u32::MAX) || start == u64::from(u32::MAX) {
        if eocd < 20 || &b[eocd - 20..eocd - 16] != b"PK\x06\x07" {
            return Err(invalid("ZIP64 locator missing"));
        }
        if u32le(b, eocd - 16) != 0 || u32le(b, eocd - 4) != 1 {
            return Err(Error::Unsupported("split ZIP64 archive".into()));
        }
        let z = usize64(u64le(b, eocd - 12))?;
        if z.checked_add(56).is_none_or(|n| n > eocd - 20) || &b[z..z + 4] != b"PK\x06\x06" {
            return Err(invalid("ZIP64 end record"));
        }
        if u64le(b, z + 4).checked_add(z as u64 + 12) != Some((eocd - 20) as u64)
            || u64le(b, z + 4) < 44
        {
            return Err(invalid("ZIP64 end extent"));
        }
        if u32le(b, z + 16) != 0 || u32le(b, z + 20) != 0 || u64le(b, z + 24) != u64le(b, z + 32) {
            return Err(invalid("ZIP64 disks or counts"));
        }
        let actual = [u64le(b, z + 32), u64le(b, z + 40), u64le(b, z + 48)];
        for (old, new, sentinel) in [
            (count, actual[0], 65535),
            (size, actual[1], u64::from(u32::MAX)),
            (start, actual[2], u64::from(u32::MAX)),
        ] {
            if old != sentinel && old != new {
                return Err(invalid("ZIP64 end records disagree"));
            }
        }
        [count, size, start] = actual;
        directory_end = z;
    }
    if count > 4096 {
        return Err(Error::Limit("ZIP exceeds 4096 entries".into()));
    }
    let (start, size) = (usize64(start)?, usize64(size)?);
    if start.checked_add(size) != Some(directory_end) {
        return Err(invalid("central directory extent"));
    }
    input.ensure(end)?;
    let b = &input.bytes;
    let mut p = start;
    let mut entries = Vec::new();
    let mut total = 0u64;
    for _ in 0..count {
        if p + 46 > directory_end || &b[p..p + 4] != b"PK\x01\x02" {
            return Err(invalid("central entry"));
        }
        let name_len = usize::from(u16le(b, p + 28));
        let extra_len = usize::from(u16le(b, p + 30));
        let q = p + 46 + name_len + extra_len + usize::from(u16le(b, p + 32));
        if name_len == 0 || q > directory_end {
            return Err(invalid("central entry lengths"));
        }
        let mut values = [
            u64::from(u32le(b, p + 24)),
            u64::from(u32le(b, p + 20)),
            u64::from(u32le(b, p + 42)),
            u64::from(u16le(b, p + 34)),
        ];
        extra64(
            &b[p + 46 + name_len..p + 46 + name_len + extra_len],
            &mut values,
            &[
                u64::from(u32::MAX),
                u64::from(u32::MAX),
                u64::from(u32::MAX),
                65535,
            ],
        )?;
        if values[3] != 0 {
            return Err(Error::Unsupported("split ZIP member".into()));
        }
        let flags = u16le(b, p + 8);
        let method = u16le(b, p + 10);
        if flags & !0x080e != 0 || ![0, 8].contains(&method) {
            return Err(Error::Unsupported(
                "encrypted ZIP, entry flags or compression method".into(),
            ));
        }
        total = total
            .checked_add(values[0])
            .ok_or_else(|| invalid("decoded size overflow"))?;
        if total > 128 * 1024 * 1024 {
            return Err(Error::Limit("ZIP decoded total exceeds 128 MiB".into()));
        }
        entries.push(Entry {
            name: b[p + 46..p + 46 + name_len].to_vec(),
            flags,
            method,
            crc: u32le(b, p + 16),
            decoded: usize64(values[0])?,
            compressed: usize64(values[1])?,
            local: usize64(values[2])?,
        });
        p = q;
    }
    if p != directory_end {
        return Err(invalid("extra central directory bytes"));
    }
    Ok((entries, start, end))
}

fn validate_entries(input: &mut Input<'_>, entries: &[Entry], central: usize) -> Result<Format> {
    let mut spans = Vec::new();
    let mut names = BTreeSet::new();
    let mut types = Vec::new();
    for entry in entries {
        input.check()?;
        let p = entry.local;
        let b = &input.bytes;
        if p.checked_add(30).is_none_or(|n| n > central) || &b[p..p + 4] != b"PK\x03\x04" {
            return Err(invalid("local header outside file area"));
        }
        let name_len = usize::from(u16le(b, p + 26));
        let extra_len = usize::from(u16le(b, p + 28));
        let data = p + 30 + name_len + extra_len;
        let mut end = data
            .checked_add(entry.compressed)
            .ok_or_else(|| invalid("compressed extent overflow"))?;
        if end > central
            || b[p + 30..p + 30 + name_len] != entry.name
            || u16le(b, p + 6) != entry.flags
            || u16le(b, p + 8) != entry.method
        {
            return Err(invalid("local/central fields disagree"));
        }
        let mut sizes = [u64::from(u32le(b, p + 22)), u64::from(u32le(b, p + 18))];
        extra64(
            &b[p + 30 + name_len..data],
            &mut sizes,
            &[u64::from(u32::MAX), u64::from(u32::MAX)],
        )?;
        if entry.flags & 8 == 0 {
            if u32le(b, p + 14) != entry.crc
                || sizes != [entry.decoded as u64, entry.compressed as u64]
            {
                return Err(invalid("local sizes or CRC disagree"));
            }
        } else {
            if (u32le(b, p + 14) != 0 && u32le(b, p + 14) != entry.crc)
                || sizes
                    .iter()
                    .zip([entry.decoded, entry.compressed])
                    .any(|(a, n)| *a != 0 && *a != n as u64)
            {
                return Err(invalid("descriptor local fields disagree"));
            }
            let wide = u32le(b, p + 22) == u32::MAX || u32le(b, p + 18) == u32::MAX;
            let mut found = None;
            // A CRC may equal the optional signature; try both legitimate layouts.
            for signature in [false, true] {
                let q = end + if signature { 4 } else { 0 };
                let n = if wide { 20 } else { 12 };
                if q + n > central || (signature && &b[end..end + 4] != b"PK\x07\x08") {
                    continue;
                }
                let (compressed, decoded) = if wide {
                    (u64le(b, q + 4), u64le(b, q + 12))
                } else {
                    (u64::from(u32le(b, q + 4)), u64::from(u32le(b, q + 8)))
                };
                if u32le(b, q) == entry.crc
                    && compressed == entry.compressed as u64
                    && decoded == entry.decoded as u64
                {
                    found = Some(q + n);
                    break;
                }
            }
            end = found.ok_or_else(|| invalid("data descriptor mismatch"))?;
        }
        let mut crc = crc32fast::Hasher::new();
        let mut decoded = 0usize;
        let save_types = entry.name == b"[Content_Types].xml";
        if entry.method == 0 {
            if entry.compressed != entry.decoded {
                return Err(invalid("stored entry lengths"));
            }
            for piece in input.bytes[data..data + entry.compressed].chunks(8192) {
                input.check()?;
                crc.update(piece);
            }
            input.work(entry.decoded)?;
            decoded = entry.decoded;
            if save_types && decoded <= 1024 * 1024 {
                types.extend_from_slice(&input.bytes[data..data + decoded]);
            }
        } else {
            let mut stream = Decompress::new(false);
            let mut output = [0u8; 8192];
            loop {
                input.work(output.len())?;
                let before = (stream.total_in(), stream.total_out());
                let status = stream
                    .decompress(
                        &input.bytes[data + before.0 as usize..data + entry.compressed],
                        &mut output,
                        FlushDecompress::None,
                    )
                    .map_err(|_| invalid("DEFLATE stream"))?;
                let n = (stream.total_out() - before.1) as usize;
                decoded += n;
                if decoded > entry.decoded {
                    return Err(invalid("decoded size exceeded"));
                }
                crc.update(&output[..n]);
                if save_types && entry.decoded <= 1024 * 1024 {
                    types.extend_from_slice(&output[..n]);
                }
                if status == Status::StreamEnd {
                    if stream.total_in() != entry.compressed as u64 {
                        return Err(invalid("extra compressed bytes"));
                    }
                    break;
                }
                if before == (stream.total_in(), stream.total_out()) {
                    return Err(invalid("truncated DEFLATE stream"));
                }
            }
        }
        if decoded != entry.decoded || crc.finalize() != entry.crc {
            return Err(invalid("entry length or CRC mismatch"));
        }
        spans.push((p, end));
        names.insert(entry.name.as_slice());
    }
    spans.sort_unstable();
    let mut end = 0;
    for (a, b) in spans {
        if a != end {
            return Err(invalid(
                "overlapping entries or unknown data between local records",
            ));
        }
        end = b;
    }
    if end != central {
        return Err(invalid("unreferenced local records"));
    }
    Ok(office_type(&types, &names).unwrap_or(Format::Zip))
}

/// Namespace-aware classification, bounded to the already CRC-checked 1 MiB manifest.
/// A malformed or ambiguous manifest remains a ZIP, never a verified Office document.
fn office_type(types: &[u8], names: &BTreeSet<&[u8]>) -> Option<Format> {
    use quick_xml::{NsReader, events::Event, name::ResolveResult};
    let variants = [
        (
            "/word/document.xml",
            "application/vnd.openxmlformats-officedocument.wordprocessingml.document.main+xml",
            Format::Docx,
        ),
        (
            "/xl/workbook.xml",
            "application/vnd.openxmlformats-officedocument.spreadsheetml.sheet.main+xml",
            Format::Xlsx,
        ),
        (
            "/ppt/presentation.xml",
            "application/vnd.openxmlformats-officedocument.presentationml.presentation.main+xml",
            Format::Pptx,
        ),
    ];
    let mut reader = NsReader::from_reader(types);
    reader.config_mut().check_end_names = true;
    let mut depth = 0usize;
    let mut root = false;
    let mut complete = false;
    let mut matched = None;
    let mut parts = BTreeSet::new();
    for _ in 0..65536 {
        let (namespace, event) = reader.read_resolved_event().ok()?;
        let valid_namespace = matches!(namespace, ResolveResult::Bound(ns) if ns.as_ref() == b"http://schemas.openxmlformats.org/package/2006/content-types");
        match event {
            Event::Start(ref e) | Event::Empty(ref e) => {
                if !valid_namespace || complete {
                    return None;
                }
                if depth == 0 {
                    if root || e.local_name().as_ref() != b"Types" {
                        return None;
                    }
                    root = true;
                } else if depth == 1 && e.local_name().as_ref() == b"Override" {
                    let mut part = None;
                    let mut mime = None;
                    for attr in e.attributes() {
                        let attr = attr.ok()?;
                        let value = attr
                            .decoded_and_normalized_value(
                                quick_xml::XmlVersion::Implicit1_0,
                                reader.decoder(),
                            )
                            .ok()?
                            .into_owned();
                        match attr.key.as_ref() {
                            b"PartName" => part = Some(value),
                            b"ContentType" => mime = Some(value),
                            _ => {}
                        }
                    }
                    let (part, mime) = (part?, mime?);
                    if !parts.insert(part.clone()) {
                        return None;
                    }
                    for (path, content_type, format) in variants {
                        if part == path
                            && mime == content_type
                            && names.contains(&path.as_bytes()[1..])
                            && matched.replace(format).is_some()
                        {
                            return None;
                        }
                    }
                } else if depth != 1 || e.local_name().as_ref() != b"Default" {
                    return None;
                }
                if matches!(event, Event::Start(_)) {
                    depth += 1;
                    if depth > 2 {
                        return None;
                    }
                } else if depth == 0 {
                    complete = true;
                }
            }
            Event::End(_) => {
                depth = depth.checked_sub(1)?;
                if depth == 0 {
                    complete = true;
                }
            }
            Event::Text(t) if t.as_ref().iter().all(u8::is_ascii_whitespace) => {}
            Event::Decl(_) | Event::Comment(_) => {}
            Event::Eof => {
                return if root && complete && depth == 0 {
                    matched
                } else {
                    None
                };
            }
            _ => return None,
        }
    }
    None
}

pub(super) fn validate(
    image: &mut Image,
    at: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
    boundary: u64,
) -> Result<(Format, Vec<u8>, Vec<usize>)> {
    let mut input = Input::new(image, at, boundary, cancel, budget)?;
    input.ensure(4)?;
    if input.bytes[..4] != *b"PK\x03\x04" && input.bytes[..4] != *b"PK\x05\x06" {
        return Err(invalid("signature"));
    }
    let mut cursor = 0;
    let mut attempts = 0;
    let mut last = None;
    loop {
        input.check()?;
        if cursor + 4 > input.limit() {
            break;
        }
        input.ensure(cursor + 4)?;
        let stop = input.bytes.len().saturating_sub(3);
        while cursor < stop {
            if &input.bytes[cursor..cursor + 4] == b"PK\x05\x06" {
                attempts += 1;
                if attempts > 1024 {
                    return Err(Error::Limit("ZIP end record search attempts".into()));
                }
                match directory(&mut input, cursor).and_then(|(entries, start, end)| {
                    validate_entries(&mut input, &entries, start).map(|format| {
                        (
                            format,
                            end,
                            entries.iter().map(|e| e.local).collect::<Vec<_>>(),
                        )
                    })
                }) {
                    Ok((format, end, records)) => return Ok((format, input.finish(end)?, records)),
                    Err(e @ (Error::Cancelled | Error::Io(_) | Error::Limit(_))) => return Err(e),
                    Err(e) => last = Some(e),
                }
            }
            cursor += 1;
        }
    }
    Err(last.unwrap_or_else(|| {
        invalid("central directory / end record not found within selected range")
    }))
}
