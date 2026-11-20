//! Variable-length text and codec information in ASF header objects.
use super::{Cursor, Guid, invalid, u16le, u32le};
use crate::Result;
const CODEC_RESERVED: Guid = uuid::uuid!("86d15241-311d-11d0-a3a4-00a0c90348f6").to_bytes_le();

fn utf16(bytes: &[u8]) -> Result<()> {
    if !bytes.len().is_multiple_of(2)
        || char::decode_utf16(bytes.chunks_exact(2).map(|word| u16le(word, 0)))
            .any(|ch| ch.is_err())
    {
        return Err(invalid());
    }
    Ok(())
}

pub(super) fn description(body: &[u8]) -> Result<()> {
    let mut cursor = Cursor(body);
    let lengths = cursor.take(10)?;
    for word in lengths.chunks_exact(2) {
        utf16(cursor.take(usize::from(u16le(word, 0)))?)?;
    }
    cursor.end()
}

pub(super) fn descriptors(body: &[u8], metadata_object: bool) -> Result<()> {
    let mut cursor = Cursor(body);
    let count = cursor.word()?;
    for _ in 0..count {
        let (kind, length);
        if metadata_object {
            let fixed = cursor.take(12)?;
            if u16le(fixed, 0) != 0 || u16le(fixed, 2) > 127 {
                return Err(invalid());
            }
            utf16(cursor.take(usize::from(u16le(fixed, 4)))?)?;
            kind = u16le(fixed, 6);
            length = u32le(fixed, 8) as usize;
        } else {
            let name_length = cursor.word()?;
            utf16(cursor.take(name_length)?)?;
            kind = u16le(cursor.take(2)?, 0);
            length = cursor.word()?;
        }
        let value = cursor.take(length)?;
        match kind {
            0 => utf16(value)?,
            1 => {}
            2 if length == if metadata_object { 2 } else { 4 } => {}
            3 if length == 4 => {}
            4 if length == 8 => {}
            5 if length == 2 => {}
            6 if metadata_object && length == 16 => {}
            _ => return Err(invalid()),
        }
    }
    cursor.end()
}

pub(super) fn codecs(body: &[u8]) -> Result<()> {
    let mut cursor = Cursor(body);
    if cursor.take(16)? != CODEC_RESERVED {
        return Err(invalid());
    }
    let count = u32le(cursor.take(4)?, 0);
    if count > 127 {
        return Err(invalid());
    }
    for _ in 0..count {
        if !matches!(cursor.word()?, 1 | 2 | 65535) {
            return Err(invalid());
        }
        for _ in 0..2 {
            let words = cursor.word()?;
            utf16(cursor.take(words * 2)?)?;
        }
        let length = cursor.word()?;
        cursor.take(length)?;
    }
    cursor.end()
}
