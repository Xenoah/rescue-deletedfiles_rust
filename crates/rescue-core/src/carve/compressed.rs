//! Exact single-stream boundaries for CRC-checked bzip2, XZ and lzip containers.
use super::{Format, container::Input};
use crate::{Error, Result};
use lzma_rust2::{Action, LzipStream, Status, StreamResult, XzStream};

const DICTIONARY_KIB: u32 = 64 * 1024;
const MAX_DECODED: u64 = 128 * 1024 * 1024;

enum Decoder {
    Bzip(bzip2::Decompress),
    Xz(Box<XzStream>),
    Lzip(Box<LzipStream>),
}

impl Decoder {
    fn step(&mut self, source: &[u8], output: &mut [u8], last: bool) -> Result<StreamResult> {
        let action = if last { Action::Finish } else { Action::Run };
        match self {
            Self::Xz(stream) => stream
                .process(source, output, action)
                .map_err(|e| Error::Invalid(format!("XZ: {e}"))),
            Self::Lzip(stream) => stream
                .process(source, output, action)
                .map_err(|e| Error::Invalid(format!("lzip: {e}"))),
            Self::Bzip(stream) => {
                let read = stream.total_in();
                let written = stream.total_out();
                let result = stream
                    .decompress(source, output)
                    .map_err(|e| Error::Invalid(format!("bzip2: {e}")))?;
                Ok(StreamResult {
                    bytes_consumed: (stream.total_in() - read) as usize,
                    bytes_produced: (stream.total_out() - written) as usize,
                    status: if result == bzip2::Status::StreamEnd {
                        Status::StreamEnd
                    } else {
                        Status::Ok
                    },
                })
            }
        }
    }
}

pub(super) fn signature(b: &[u8]) -> Option<Format<'static>> {
    match *b.first()? {
        0xfd if b.starts_with(b"\xfd7zXZ\0") => Some(Format::Xz),
        b'L' if b.starts_with(b"LZIP\x01") => Some(Format::Lzip),
        b'B' if b.starts_with(b"BZh")
            && matches!(b.get(3), Some(b'1'..=b'9'))
            && matches!(b.get(4..10), Some(b"1AY&SY" | b"\x17\x72\x45\x38\x50\x90")) =>
        {
            Some(Format::Bzip)
        }
        _ => None,
    }
}

pub(super) fn parse(format: Format, input: &mut Input<'_>) -> Result<usize> {
    let mut decoder = match format {
        Format::Bzip => Decoder::Bzip(bzip2::Decompress::new(false)),
        Format::Xz => Decoder::Xz(Box::new(XzStream::new_mem_limit(false, DICTIONARY_KIB))),
        Format::Lzip => Decoder::Lzip(Box::new(LzipStream::new_mem_limit(false, DICTIONARY_KIB))),
        _ => return Err(Error::Invalid("unknown compressed format".into())),
    };
    let mut buffer = vec![0; 32768];
    let mut pos = 0;
    let mut total = 0;
    let mut drain = false;
    loop {
        input.check()?;
        if !drain && pos == input.bytes.len() && pos < input.limit() {
            input.ensure(pos + 1)?;
        }
        let last = input.bytes.len() == input.limit();
        let step = decoder.step(&input.bytes[pos..], &mut buffer, last)?;
        pos += step.bytes_consumed;
        total += step.bytes_produced as u64;
        input.work(step.bytes_produced)?;
        if total > MAX_DECODED {
            return Err(Error::Limit(
                "decoded archive stream exceeds 128 MiB".into(),
            ));
        }
        if step.status == Status::StreamEnd {
            // LZIP can buffer a portion of the next file before verifying its trailer.
            let unused = match &decoder {
                Decoder::Lzip(s) => s.unused_input().len(),
                _ => 0,
            };
            return pos
                .checked_sub(unused)
                .filter(|end| *end > 0)
                .ok_or_else(|| Error::Invalid("compressed stream boundary".into()));
        }
        if step.bytes_produced == 0 && step.bytes_consumed == 0 {
            if drain && pos == input.bytes.len() && pos < input.limit() {
                drain = false;
                continue;
            }
            return Err(Error::Invalid("incomplete compressed stream".into()));
        }
        drain = step.bytes_produced == buffer.len();
    }
}
