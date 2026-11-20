//! Locate a supported XML document’s root end in the source encoding and validate the bounded document.
mod encoding;
use super::container::{Input, InputCursor};
use crate::{Error, Result};
use encoding::{Encoding, MarkupCursor};
use quick_xml::{Reader, events::Event};

pub(super) const MAX_BYTES: usize = 8 * 1024 * 1024;
const BOM: &[u8] = b"\xef\xbb\xbf";

pub(super) fn signature(bytes: &[u8]) -> bool {
    bytes
        .strip_prefix(BOM)
        .unwrap_or(bytes)
        .strip_prefix(b"<?xml")
        .and_then(|v| v.first())
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

fn boundary(input: &mut Input<'_>, bom: usize, encoding: Encoding) -> Result<usize> {
    // Start after the BOM so the event reader's offsets have the same meaning on every build.
    let mut reader = Reader::from_reader(MarkupCursor::new(
        InputCursor {
            limit: MAX_BYTES,
            input,
            cursor: bom,
            failure: None,
        },
        encoding,
    ));
    reader.config_mut().enable_all_checks(true);
    let mut event = Vec::new();
    let mut depth = 0usize;
    for index in 0..131072 {
        reader.get_mut().raw.input.check()?;
        let item = match reader.read_event_into(&mut event) {
            Ok(v) => v,
            Err(err) => {
                return Err(reader
                    .get_mut()
                    .raw
                    .failure
                    .take()
                    .unwrap_or_else(|| Error::Invalid(format!("XML: {err}"))));
            }
        };
        match item {
            Event::Decl(decl) if index == 0 => {
                let version = decl
                    .version()
                    .map_err(|e| Error::Invalid(format!("XML declaration: {e}")))?;
                if version.as_ref() != b"1.0"
                    || decl
                        .encoding()
                        .is_some_and(|e| !e.is_ok_and(|s| encoding.accepts(&s)))
                {
                    return Err(Error::Unsupported(
                        "XML declaration version or encoding differs from the source".into(),
                    ));
                }
            }
            Event::Decl(_) | Event::DocType(_) => {
                return Err(Error::Unsupported(
                    "multiple XML declarations or DTD".into(),
                ));
            }
            Event::Start(ref e) | Event::Empty(ref e) => {
                if e.attributes().take(4097).count() > 4096 {
                    return Err(Error::Limit("XML element exceeds 4096 attributes".into()));
                }
                if matches!(item, Event::Start(_)) {
                    depth += 1;
                    if depth > 256 {
                        return Err(Error::Limit("XML nesting exceeds 256".into()));
                    }
                } else if depth == 0 {
                    return Ok(reader.get_ref().raw.cursor);
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| Error::Invalid("unmatched XML end".into()))?;
                if depth == 0 {
                    return Ok(reader.get_ref().raw.cursor);
                }
            }
            Event::Eof => return Err(Error::Invalid("XML root end missing within 8 MiB".into())),
            _ => {}
        }
        event.clear();
    }
    Err(Error::Limit("XML event count exceeds 131072".into()))
}

/// Look only for an XML root whose local name is svg. Namespace identity is checked after parsing.
pub(super) fn svg_signature(bytes: &[u8]) -> bool {
    let bytes = bytes.strip_prefix(BOM).unwrap_or(bytes);
    let Some(rest) = bytes.strip_prefix(b"<") else {
        return false;
    };
    let token = rest
        .iter()
        .take(128)
        .position(|byte| byte.is_ascii_whitespace() || matches!(byte, b'/' | b'>'));
    let Some(end) = token else {
        return false;
    };
    let name = &rest[..end];
    name == b"svg"
        || name.strip_suffix(b":svg").is_some_and(|prefix| {
            !prefix.is_empty()
                && prefix
                    .iter()
                    .all(|c| c.is_ascii_alphanumeric() || matches!(c, b'_' | b'-' | b'.'))
        })
}

pub(super) fn utf16_signature(bytes: &[u8]) -> bool {
    let (encoding, bom) = Encoding::detect(bytes);
    if encoding == Encoding::Utf8 {
        return false;
    }
    let mut head = [0u8; 132];
    let count = head.len().min((bytes.len() - bom) / 2);
    for (result, pair) in head[..count]
        .iter_mut()
        .zip(bytes[bom..].as_chunks::<2>().0.iter())
    {
        *result = encoding.project(pair);
    }
    signature(&head[..count]) || svg_signature(&head[..count])
}

fn document(xml: &str) -> Result<roxmltree::Document<'_>> {
    let options = roxmltree::ParsingOptions {
        nodes_limit: 65536,
        allow_dtd: false,
        entity_resolver: None,
    };
    roxmltree::Document::parse_with_options(xml, options)
        .map_err(|e| Error::Invalid(format!("XML: {e}")))
}

fn svg_root(doc: &roxmltree::Document<'_>) -> bool {
    doc.root_element()
        .has_tag_name(("http://www.w3.org/2000/svg", "svg"))
}

/// A gzip member independently delimits its complete XML, including the prolog and epilog.
pub(super) fn complete_svg(bytes: &[u8]) -> bool {
    if bytes.len() > MAX_BYTES {
        return false;
    }
    let (encoding, bom) = Encoding::detect(bytes);
    let Ok(text) = encoding.decode(&bytes[bom..]) else {
        return false;
    };
    let mut reader = Reader::from_reader(text.as_bytes());
    let mut depth = 0usize;
    for i in 0..131072 {
        match reader.read_event() {
            Ok(Event::Decl(d)) if i == 0 => {
                if !d.version().is_ok_and(|v| v.as_ref() == b"1.0")
                    || d.encoding()
                        .is_some_and(|e| !e.is_ok_and(|v| encoding.accepts(&v)))
                {
                    return false;
                }
            }
            Ok(Event::Decl(_) | Event::DocType(_)) | Err(_) => return false,
            Ok(Event::Start(e)) => {
                depth += 1;
                if depth > 256 || e.attributes().take(4097).count() > 4096 {
                    return false;
                }
            }
            Ok(Event::Empty(e)) if e.attributes().take(4097).count() > 4096 => return false,
            Ok(Event::End(_)) => {
                let Some(n) = depth.checked_sub(1) else {
                    return false;
                };
                depth = n;
            }
            Ok(Event::Eof) => return depth == 0 && document(&text).is_ok_and(|doc| svg_root(&doc)),
            _ => {}
        }
    }
    false
}

pub(super) fn parse(input: &mut Input<'_>, svg_only: bool) -> Result<(usize, Vec<usize>, bool)> {
    input.ensure(6)?;
    let (encoding, bom) = Encoding::detect(&input.bytes);
    input.ensure(bom + 6)?;
    if !(signature(&input.bytes)
        || svg_only && (svg_signature(&input.bytes) || utf16_signature(&input.bytes)))
    {
        return Err(Error::Invalid("XML declaration or SVG root missing".into()));
    }
    let end = boundary(input, bom, encoding)?;
    input.work(end)?;
    let text = encoding.decode(&input.bytes[bom..end])?;
    let doc = document(&text)?;
    let svg = svg_root(&doc);
    if svg_only && !svg {
        return Err(Error::Invalid("SVG root namespace missing".into()));
    }
    let mut internal = Vec::new();
    if bom != 0 && encoding == Encoding::Utf8 {
        internal.push(bom);
    }
    if svg && encoding == Encoding::Utf8 {
        for (offset, &byte) in input.bytes[..end].iter().enumerate().skip(bom + 1) {
            if (byte == b'<' || byte == 0xef)
                && (svg_signature(&input.bytes[offset..end])
                    || signature(&input.bytes[offset..end]))
            {
                if internal.len() >= 65536 {
                    return Err(Error::Limit("SVG internal signature limit".into()));
                }
                internal.push(offset);
            }
        }
    }
    Ok((end, internal, svg))
}
