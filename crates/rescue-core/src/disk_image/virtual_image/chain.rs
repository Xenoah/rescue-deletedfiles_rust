//! Publish a RAW image from explicit youngest-to-oldest overlay layers.
use crate::{
    Error, Result,
    disk_image::{ImportedFile, RawExpectation, publish_output},
};
use sha2::{Digest, Sha256};
use std::{
    fs::{File, Metadata},
    io::{Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

pub(super) struct Layer<T> {
    pub file: File,
    pub before: Metadata,
    pub layout: T,
}

// Each layer shortens the span to its next allocation boundary. False means
// that the next older layer supplies the bytes until that boundary.
pub(super) fn flatten<T>(
    members: &mut [Layer<T>],
    size: u64,
    output: &Path,
    expected: RawExpectation,
    cancel: &AtomicBool,
    read: impl FnMut(&mut Layer<T>, u64, &mut [u8], &mut usize) -> Result<bool>,
) -> Result<ImportedFile> {
    flatten_checked(members, size, output, expected, cancel, read, None)
}

pub(super) fn flatten_checked<T>(
    members: &mut [Layer<T>],
    size: u64,
    output: &Path,
    expected: RawExpectation,
    cancel: &AtomicBool,
    mut read: impl FnMut(&mut Layer<T>, u64, &mut [u8], &mut usize) -> Result<bool>,
    descriptor: Option<(&File, &Metadata)>,
) -> Result<ImportedFile> {
    let expected_digest = expected.expected_digest(size)?;
    publish_output(output, cancel, |out| {
        let mut digest = Sha256::new();
        let mut buf = vec![0; 256 * 1024];
        let mut guest = 0;
        while guest < size {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let mut n = (size - guest).min(buf.len() as u64) as usize;
            let mut found = false;
            for member in &mut *members {
                if read(member, guest, &mut buf, &mut n)? {
                    found = true;
                    break;
                }
            }
            if !found {
                buf[..n].fill(0);
            }
            digest.update(&buf[..n]);
            if buf[..n].iter().any(|&b| b != 0) {
                out.seek(SeekFrom::Start(guest))?;
                out.write_all(&buf[..n])?;
            }
            guest += n as u64;
        }
        crate::disk_image::split_raw::verify_digest(&digest, expected_digest)?;
        out.set_len(size)?;
        for (file, before) in members
            .iter()
            .map(|m| (&m.file, &m.before))
            .chain(descriptor)
        {
            let after = file.metadata()?;
            if after.len() != before.len() || after.modified()? != before.modified()? {
                return Err(Error::SourceChanged);
            }
        }
        Ok((size, digest))
    })
}
