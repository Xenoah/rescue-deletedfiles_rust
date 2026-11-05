//! Append-only read map. A readable range is recorded only after image data is durable.
//! Missing bytes are never inferred to be zero; CRC-invalid tails remain unattempted.
use crate::{Error, Result};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeMap,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::PathBuf,
};

const MAGIC: &[u8; 8] = b"RSCMAP01";
const RECORD_BYTES: usize = 76;
const MAX_HEADER: usize = 64 * 1024;
const MAX_JOURNAL: u64 = 512 * 1024 * 1024;
const MAX_SEGMENTS: usize = 1_048_576;
pub(crate) const IMAGE_NAME: &str = "image.rescue.raw";
pub(crate) const MAP_NAME: &str = "image.rescue.raw.rmap";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct AcquisitionIdentity {
    pub path: PathBuf,
    pub length: u64,
    pub sector_size: u32,
    pub identity: String,
    /// Regular-file modification time. Native-device identity uses the OS geometry/identity.
    pub modified_ns: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub(crate) struct Header {
    pub schema_version: u32,
    pub id: String,
    pub source: AcquisitionIdentity,
    pub block_size: u32,
    pub image_name: String,
    pub created_unix_ms: u64,
}
impl Header {
    fn validate(&self) -> Result<()> {
        uuid::Uuid::parse_str(&self.id).map_err(|_| invalid("image identifier"))?;
        let sector = self.source.sector_size;
        if self.schema_version != 1
            || self.created_unix_ms == 0
            || self.image_name != IMAGE_NAME
            || !(512..=65536).contains(&sector)
            || !sector.is_power_of_two()
            || !(sector..=16 * 1024 * 1024).contains(&self.block_size)
            || !self.block_size.is_multiple_of(sector)
        {
            return Err(invalid("header geometry or version"));
        }
        Ok(())
    }
}
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReadState {
    Unattempted,
    Copied,
    Failed,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct MissingRange {
    /// Absolute byte offset in the acquired source, including any partition offset.
    pub source_offset: u64,
    pub length: u64,
    /// Populated when a filesystem stream can map this gap to file bytes.
    pub logical_offset: Option<u64>,
    pub logical_length: Option<u64>,
    pub reason: ReadState,
}
#[derive(Clone, Debug, PartialEq, Eq)]
pub(crate) struct Segment {
    pub offset: u64,
    pub length: u64,
    pub state: ReadState,
    pub attempts: u8,
    pub sha256: [u8; 32],
    pub error_code: i32,
    pub read_unix_ms: u64,
}
impl Segment {
    pub fn end(&self) -> u64 {
        self.offset + self.length
    }
    fn pending(length: u64) -> Self {
        Self {
            offset: 0,
            length,
            state: ReadState::Unattempted,
            attempts: 0,
            sha256: [0; 32],
            error_code: 0,
            read_unix_ms: 0,
        }
    }
    fn record(&self, sequence: u64) -> [u8; RECORD_BYTES] {
        let mut bytes = [0; RECORD_BYTES];
        bytes[..8].copy_from_slice(&self.offset.to_le_bytes());
        bytes[8..16].copy_from_slice(&self.length.to_le_bytes());
        bytes[16] = match self.state {
            ReadState::Unattempted => 0,
            ReadState::Copied => 1,
            ReadState::Failed => 2,
        };
        bytes[17] = self.attempts;
        bytes[20..24].copy_from_slice(&self.error_code.to_le_bytes());
        bytes[24..56].copy_from_slice(&self.sha256);
        bytes[56..64].copy_from_slice(&sequence.to_le_bytes());
        bytes[64..72].copy_from_slice(&self.read_unix_ms.to_le_bytes());
        let checksum = crc32fast::hash(&bytes[..72]);
        bytes[72..].copy_from_slice(&checksum.to_le_bytes());
        bytes
    }
    fn parse(bytes: &[u8; RECORD_BYTES], sequence: u64) -> Result<Self> {
        if u64::from_le_bytes(bytes[56..64].try_into().unwrap()) != sequence
            || bytes[18..20] != [0; 2]
        {
            return Err(invalid("record sequence or reserved fields"));
        }
        let state = match bytes[16] {
            1 => ReadState::Copied,
            2 => ReadState::Failed,
            _ => return Err(invalid("record state")),
        };
        let error_code = i32::from_le_bytes(bytes[20..24].try_into().unwrap());
        let sha256 = bytes[24..56].try_into().unwrap();
        if bytes[17] == 0
            || bytes[17] > 4
            || (state == ReadState::Copied && error_code != 0)
            || (state == ReadState::Failed && sha256 != [0; 32])
        {
            return Err(invalid("record attempts/checksum fields"));
        }
        Ok(Self {
            offset: u64::from_le_bytes(bytes[..8].try_into().unwrap()),
            length: u64::from_le_bytes(bytes[8..16].try_into().unwrap()),
            state,
            attempts: bytes[17],
            sha256,
            error_code,
            read_unix_ms: u64::from_le_bytes(bytes[64..72].try_into().unwrap()),
        })
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct ReadMapSummary {
    pub schema_version: u32,
    pub image_id: String,
    pub length: u64,
    pub copied_bytes: u64,
    pub failed_bytes: u64,
    pub unattempted_bytes: u64,
    pub records: u64,
    pub ignored_incomplete_tail: bool,
    pub created_unix_ms: u64,
}
pub(crate) struct ReadMap {
    pub header: Header,
    pub segments: BTreeMap<u64, Segment>,
    pub records: u64,
    pub durable_length: u64,
    pub ignored_tail: bool,
    digest: Sha256,
    counts: [u64; 3],
}
fn invalid(s: &str) -> Error {
    Error::Invalid(format!("read map: {s}"))
}
impl ReadMap {
    fn empty(header: Header, durable_length: u64, digest: Sha256) -> Self {
        let mut segments = BTreeMap::new();
        if header.source.length != 0 {
            segments.insert(0, Segment::pending(header.source.length));
        }
        let counts = [header.source.length, 0, 0];
        Self {
            header,
            segments,
            records: 0,
            durable_length,
            ignored_tail: false,
            digest,
            counts,
        }
    }
    pub fn create(file: &mut File, header: Header) -> Result<Self> {
        header.validate()?;
        let data = serde_json::to_vec(&header)?;
        if data.len() > MAX_HEADER {
            return Err(Error::Limit("read-map header".into()));
        }
        let mut bytes = Vec::with_capacity(12 + data.len() + 32);
        bytes.extend_from_slice(MAGIC);
        bytes.extend_from_slice(&(data.len() as u32).to_le_bytes());
        bytes.extend(data);
        let hash = Sha256::digest(&bytes);
        bytes.extend(hash);
        file.write_all(&bytes)?;
        file.sync_all()?;
        let mut digest = Sha256::new();
        digest.update(&bytes);
        Ok(Self::empty(header, bytes.len() as u64, digest))
    }
    pub fn load(file: &mut File) -> Result<Self> {
        let length = file.metadata()?.len();
        if length > MAX_JOURNAL {
            return Err(Error::Limit("read-map journal exceeds 512 MiB".into()));
        }
        file.seek(SeekFrom::Start(0))?;
        let mut prefix = [0u8; 12];
        file.read_exact(&mut prefix)?;
        if &prefix[..8] != MAGIC {
            return Err(invalid("signature"));
        }
        let count = u32::from_le_bytes(prefix[8..12].try_into().unwrap()) as usize;
        if count > MAX_HEADER {
            return Err(Error::Limit("read-map header".into()));
        }
        let mut data = vec![0; count];
        file.read_exact(&mut data)?;
        let mut expected = [0; 32];
        file.read_exact(&mut expected)?;
        let mut digest = Sha256::new();
        digest.update(prefix);
        digest.update(&data);
        if digest.clone().finalize().as_slice() != expected {
            return Err(invalid("header checksum"));
        }
        digest.update(expected);
        let header: Header = serde_json::from_slice(&data)?;
        header.validate()?;
        let mut map = Self::empty(header, 12 + count as u64 + 32, digest);
        while map.durable_length < length {
            if length - map.durable_length < RECORD_BYTES as u64 {
                map.ignored_tail = true;
                break;
            }
            let mut bytes = [0; RECORD_BYTES];
            file.read_exact(&mut bytes)?;
            let crc = u32::from_le_bytes(bytes[72..].try_into().unwrap());
            if crc32fast::hash(&bytes[..72]) != crc {
                if map.durable_length + RECORD_BYTES as u64 == length {
                    map.ignored_tail = true;
                    break;
                }
                return Err(invalid("corruption before the journal tail"));
            }
            let segment = Segment::parse(&bytes, map.records + 1)?;
            map.apply(&segment)?;
            map.records += 1;
            map.durable_length += RECORD_BYTES as u64;
            map.digest.update(bytes);
        }
        Ok(map)
    }
    pub fn summary(&self) -> ReadMapSummary {
        let [pending, copied, failed] = self.counts;
        ReadMapSummary {
            schema_version: 1,
            image_id: self.header.id.clone(),
            length: self.header.source.length,
            copied_bytes: copied,
            failed_bytes: failed,
            unattempted_bytes: pending,
            records: self.records,
            ignored_incomplete_tail: self.ignored_tail,
            created_unix_ms: self.header.created_unix_ms,
        }
    }
    pub fn digest(&self) -> String {
        format!("{:x}", self.digest.clone().finalize())
    }
    fn check(&self, segment: &Segment) -> Result<Vec<Segment>> {
        let end = segment
            .offset
            .checked_add(segment.length)
            .ok_or_else(|| invalid("range overflow"))?;
        let sector = u64::from(self.header.source.sector_size);
        if segment.length == 0
            || end > self.header.source.length
            || !segment.offset.is_multiple_of(sector)
            || (!segment.length.is_multiple_of(sector) && end != self.header.source.length)
            || segment.state == ReadState::Unattempted
            || segment.length > u64::from(self.header.block_size)
            || segment.attempts == 0
            || segment.attempts > 4
            || segment.read_unix_ms == 0
            || (segment.state == ReadState::Copied && segment.error_code != 0)
            || (segment.state == ReadState::Failed && segment.sha256 != [0; 32])
        {
            return Err(invalid("range bounds or alignment"));
        }
        let start = *self
            .segments
            .range(..=segment.offset)
            .next_back()
            .ok_or_else(|| invalid("map coverage"))?
            .0;
        let affected = self
            .segments
            .range(start..end)
            .map(|(_, s)| s.clone())
            .collect::<Vec<_>>();
        if affected.iter().any(|s| s.state == ReadState::Copied) {
            return Err(invalid("overwriting already copied ranges is forbidden"));
        }
        if affected.iter().any(|s| segment.attempts != s.attempts + 1) {
            return Err(invalid("retry counter must advance exactly once"));
        }
        if self
            .segments
            .len()
            .saturating_sub(affected.len())
            .saturating_add(3)
            > MAX_SEGMENTS
        {
            return Err(Error::Limit("read-map interval index".into()));
        }
        Ok(affected)
    }
    pub fn ranges(&self, offset: u64, length: u64) -> Result<Vec<Segment>> {
        let end = offset
            .checked_add(length)
            .filter(|e| *e <= self.header.source.length)
            .ok_or_else(|| invalid("read range"))?;
        if length == 0 {
            return Ok(Vec::new());
        }
        let start = *self
            .segments
            .range(..=offset)
            .next_back()
            .ok_or_else(|| invalid("map coverage"))?
            .0;
        Ok(self
            .segments
            .range(start..end)
            .map(|(_, s)| s.clone())
            .collect())
    }
    pub fn gaps(&self, offset: u64, length: u64) -> Result<Vec<MissingRange>> {
        let end = offset
            .checked_add(length)
            .ok_or_else(|| invalid("read overflow"))?;
        Ok(self
            .ranges(offset, length)?
            .into_iter()
            .filter(|s| s.state != ReadState::Copied)
            .map(|s| {
                let start = s.offset.max(offset);
                MissingRange {
                    source_offset: start,
                    length: s.end().min(end) - start,
                    logical_offset: None,
                    logical_length: None,
                    reason: s.state,
                }
            })
            .collect())
    }
    /// Return the next bounded attempt without materializing a second interval index.
    pub fn next_attempt(&self, at: u64, pass: u8) -> Option<Segment> {
        let start = self
            .segments
            .range(..=at)
            .next_back()
            .map(|(k, _)| *k)
            .unwrap_or(at);
        for (_, s) in self.segments.range(start..) {
            if s.end() <= at || s.state == ReadState::Copied || s.attempts != pass {
                continue;
            }
            let offset = s.offset.max(at);
            let block = if pass == 0 {
                u64::from(self.header.block_size)
            } else {
                u64::from(self.header.source.sector_size)
            };
            return Some(Segment {
                offset,
                length: (s.end() - offset).min(block),
                state: ReadState::Failed,
                attempts: pass + 1,
                sha256: [0; 32],
                error_code: 0,
                read_unix_ms: 0,
            });
        }
        None
    }
    pub fn apply(&mut self, segment: &Segment) -> Result<()> {
        let affected = self.check(segment)?;
        let end = segment.end();
        for old in affected {
            let replaced = old.end().min(end) - old.offset.max(segment.offset);
            self.counts[old.state as usize] -= replaced;
            self.segments.remove(&old.offset);
            if old.offset < segment.offset {
                let mut left = old.clone();
                left.length = segment.offset - old.offset;
                self.segments.insert(left.offset, left);
            }
            if old.end() > end {
                let mut right = old.clone();
                right.offset = end;
                right.length = old.end() - end;
                self.segments.insert(right.offset, right);
            }
        }
        self.counts[segment.state as usize] += segment.length;
        self.segments.insert(segment.offset, segment.clone());
        Ok(())
    }
    /// Caller must sync image bytes before passing any Copied records here.
    pub fn append(&mut self, file: &mut File, segment: Segment) -> Result<()> {
        if self.durable_length + RECORD_BYTES as u64 > MAX_JOURNAL {
            return Err(Error::Limit("read-map journal exceeds 512 MiB".into()));
        }
        self.check(&segment)?;
        let bytes = segment.record(self.records + 1);
        // A previously incomplete tail is discarded only under the writer's exclusive lock.
        file.set_len(self.durable_length)?;
        file.seek(SeekFrom::Start(self.durable_length))?;
        file.write_all(&bytes)?;
        file.sync_all()?;
        self.apply(&segment)?;
        self.records += 1;
        self.durable_length += RECORD_BYTES as u64;
        self.digest.update(bytes);
        self.ignored_tail = false;
        Ok(())
    }
}
