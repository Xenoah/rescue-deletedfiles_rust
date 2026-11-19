//! Resolve Btrfs logical byte addresses for single-device single/DUP allocation profiles.
use super::{bad, u16le, u64le};
use crate::{Error, Result};
use std::collections::BTreeMap;

#[derive(Default, PartialEq, Eq)]
pub(super) struct Chunks(BTreeMap<u64, Chunk>);
#[derive(PartialEq, Eq)]
struct Chunk {
    length: u64,
    kind: u64,
    physical: Vec<u64>,
}
pub(super) struct Device {
    pub id: u64,
    pub uuid: [u8; 16],
    pub length: u64,
    pub sector: u64,
}

impl Chunks {
    pub fn add(&mut self, logical: u64, raw: &[u8], dev: &Device) -> Result<()> {
        if raw.len() < 48 {
            return Err(bad("short chunk record"));
        }
        let count = usize::from(u16le(raw, 44));
        let flags = u64le(raw, 24);
        let length = u64le(raw, 0);
        if flags & !0x27 != 0 {
            return Err(Error::Unsupported(
                "Btrfs chunk profile requires single-device single or DUP allocation".into(),
            ));
        }
        if length == 0
            || !length.is_multiple_of(dev.sector)
            || !logical.is_multiple_of(dev.sector)
            || logical.checked_add(length).is_none()
            || flags & 7 == 0
            || count != if flags & 0x20 != 0 { 2 } else { 1 }
            || raw.len() != 48 + count * 32
        {
            return Err(bad("invalid chunk size, flags or stripe count"));
        }
        let mut physical = Vec::new();
        for stripe in raw[48..].as_chunks::<32>().0 {
            let at = u64le(stripe, 8);
            if u64le(stripe, 0) != dev.id
                || stripe[16..32] != dev.uuid
                || !at.is_multiple_of(dev.sector)
                || at.checked_add(length).is_none_or(|n| n > dev.length)
            {
                return Err(bad("chunk stripe points outside the selected device"));
            }
            physical.push(at);
        }
        if physical.len() == 2 && physical[0].abs_diff(physical[1]) < length {
            return Err(bad("overlapping DUP stripes"));
        }
        let chunk = Chunk {
            length,
            kind: flags & 7,
            physical,
        };
        if self.0.len() >= 65536 {
            return Err(Error::Limit("Btrfs chunk map exceeds 65536 records".into()));
        }
        if self.0.insert(logical, chunk).is_some() {
            return Err(bad("duplicate chunk mapping"));
        }
        Ok(())
    }
    pub fn bootstrap(sb: &[u8], dev: &Device) -> Result<Self> {
        let n = super::u32le(sb, 160) as usize;
        if !(97..=2048).contains(&n) {
            return Err(bad("invalid system chunk array length"));
        }
        let mut data = &sb[811..811 + n];
        let mut map = Self::default();
        while !data.is_empty() {
            if data.len() < 65 || u64le(data, 0) != 256 || data[8] != 228 {
                return Err(bad("invalid system chunk key"));
            }
            let bytes = 48 + usize::from(u16le(data, 17 + 44)) * 32;
            if bytes > data.len() - 17 || u64le(data, 17 + 24) & 2 == 0 {
                return Err(bad("invalid bootstrap system stripe"));
            }
            map.add(u64le(data, 9), &data[17..17 + bytes], dev)?;
            data = &data[17 + bytes..];
        }
        map.validate()?;
        Ok(map)
    }
    pub fn validate(&self) -> Result<()> {
        let mut end = 0;
        let mut ranges = Vec::new();
        for (&start, chunk) in &self.0 {
            if start < end {
                return Err(bad("overlapping logical chunk ranges"));
            }
            end = start + chunk.length;
            for &at in &chunk.physical {
                ranges.push((at, at + chunk.length));
            }
        }
        ranges.sort_unstable();
        if ranges.windows(2).any(|pair| pair[0].1 > pair[1].0) {
            return Err(bad("overlapping physical chunk ranges"));
        }
        Ok(())
    }
    pub fn includes(&self, other: &Self) -> bool {
        other.0.iter().all(|(k, c)| self.0.get(k) == Some(c))
    }
    pub fn addresses(&self, logical: u64, size: usize, kind: u64) -> Result<Vec<u64>> {
        let (&start, chunk) = self
            .0
            .range(..=logical)
            .next_back()
            .ok_or_else(|| bad("logical address is unmapped"))?;
        let within = logical - start;
        if chunk.kind & kind == 0
            || within
                .checked_add(size as u64)
                .is_none_or(|n| n > chunk.length)
        {
            return Err(bad("tree or file extent exceeds its chunk mapping"));
        }
        Ok(chunk.physical.iter().map(|&p| p + within).collect())
    }
}
