//! POSIX ustar, GNU long names and per-entry PAX headers. Archive members stay in the archive.
use super::container::Input;
use crate::{Error, Result};
const BLOCK: usize = 512;

pub(super) fn signature(bytes: &[u8]) -> bool {
    matches!(
        bytes.get(257..265),
        Some(b"ustar\x0000") | Some(b"ustar  \0")
    )
}
fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("TAR: {reason}"))
}

fn octal(field: &[u8]) -> Option<u64> {
    let first = field.iter().position(|&c| c != b' ')?;
    let field = &field[first..];
    let n = field
        .iter()
        .take_while(|&&c| matches!(c, b'0'..=b'7'))
        .count();
    if n == 0 || !field[n..].iter().all(|&c| matches!(c, 0 | b' ')) {
        return None;
    }
    field[..n].iter().try_fold(0u64, |a, &c| {
        a.checked_mul(8)?.checked_add(u64::from(c - b'0'))
    })
}
fn size(field: &[u8]) -> Option<usize> {
    let val = if field.first() == Some(&0x80) {
        field[1..]
            .iter()
            .try_fold(0u64, |a, &b| a.checked_mul(256)?.checked_add(u64::from(b)))?
    } else {
        octal(field)?
    };
    usize::try_from(val).ok()
}
fn checksum(h: &[u8]) -> bool {
    let Some(stored) = octal(&h[148..156]) else {
        return false;
    };
    let mut unsigned = 8 * 32u64;
    let mut signed = 8 * 32i64;
    for &b in h[..148].iter().chain(&h[156..]) {
        unsigned += u64::from(b);
        signed += i64::from(b as i8);
    }
    stored == unsigned || i64::try_from(stored) == Ok(signed)
}

#[derive(Clone, Copy, Default)]
enum SizeOverride {
    #[default]
    Inherit,
    Clear,
    Bytes(usize),
}
fn pax(data: &[u8], present: &mut SizeOverride) -> Result<()> {
    if data.len() > 1 << 20 {
        return Err(Error::Limit("TAR extended header exceeds 1 MiB".into()));
    }
    let mut rest = data;
    while !rest.is_empty() {
        let space = rest
            .iter()
            .take(12)
            .position(|&c| c == b' ')
            .ok_or_else(|| invalid("invalid PAX record length"))?;
        let digits = &rest[..space];
        if digits.is_empty() || !digits.iter().all(u8::is_ascii_digit) {
            return Err(invalid("invalid PAX length digits"));
        }
        let length = std::str::from_utf8(digits)
            .ok()
            .and_then(|s| s.parse::<usize>().ok())
            .ok_or_else(|| invalid("PAX length overflow"))?;
        if length <= space + 2 || length > rest.len() || rest[length - 1] != b'\n' {
            return Err(invalid("PAX record exceeds extended header"));
        }
        let record = &rest[space + 1..length - 1];
        let equals = record
            .iter()
            .position(|&b| b == b'=')
            .filter(|&i| i > 0)
            .ok_or_else(|| invalid("missing PAX keyword"))?;
        let key = &record[..equals];
        let value = &record[equals + 1..];
        if key.starts_with(b"GNU.sparse") || matches!(key, b"SCHILY.realsize" | b"SCHILY.holesdata")
        {
            return Err(Error::Unsupported(
                "sparse TAR representation requires an external archive reader".into(),
            ));
        }
        if key == b"size" {
            *present = if value.is_empty() {
                SizeOverride::Clear
            } else {
                let n = std::str::from_utf8(value)
                    .ok()
                    .filter(|s| s.bytes().all(|b| b.is_ascii_digit()))
                    .and_then(|s| s.parse::<usize>().ok())
                    .ok_or_else(|| invalid("invalid PAX file size"))?;
                SizeOverride::Bytes(n)
            };
        }
        rest = &rest[length..];
    }
    Ok(())
}

pub(super) fn parse(input: &mut Input<'_>, blocking: u16) -> Result<(usize, Vec<usize>)> {
    if !(1..=1024).contains(&blocking) {
        return Err(invalid("invalid blocking factor"));
    }
    let mut at = 0usize;
    let mut internal = Vec::new();
    let mut count = 0;
    let mut has_member = false;
    let mut pending = false;
    let mut global = SizeOverride::Inherit;
    let mut local = SizeOverride::Inherit;
    loop {
        input.ensure(at + BLOCK)?;
        let h = &input.bytes[at..at + BLOCK];
        if h.iter().all(|&v| v == 0) {
            if !has_member || pending {
                return Err(invalid("no complete following member"));
            }
            let end = (at + 2 * BLOCK).next_multiple_of(usize::from(blocking) * BLOCK);
            input.ensure(end)?;
            if input.bytes[at..end].iter().any(|&v| v != 0) {
                return Err(invalid("missing end markers or nonzero record padding"));
            }
            return Ok((end, internal));
        }
        if !signature(h) || !checksum(h) || h[0] == 0 {
            return Err(invalid("member header magic, name or checksum"));
        }
        if count >= 65536 {
            return Err(Error::Limit("TAR exceeds 65536 member headers".into()));
        }
        if at != 0 {
            internal.push(at);
        }
        count += 1;
        let kind = h[156];
        let mut length = size(&h[124..136]).ok_or_else(|| invalid("invalid member size"))?;
        match kind {
            0 | b'0' | b'7' => {
                if let SizeOverride::Bytes(n) = match local {
                    SizeOverride::Inherit => global,
                    other => other,
                } {
                    length = n;
                }
            }
            b'1'..=b'6' if length == 0 => {}
            b'x' | b'g' | b'L' | b'K' => {}
            _ => {
                return Err(invalid(
                    "unsupported member type or nonzero special-file size",
                ));
            }
        }
        let start = at + BLOCK;
        let end = start
            .checked_add(length)
            .filter(|&n| n <= input.limit())
            .ok_or_else(|| invalid("member extends past source or carving limit"))?;
        let next = end.next_multiple_of(BLOCK);
        input.ensure(next)?;
        if input.bytes[end..next].iter().any(|&v| v != 0) {
            return Err(invalid("nonzero member padding"));
        }
        let data = &input.bytes[start..end];
        match kind {
            b'x' => {
                pax(data, &mut local)?;
                pending = true;
            }
            b'g' => {
                pax(data, &mut global)?;
            }
            b'L' | b'K' => {
                if data.is_empty() || data.last() != Some(&0) || data[..data.len() - 1].contains(&0)
                {
                    return Err(invalid("invalid GNU long name or link"));
                }
                pending = true;
            }
            _ => {
                has_member = true;
                pending = false;
                local = SizeOverride::Inherit;
            }
        }
        at = next;
    }
}
