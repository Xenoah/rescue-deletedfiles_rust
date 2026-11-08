use super::{invalid, u16_at, u32_at, u64_at};
use crate::Result;

pub(super) struct Name {
    pub text: String,
    pub verified: bool,
    pub warnings: Vec<String>,
}

pub(super) fn fat32_name(
    short: &[u8; 32],
    long: &[[u8; 32]],
    encoding: crate::FatShortNameEncoding,
) -> Name {
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
    if encoding == crate::FatShortNameEncoding::Cp932 {
        return cp932_name(short, warnings);
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

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02X}")).collect()
}
fn cp932_trail(byte: u8) -> bool {
    (0x40..=0xfc).contains(&byte) && byte != 0x7f
}

/// Decode at known byte boundaries. CP932's four Windows private-use singles
/// are handled explicitly; they are not all accepted by WHATWG Shift_JIS.
fn cp932(bytes: &[u8]) -> Option<(String, Vec<usize>)> {
    let mut text = String::new();
    let mut at = 0;
    let mut boundaries = vec![0];
    while at < bytes.len() {
        let byte = bytes[at];
        if (0x81..=0x9f).contains(&byte) || (0xe0..=0xfc).contains(&byte) {
            let trail = *bytes.get(at + 1)?;
            if !cp932_trail(trail) {
                return None;
            }
            text.push_str(
                &encoding_rs::SHIFT_JIS
                    .decode_without_bom_handling_and_without_replacement(&bytes[at..at + 2])?,
            );
            at += 2;
        } else {
            let character = match byte {
                0x20..=0x7e => char::from(byte),
                0x80 => '\u{80}',
                0xa0 => '\u{f8f0}',
                0xa1..=0xdf => char::from_u32(0xff61 + u32::from(byte - 0xa1))?,
                0xfd..=0xff => char::from_u32(0xf8f1 + u32::from(byte - 0xfd))?,
                _ => return None,
            };
            text.push(character);
            at += 1;
        }
        boundaries.push(at);
    }
    Some((text, boundaries))
}

fn cp932_part(bytes: &[u8], deleted: bool, lowercase: bool, warnings: &mut Vec<String>) -> String {
    let end = bytes.iter().rposition(|b| *b != b' ').map_or(0, |n| n + 1);
    let bytes = &bytes[..end];
    let mut text = if deleted && !bytes.is_empty() {
        let after_one = cp932(&bytes[1..]);
        let after_two = bytes
            .get(1)
            .filter(|b| cp932_trail(**b))
            .and_then(|_| cp932(&bytes[2..]));
        match (after_one, after_two) {
            (Some((one, first)), Some((two, second)))
                if bytes.iter().skip(1).any(|b| *b >= 128) =>
            {
                // A removed byte could have been an ASCII byte or a DBCS lead.
                // Keep undecidable bytes explicit until both alignments meet.
                let boundary = first
                    .iter()
                    .map(|n| n + 1)
                    .find(|n| second.iter().any(|m| m + 2 == *n))
                    .unwrap_or(bytes.len());
                let suffix = cp932(&bytes[boundary..]).unwrap().0;
                warnings.push(format!("CP932_LEADING_BOUNDARY_AMBIGUOUS: suffix after one removed byte={one:?}; suffix after a removed DBCS lead and its surviving trail={two:?}. Bytes in braces are undecidable, not decoded characters."));
                format!("?{{{}}}{suffix}", hex(&bytes[1..boundary]))
            }
            (Some((text, _)), _) => format!("?{text}"),
            (None, Some((text, _))) => {
                warnings.push("CP932_MISSING_DBCS_LEAD: the surviving trail cannot identify the deleted first character".into());
                format!("?{text}")
            }
            (None, None) => {
                warnings.push("CP932_INVALID_BYTES: undecodable short-name bytes are shown as hexadecimal in braces".into());
                format!("?{{{}}}", hex(&bytes[1..]))
            }
        }
    } else if let Some((text, _)) = cp932(bytes) {
        text
    } else {
        warnings.push(
            "CP932_INVALID_BYTES: undecodable short-name bytes are shown as hexadecimal in braces"
                .into(),
        );
        format!("{{{}}}", hex(bytes))
    };
    if lowercase {
        text.make_ascii_lowercase();
    }
    text
}

fn cp932_name(short: &[u8; 32], mut warnings: Vec<String>) -> Name {
    let deleted = short[0] == 0xe5;
    let verified_ascii =
        !deleted && short[..11].iter().all(|b| (32..127).contains(b)) && warnings.is_empty();
    warnings.push(format!("FAT_SHORT_NAME_BYTES: raw_hex={}; encoding=cp932; selection=explicit; deleted_first_byte={deleted}. FAT does not record its OEM code page; non-ASCII path spelling is not verified.", hex(&short[..11])));
    let mut base: [u8; 8] = short[..8].try_into().unwrap();
    if base[0] == 5 {
        base[0] = 0xe5;
        warnings.push("CP932_ESCAPED_LEAD: stored 05 represents E5, not a deletion marker".into());
    }
    let mut text = cp932_part(&base, deleted, short[12] & 8 != 0, &mut warnings);
    let extension = cp932_part(&short[8..11], false, short[12] & 16 != 0, &mut warnings);
    if !extension.is_empty() {
        text.push('.');
        text.push_str(&extension);
    }
    Name {
        text,
        verified: verified_ascii,
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
    // Empty only after the explicitly opted-in, validated system stream failed
    // its table checksum/decoding. Do not substitute Unicode casing rules.
    if !upcase.is_empty() {
        let mut name_hash = 0u16;
        for code in &codes {
            let upper = upcase
                .get(usize::from(*code))
                .ok_or_else(|| invalid("incomplete exFAT upcase table"))?;
            for byte in upper.to_le_bytes() {
                name_hash = name_hash.rotate_right(1).wrapping_add(u16::from(byte));
            }
        }
        if name_hash != u16_at(stream, 4)? {
            return Err(invalid("exFAT name hash mismatch"));
        }
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

#[cfg(test)]
mod codepage_tests {
    use super::*;
    use crate::FatShortNameEncoding::{Ascii, Cp932};
    fn short(base: &[u8]) -> [u8; 32] {
        let mut slot = [0; 32];
        slot[..11].fill(b' ');
        slot[..base.len()].copy_from_slice(base);
        slot[8..11].copy_from_slice(b"TXT");
        slot[11] = 0x20;
        slot
    }
    #[test]
    fn cp932_known_windows_mappings_and_raw_provenance() {
        // Independent Windows CP932 examples, including both IBM extension encodings.
        for (bytes, expected) in [
            (&[0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea][..], "日本語"),
            (&[0xfa, 0x40], "ⅰ"),
            (&[0xee, 0xef], "ⅰ"),
            (&[0x87, 0x90], "≒"),
            (&[0x81, 0x60, 0x81, 0x61, 0x81, 0x7c], "～∥－"),
            (&[0xb6, 0xc0, 0xb6, 0xc5], "ｶﾀｶﾅ"),
            (
                &[0xa0, 0xfd, 0xfe, 0xff],
                "\u{f8f0}\u{f8f1}\u{f8f2}\u{f8f3}",
            ),
        ] {
            let slot = short(bytes);
            let name = fat32_name(&slot, &[], Cp932);
            assert_eq!(name.text, format!("{expected}.TXT"));
            assert!(
                !name.verified,
                "the volume never declared its OEM code page"
            );
            assert!(name.warnings.iter().any(|w| w.contains(&hex(&slot[..11]))));
        }
        let legacy = fat32_name(&short(&[0x93, 0xfa]), &[], Ascii);
        assert!(legacy.text.contains('\u{fffd}'));
        assert!(!legacy.warnings.iter().any(|w| w.contains("encoding=cp932")));
    }
    #[test]
    fn deleted_lead_does_not_invent_a_character_or_choose_an_ambiguous_boundary() {
        let mut slot = short(&[0x93, 0xfa, 0x96, 0x7b, 0x8c, 0xea]);
        slot[0] = 0xe5;
        let name = fat32_name(&slot, &[], Cp932);
        assert_eq!(name.text, "?{FA967B}語.TXT");
        assert!(!name.verified);
        assert!(
            name.warnings
                .iter()
                .any(|w| w.starts_with("CP932_LEADING_BOUNDARY_AMBIGUOUS:") && w.contains("本語"))
        );
        assert!(
            name.warnings
                .iter()
                .any(|w| w.contains("E5FA967B8CEA2020545854"))
        );
    }
    #[test]
    fn escaped_e5_trail_backslash_and_invalid_lead_are_explicit() {
        let mut slot = short(&[0x05, 0x5c]);
        assert_eq!(fat32_name(&slot, &[], Cp932).text, "藹.TXT");
        slot = short(&[b'A', 0x81]);
        let invalid = fat32_name(&slot, &[], Cp932);
        assert_eq!(invalid.text, "{4181}.TXT");
        assert!(
            invalid
                .warnings
                .iter()
                .any(|w| w.starts_with("CP932_INVALID_BYTES:"))
        );
        let mut ascii = short(b"NAME");
        ascii[12] = 0x18;
        let name = fat32_name(&ascii, &[], Cp932);
        assert_eq!(name.text, "name.txt");
        assert!(name.verified);
    }
    #[test]
    fn valid_unicode_lfn_takes_precedence_over_the_selected_oem_code_page() {
        let slot = short(b"NAME");
        let mut long = [0; 32];
        long[0] = 0x41;
        long[11] = 0x0f;
        long[13] = short_checksum(&slot[..11]);
        let mut codes = "日本語.txt".encode_utf16().collect::<Vec<_>>();
        codes.push(0);
        codes.resize(13, 0xffff);
        for (offset, code) in [1, 3, 5, 7, 9, 14, 16, 18, 20, 22, 24, 28, 30]
            .into_iter()
            .zip(codes)
        {
            long[offset..offset + 2].copy_from_slice(&code.to_le_bytes());
        }
        let name = fat32_name(&slot, &[long], Cp932);
        assert_eq!(name.text, "日本語.txt");
        assert!(name.verified);
        assert!(name.warnings.is_empty());
    }
}
