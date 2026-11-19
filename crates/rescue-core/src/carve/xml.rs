//! Delimit a declared UTF-8 XML document at its root end, then validate the bounded document.
use super::container::Input;
use crate::{Error, Result};
use quick_xml::{Reader, events::Event};
use std::io::{self, BufRead, Read};

const MAX_BYTES: usize = 8 * 1024 * 1024;
const BOM: &[u8] = b"\xef\xbb\xbf";

pub(super) fn signature(bytes: &[u8]) -> bool {
    bytes
        .strip_prefix(BOM)
        .unwrap_or(bytes)
        .strip_prefix(b"<?xml")
        .and_then(|v| v.first())
        .is_some_and(|b| matches!(b, b' ' | b'\t' | b'\r' | b'\n'))
}

struct Source<'a, 'b> {
    input: &'a mut Input<'b>,
    cursor: usize,
    failure: Option<Error>,
}
impl BufRead for Source<'_, '_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let limit = self.input.limit().min(MAX_BYTES);
        if self.cursor >= limit {
            return Ok(&[]);
        }
        if self.cursor == self.input.bytes.len()
            && let Err(err) = self.input.ensure(self.cursor + 1)
        {
            self.failure = Some(err);
            return Err(io::Error::other("cannot read XML input"));
        }
        Ok(&self.input.bytes[self.cursor..self.input.bytes.len().min(limit)])
    }
    fn consume(&mut self, n: usize) {
        self.cursor += n;
    }
}
impl Read for Source<'_, '_> {
    fn read(&mut self, dest: &mut [u8]) -> io::Result<usize> {
        let n = self.fill_buf()?.read(dest)?;
        self.consume(n);
        Ok(n)
    }
}

fn boundary(input: &mut Input<'_>, bom: usize) -> Result<usize> {
    // Start after the BOM so the event reader's offsets have the same meaning on every build.
    let mut reader = Reader::from_reader(Source {
        input,
        cursor: bom,
        failure: None,
    });
    reader.config_mut().enable_all_checks(true);
    let mut event = Vec::new();
    let mut depth = 0usize;
    for index in 0..131072 {
        reader.get_mut().input.check()?;
        let item = match reader.read_event_into(&mut event) {
            Ok(v) => v,
            Err(err) => {
                return Err(reader
                    .get_mut()
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
                        .is_some_and(|e| !e.is_ok_and(|s| s.eq_ignore_ascii_case(b"utf-8")))
                {
                    return Err(Error::Unsupported(
                        "XML carving requires XML 1.0 and UTF-8".into(),
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
                    return Ok(reader.get_ref().cursor);
                }
            }
            Event::End(_) => {
                depth = depth
                    .checked_sub(1)
                    .ok_or_else(|| Error::Invalid("unmatched XML end".into()))?;
                if depth == 0 {
                    return Ok(reader.get_ref().cursor);
                }
            }
            Event::Eof => return Err(Error::Invalid("XML root end missing within 8 MiB".into())),
            _ => {}
        }
        event.clear();
    }
    Err(Error::Limit("XML event count exceeds 131072".into()))
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<(usize, Vec<usize>)> {
    input.ensure(6)?;
    let bom = if input.bytes.starts_with(BOM) { 3 } else { 0 };
    input.ensure(bom + 6)?;
    if !signature(&input.bytes) {
        return Err(Error::Invalid("XML declaration missing".into()));
    }
    let end = boundary(input, bom)?;
    input.work(end)?;
    let xml = std::str::from_utf8(&input.bytes[bom..end])
        .map_err(|_| Error::Invalid("invalid UTF-8 XML".into()))?;
    let options = roxmltree::ParsingOptions {
        nodes_limit: 65536,
        allow_dtd: false,
        entity_resolver: None,
    };
    roxmltree::Document::parse_with_options(xml, options)
        .map_err(|e| Error::Invalid(format!("XML: {e}")))?;
    Ok((end, if bom == 0 { Vec::new() } else { vec![bom] }))
}
