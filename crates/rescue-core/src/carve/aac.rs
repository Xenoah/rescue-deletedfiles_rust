//! Contiguous, unprotected mono/stereo AAC-LC in Audio Data Transport Stream framing.
use super::container::Input;
use crate::{Error, Result};
use symphonia_codec_aac::AacDecoder;
use symphonia_core::{
    codecs::audio::{
        AudioCodecParameters, AudioDecoder, AudioDecoderOptions, well_known::CODEC_ID_AAC,
    },
    io::{BitReaderLtr, ReadBitsLtr},
    packet::PacketRef,
    units::{Duration, Timestamp},
};

#[derive(Clone, Copy, PartialEq, Eq)]
struct Config {
    mpeg2: bool,
    rate: u8,
    channels: u8,
}
fn header(bytes: &[u8]) -> Option<(Config, usize)> {
    let h = bytes.get(..7)?;
    // The layer, CRC flag, AAC profile and number of raw data blocks are explicit.
    if h[0] != 0xff || !matches!(h[1], 0xf1 | 0xf9) || h[2] >> 6 != 1 || h[6] & 3 != 0 {
        return None;
    }
    let rate = (h[2] >> 2) & 15;
    let channels = (h[2] & 1) << 2 | h[3] >> 6;
    let length = (usize::from(h[3] & 3) << 11) | (usize::from(h[4]) << 3) | usize::from(h[5] >> 5);
    (rate < 13 && matches!(channels, 1 | 2) && length > 7).then_some((
        Config {
            mpeg2: h[1] == 0xf9,
            rate,
            channels,
        },
        length,
    ))
}

pub(super) const LOOKAHEAD: u64 = 2 * 8191 + 6;
pub(super) fn signature(bytes: &[u8]) -> bool {
    let Some((first, n)) = header(bytes) else {
        return false;
    };
    let Some((second, m)) = bytes.get(n..).and_then(header) else {
        return false;
    };
    first == second
        && bytes
            .get(n + m..)
            .and_then(header)
            .is_some_and(|(third, _)| third == first)
}
fn invalid(reason: &str) -> Error {
    Error::Invalid(format!("AAC/ADTS: {reason}"))
}

// An AAC decoder may render an empty block as silence. Verify that leading
// ancillary elements reach a coded channel and that ID_END precedes byte padding.
fn audio_block(payload: &[u8], channels: u8) -> std::io::Result<bool> {
    if payload.len() < 2 {
        return Ok(false);
    }
    let tail = u16::from_be_bytes(payload[payload.len() - 2..].try_into().unwrap());
    let pad = tail.trailing_zeros();
    if pad > 7 || (tail >> pad) & 7 != 7 {
        return Ok(false);
    }
    let mut bits = BitReaderLtr::new(payload);
    loop {
        match bits.read_bits_leq32(3)? {
            0 => return Ok(true),
            1 => return Ok(channels == 2),
            4 => {
                bits.ignore_bits(4)?;
                let align = bits.read_bits_leq32(1)? != 0;
                let mut count = bits.read_bits_leq32(8)?;
                if count == 255 {
                    count += bits.read_bits_leq32(8)?;
                }
                if align {
                    bits.realign();
                }
                bits.ignore_bits(count * 8)?;
            }
            6 => {
                let mut count = bits.read_bits_leq32(4)?;
                if count == 15 {
                    count = 14 + bits.read_bits_leq32(8)?;
                }
                bits.ignore_bits(count * 8)?;
            }
            _ => return Ok(false),
        }
    }
}

pub(super) fn parse(input: &mut Input<'_>) -> Result<usize> {
    input.ensure(7)?;
    let (config, _) = header(&input.bytes).ok_or_else(|| invalid("unsupported ADTS header"))?;
    let mut params = AudioCodecParameters::new();
    // ISO AudioSpecificConfig: AAC-LC object type, frequency index, then channel config.
    params.for_codec(CODEC_ID_AAC).with_extra_data(
        vec![
            0x10 | (config.rate >> 1),
            (config.rate & 1) << 7 | config.channels << 3,
        ]
        .into_boxed_slice(),
    );
    let mut decoder = AacDecoder::try_new(&params, &AudioDecoderOptions::default())
        .map_err(|_| invalid("unsupported audio configuration"))?;
    let mut end = 0;
    let mut frames = 0;
    while end < input.limit() {
        let rest = input.limit() - end;
        input.ensure(end + rest.min(2))?;
        let look = &input.bytes[end..];
        if look[0] != 0xff || (rest >= 2 && look[1] & 0xf6 != 0xf0) {
            break;
        }
        input.ensure(end + 7)?;
        let (next, length) = header(&input.bytes[end..])
            .ok_or_else(|| invalid("damaged or unsupported ADTS frame"))?;
        if next != config {
            break;
        }
        input.ensure(end + length)?;
        input.work(1024 * usize::from(config.channels) * 4)?;
        if !audio_block(&input.bytes[end + 7..end + length], config.channels).unwrap_or(false) {
            return Err(invalid(
                "missing coded channels or invalid raw-block termination",
            ));
        }
        let packet = PacketRef::new(
            0,
            Timestamp::ZERO,
            Duration::new(1024),
            &input.bytes[end + 7..end + length],
        );
        let audio = decoder
            .decode_ref(&packet)
            .map_err(|_| invalid("undecodable AAC-LC payload"))?;
        if audio.frames() != 1024 || audio.spec().channels().count() != usize::from(config.channels)
        {
            return Err(invalid("unexpected decoded audio geometry"));
        }
        frames += 1;
        end += length;
    }
    if frames < 3 {
        return Err(invalid("at least three matching frames are required"));
    }
    Ok(end)
}
