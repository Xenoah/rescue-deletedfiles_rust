//! Bound a UTF-8 JSON object or array by its grammar; surrounding whitespace is ambiguous.
use super::container::Input;
use crate::{Error, Result};

const BOM: &[u8] = b"\xef\xbb\xbf";
const LIMIT: usize = 4 << 20;
fn space(b: u8) -> bool {
    matches!(b, b' ' | b'\r' | b'\n' | b'\t')
}
fn trim_start(bytes: &[u8]) -> &[u8] {
    &bytes[bytes.iter().position(|&b| !space(b)).unwrap_or(bytes.len())..]
}

pub(super) fn signature(bytes: &[u8], explicit: bool) -> bool {
    // Make detection independent of a scan chunk boundary.
    let prefix = &bytes[..bytes.len().min(40)];
    let b = prefix.strip_prefix(BOM).map(trim_start).unwrap_or(prefix);
    let Some((&open, body)) = b.split_first() else {
        return false;
    };
    let inner = trim_start(body);
    if explicit {
        return match open {
            b'{' => matches!(inner.first(), Some(b'"' | b'}')),
            b'[' => match inner.first() {
                Some(b't' | b'f' | b'n') => [b"true".as_slice(), b"false", b"null"]
                    .iter()
                    .any(|literal| inner.starts_with(literal) || literal.starts_with(inner)),
                Some(b']' | b'[' | b'{' | b'"' | b'-' | b'0'..=b'9') => true,
                _ => false,
            },
            _ => false,
        };
    }
    // An automatically discovered object needs a quoted, nonempty first key
    // followed by a colon. The explicit filter also accepts shorter empty values
    // and arrays of primitive values, which occur frequently in unrelated data.
    let object = match open {
        b'{' => inner,
        b'[' if inner.starts_with(b"{") => trim_start(&inner[1..]),
        _ => return false,
    };
    let Some(key) = object.strip_prefix(b"\"") else {
        return false;
    };
    let mut escaped = false;
    for (i, &v) in key.iter().enumerate() {
        if v < 32 {
            return false;
        }
        if escaped {
            escaped = false;
        } else if v == b'\\' {
            escaped = true;
        } else if v == b'"' {
            return i > 0 && trim_start(&key[i + 1..]).starts_with(b":");
        }
    }
    false
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(2)?;
    let bom = if input.bytes.starts_with(BOM) {
        BOM.len()
    } else {
        0
    };
    let mut pos = bom;
    while pos < LIMIT {
        input.ensure(pos + 1)?;
        if !space(input.bytes[pos]) {
            break;
        }
        pos += 1;
    }
    if pos >= LIMIT || !matches!(input.bytes.get(pos), Some(b'{' | b'[')) {
        return Err(Error::Invalid("JSON object or array expected".into()));
    }
    let mut stack = Vec::new();
    let mut quoted = false;
    let mut escape = false;
    let mut tokens = 0;
    while pos < LIMIT {
        if pos.is_multiple_of(4096) {
            input.check()?;
        }
        input.ensure(pos + 1)?;
        let b = input.bytes[pos];
        pos += 1;
        if quoted {
            if escape {
                escape = false;
            } else if b == b'\\' {
                escape = true;
            } else if b == b'"' {
                quoted = false;
            }
            continue;
        }
        match b {
            b'"' => quoted = true,
            b'{' | b'[' => {
                stack.push(if b == b'{' { b'}' } else { b']' });
                if stack.len() > 120 {
                    return Err(Error::Limit("JSON exceeds 120 nesting levels".into()));
                }
                tokens += 1;
            }
            b',' | b':' => tokens += 1,
            b'}' | b']' => {
                if stack.pop() != Some(b) {
                    return Err(Error::Invalid("JSON closing delimiter".into()));
                }
                if stack.is_empty() {
                    input.work(pos)?;
                    serde_json::from_slice::<serde_json::Value>(&input.bytes[bom..pos])
                        .map_err(|e| Error::Invalid(format!("JSON: {e}")))?;
                    return Ok(pos);
                }
            }
            _ => {}
        }
        if tokens > 65536 {
            return Err(Error::Limit("JSON exceeds 65536 structural tokens".into()));
        }
    }
    Err(Error::Limit("JSON exceeds 4 MiB".into()))
}
