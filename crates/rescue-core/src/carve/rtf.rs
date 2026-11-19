//! RTF group boundaries, escaped symbols and counted binary data.
use super::container::Input;
use crate::{Error, Result};

fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("RTF: {reason}"))
}

fn byte(input: &mut Input<'_>, pos: usize) -> Result<u8> {
    input.ensure(pos + 1)?;
    Ok(input.bytes[pos])
}

fn control(input: &mut Input<'_>, mut pos: usize) -> Result<usize> {
    let symbol = byte(input, pos)?;
    if symbol == b'\'' {
        input.ensure(pos + 3)?;
        if !input.bytes[pos + 1..pos + 3]
            .iter()
            .all(u8::is_ascii_hexdigit)
        {
            return Err(invalid("incomplete hexadecimal escape"));
        }
        return Ok(pos + 3);
    }
    if !symbol.is_ascii_alphabetic() {
        if symbol == 0 || !symbol.is_ascii() {
            return Err(invalid("invalid control symbol"));
        }
        return Ok(pos + 1);
    }
    let start = pos;
    while byte(input, pos)?.is_ascii_alphabetic() {
        pos += 1;
        if pos - start > 32 {
            return Err(invalid("control word exceeds 32 letters"));
        }
    }
    let binary = &input.bytes[start..pos] == b"bin";
    let negative = input.bytes[pos] == b'-' && byte(input, pos + 1)?.is_ascii_digit();
    pos += usize::from(negative);
    let first_digit = pos;
    let mut parameter = 0u32;
    while byte(input, pos)?.is_ascii_digit() {
        parameter = parameter
            .checked_mul(10)
            .and_then(|v| v.checked_add(u32::from(input.bytes[pos] - b'0')))
            .ok_or_else(|| invalid("numeric parameter exceeds 32 bits"))?;
        pos += 1;
        if pos - first_digit > 10 {
            return Err(invalid("numeric parameter longer than ten digits"));
        }
    }
    let has_number = pos != first_digit;
    pos += usize::from(input.bytes[pos] == b' ');
    if binary {
        if !has_number || negative {
            return Err(invalid("binary payload needs a nonnegative byte count"));
        }
        pos = pos
            .checked_add(parameter as usize)
            .filter(|n| *n <= input.limit())
            .ok_or_else(|| invalid("binary data outside carving range"))?;
        input.ensure(pos)?;
    }
    Ok(pos)
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<usize> {
    const HEADER: &[u8] = b"{\\rtf1";
    input.ensure(HEADER.len() + 1)?;
    if !input.bytes.starts_with(HEADER) || input.bytes[HEADER.len()].is_ascii_alphanumeric() {
        return Err(invalid("version or header delimiter"));
    }
    let mut pos = HEADER.len();
    let mut depth = 1u32;
    let mut groups = 1usize;
    loop {
        input.check()?;
        match byte(input, pos)? {
            b'{' => {
                depth += 1;
                groups += 1;
                if depth > 256 || groups > 65536 {
                    return Err(Error::Limit("RTF group count or nesting limit".into()));
                }
                pos += 1;
            }
            b'}' => {
                pos += 1;
                depth -= 1;
                if depth == 0 {
                    return Ok(pos);
                }
            }
            b'\\' => pos = control(input, pos + 1)?,
            0..=8 | 11..=12 | 14..=31 => {
                return Err(invalid("control byte outside binary payload"));
            }
            _ => {
                pos += 1;
                let tail = &input.bytes[pos..];
                pos += tail
                    .iter()
                    .position(|b| matches!(b, b'{' | b'}' | b'\\' | 0..=8 | 11..=12 | 14..=31))
                    .unwrap_or(tail.len());
            }
        }
    }
}
