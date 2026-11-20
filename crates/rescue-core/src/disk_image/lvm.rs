//! Read linear and striped LVM2 logical volumes from explicitly supplied physical images.
mod layout;
mod pv;
mod text;
use super::{
    ImageInput, ImportedFile, RawExpectation, publish_output, split_raw, write_sparse_chunk,
};
use crate::{Error, Result};
use layout::Group;
pub use layout::VolumeInfo;
use pv::Physical;
use sha2::{Digest, Sha256};
use std::{
    io::{Read, Seek, SeekFrom},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

fn invalid(s: &str) -> Error {
    Error::Invalid(format!("LVM2: {s}"))
}
fn open(
    image: &ImageInput,
    output: Option<&Path>,
    cancel: &AtomicBool,
) -> Result<(Vec<Physical>, Group)> {
    if image.segment.len() >= 32
        || image.image_type.as_deref().is_some_and(|k| k != "lvm2")
        || image.sector_size.is_some_and(|sz| sz != 512)
    {
        return Err(Error::Request("choose up to 32 complete LVM2 physical-volume images; metadata addresses are in 512-byte sectors".into()));
    }
    let (_, paths) = image.arguments(output)?;
    let mut physical = Vec::new();
    let mut metadata = None;
    for p in paths {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let path = Path::new(&p);
        if !path.is_file() || crate::is_device(path) {
            return Err(Error::Request(
                "supply read-only regular images of LVM PVs".into(),
            ));
        }
        let pv = Physical::open(path, &mut metadata)?;
        if physical.iter().any(|old: &Physical| old.id == pv.id) {
            return Err(invalid("duplicate PV UUID"));
        }
        physical.push(pv);
    }
    let tree = text::parse(&metadata.ok_or_else(|| invalid("no active metadata copy"))?)?;
    let group = Group::parse(&tree, &physical)?;
    for p in &physical {
        p.unchanged()?;
    }
    Ok((physical, group))
}
pub fn list_lvm_volumes(image: &ImageInput, cancel: &AtomicBool) -> Result<Vec<VolumeInfo>> {
    let (_, group) = open(image, None, cancel)?;
    Ok(group.volumes.into_iter().map(|v| v.info).collect())
}
pub fn import_lvm_volume(
    image: &ImageInput,
    selector: &str,
    output: &Path,
    expected: RawExpectation<'_>,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    let (mut disks, group) = open(image, Some(output), cancel)?;
    let mut matches = group
        .volumes
        .iter()
        .filter(|v| v.info.name == selector || v.info.id == selector);
    let lv = matches
        .next()
        .ok_or_else(|| invalid("logical volume name or UUID not found"))?;
    if matches.next().is_some() {
        return Err(invalid("ambiguous logical volume name or UUID"));
    }
    let digest = expected.expected_digest(lv.info.bytes)?;
    publish_output(output, cancel, |out| {
        let mut hash = Sha256::new();
        let mut buf = vec![0u8; 1024 * 1024];
        for s in &lv.segments {
            let mut pos = 0;
            let lanes = s.lanes.len() as u64;
            while pos < s.bytes {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let stripe = pos / s.stripe_bytes;
                let within = pos % s.stripe_bytes;
                let (slot, base) = s.lanes[(stripe % lanes) as usize];
                let offset = base + (stripe / lanes) * s.stripe_bytes + within;
                let n = (s.bytes - pos)
                    .min(s.stripe_bytes - within)
                    .min(buf.len() as u64) as usize;
                let file = &mut disks[slot].file;
                file.seek(SeekFrom::Start(offset))?;
                file.read_exact(&mut buf[..n])?;
                hash.update(&buf[..n]);
                write_sparse_chunk(out, &buf[..n])?;
                pos += n as u64;
            }
        }
        out.set_len(lv.info.bytes)?;
        for p in &disks {
            p.unchanged()?;
        }
        split_raw::verify_digest(&hash, digest)?;
        Ok((lv.info.bytes, hash))
    })
}
