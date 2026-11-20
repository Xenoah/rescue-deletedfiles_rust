//! Strictly consume every coded difference of a DNG lossless-JPEG tile (T.81 Annex H).
use super::{Bits, EntropySource, Huffman, invalid};
use crate::{Error, Result};
use std::sync::atomic::{AtomicBool, Ordering};

struct Cursor<'a>(&'a [u8]);
impl EntropySource for Cursor<'_> {
    fn byte(&mut self) -> Result<u8> {
        let (first, rest) = self
            .0
            .split_first()
            .ok_or_else(|| invalid("truncated lossless JPEG"))?;
        self.0 = rest;
        Ok(*first)
    }
}
impl<'a> Cursor<'a> {
    fn segment(&mut self) -> Result<&'a [u8]> {
        let length = u16::from_be_bytes([self.byte()?, self.byte()?]) as usize;
        let size = length
            .checked_sub(2)
            .ok_or_else(|| invalid("invalid lossless JPEG segment"))?;
        let (data, rest) = self
            .0
            .split_at_checked(size)
            .ok_or_else(|| invalid("truncated lossless JPEG segment"))?;
        self.0 = rest;
        Ok(data)
    }
}

fn tables(mut segment: &[u8], huffman: &mut [Option<Huffman>; 4]) -> Result<()> {
    if segment.is_empty() {
        return Err(invalid("empty lossless JPEG Huffman segment"));
    }
    while !segment.is_empty() {
        let header = segment
            .get(..17)
            .ok_or_else(|| invalid("truncated lossless JPEG Huffman table"))?;
        let id = usize::from(header[0]);
        let counts = &header[1..];
        let n = counts.iter().map(|&v| usize::from(v)).sum::<usize>();
        let symbols = segment
            .get(17..17 + n)
            .ok_or_else(|| invalid("truncated lossless JPEG symbols"))?;
        if id >= 4 || symbols.iter().any(|&s| s > 16) {
            return Err(invalid("lossless JPEG DC selector or category"));
        }
        huffman[id] = Some(Huffman::parse(counts, symbols, true)?);
        segment = &segment[17 + n..];
    }
    Ok(())
}

pub(in crate::carve) fn check_cfa_tile(
    data: &[u8],
    expected: usize,
    precision: usize,
    cancel: &AtomicBool,
) -> Result<()> {
    let mut input = Cursor(data);
    if input.marker()? != 0xd8 {
        return Err(invalid("lossless JPEG SOI"));
    }
    let mut huffman = std::array::from_fn(|_| None);
    let mut ids = Vec::new();
    let mut interval = 0;
    for _ in 0..1024 {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        match input.marker()? {
            0xc4 => tables(input.segment()?, &mut huffman)?,
            0xc3 => {
                let f = input.segment()?;
                if f.len() < 6 || !ids.is_empty() || usize::from(f[0]) != precision {
                    return Err(invalid("lossless JPEG frame precision"));
                }
                let h = usize::from(u16::from_be_bytes([f[1], f[2]]));
                let w = usize::from(u16::from_be_bytes([f[3], f[4]]));
                let nc = usize::from(f[5]);
                if !(1..=4).contains(&nc)
                    || f.len() != 6 + 3 * nc
                    || w.checked_mul(h).and_then(|v| v.checked_mul(nc)) != Some(expected)
                {
                    return Err(invalid("lossless JPEG frame and DNG sample counts differ"));
                }
                for comp in f[6..].as_chunks::<3>().0 {
                    if ids.contains(&comp[0]) || comp[1] != 0x11 || comp[2] > 3 {
                        return Err(invalid("lossless JPEG component layout"));
                    }
                    ids.push(comp[0]);
                }
            }
            0xdd => {
                let d: [u8; 2] = input
                    .segment()?
                    .try_into()
                    .map_err(|_| invalid("lossless JPEG restart interval"))?;
                interval = usize::from(u16::from_be_bytes(d));
            }
            0xe0..=0xef | 0xfe => {
                input.segment()?;
            }
            0xda => {
                let s = input.segment()?;
                let nc = ids.len();
                if nc == 0
                    || s.len() != 1 + 2 * nc + 3
                    || usize::from(s[0]) != nc
                    || !(1..=7).contains(&s[s.len() - 3])
                    || s[s.len() - 2] != 0
                    || usize::from(s[s.len() - 1]) >= precision
                    || s[s.len() - 1] > 15
                {
                    return Err(invalid("lossless JPEG scan header or predictor"));
                }
                let mut scan_ids = Vec::new();
                let mut order = Vec::new();
                for comp in s[1..1 + 2 * nc].as_chunks::<2>().0 {
                    if !ids.contains(&comp[0]) || scan_ids.contains(&comp[0]) || comp[1] & 15 != 0 {
                        return Err(invalid("lossless JPEG scan component"));
                    }
                    scan_ids.push(comp[0]);
                    order.push(
                        huffman
                            .get(usize::from(comp[1] >> 4))
                            .and_then(Option::as_ref)
                            .ok_or_else(|| invalid("missing lossless JPEG DC table"))?,
                    );
                }
                let mut bits = Bits {
                    input: &mut input,
                    byte: 0,
                    left: 0,
                };
                let mut restart = 0;
                for i in 0..expected / nc {
                    if i % 4096 == 0 && cancel.load(Ordering::Relaxed) {
                        return Err(Error::Cancelled);
                    }
                    if i > 0 && interval != 0 && i % interval == 0 {
                        if bits.finish_byte(true)? != 0xd0 + restart {
                            return Err(invalid("lossless JPEG restart sequence"));
                        }
                        restart = (restart + 1) % 8;
                    }
                    for table in &order {
                        let category = table.symbol(&mut bits)?;
                        // Difference 32768 is represented by category 16 alone (H.1.2.2).
                        if category < 16 {
                            bits.take(category)?;
                        }
                    }
                }
                if bits.finish_byte(true)? != 0xd9 || !input.0.is_empty() {
                    return Err(invalid("lossless JPEG entropy length or EOI"));
                }
                return Ok(());
            }
            _ => return Err(invalid("unsupported lossless JPEG marker")),
        }
    }
    Err(Error::Limit(
        "lossless JPEG tile exceeds 1024 header markers".into(),
    ))
}
