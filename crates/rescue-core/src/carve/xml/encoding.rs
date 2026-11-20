//! Keep original disk offsets while locating XML markup in BOM-tagged UTF-16.
use super::{BOM, InputCursor};
use crate::{Error, Result};
use std::{
    borrow::Cow,
    io::{self, BufRead, Read},
};

#[derive(Clone, Copy, PartialEq)]
pub(super) enum Encoding {
    Utf8,
    Little,
    Big,
}

impl Encoding {
    pub fn detect(bytes: &[u8]) -> (Self, usize) {
        if bytes.starts_with(&[0xff, 0xfe]) {
            (Self::Little, 2)
        } else if bytes.starts_with(&[0xfe, 0xff]) {
            (Self::Big, 2)
        } else {
            (Self::Utf8, if bytes.starts_with(BOM) { 3 } else { 0 })
        }
    }

    pub fn accepts(self, name: &[u8]) -> bool {
        match self {
            Self::Utf8 => name.eq_ignore_ascii_case(b"utf-8"),
            Self::Little => {
                name.eq_ignore_ascii_case(b"utf-16le") || name.eq_ignore_ascii_case(b"utf-16")
            }
            Self::Big => {
                name.eq_ignore_ascii_case(b"utf-16be") || name.eq_ignore_ascii_case(b"utf-16")
            }
        }
    }

    fn unit(self, pair: &[u8; 2]) -> u16 {
        if self == Self::Little {
            u16::from_le_bytes(*pair)
        } else {
            u16::from_be_bytes(*pair)
        }
    }

    pub fn project(self, pair: &[u8; 2]) -> u8 {
        let value = self.unit(pair);
        if value < 128 { value as u8 } else { b'X' }
    }

    pub fn decode(self, bytes: &[u8]) -> Result<Cow<'_, str>> {
        if self == Self::Utf8 {
            return std::str::from_utf8(bytes)
                .map(Cow::Borrowed)
                .map_err(|_| Error::Invalid("invalid UTF-8 XML".into()));
        }
        if !bytes.len().is_multiple_of(2) {
            return Err(Error::Invalid("odd UTF-16 XML byte count".into()));
        }
        let text: std::result::Result<String, _> =
            char::decode_utf16(bytes.as_chunks::<2>().0.iter().map(|pair| self.unit(pair)))
                .collect();
        text.map(Cow::Owned)
            .map_err(|_| Error::Invalid("unpaired UTF-16 surrogate".into()))
    }
}

/// XML delimiters are ASCII. The boundary reader sees one byte per UTF-16 code unit;
/// the full document is subsequently decoded strictly and its actual names validated.
pub(super) struct MarkupCursor<'a, 'b> {
    pub raw: InputCursor<'a, 'b>,
    encoding: Encoding,
    window: Vec<u8>,
    position: usize,
}

impl<'a, 'b> MarkupCursor<'a, 'b> {
    pub fn new(raw: InputCursor<'a, 'b>, encoding: Encoding) -> Self {
        Self {
            raw,
            encoding,
            window: Vec::new(),
            position: 0,
        }
    }
}
impl BufRead for MarkupCursor<'_, '_> {
    fn fill_buf(&mut self) -> io::Result<&[u8]> {
        if self.encoding == Encoding::Utf8 {
            return self.raw.fill_buf();
        }
        if self.position == self.window.len() {
            let start = self.raw.cursor;
            let limit = self.raw.limit.min(self.raw.input.limit());
            if start + 2 > limit {
                return Ok(&[]);
            }
            if let Err(e) = self.raw.input.ensure(start + 2) {
                self.raw.failure = Some(e);
                return Err(io::Error::other("unable to read UTF-16 source"));
            }
            let end = limit.min(self.raw.input.bytes.len()).min(start + 8192);
            self.window.clear();
            self.window.extend(
                self.raw.input.bytes[start..end]
                    .as_chunks::<2>()
                    .0
                    .iter()
                    .map(|pair| self.encoding.project(pair)),
            );
            self.position = 0;
        }
        Ok(&self.window[self.position..])
    }
    fn consume(&mut self, amount: usize) {
        if self.encoding == Encoding::Utf8 {
            self.raw.consume(amount);
        } else {
            self.position += amount;
            self.raw.consume(amount * 2);
        }
    }
}
impl Read for MarkupCursor<'_, '_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        let n = self.fill_buf()?.read(output)?;
        self.consume(n);
        Ok(n)
    }
}
