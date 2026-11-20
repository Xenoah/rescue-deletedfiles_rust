//! Bounded read-ahead shared by document/container parsers. No parser follows URLs.
use super::charge;
use crate::{Error, Result, source::Image};
use std::io::{self, BufRead, Read};
use std::sync::atomic::{AtomicBool, Ordering};

pub(super) const MAX_ENCODED: usize = 64 * 1024 * 1024;
pub(super) struct Input<'a> {
    image: &'a mut Image,
    start: u64,
    limit: usize,
    cancel: &'a AtomicBool,
    budget: &'a mut u64,
    pub bytes: Vec<u8>,
}
impl<'a> Input<'a> {
    pub fn new(
        image: &'a mut Image,
        start: u64,
        boundary: u64,
        cancel: &'a AtomicBool,
        budget: &'a mut u64,
    ) -> Result<Self> {
        let remaining = boundary
            .checked_sub(start)
            .ok_or_else(|| Error::Invalid("container outside selected range".into()))?;
        Ok(Self {
            image,
            start,
            limit: remaining.min(MAX_ENCODED as u64) as usize,
            cancel,
            budget,
            bytes: Vec::new(),
        })
    }
    pub fn ensure(&mut self, end: usize) -> Result<()> {
        if end > self.limit {
            return Err(Error::Invalid(
                "container exceeds selected range or 64 MiB carving limit".into(),
            ));
        }
        while self.bytes.len() < end {
            self.check()?;
            let n = (end - self.bytes.len())
                .clamp(4096, 64 * 1024)
                .min(self.limit - self.bytes.len());
            charge(self.budget, n)?;
            let offset = self.start + self.bytes.len() as u64;
            let chunk = match self.image.read(offset, n) {
                Err(Error::MissingRanges(gaps)) if n > end - self.bytes.len() => {
                    // Read-ahead may encounter a later missing sector after the file ends.
                    let absolute = self.image.absolute_offset(offset);
                    let prefix = gaps
                        .iter()
                        .map(|g| g.source_offset.saturating_sub(absolute))
                        .min()
                        .unwrap_or(0)
                        .min(n as u64) as usize;
                    if prefix < end - self.bytes.len() || prefix == n {
                        return Err(Error::MissingRanges(gaps));
                    }
                    charge(self.budget, prefix)?;
                    self.image.read(offset, prefix)?
                }
                result => result?,
            };
            self.bytes.extend(chunk);
        }
        Ok(())
    }
    /// Reuse the standalone JPEG entropy and pixel checks on an embedded DNG preview.
    pub(super) fn check_jpeg_preview(
        &mut self,
        range: std::ops::Range<usize>,
        geometry: (usize, usize),
    ) -> Result<()> {
        self.ensure(range.end)?;
        let data = super::jpeg::validate_with_dimensions(
            self.image,
            self.start + range.start as u64,
            self.cancel,
            self.budget,
            self.start + range.end as u64,
            Some(geometry),
        )?;
        if data.len() != range.len() {
            return Err(Error::Invalid(
                "DNG JPEG preview length differs from its strip or tile".into(),
            ));
        }
        Ok(())
    }
    pub(super) fn cancel_flag(&self) -> &AtomicBool {
        self.cancel
    }
    pub fn check(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    pub fn work(&mut self, n: usize) -> Result<()> {
        self.check()?;
        charge(self.budget, n)
    }
    pub fn limit(&self) -> usize {
        self.limit
    }
    pub fn is_source_end(&self, offset: usize) -> bool {
        self.start.checked_add(offset as u64) == Some(self.image.length)
    }
    pub fn finish(mut self, end: usize) -> Result<Vec<u8>> {
        self.ensure(end)?;
        self.bytes.truncate(end);
        Ok(self.bytes)
    }
}

pub(super) struct InputCursor<'a, 'b> {
    pub input: &'a mut Input<'b>,
    pub limit: usize,
    pub cursor: usize,
    pub failure: Option<Error>,
}
impl BufRead for InputCursor<'_, '_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        let limit = self.input.limit().min(self.limit);
        if self.cursor >= limit {
            return Ok(&[]);
        }
        if self.cursor == self.input.bytes.len()
            && let Err(err) = self.input.ensure(self.cursor + 1)
        {
            self.failure = Some(err);
            return Err(io::Error::other("cannot read source extent"));
        }
        Ok(&self.input.bytes[self.cursor..self.input.bytes.len().min(limit)])
    }
    fn consume(&mut self, n: usize) {
        self.cursor += n;
    }
}
impl Read for InputCursor<'_, '_> {
    fn read(&mut self, dest: &mut [u8]) -> io::Result<usize> {
        let n = self.fill_buf()?.read(dest)?;
        self.consume(n);
        Ok(n)
    }
}

pub(super) fn u16le(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(b[p..p + 2].try_into().unwrap())
}
pub(super) fn u32le(b: &[u8], p: usize) -> u32 {
    u32::from_le_bytes(b[p..p + 4].try_into().unwrap())
}
pub(super) fn u64le(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}
pub(super) fn usize64(n: u64) -> Result<usize> {
    usize::try_from(n)
        .ok()
        .filter(|n| *n <= MAX_ENCODED)
        .ok_or_else(|| Error::Limit("container extent exceeds 64 MiB".into()))
}
