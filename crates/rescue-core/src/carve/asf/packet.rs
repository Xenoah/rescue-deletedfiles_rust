//! Fixed-size ASF packets, including single and multiple payloads.
use super::{Cursor, invalid};
use crate::Result;

pub(super) fn validate(
    data: &[u8],
    streams: &[bool; 128],
    observed: &mut [bool; 128],
    gst_lengths: bool,
) -> Result<()> {
    let mut packet = Cursor(data);
    if packet.0.first() == Some(&0x82) {
        packet.take(3)?;
    }
    let flags = packet.byte()?;
    let property = packet.byte()?;
    if flags & 0x80 != 0 || property >> 6 != 1 {
        return Err(invalid());
    }
    let declared = packet.field(flags >> 5, data.len())?;
    packet.field(flags >> 1, 0)?; // packet sequence
    let padding = packet.field(flags >> 3, 0)?;
    if padding >= data.len()
        || !(declared == data.len() || gst_lengths && declared == data.len() - padding)
    {
        return Err(invalid());
    }
    packet.take(6)?; // send time and duration
    let useful = packet.0.len().checked_sub(padding).ok_or_else(invalid)?;
    let media = packet.take(useful)?;
    if packet.0.iter().any(|&byte| byte != 0) {
        return Err(invalid());
    }
    packet = Cursor(media);
    let multi = flags & 1 != 0;
    let descriptor = if multi { packet.byte()? } else { 1 };
    let count = descriptor & 63;
    if count == 0 || multi && descriptor >> 6 == 0 {
        return Err(invalid());
    }
    for _ in 0..count {
        let stream = usize::from(packet.byte()? & 0x7f);
        if !streams[stream] {
            return Err(invalid());
        }
        observed[stream] = true;
        packet.field(property >> 4, 0)?; // media object number
        let offset = packet.field(property >> 2, 0)?;
        let replicated = packet.field(property, 0)?;
        let fields = packet.take(replicated)?;
        let len = if multi {
            packet.field(descriptor >> 6, 0)?
        } else {
            packet.0.len()
        };
        let payload = packet.take(len)?;
        if len == 0 {
            return Err(invalid());
        }
        match fields.len() {
            0 => {}
            1 => {
                let mut compressed = Cursor(payload);
                while !compressed.0.is_empty() {
                    let n = usize::from(compressed.byte()?);
                    if n == 0 {
                        return Err(invalid());
                    }
                    compressed.take(n)?;
                }
            }
            8 => {
                let object = super::u32le(fields, 0) as usize;
                if offset.checked_add(len).is_none_or(|end| end > object) {
                    return Err(invalid());
                }
            }
            _ => return Err(invalid()),
        }
    }
    if !packet.0.is_empty() {
        return Err(invalid());
    }
    Ok(())
}
