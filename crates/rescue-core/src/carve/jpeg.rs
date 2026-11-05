//! Contiguous 8-bit Huffman JPEG. Validate exact entropy consumption independently
//! before asking the pixel decoder to decode: display decoders can repair truncation.
use super::charge;
use crate::{Error, Result, source::Image};
use std::sync::atomic::{AtomicBool, Ordering};
use zune_core::{bytestream::ZCursor, options::DecoderOptions};
use zune_jpeg::JpegDecoder;

const MAX_ENCODED: usize = 64 * 1024 * 1024;
const MAX_PIXELS: usize = 64 * 1024 * 1024;
const MAX_COEFFICIENTS: usize = 128 * 1024 * 1024;
const MAX_MARKERS: usize = 65_536;
const MAX_SCANS: usize = 256;

fn invalid(message: &str) -> Error {
    Error::Invalid(format!("JPEG: {message}"))
}

/// Keep the exact encoded bytes, but fetch at most 4 KiB ahead. Never cross the
/// user-selected range. Truncate read-ahead at the parsed EOI before hashing.
struct Input<'a> {
    image: &'a mut Image,
    offset: u64,
    boundary: u64,
    bytes: Vec<u8>,
    at: usize,
    cancel: &'a AtomicBool,
    budget: &'a mut u64,
}
impl Input<'_> {
    fn stopped(&self) -> Result<()> {
        if self.cancel.load(Ordering::Relaxed) {
            Err(Error::Cancelled)
        } else {
            Ok(())
        }
    }
    fn ensure(&mut self, count: usize) -> Result<()> {
        let end = self
            .at
            .checked_add(count)
            .ok_or_else(|| invalid("offset overflow"))?;
        if end > MAX_ENCODED {
            return Err(Error::Limit("JPEG encoded extent exceeds 64 MiB".into()));
        }
        if self
            .offset
            .checked_add(end as u64)
            .is_none_or(|n| n > self.boundary)
        {
            return Err(invalid("truncated or outside selected range"));
        }
        while self.bytes.len() < end {
            self.stopped()?;
            let start = self.offset + self.bytes.len() as u64;
            let count = 4096
                .min(MAX_ENCODED - self.bytes.len())
                .min((self.boundary - start) as usize);
            charge(self.budget, count)?;
            self.bytes.extend(self.image.read(start, count)?);
        }
        Ok(())
    }
    fn byte(&mut self) -> Result<u8> {
        if self.at == self.bytes.len() {
            self.ensure(1)?;
        }
        let value = self.bytes[self.at];
        self.at += 1;
        Ok(value)
    }
    fn marker(&mut self) -> Result<u8> {
        self.stopped()?;
        if self.byte()? != 0xff {
            return Err(invalid("extra bytes between markers"));
        }
        loop {
            let code = self.byte()?;
            if code == 0 {
                return Err(invalid("stuffed byte outside entropy data"));
            }
            if code != 0xff {
                return Ok(code);
            }
        }
    }
    fn segment(&mut self) -> Result<Vec<u8>> {
        let length = (usize::from(self.byte()?) << 8) | usize::from(self.byte()?);
        let count = length
            .checked_sub(2)
            .ok_or_else(|| invalid("segment length below two"))?;
        self.ensure(count)?;
        let value = self.bytes[self.at..self.at + count].to_vec();
        self.at += count;
        Ok(value)
    }
}

/// No implicit zero fill, marker seeking or error repair. At an entropy boundary
/// there may only be 0..7 one padding bits and the expected marker.
struct Bits<'a, 'b> {
    input: &'a mut Input<'b>,
    byte: u8,
    left: u8,
}
impl Bits<'_, '_> {
    fn bit(&mut self) -> Result<u32> {
        if self.left == 0 {
            self.byte = self.input.byte()?;
            if self.byte == 0xff && self.input.byte()? != 0 {
                return Err(invalid("premature marker in entropy data"));
            }
            self.left = 8;
        }
        self.left -= 1;
        Ok(u32::from((self.byte >> self.left) & 1))
    }
    fn take(&mut self, count: u8) -> Result<u32> {
        let mut value = 0;
        for _ in 0..count {
            value = (value << 1) | self.bit()?;
        }
        Ok(value)
    }
    fn boundary(&mut self) -> Result<u8> {
        if self.left > 0 {
            let mask = (1u16 << self.left) - 1;
            if u16::from(self.byte) & mask != mask {
                return Err(invalid("non-one entropy padding"));
            }
        }
        self.left = 0;
        self.input.marker()
    }
}

#[derive(Clone)]
struct Huffman {
    first: [u32; 16],
    counts: [u8; 16],
    indices: [usize; 16],
    symbols: Vec<u8>,
}
impl Huffman {
    fn parse(counts: &[u8], symbols: &[u8], dc: bool) -> Result<Self> {
        if symbols.is_empty() || symbols.len() > 256 {
            return Err(invalid("empty or oversized Huffman table"));
        }
        let mut table = Self {
            first: [0; 16],
            counts: counts.try_into().unwrap(),
            indices: [0; 16],
            symbols: symbols.to_vec(),
        };
        let (mut code, mut index) = (0, 0);
        for (length, &count) in counts.iter().enumerate() {
            // The all-ones code is forbidden, so fill bits cannot be coefficients.
            if code + u32::from(count) >= 1 << (length + 1) {
                return Err(invalid("oversubscribed or all-ones Huffman code"));
            }
            table.first[length] = code;
            table.indices[length] = index;
            code = (code + u32::from(count)) << 1;
            index += usize::from(count);
        }
        if symbols
            .iter()
            .any(|&s| if dc { s > 11 } else { s & 15 > 10 })
        {
            return Err(invalid("Huffman symbol outside 8-bit DCT range"));
        }
        Ok(table)
    }
    fn symbol(&self, bits: &mut Bits<'_, '_>) -> Result<u8> {
        let mut code = 0;
        for length in 0..16 {
            code = (code << 1) | bits.bit()?;
            if code >= self.first[length]
                && code - self.first[length] < u32::from(self.counts[length])
            {
                return Ok(
                    self.symbols[self.indices[length] + (code - self.first[length]) as usize]
                );
            }
        }
        Err(invalid("invalid Huffman code"))
    }
}

struct Component {
    id: u8,
    h: usize,
    v: usize,
    quant: usize,
    cols: usize,
    rows: usize,
    stride: usize,
    nonzero: Vec<u64>,
    levels: [i8; 64],
}
struct Frame {
    mode: u8,
    mcu_cols: usize,
    mcu_rows: usize,
    components: Vec<Component>,
}
impl Frame {
    fn parse(mode: u8, data: &[u8], budget: &mut u64) -> Result<Self> {
        if data.len() < 6 || data.len() != 6 + usize::from(data[5]) * 3 {
            return Err(invalid("frame length"));
        }
        if data[0] != 8 {
            return Err(Error::Unsupported(
                "JPEG precision other than eight bits".into(),
            ));
        }
        let height = usize::from(u16::from_be_bytes([data[1], data[2]]));
        let width = usize::from(u16::from_be_bytes([data[3], data[4]]));
        if width == 0 || height == 0 {
            return Err(Error::Unsupported(
                "JPEG zero dimensions / deferred height".into(),
            ));
        }
        if ![1, 3, 4].contains(&data[5]) {
            return Err(Error::Unsupported("JPEG component count".into()));
        }
        if width * height * 4 > MAX_PIXELS {
            return Err(Error::Limit("JPEG pixel reservation exceeds 64 MiB".into()));
        }
        let mut components: Vec<Component> = Vec::new();
        for d in data[6..].as_chunks::<3>().0 {
            let (h, v) = (usize::from(d[1] >> 4), usize::from(d[1] & 15));
            if !(1..=4).contains(&h)
                || !(1..=4).contains(&v)
                || d[2] > 3
                || components.iter().any(|c| c.id == d[0])
            {
                return Err(invalid(
                    "frame component, sampling or quantization selector",
                ));
            }
            components.push(Component {
                id: d[0],
                h,
                v,
                quant: usize::from(d[2]),
                cols: 0,
                rows: 0,
                stride: 0,
                nonzero: Vec::new(),
                levels: [-1; 64],
            });
        }
        let max_h = components.iter().map(|c| c.h).max().unwrap();
        let max_v = components.iter().map(|c| c.v).max().unwrap();
        let mcu_cols = width.div_ceil(8 * max_h);
        let mcu_rows = height.div_ceil(8 * max_v);
        if components.iter().map(|c| c.h * c.v).sum::<usize>() > 10 {
            return Err(Error::Unsupported(
                "JPEG more than ten blocks per MCU".into(),
            ));
        }
        let mut memory = 0;
        for c in &mut components {
            c.cols = (width * c.h).div_ceil(8 * max_h);
            c.rows = (height * c.v).div_ceil(8 * max_v);
            c.stride = mcu_cols * c.h;
            let blocks = c.stride * mcu_rows * c.v;
            memory += blocks * 64 * 2; // Decoder's i16 DCT coefficient storage.
            if memory > MAX_COEFFICIENTS {
                return Err(Error::Limit("JPEG DCT coefficients exceed 128 MiB".into()));
            }
            charge(budget, blocks * 8)?;
            c.nonzero = vec![0; blocks];
        }
        charge(budget, memory)?;
        Ok(Self {
            mode,
            mcu_cols,
            mcu_rows,
            components,
        })
    }
}

#[derive(Clone, Copy)]
struct ScanComponent {
    index: usize,
    dc: usize,
    ac: usize,
}
struct Scan {
    components: Vec<ScanComponent>,
    start: u8,
    end: u8,
    high: u8,
    low: u8,
}
impl Scan {
    fn parse(
        data: &[u8],
        frame: &Frame,
        tables: &[Option<Huffman>; 8],
        quant: &[Option<u8>; 4],
    ) -> Result<Self> {
        if data.is_empty()
            || data[0] == 0
            || data[0] > 4
            || data.len() != 4 + usize::from(data[0]) * 2
        {
            return Err(invalid("scan length or component count"));
        }
        let end = data.len();
        let scan = Self {
            components: Vec::new(),
            start: data[end - 3],
            end: data[end - 2],
            high: data[end - 1] >> 4,
            low: data[end - 1] & 15,
        };
        if scan.start > scan.end || scan.end > 63 || scan.high > 13 || scan.low > 13 {
            return Err(invalid("spectral selection / successive approximation"));
        }
        if frame.mode != 0xc2 {
            if (scan.start, scan.end, scan.high, scan.low) != (0, 63, 0, 0) {
                return Err(invalid("sequential scan parameters"));
            }
        } else if (scan.start == 0 && scan.end != 0)
            || (scan.start > 0 && data[0] != 1)
            || (scan.high > 0 && scan.low + 1 != scan.high)
        {
            return Err(invalid("progressive scan parameters"));
        }
        let mut scan = scan;
        for d in data[1..end - 3].as_chunks::<2>().0 {
            let index = frame
                .components
                .iter()
                .position(|c| c.id == d[0])
                .ok_or_else(|| invalid("unknown scan component"))?;
            let c = &frame.components[index];
            let (dc, ac) = (usize::from(d[1] >> 4), usize::from(d[1] & 15));
            if dc > 3 || ac > 3 || scan.components.iter().any(|s| s.index == index) {
                return Err(invalid("scan table selector / duplicate component"));
            }
            if quant[c.quant].is_none() || (frame.mode == 0xc0 && quant[c.quant] != Some(0)) {
                return Err(invalid("missing or non-baseline quantization table"));
            }
            if (scan.start == 0 && scan.high == 0 && tables[dc].is_none())
                || ((scan.end > 0 || frame.mode != 0xc2) && tables[4 + ac].is_none())
            {
                return Err(invalid("missing Huffman table"));
            }
            if frame.mode == 0xc2 && ((scan.start == 0 && ac != 0) || (scan.start > 0 && dc != 0)) {
                return Err(invalid("unused progressive table selector"));
            }
            for &previous in &c.levels[usize::from(scan.start)..=usize::from(scan.end)] {
                if (scan.high == 0 && previous != -1)
                    || (scan.high != 0 && previous != scan.high as i8)
                {
                    return Err(invalid("duplicate or out-of-order coefficient scan"));
                }
            }
            if scan.start > 0 && c.levels[0] < 0 {
                return Err(invalid("AC before DC scan"));
            }
            scan.components.push(ScanComponent { index, dc, ac });
        }
        Ok(scan)
    }

    fn block(
        &self,
        bits: &mut Bits<'_, '_>,
        tables: &[Option<Huffman>; 8],
        selection: ScanComponent,
        mask: &mut u64,
        eob: &mut u32,
        progressive: bool,
    ) -> Result<()> {
        charge(bits.input.budget, 64)?; // Bound work across many successive scans.
        if self.start == 0 {
            if self.high == 0 {
                let count = tables[selection.dc].as_ref().unwrap().symbol(bits)?;
                bits.take(count)?;
            } else {
                bits.bit()?;
            }
            if progressive {
                return Ok(());
            }
        }
        let table = tables[4 + selection.ac].as_ref().unwrap();
        let mut k = usize::from(self.start.max(1));
        let end = usize::from(self.end);
        if self.high == 0 {
            if *eob > 0 {
                *eob -= 1;
                return Ok(());
            }
            while k <= end {
                let symbol = table.symbol(bits)?;
                let (run, count) = (usize::from(symbol >> 4), symbol & 15);
                if count == 0 {
                    if run == 15 {
                        k += 16;
                        if k > end + 1 {
                            return Err(invalid("zero run exceeds coefficient band"));
                        }
                    } else {
                        if !progressive && run != 0 {
                            return Err(invalid("progressive EOB in sequential scan"));
                        }
                        *eob = (1u32 << run) + bits.take(run as u8)? - 1;
                        break;
                    }
                } else {
                    k += run;
                    if k > end {
                        return Err(invalid("AC run exceeds coefficient band"));
                    }
                    bits.take(count)?;
                    *mask |= 1u64 << k;
                    k += 1;
                }
            }
        } else {
            if *eob == 0 {
                while k <= end {
                    let symbol = table.symbol(bits)?;
                    let (mut zeros, count) = (usize::from(symbol >> 4), symbol & 15);
                    if count > 1 {
                        return Err(invalid("refinement coefficient has more than one bit"));
                    }
                    if count == 0 && zeros != 15 {
                        *eob = (1u32 << zeros) + bits.take(zeros as u8)?;
                        break;
                    }
                    if count == 1 {
                        bits.bit()?;
                    } else {
                        zeros = 16;
                    }
                    while k <= end {
                        if *mask & (1u64 << k) != 0 {
                            bits.bit()?;
                        } else if zeros > 0 {
                            zeros -= 1;
                        } else {
                            break;
                        }
                        k += 1;
                        if count == 0 && zeros == 0 {
                            break;
                        }
                    }
                    if zeros != 0 {
                        return Err(invalid("refinement zero run exceeds band"));
                    }
                    if count == 1 {
                        if k > end {
                            return Err(invalid("refinement coefficient outside band"));
                        }
                        *mask |= 1u64 << k;
                        k += 1;
                    }
                }
            }
            if *eob > 0 {
                while k <= end {
                    if *mask & (1u64 << k) != 0 {
                        bits.bit()?;
                    }
                    k += 1;
                }
                *eob -= 1;
            }
        }
        Ok(())
    }

    /// Returns the already-consumed marker following this scan.
    fn entropy(
        &self,
        input: &mut Input<'_>,
        frame: &mut Frame,
        tables: &[Option<Huffman>; 8],
        restart: usize,
    ) -> Result<u8> {
        let interleaved = self.components.len() > 1;
        let single = &frame.components[self.components[0].index];
        let (cols, rows) = if interleaved {
            (frame.mcu_cols, frame.mcu_rows)
        } else {
            (single.cols, single.rows)
        };
        let mut bits = Bits {
            input,
            byte: 0,
            left: 0,
        };
        let (mut eob, mut rst) = (0, 0);
        for mcu in 0..cols * rows {
            if mcu.is_multiple_of(256) {
                bits.input.stopped()?;
            }
            if mcu > 0 && restart > 0 && mcu.is_multiple_of(restart) {
                if eob != 0 || bits.boundary()? != 0xd0 + rst {
                    return Err(invalid("restart interval, sequence or unfinished EOB run"));
                }
                rst = (rst + 1) & 7;
            }
            for &selection in &self.components {
                let c = &mut frame.components[selection.index];
                let (h, v) = if interleaved { (c.h, c.v) } else { (1, 1) };
                for y in 0..v {
                    for x in 0..h {
                        let index = (mcu / cols * v + y) * c.stride + (mcu % cols * h + x);
                        self.block(
                            &mut bits,
                            tables,
                            selection,
                            &mut c.nonzero[index],
                            &mut eob,
                            frame.mode == 0xc2,
                        )?;
                    }
                }
            }
        }
        if eob != 0 {
            return Err(invalid("EOB run extends beyond scan"));
        }
        let marker = bits.boundary()?;
        for selection in &self.components {
            frame.components[selection.index].levels
                [usize::from(self.start)..=usize::from(self.end)]
                .fill(self.low as i8);
        }
        Ok(marker)
    }
}

pub(super) fn validate(
    image: &mut Image,
    offset: u64,
    cancel: &AtomicBool,
    budget: &mut u64,
    boundary: u64,
) -> Result<Vec<u8>> {
    let mut input = Input {
        image,
        offset,
        boundary,
        bytes: Vec::new(),
        at: 0,
        cancel,
        budget,
    };
    if input.byte()? != 0xff || input.byte()? != 0xd8 {
        return Err(invalid("SOI signature"));
    }
    let mut frame: Option<Frame> = None;
    let mut tables: [Option<Huffman>; 8] = std::array::from_fn(|_| None);
    let mut quant = [None; 4];
    let (mut scans, mut restart, mut pending) = (0, 0, None);
    let mut finished = false;
    for _ in 0..MAX_MARKERS {
        input.stopped()?;
        let marker = match pending.take() {
            Some(m) => m,
            None => input.marker()?,
        };
        match marker {
            0xd9 => {
                let frame = frame.as_ref().ok_or_else(|| invalid("EOI without frame"))?;
                if scans == 0
                    || frame
                        .components
                        .iter()
                        .any(|c| c.levels.iter().any(|&l| l != 0))
                {
                    return Err(Error::Unsupported("JPEG missing component / spectral band / final refinement; incomplete progressive images are not validated".into()));
                }
                finished = true;
                break;
            }
            0xd8 | 0xd0..=0xd7 | 0x00 | 0xff => {
                return Err(invalid("unexpected standalone marker"));
            }
            0xc0..=0xc2 => {
                if frame.is_some() {
                    return Err(invalid("multiple frames"));
                }
                frame = Some(Frame::parse(marker, &input.segment()?, input.budget)?);
            }
            0xc4 => {
                let data = input.segment()?;
                if data.is_empty() {
                    return Err(invalid("empty DHT"));
                }
                let mut at = 0;
                while at < data.len() {
                    if data.len() - at < 17 {
                        return Err(invalid("DHT counts truncated"));
                    }
                    let selector = data[at];
                    let (class, id) = (selector >> 4, selector & 15);
                    if class > 1 || id > 3 {
                        return Err(invalid("DHT selector"));
                    }
                    let counts = &data[at + 1..at + 17];
                    let count = counts.iter().map(|&n| usize::from(n)).sum::<usize>();
                    if at + 17 + count > data.len() {
                        return Err(invalid("DHT symbols truncated"));
                    }
                    tables[usize::from(class) * 4 + usize::from(id)] = Some(Huffman::parse(
                        counts,
                        &data[at + 17..at + 17 + count],
                        class == 0,
                    )?);
                    at += 17 + count;
                }
            }
            0xdb => {
                let data = input.segment()?;
                if data.is_empty() {
                    return Err(invalid("empty DQT"));
                }
                let mut at = 0;
                while at < data.len() {
                    let (precision, id) = (data[at] >> 4, usize::from(data[at] & 15));
                    at += 1;
                    if precision > 1 || id > 3 {
                        return Err(invalid("DQT selector"));
                    }
                    let width = usize::from(precision) + 1;
                    if at + 64 * width > data.len() {
                        return Err(invalid("DQT truncated"));
                    }
                    if data[at..at + 64 * width]
                        .chunks_exact(width)
                        .any(|v| v.iter().all(|&n| n == 0))
                    {
                        return Err(invalid("zero quantization value"));
                    }
                    quant[id] = Some(precision);
                    at += 64 * width;
                }
            }
            0xdd => {
                let data = input.segment()?;
                if data.len() != 2 {
                    return Err(invalid("DRI length"));
                }
                restart = usize::from(u16::from_be_bytes(data.try_into().unwrap()));
            }
            0xda => {
                scans += 1;
                if scans > MAX_SCANS {
                    return Err(Error::Limit("JPEG scan count exceeds 256".into()));
                }
                let frame = frame.as_mut().ok_or_else(|| invalid("SOS before SOF"))?;
                let scan = Scan::parse(&input.segment()?, frame, &tables, &quant)?;
                pending = Some(scan.entropy(&mut input, frame, &tables, restart)?);
            }
            0xe0..=0xef | 0xfe => {
                input.segment()?;
            }
            _ => {
                return Err(Error::Unsupported(format!(
                    "JPEG marker FF{marker:02X}; arithmetic, lossless, hierarchical and deferred-height JPEG are unsupported"
                )));
            }
        }
    }
    if !finished {
        return Err(Error::Limit("JPEG marker count exceeds 65536".into()));
    }
    input.bytes.truncate(input.at);
    // Drop coefficient occupancy maps before the independent pixel decode.
    drop(frame);
    input.stopped()?;
    let options = DecoderOptions::new_safe()
        .set_strict_mode(true)
        .set_use_unsafe(false)
        .set_max_width(65535)
        .set_max_height(65535)
        .jpeg_set_max_scans(MAX_SCANS);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(&input.bytes), options);
    decoder
        .decode_headers()
        .map_err(|e| invalid(&format!("pixel decoder header: {e}")))?;
    let size = decoder
        .output_buffer_size()
        .filter(|&n| n <= MAX_PIXELS)
        .ok_or_else(|| Error::Limit("JPEG decoded pixels exceed 64 MiB".into()))?;
    charge(input.budget, size)?;
    let mut pixels = vec![0; size];
    decoder
        .decode_into(&mut pixels)
        .map_err(|e| invalid(&format!("pixel decode: {e}")))?;
    input.stopped()?;
    Ok(input.bytes)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn valid_progressive_restarts_and_bounded_reading() {
        let jpeg = include_bytes!("../../../../tests/jpeg/restart-3-1.jpg");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.raw");
        let mut bytes = jpeg.to_vec();
        bytes.extend_from_slice(b"unrelated following data");
        std::fs::write(&path, bytes).unwrap();
        let mut image = Image::open(&path).unwrap();
        let end = image.length;
        let stop = AtomicBool::new(false);
        assert_eq!(
            validate(&mut image, 0, &stop, &mut (4 << 30), end).unwrap(),
            jpeg
        );
        assert!(matches!(
            validate(&mut image, 0, &stop, &mut 1, end),
            Err(Error::Limit(_))
        ));
        assert!(validate(&mut image, 0, &stop, &mut (4 << 30), jpeg.len() as u64 - 1).is_err());
        stop.store(true, Ordering::Relaxed);
        assert!(matches!(
            validate(&mut image, 0, &stop, &mut (4 << 30), end),
            Err(Error::Cancelled)
        ));
        let mut input = Input {
            image: &mut image,
            offset: 0,
            boundary: u64::MAX,
            bytes: Vec::new(),
            at: MAX_ENCODED,
            cancel: &stop,
            budget: &mut 0,
        };
        assert!(matches!(input.ensure(1), Err(Error::Limit(_))));
    }

    #[test]
    fn premature_eoi_and_padding_are_not_repaired() {
        let jpeg = include_bytes!("../../../../tests/jpeg/separate-1.jpg");
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("source.raw");
        let mut invalid_padding = jpeg.to_vec();
        let last = invalid_padding.len() - 3;
        invalid_padding[last] &= 0xfe;
        let mut missing_entropy = jpeg[..jpeg.len() - 3].to_vec();
        missing_entropy.extend_from_slice(&[0xff, 0xd9]);
        for bytes in [invalid_padding, missing_entropy] {
            std::fs::write(&path, &bytes).unwrap();
            let mut image = Image::open(&path).unwrap();
            assert!(matches!(
                validate(
                    &mut image,
                    0,
                    &AtomicBool::new(false),
                    &mut (4 << 30),
                    bytes.len() as u64
                ),
                Err(Error::Invalid(_))
            ));
        }
    }
}
