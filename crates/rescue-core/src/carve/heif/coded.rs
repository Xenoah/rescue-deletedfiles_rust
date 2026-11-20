//! Check declared HEVC NAL units and AV1 low-overhead OBU boundaries.
use super::{Fields, invalid};
use crate::Result;

const MAX_UNITS: usize = 65_536;

fn nal(unit: &[u8]) -> Result<u8> {
    if unit.len() < 3 || unit[0] & 0x80 != 0 || unit[1] & 7 == 0 {
        return Err(invalid("invalid HEVC NAL header"));
    }
    Ok((unit[0] >> 1) & 63)
}

pub(super) fn hevc(config: &[u8], payload: &[u8]) -> Result<()> {
    if config.len() < 23 || config[0] != 1 {
        return Err(invalid("HEVC configuration record"));
    }
    let width = usize::from((config[21] & 3) + 1);
    let mut f = Fields {
        rest: &config[22..],
    };
    let mut sets = [false; 3];
    let mut units = 0;
    for _ in 0..f.num(1)? {
        let header = f.num(1)?;
        if header & 0x40 != 0 {
            return Err(invalid("HEVC array flags"));
        }
        let kind = (header & 63) as u8;
        let count = f.num(2)?;
        units += count;
        if units > MAX_UNITS {
            return Err(invalid("too many HEVC configuration units"));
        }
        for _ in 0..count {
            let size = f.num(2)?;
            if nal(f.take(size)?)? != kind {
                return Err(invalid("HEVC array type mismatch"));
            }
            if (32..=34).contains(&kind) {
                sets[usize::from(kind - 32)] = true;
            }
        }
    }
    f.finish()?;
    if !sets.iter().all(|v| *v) {
        return Err(invalid("missing HEVC parameter set"));
    }
    let mut f = Fields { rest: payload };
    let mut picture = false;
    let mut count = 0;
    while !f.rest.is_empty() {
        count += 1;
        if count > MAX_UNITS {
            return Err(invalid("too many HEVC picture units"));
        }
        let size = f.num(width)?;
        picture |= nal(f.take(size)?)? <= 31;
    }
    if !picture {
        return Err(invalid("missing HEVC coded picture"));
    }
    Ok(())
}

#[derive(Default)]
struct Av1Units {
    sequence: bool,
    frame_header: bool,
    tile_group: bool,
    frame: bool,
}

impl Av1Units {
    fn read(&mut self, input: &[u8], configuration: bool) -> Result<()> {
        let mut f = Fields { rest: input };
        let mut count = 0;
        while !f.rest.is_empty() {
            count += 1;
            if count > MAX_UNITS {
                return Err(invalid("too many AV1 OBUs"));
            }
            let header = f.num(1)?;
            if header & 0x81 != 0 {
                return Err(invalid("AV1 OBU header"));
            }
            let kind = (header >> 3) & 15;
            if header & 4 != 0 && f.num(1)? & 7 != 0 {
                return Err(invalid("AV1 OBU extension"));
            }
            let size = if header & 2 != 0 {
                let mut size = 0_u64;
                for index in 0..8 {
                    let next = f.num(1)? as u64;
                    size |= (next & 127) << (7 * index);
                    if next & 128 == 0 {
                        break;
                    }
                    if index == 7 {
                        return Err(invalid("AV1 OBU length overflow"));
                    }
                }
                usize::try_from(size).map_err(|_| invalid("AV1 OBU too large"))?
            } else if !configuration {
                f.rest.len()
            } else {
                return Err(invalid("unsized AV1 configuration OBU"));
            };
            f.take(size)?;
            match kind {
                1 if size > 0 => self.sequence = true,
                2 if size == 0 && !configuration => {}
                3 if size > 0 && !configuration => self.frame_header = true,
                4 if size > 0 && self.frame_header && !configuration => self.tile_group = true,
                6 if size > 0 && !configuration => self.frame = true,
                5 if size > 0 => {}
                7 | 8 if size > 0 && !configuration => {}
                15 => {}
                _ => return Err(invalid("invalid or unsupported AV1 OBU")),
            }
        }
        Ok(())
    }
}

pub(super) fn av1(config: &[u8], payload: &[u8]) -> Result<()> {
    if config.len() < 4
        || config[0] != 0x81
        || config[3] & 0xe0 != 0
        || (config[3] & 0x10 == 0 && config[3] & 15 != 0)
    {
        return Err(invalid("AV1 configuration record"));
    }
    let mut units = Av1Units::default();
    units.read(&config[4..], true)?;
    units.read(payload, false)?;
    if !units.sequence || !(units.frame || units.frame_header && units.tile_group) {
        return Err(invalid("missing AV1 sequence or picture"));
    }
    Ok(())
}
