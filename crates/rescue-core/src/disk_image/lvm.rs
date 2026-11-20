//! Read linear and striped LVM2 logical volumes from explicitly supplied physical images.
mod layout;
mod pv;
mod snapshot;
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
    let exceptions = lv
        .history
        .as_ref()
        .map(|history| history.load(&mut disks, lv.info.bytes, cancel))
        .transpose()?;
    publish_output(output, cancel, |out| {
        let mut hash = Sha256::new();
        let mut buffer = vec![0u8; 1024 * 1024];
        let mut pos = 0u64;
        while pos < lv.info.bytes {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let n = (lv.info.bytes - pos).min(buffer.len() as u64) as usize;
            let data = &mut buffer[..n];
            if let Some((map, history)) = exceptions.as_ref().zip(lv.history.as_ref()) {
                map.read(&mut disks, &lv.data, history, pos, data)?;
            } else {
                lv.data.read(&mut disks, pos, data)?;
            }
            hash.update(&*data);
            write_sparse_chunk(out, data)?;
            pos += n as u64;
        }
        out.set_len(lv.info.bytes)?;
        for p in &disks {
            p.unchanged()?;
        }
        split_raw::verify_digest(&hash, digest)?;
        Ok((lv.info.bytes, hash))
    })
}
