//! Bounded LZNT1 decoder for NTFS compression units (4 KiB chunks).
use crate::{Error, Result};

fn invalid(message: &str) -> Error {
    Error::Invalid(format!("LZNT1: {message}"))
}

pub(crate) fn decompress(input: &[u8], required: usize) -> Result<Vec<u8>> {
    if required == 0 || required > 65536 {
        return Err(invalid("invalid compression unit size"));
    }
    let mut output = Vec::with_capacity(required);
    let mut cursor = 0;
    while output.len() < required {
        let header = input
            .get(cursor..cursor + 2)
            .ok_or_else(|| invalid("truncated chunk header"))?;
        let header = u16::from_le_bytes([header[0], header[1]]);
        cursor += 2;
        if header & 0x7000 != 0x3000 {
            return Err(invalid("invalid or premature chunk terminator"));
        }
        let length = usize::from(header & 0xfff) + 1;
        let body = input
            .get(cursor..cursor + length)
            .ok_or_else(|| invalid("truncated chunk body"))?;
        cursor += length;
        let mut chunk = Vec::with_capacity(4096);
        if header & 0x8000 == 0 {
            chunk.extend_from_slice(body);
        } else {
            let mut at = 0;
            while at < body.len() {
                let flags = body[at];
                at += 1;
                for bit in 0..8 {
                    if at == body.len() {
                        break;
                    }
                    if flags & (1 << bit) == 0 {
                        if chunk.len() == 4096 {
                            return Err(invalid("chunk exceeds 4096 bytes"));
                        }
                        chunk.push(body[at]);
                        at += 1;
                    } else {
                        let encoded = body
                            .get(at..at + 2)
                            .ok_or_else(|| invalid("truncated backreference"))?;
                        at += 2;
                        let token = u16::from_le_bytes([encoded[0], encoded[1]]) as usize;
                        let mut length_mask = 0xfff;
                        let mut shift = 12;
                        let mut position = chunk.len().saturating_sub(1);
                        while position >= 16 {
                            length_mask >>= 1;
                            shift -= 1;
                            position >>= 1;
                        }
                        let length = (token & length_mask) + 3;
                        let distance = (token >> shift) + 1;
                        if distance > chunk.len() || chunk.len() + length > 4096 {
                            return Err(invalid("backreference outside chunk"));
                        }
                        for _ in 0..length {
                            chunk.push(chunk[chunk.len() - distance]);
                        }
                    }
                }
            }
        }
        if chunk.is_empty() || (chunk.len() < 4096 && output.len() + chunk.len() < required) {
            return Err(invalid("short nonfinal chunk"));
        }
        output.extend_from_slice(&chunk[..chunk.len().min(required - output.len())]);
    }
    Ok(output)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn overlapping_backreference_and_uncompressed_chunks() {
        // Literal A followed by a distance-one match of 4095 bytes.
        assert_eq!(
            decompress(&[3, 0xb0, 2, b'A', 0xfc, 0x0f], 4096).unwrap(),
            vec![b'A'; 4096]
        );
        let mut encoded = vec![0xff, 0x3f];
        encoded.extend((0..4096).map(|n| (n % 251) as u8));
        assert_eq!(decompress(&encoded, 4096).unwrap(), encoded[2..]);
    }

    #[test]
    fn malformed_chunks_cannot_invent_missing_bytes_or_exceed_bounds() {
        for input in [
            vec![],
            vec![0],
            vec![0, 0],
            vec![0, 0x30],
            vec![2, 0xb0, 1, 0, 0],    // backreference before any literal
            vec![2, 0xb0, 2, b'A', 0], // incomplete token
            vec![3, 0xb0, 2, b'A', 0xff, 0x0f], // exceeds 4096
            vec![0, 0x30, b'A'],       // short output cannot be padded with zero
        ] {
            assert!(decompress(&input, 4096).is_err(), "{input:?}");
        }
        assert!(decompress(&[], 65537).is_err());
    }

    #[test]
    fn malformed_corpus_never_panics() {
        let mut seed = 0x7193_4567u32;
        for length in 0..2048 {
            let mut input = vec![0; length];
            for byte in &mut input {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *byte = seed as u8;
            }
            let _ = decompress(&input, 4096);
        }
    }
}
