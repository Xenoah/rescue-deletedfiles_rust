use super::{invalid, u16_at, u32_at, u64_at};
use crate::Result;

pub(super) struct Name {
    pub text: String,
    pub verified: bool,
    pub warnings: Vec<String>,
}

pub(super) fn fat32_name(short: &[u8; 32], long: &[[u8; 32]]) -> Name {
    let deleted = short[0] == 0xe5;
    let mut warnings = Vec::new();
    if !long.is_empty() {
        if let Ok(text) = long_name(short, long) {
            if deleted {
                warnings.push("Deleted LFN order and the short-name leading byte were reconstructed from adjacent entries/checksum".into());
            }
            return Name {
                text,
                verified: !deleted,
                warnings,
            };
        }
        warnings
            .push("Invalid/orphaned LFN entries ignored; using the surviving short name".into());
    }
    if deleted {
        warnings
            .push("The first short-name byte was deleted; '?' marks the unknown character".into());
    }
    if short[..11]
        .iter()
        .enumerate()
        .any(|(i, b)| *b >= 128 && !(i == 0 && deleted))
    {
        warnings.push(
            "Non-ASCII short-name bytes need an OEM code page; replacement characters are shown"
                .into(),
        );
    }
    let part = |bytes: &[u8], first: bool, lowercase: bool| {
        let mut text = bytes
            .iter()
            .enumerate()
            .map(|(i, b)| {
                if first && i == 0 && deleted {
                    '?'
                } else if (32..127).contains(b) {
                    char::from(*b)
                } else {
                    '\u{fffd}'
                }
            })
            .collect::<String>()
            .trim_end_matches(' ')
            .to_string();
        if lowercase {
            text.make_ascii_lowercase();
        }
        text
    };
    let mut text = part(&short[..8], true, short[12] & 8 != 0);
    let extension = part(&short[8..11], false, short[12] & 16 != 0);
    if !extension.is_empty() {
        text.push('.');
        text.push_str(&extension);
    }
    Name {
        text,
        verified: !deleted && warnings.is_empty(),
        warnings,
    }
}

fn short_checksum(name: &[u8]) -> u8 {
    name.iter()
        .fold(0u8, |sum, byte| sum.rotate_right(1).wrapping_add(*byte))
}

fn long_name(short: &[u8; 32], entries: &[[u8; 32]]) -> Result<String> {
    if entries.len() > 20 {
        return Err(invalid("too many LFN slots"));
    }
    let deleted = short[0] == 0xe5;
    let checksum = entries[0][13];
    let mut sfn: [u8; 11] = short[..11].try_into().unwrap();
    if deleted {
        let leading = (1..=255)
            .find(|byte| {
                if (*byte < 0x21 && *byte != 5)
                    || *byte == 0xe5
                    || b"\"*+,./:;<=>?[\\]|".contains(byte)
                {
                    return false;
                }
                sfn[0] = *byte;
                short_checksum(&sfn) == checksum
            })
            .ok_or_else(|| invalid("deleted LFN checksum"))?;
        sfn[0] = leading;
    }
    if short_checksum(&sfn) != checksum {
        return Err(invalid("LFN checksum mismatch"));
    }
    let mut codes = Vec::with_capacity(entries.len() * 13);
    for (index, entry) in entries.iter().rev().enumerate() {
        let expected = (index + 1) as u8 | if index + 1 == entries.len() { 0x40 } else { 0 };
        if entry[11] != 0x0f
            || entry[12] != 0
            || entry[13] != checksum
            || u16_at(entry, 26)? != 0
            || if deleted {
                entry[0] != 0xe5
            } else {
                entry[0] != expected
            }
        {
            return Err(invalid("LFN sequence/fields"));
        }
        for offset in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30] {
            codes.push(u16_at(entry, offset)?);
        }
    }
    let length = codes.iter().position(|c| *c == 0).unwrap_or(codes.len());
    if length == 0
        || length > 255
        || codes[..length].contains(&0xffff)
        || (length < codes.len() && codes[length + 1..].iter().any(|c| *c != 0xffff))
    {
        return Err(invalid("LFN termination/padding"));
    }
    String::from_utf16(&codes[..length]).map_err(|_| invalid("LFN UTF-16"))
}

pub(super) struct ExfatEntry {
    pub name: String,
    pub deleted: bool,
    pub directory: bool,
    pub first: u32,
    pub size: u64,
    pub initialized: u64,
    pub contiguous: bool,
    pub created: Option<crate::FileTimestamp>,
    pub modified: Option<crate::FileTimestamp>,
}

pub(super) fn exfat_entry(entries: &[[u8; 32]], upcase: &[u16]) -> Result<ExfatEntry> {
    let primary = entries
        .first()
        .ok_or_else(|| invalid("empty exFAT entry set"))?;
    let deleted = primary[0] == 5;
    if ![5, 0x85].contains(&primary[0])
        || entries.len() != usize::from(primary[1]) + 1
        || entries.len() < 3
    {
        return Err(invalid("exFAT entry-set count/type"));
    }
    let checksum = u16_at(primary, 2)?;
    let sum = |restore: bool| {
        entries
            .iter()
            .flatten()
            .enumerate()
            .filter(|(i, _)| ![2, 3].contains(i))
            .fold(0u16, |sum, (i, b)| {
                sum.rotate_right(1)
                    .wrapping_add(u16::from(if restore && i % 32 == 0 {
                        *b | 0x80
                    } else {
                        *b
                    }))
            })
    };
    if sum(false) != checksum && !(deleted && sum(true) == checksum) {
        return Err(invalid("exFAT entry-set checksum mismatch"));
    }
    let stream = &entries[1];
    let active = if deleted { 0 } else { 0x80 };
    let length = usize::from(stream[3]);
    if stream[0] != 0x40 | active
        || stream[1] & !3 != 0
        || stream[1] & 1 == 0
        || length == 0
        || entries.len() != 2 + length.div_ceil(15)
    {
        return Err(invalid("exFAT stream flags/name slots"));
    }
    let mut codes = Vec::new();
    for entry in &entries[2..] {
        if entry[0] != 0x41 | active || entry[1] != 0 {
            return Err(invalid("exFAT filename entry"));
        }
        for pair in entry[2..].as_chunks::<2>().0 {
            codes.push(u16::from_le_bytes(*pair));
        }
    }
    codes.truncate(length);
    if codes.contains(&0) {
        return Err(invalid("exFAT embedded name terminator"));
    }
    let name = String::from_utf16(&codes).map_err(|_| invalid("exFAT name UTF-16"))?;
    let mut name_hash = 0u16;
    for code in &codes {
        for byte in upcase[usize::from(*code)].to_le_bytes() {
            name_hash = name_hash.rotate_right(1).wrapping_add(u16::from(byte));
        }
    }
    if name_hash != u16_at(stream, 4)? {
        return Err(invalid("exFAT name hash mismatch"));
    }
    let size = u64_at(stream, 24)?;
    let initialized = u64_at(stream, 8)?;
    if initialized > size {
        return Err(invalid("exFAT valid data length exceeds data length"));
    }
    let attributes = u16_at(primary, 4)?;
    if attributes & !0x37 != 0 {
        return Err(invalid("exFAT reserved file attributes"));
    }
    Ok(ExfatEntry {
        name,
        deleted,
        directory: attributes & 16 != 0,
        first: u32_at(stream, 20)?,
        size,
        initialized,
        contiguous: stream[1] & 2 != 0,
        created: crate::timestamps::dos(
            (u32_at(primary, 8)? >> 16) as u16,
            u32_at(primary, 8)? as u16,
            primary[20],
            Some(primary[22]),
        ),
        modified: crate::timestamps::dos(
            (u32_at(primary, 12)? >> 16) as u16,
            u32_at(primary, 12)? as u16,
            primary[21],
            Some(primary[23]),
        ),
    })
}

pub(super) fn upcase_table(bytes: &[u8], expected: u32) -> Result<Vec<u16>> {
    let actual = bytes.iter().fold(0u32, |sum, byte| {
        sum.rotate_right(1).wrapping_add(u32::from(*byte))
    });
    if actual != expected || !bytes.len().is_multiple_of(2) {
        return Err(invalid("exFAT upcase checksum/length"));
    }
    let mut table = Vec::with_capacity(65536);
    let mut at = 0;
    while at < bytes.len() && table.len() < 65536 {
        let code = u16_at(bytes, at)?;
        at += 2;
        if code == 0xffff && table.len() != 65535 {
            let count = usize::from(u16_at(bytes, at)?);
            at += 2;
            if count == 0 || table.len() + count > 65536 {
                return Err(invalid("exFAT upcase skip run"));
            }
            for _ in 0..count {
                table.push(table.len() as u16);
            }
        } else {
            table.push(code);
        }
    }
    if table.len() != 65536 || at != bytes.len() {
        return Err(invalid("incomplete exFAT upcase table"));
    }
    Ok(table)
}
