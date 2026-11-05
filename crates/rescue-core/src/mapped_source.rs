//! Strict reads from an acquisition. A copied block is checksum-verified before use.
use crate::{
    Error, MissingRange, Result,
    read_map::{ReadMap, ReadState},
};
use sha2::{Digest, Sha256};
use std::{
    collections::VecDeque,
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    path::Path,
    time::SystemTime,
};

const CACHE_BYTES: usize = 32 * 1024 * 1024;
pub(crate) struct MappedSource {
    map_file: File,
    map: ReadMap,
    image_stamp: (u64, SystemTime),
    map_stamp: (u64, SystemTime),
    cache: VecDeque<(u64, Vec<u8>)>,
    cached: usize,
}
fn stamp(meta: Metadata) -> Result<(u64, SystemTime)> {
    Ok((meta.len(), meta.modified()?))
}
impl MappedSource {
    pub fn open(path: &Path, image: &File) -> Result<Self> {
        let mut map_file = File::open(path)?;
        map_file
            .try_lock_shared()
            .map_err(|e| Error::Request(format!("read map is being written: {e}")))?;
        let map = ReadMap::load(&mut map_file)?;
        let image_stamp = stamp(image.metadata()?)?;
        let map_stamp = stamp(map_file.metadata()?)?;
        if image_stamp.0 > map.header.source.length
            || map
                .segments
                .values()
                .any(|s| s.state == ReadState::Copied && s.end() > image_stamp.0)
        {
            return Err(Error::Invalid(
                "read map refers beyond acquired image bytes".into(),
            ));
        }
        Ok(Self {
            map_file,
            map,
            image_stamp,
            map_stamp,
            cache: VecDeque::new(),
            cached: 0,
        })
    }
    pub fn length(&self) -> u64 {
        self.map.header.source.length
    }
    pub fn sector(&self) -> u32 {
        self.map.header.source.sector_size
    }
    pub fn digest(&self) -> String {
        self.map.digest()
    }
    pub fn missing_bytes(&self) -> u64 {
        let s = self.map.summary();
        s.failed_bytes + s.unattempted_bytes
    }
    pub fn check(&self, image: &File) -> Result<()> {
        if stamp(image.metadata()?)? != self.image_stamp
            || stamp(self.map_file.metadata()?)? != self.map_stamp
        {
            return Err(Error::SourceChanged);
        }
        Ok(())
    }
    pub fn read_available(
        &mut self,
        image: &mut File,
        offset: u64,
        length: usize,
    ) -> Result<(Vec<u8>, Vec<MissingRange>)> {
        self.check(image)?;
        let gaps = self.map.gaps(offset, length as u64)?;
        let mut output = vec![0; length];
        for segment in self.map.ranges(offset, length as u64)? {
            if segment.state != ReadState::Copied {
                continue;
            }
            if !self.cache.iter().any(|(at, _)| *at == segment.offset) {
                let mut bytes = vec![0; segment.length as usize];
                image.seek(SeekFrom::Start(segment.offset))?;
                image.read_exact(&mut bytes)?;
                if Sha256::digest(&bytes).as_slice() != segment.sha256 {
                    return Err(Error::Invalid(format!(
                        "acquired block checksum mismatch at {}",
                        segment.offset
                    )));
                }
                while self.cached + bytes.len() > CACHE_BYTES {
                    let (_, removed) = self
                        .cache
                        .pop_front()
                        .ok_or_else(|| Error::Limit("acquisition verification cache".into()))?;
                    self.cached -= removed.len();
                }
                self.cached += bytes.len();
                self.cache.push_back((segment.offset, bytes));
            }
            let bytes = &self
                .cache
                .iter()
                .find(|(at, _)| *at == segment.offset)
                .unwrap()
                .1;
            let start = segment.offset.max(offset);
            let end = segment.end().min(offset + length as u64);
            output[(start - offset) as usize..(end - offset) as usize].copy_from_slice(
                &bytes[(start - segment.offset) as usize..(end - segment.offset) as usize],
            );
        }
        self.check(image)?;
        Ok((output, gaps))
    }
    pub fn read(&mut self, image: &mut File, offset: u64, length: usize) -> Result<Vec<u8>> {
        let gaps = self.map.gaps(offset, length as u64)?;
        if !gaps.is_empty() {
            return Err(Error::MissingRanges(gaps));
        }
        self.read_available(image, offset, length)
            .map(|(bytes, _)| bytes)
    }
}
