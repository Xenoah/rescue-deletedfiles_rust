//! Finite ASF files with fixed-length media packets.
mod metadata;
mod packet;
use super::{
    Format,
    container::{Input, u32le},
};
use crate::{Error, Result};
type Guid = [u8; 16];
pub(super) const MAGIC: Guid = uuid::uuid!("75b22630-668e-11cf-a6d9-00aa0062ce6c").to_bytes_le();
const FILE: Guid = uuid::uuid!("8cabdca1-a947-11cf-8ee4-00c00c205365").to_bytes_le();
const STREAM: Guid = uuid::uuid!("b7dc0791-a9b7-11cf-8ee6-00c00c205365").to_bytes_le();
const DATA: Guid = uuid::uuid!("75b22636-668e-11cf-a6d9-00aa0062ce6c").to_bytes_le();
const SIMPLE_INDEX: Guid = uuid::uuid!("33000890-e5b1-11cf-89f4-00a0c90349cb").to_bytes_le();
const AUDIO: Guid = uuid::uuid!("f8699e40-5b4d-11cf-a8fd-00805f5c442b").to_bytes_le();
const VIDEO: Guid = uuid::uuid!("bc19efc0-5b4d-11cf-a8fd-00805f5c442b").to_bytes_le();
const EXTENSION: Guid = uuid::uuid!("5fbf03b5-a92e-11cf-8ee3-00c00c205365").to_bytes_le();
const EXT_RESERVED: Guid = uuid::uuid!("abd3d211-a9ba-11cf-8ee6-00c00c205365").to_bytes_le();
const EXT_STREAM: Guid = uuid::uuid!("14e6a5cb-c672-4332-8399-a96952065b5a").to_bytes_le();
const METADATA: Guid = uuid::uuid!("c5f8cbea-5baf-4877-8467-aa8c44fa4cca").to_bytes_le();
const DESCRIPTION: Guid = uuid::uuid!("75b22633-668e-11cf-a6d9-00aa0062ce6c").to_bytes_le();
const EXT_DESCRIPTION: Guid = uuid::uuid!("d2d0a440-e307-11d2-97f0-00a0c95ea850").to_bytes_le();
const CODECS: Guid = uuid::uuid!("86d15240-311d-11d0-a3a4-00a0c90348f6").to_bytes_le();
const PADDING: Guid = uuid::uuid!("1806d474-cadf-4509-a4ba-9aabcb96aae8").to_bytes_le();
fn invalid() -> Error {
    Error::Invalid("invalid or unsupported ASF header, stream, packet or index".into())
}
fn u16le(b: &[u8], p: usize) -> u16 {
    u16::from_le_bytes(b[p..p + 2].try_into().unwrap())
}
fn u64le(b: &[u8], p: usize) -> u64 {
    u64::from_le_bytes(b[p..p + 8].try_into().unwrap())
}
fn size(n: u64) -> Result<usize> {
    usize::try_from(n).map_err(|_| invalid())
}

struct Cursor<'a>(&'a [u8]);
impl<'a> Cursor<'a> {
    fn take(&mut self, n: usize) -> Result<&'a [u8]> {
        let (part, remaining) = self.0.split_at_checked(n).ok_or_else(invalid)?;
        self.0 = remaining;
        Ok(part)
    }
    fn word(&mut self) -> Result<usize> {
        Ok(usize::from(u16le(self.take(2)?, 0)))
    }
    fn end(self) -> Result<()> {
        if self.0.is_empty() {
            Ok(())
        } else {
            Err(invalid())
        }
    }
    fn byte(&mut self) -> Result<u8> {
        Ok(self.take(1)?[0])
    }
    fn field(&mut self, kind: u8, default: usize) -> Result<usize> {
        Ok(match kind & 3 {
            0 => default,
            1 => usize::from(self.byte()?),
            2 => usize::from(u16le(self.take(2)?, 0)),
            _ => u32le(self.take(4)?, 0) as usize,
        })
    }
    fn object(&mut self) -> Result<(Guid, &'a [u8])> {
        let h = self.take(24)?;
        let body = size(u64le(h, 16))?.checked_sub(24).ok_or_else(invalid)?;
        Ok((h[..16].try_into().unwrap(), self.take(body)?))
    }
}

struct Header {
    id: Guid,
    file_bytes: usize,
    packet_bytes: usize,
    packets: usize,
    streams: [bool; 128],
    extended: [bool; 128],
    windows_audio: bool,
    other_audio: bool,
    windows_video: bool,
    other_video: bool,
    short_count: bool,
}
impl Default for Header {
    fn default() -> Self {
        Self {
            id: [0; 16],
            file_bytes: 0,
            packet_bytes: 0,
            packets: 0,
            streams: [false; 128],
            extended: [false; 128],
            windows_audio: false,
            other_audio: false,
            windows_video: false,
            other_video: false,
            short_count: false,
        }
    }
}
impl Header {
    fn properties(&mut self, body: &[u8]) -> Result<()> {
        if self.file_bytes != 0 || body.len() != 80 || u32le(body, 64) & !2 != 0 {
            return Err(invalid());
        }
        self.id = body[..16].try_into().unwrap();
        self.file_bytes = size(u64le(body, 16))?;
        self.packets = size(u64le(body, 32))?;
        self.packet_bytes = u32le(body, 68) as usize;
        if self.file_bytes == 0
            || self.packets == 0
            || self.packets > 1_048_576
            || !(16..=65536).contains(&self.packet_bytes)
            || u32le(body, 72) as usize != self.packet_bytes
        {
            return Err(invalid());
        }
        Ok(())
    }
    fn stream(&mut self, b: &[u8]) -> Result<()> {
        if b.len() < 54 {
            return Err(invalid());
        }
        let n = u32le(b, 40) as usize;
        let correction = u32le(b, 44) as usize;
        let id = usize::from(u16le(b, 48));
        if id == 0
            || id >= 128
            || self.streams[id]
            || n.checked_add(correction) != Some(b.len() - 54)
        {
            return Err(invalid());
        }
        self.streams[id] = true;
        let t = &b[54..54 + n];
        if b[..16] == AUDIO {
            if t.len() < 16
                || u16le(t, 2) == 0
                || u32le(t, 4) == 0
                || (t.len() != 16 && (t.len() < 18 || usize::from(u16le(t, 16)) != t.len() - 18))
            {
                return Err(invalid());
            }
            if (0x160..=0x163).contains(&u16le(t, 0)) {
                self.windows_audio = true;
            } else {
                self.other_audio = true;
            }
        } else if b[..16] == VIDEO {
            if t.len() < 51
                || u32le(t, 0) == 0
                || u32le(t, 4) == 0
                || t[8] != 2
                || usize::from(u16le(t, 9)) != t.len() - 11
                || u32le(t, 11) < 40
                || u32le(t, 11) as usize > t.len() - 11
                || u32le(t, 0) != u32le(t, 15)
                || u32le(t, 4) != u32le(t, 19)
            {
                return Err(invalid());
            }
            if matches!(&t[27..31], b"WMV1" | b"WMV2" | b"WMV3" | b"WVC1") {
                self.windows_video = true;
            } else {
                self.other_video = true;
            }
        } else {
            return Err(invalid());
        }
        Ok(())
    }
    fn extension(&mut self, b: &[u8]) -> Result<()> {
        if b.len() < 22
            || b[..16] != EXT_RESERVED
            || u16le(b, 16) != 6
            || u32le(b, 18) as usize != b.len() - 22
        {
            return Err(invalid());
        }
        let mut nested = Cursor(&b[22..]);
        let mut count = 0;
        while !nested.0.is_empty() {
            count += 1;
            if count > 512 {
                return Err(invalid());
            }
            let (kind, body) = nested.object()?;
            match kind {
                EXT_STREAM => {
                    if body.len() != 64 || u32le(body, 60) != 0 {
                        return Err(invalid());
                    }
                    let id = usize::from(u16le(body, 48));
                    if id == 0 || id >= 128 || self.extended[id] {
                        return Err(invalid());
                    }
                    self.extended[id] = true;
                }
                METADATA => metadata::descriptors(body, true)?,
                _ => return Err(invalid()),
            }
        }
        Ok(())
    }
    fn kind(&self) -> Format<'static> {
        if self.windows_video && !self.other_video {
            Format::Wmv
        } else if !self.windows_video
            && !self.other_video
            && self.windows_audio
            && !self.other_audio
        {
            Format::Wma
        } else {
            Format::Asf
        }
    }
}

fn header(data: &[u8], declared: usize) -> Result<Header> {
    let mut result = Header::default();
    let mut objects = Cursor(data);
    let mut count = 0;
    let mut descriptive = 0u8;
    let mut extension = false;
    let mut codec = false;
    while !objects.0.is_empty() {
        count += 1;
        if count > 512 {
            return Err(invalid());
        }
        let (kind, b) = objects.object()?;
        match kind {
            FILE => result.properties(b)?,
            STREAM => result.stream(b)?,
            EXTENSION if !extension => {
                result.extension(b)?;
                extension = true;
            }
            CODECS if !codec => {
                metadata::codecs(b)?;
                codec = true;
            }
            DESCRIPTION | EXT_DESCRIPTION | PADDING => {
                let flag = match kind {
                    DESCRIPTION => 1,
                    EXT_DESCRIPTION => 2,
                    _ => 4,
                };
                if descriptive & flag != 0 {
                    return Err(invalid());
                }
                match kind {
                    DESCRIPTION => metadata::description(b)?,
                    EXT_DESCRIPTION => metadata::descriptors(b, false)?,
                    _ => {}
                }
                descriptive |= flag;
            }
            _ => return Err(invalid()),
        }
    }
    // GStreamer's asfmux counts File Properties, Stream Properties and Header Extension
    // but omits optional description and padding objects from the count.
    result.short_count = declared != count;
    if result.short_count
        && (codec || !extension || declared + descriptive.count_ones() as usize != count)
        || result.file_bytes == 0
        || !result.streams.iter().any(|&v| v)
        || result
            .extended
            .iter()
            .zip(result.streams)
            .any(|(&ext, stream)| ext && !stream)
    {
        return Err(invalid());
    }
    Ok(result)
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<(Format<'static>, usize)> {
    input.ensure(30)?;
    let h = &input.bytes;
    let end = size(u64le(h, 16))?;
    let count = u32le(h, 24) as usize;
    if h[..16] != MAGIC
        || h[28..30] != [1, 2]
        || !(30..=4 << 20).contains(&end)
        || !(1..=512).contains(&count)
    {
        return Err(invalid());
    }
    input.ensure(end)?;
    let h = header(&input.bytes[30..end], count)?;
    input.ensure(h.file_bytes)?;
    input.work(h.file_bytes)?;
    let mut tail = Cursor(input.bytes.get(end..h.file_bytes).ok_or_else(invalid)?);
    let (kind, data) = tail.object()?;
    if kind != DATA
        || data.len() < 26
        || data[..16] != h.id
        || u64le(data, 16) != h.packets as u64
        || data[24..26] != [1, 1]
        || h.packets.checked_mul(h.packet_bytes) != Some(data.len() - 26)
    {
        return Err(invalid());
    }
    let mut observed = [false; 128];
    for p in data[26..].chunks_exact(h.packet_bytes) {
        input.check()?;
        packet::validate(p, &h.streams, &mut observed, h.short_count)?;
    }
    if observed != h.streams {
        return Err(invalid());
    }
    let mut indices = 0;
    while !tail.0.is_empty() {
        let (kind, b) = tail.object()?;
        indices += 1;
        if indices > 127
            || kind != SIMPLE_INDEX
            || b.len() < 32
            || b[..16] != h.id
            || u64le(b, 16) == 0
            || !(b.len() - 32).is_multiple_of(6)
            || u32le(b, 28) as usize != (b.len() - 32) / 6
        {
            return Err(invalid());
        }
        let max = u32le(b, 24);
        let mut previous = 0;
        let mut seen_reference = false;
        for entry in b[32..].as_chunks::<6>().0 {
            let start = u32le(entry, 0);
            let n = u32::from(u16le(entry, 4));
            // FFmpeg writes (0, 0) for preroll preceding the first indexed keyframe.
            if n == 0 && start == 0 && !seen_reference {
                continue;
            }
            seen_reference = true;
            if n == 0
                || n > max
                || start < previous
                || u64::from(start) + u64::from(n) > h.packets as u64
            {
                return Err(invalid());
            }
            previous = start;
        }
    }
    Ok((h.kind(), h.file_bytes))
}
