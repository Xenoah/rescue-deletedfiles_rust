//! Carve a conservative subset of declared HTML5 using source byte offsets.
use super::container::{Input, InputCursor};
use crate::{Error, Result};
use html5gum::{DefaultEmitter, HtmlString, IoReader, StartTag, Token, Tokenizer};
use std::io::{self, BufRead, Read};

const BOM: &[u8] = b"\xef\xbb\xbf";
const MAX_BYTES: usize = 2 * 1024 * 1024;
const PADDING: usize = 16;

pub(super) fn signature(data: &[u8]) -> bool {
    let data = data.strip_prefix(BOM).unwrap_or(data);
    data.get(..9)
        .is_some_and(|s| s.eq_ignore_ascii_case(b"<!doctype"))
        && data.get(9).is_some_and(u8::is_ascii_whitespace)
}

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("HTML: {reason}"))
}

// A NUL is never legal in our UTF-8 subset. Let the tokenizer see EOF there, then
// independently verify the disk boundary if no explicit end tag was emitted.
struct HtmlReader<'a, 'b>(InputCursor<'a, 'b>);

impl Read for HtmlReader<'_, '_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        if self.0.failure.is_some() {
            return Ok(0);
        }
        let bytes = match self.0.fill_buf() {
            Ok(bytes) => bytes,
            // Let the tokenizer consume its last few buffered bytes. Its fast path
            // reads ahead even while processing the final '>' of an explicit end.
            Err(_) => return Ok(0),
        };
        let len = out.len().min(bytes.len());
        let n = bytes[..len].iter().position(|&b| b == 0).unwrap_or(len);
        out[..n].copy_from_slice(&bytes[..n]);
        self.0.consume(n);
        Ok(n)
    }
}

fn void(name: &[u8]) -> bool {
    matches!(
        name,
        b"area"
            | b"base"
            | b"br"
            | b"col"
            | b"embed"
            | b"hr"
            | b"img"
            | b"input"
            | b"link"
            | b"meta"
            | b"param"
            | b"source"
            | b"track"
            | b"wbr"
    )
}

fn check_meta(tag: &StartTag<usize>) -> Result<()> {
    let attr = |name: &[u8]| tag.attributes.get(name).map(|v| v.value.as_ref());
    if attr(b"charset").is_some_and(|v| !v.eq_ignore_ascii_case(b"utf-8")) {
        return Err(invalid("unsupported character encoding"));
    }
    if attr(b"http-equiv").is_some_and(|v| v.eq_ignore_ascii_case(b"content-type")) {
        let content = attr(b"content").ok_or_else(|| invalid("missing Content-Type"))?;
        let mut fields = content.split(|b| *b == b';');
        if !fields
            .next()
            .is_some_and(|v| v.trim_ascii().eq_ignore_ascii_case(b"text/html"))
        {
            return Err(invalid("unsupported Content-Type"));
        }
        for field in fields {
            let mut parts = field.splitn(2, |b| *b == b'=');
            if parts
                .next()
                .is_some_and(|key| key.trim_ascii().eq_ignore_ascii_case(b"charset"))
                && !parts
                    .next()
                    .is_some_and(|value| value.trim_ascii().eq_ignore_ascii_case(b"utf-8"))
            {
                return Err(invalid("unsupported Content-Type charset"));
            }
        }
    }
    Ok(())
}

fn boundary(reader: &mut HtmlReader<'_, '_>) -> Result<Option<usize>> {
    let mut emitter = DefaultEmitter::<usize>::new_with_span();
    emitter.naively_switch_states(true);
    let tokens = Tokenizer::new_with_emitter(IoReader::new(reader), emitter);
    let mut stack: Vec<HtmlString> = Vec::new();
    let mut saw_tag = false;
    let mut closed = false;
    let mut substantive_end = false;
    for (i, token) in tokens.enumerate() {
        if i >= 131072 {
            return Err(Error::Limit("HTML token limit exceeded".into()));
        }
        let token = token.map_err(|_| invalid("unreadable source"))?;
        if i == 0 {
            if matches!(token, Token::Doctype(d) if d.name == b"html" && !d.force_quirks && d.public_identifier.is_none() && d.system_identifier.is_none())
            {
                continue;
            }
            return Err(invalid("expected an HTML5 doctype"));
        }
        match token {
            Token::StartTag(tag) => {
                let name = tag.name.as_slice();
                if matches!(
                    name,
                    b"svg"
                        | b"math"
                        | b"template"
                        | b"noscript"
                        | b"plaintext"
                        | b"noframes"
                        | b"noframe"
                ) || name == b"html" && saw_tag
                    || name.len() > 256
                    || tag.attributes.len() > 1024
                    || stack.len() >= 256
                {
                    return Err(invalid("unsupported context or nesting"));
                }
                if name == b"meta" {
                    check_meta(&tag)?;
                }
                if !void(name) {
                    if tag.self_closing {
                        return Err(invalid("self-closing non-void element"));
                    }
                    stack.push(tag.name);
                }
                saw_tag = true;
                substantive_end = false;
            }
            Token::EndTag(tag) => {
                if stack.pop().as_ref() != Some(&tag.name) {
                    return Err(invalid("mismatched closing tag"));
                }
                if tag.name == b"html" {
                    return Ok(Some(tag.span.end));
                }
                closed = true;
                substantive_end = true;
            }
            Token::String(text) if text.iter().any(|b| !b.is_ascii_whitespace()) => {
                substantive_end = false;
            }
            Token::String(_) | Token::Comment(_) => {}
            _ => return Err(invalid("invalid token or repeated declaration")),
        }
    }
    if !closed || !substantive_end || !stack.is_empty() {
        return Err(invalid("missing complete document ending"));
    }
    Ok(None)
}

pub(super) fn parse(input: &mut Input<'_>, known_length: bool) -> Result<(usize, Vec<usize>)> {
    input.ensure(10)?;
    let bom = usize::from(input.bytes.starts_with(BOM)) * BOM.len();
    input.ensure(bom + 10)?;
    if !signature(&input.bytes) {
        return Err(invalid("missing doctype"));
    }
    let mut reader = HtmlReader(InputCursor {
        input,
        limit: MAX_BYTES,
        cursor: bom,
        failure: None,
    });
    let result = boundary(&mut reader);
    if let Some(err) = reader.0.failure.take()
        && !(matches!(err, Error::MissingRanges(_))
            && matches!(result, Ok(Some(end)) if end + bom <= reader.0.cursor))
    {
        return Err(err);
    }
    let end = match result? {
        Some(end) => bom + end,
        None => {
            let end = reader.0.cursor;
            if !(input.is_source_end(end) || known_length && end == input.limit()) {
                if end >= MAX_BYTES {
                    return Err(invalid("no boundary within 2 MiB"));
                }
                input.ensure(end + PADDING)?;
                if input.bytes[end..end + PADDING] != [0; PADDING] {
                    return Err(invalid("implicit ending needs zero padding or source EOF"));
                }
            }
            end
        }
    };
    input.ensure(end)?;
    input.work(end)?;
    std::str::from_utf8(&input.bytes[bom..end]).map_err(|_| invalid("invalid UTF-8"))?;
    Ok((end, if bom > 0 { vec![bom] } else { Vec::new() }))
}
