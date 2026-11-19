//! Decode one bounded UDIF run. Every compressed byte must belong to the stream.
use super::{Result, invalid};

fn adc(encoded: &[u8], out: &mut [u8]) -> Result<()> {
    let mut p = 0;
    let mut written = 0;
    while p < encoded.len() {
        let tag = encoded[p] as usize;
        p += 1;
        if tag & 0x80 != 0 {
            let n = (tag & 127) + 1;
            let literal = encoded
                .get(p..p + n)
                .ok_or_else(|| invalid("ADC literal length"))?;
            out.get_mut(written..written + n)
                .ok_or_else(|| invalid("ADC output length"))?
                .copy_from_slice(literal);
            p += n;
            written += n;
        } else {
            let first = *encoded.get(p).ok_or_else(|| invalid("ADC distance"))? as usize;
            p += 1;
            let (length, distance) = if tag & 64 == 0 {
                (((tag & 63) >> 2) + 3, ((tag & 3) << 8) + first + 1)
            } else {
                let second = *encoded.get(p).ok_or_else(|| invalid("ADC distance"))? as usize;
                p += 1;
                ((tag & 63) + 4, (first << 8) + second + 1)
            };
            if distance > written || length > out.len() - written {
                return Err(invalid("ADC copy outside decoded run"));
            }
            for _ in 0..length {
                out[written] = out[written - distance];
                written += 1;
            }
        }
    }
    if written != out.len() {
        return Err(invalid("ADC output length"));
    }
    Ok(())
}

pub(super) fn decode(kind: u32, encoded: &[u8], length: usize) -> Result<Vec<u8>> {
    // One spare byte lets the codec consume its trailer and distinguishes
    // oversized runs from an exactly full output buffer.
    let mut out = vec![0; length + 1];
    let valid = match kind {
        super::ADC => {
            adc(encoded, &mut out[..length])?;
            true
        }
        super::ZLIB => {
            let mut codec = flate2::Decompress::new(true);
            let status = codec
                .decompress(encoded, &mut out, flate2::FlushDecompress::Finish)
                .map_err(|_| invalid("zlib stream"))?;
            status == flate2::Status::StreamEnd
                && codec.total_in() == encoded.len() as u64
                && codec.total_out() == length as u64
        }
        super::BZIP => {
            let mut codec = bzip2::Decompress::new(false);
            let status = codec
                .decompress(encoded, &mut out)
                .map_err(|_| invalid("bzip2 stream"))?;
            status == bzip2::Status::StreamEnd
                && codec.total_in() == encoded.len() as u64
                && codec.total_out() == length as u64
        }
        _ => return Err(invalid("unexpected codec")),
    };
    if !valid {
        return Err(invalid("compressed run length or checksum"));
    }
    out.truncate(length);
    Ok(out)
}
