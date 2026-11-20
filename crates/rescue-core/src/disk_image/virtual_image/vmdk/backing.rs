//! Bind a sparse child to explicitly selected parent files before reading guest sectors.
use super::{Block, Grain, Header, Lineage, Vmdk, invalid, read_at, validate_descriptor};
use crate::{
    Error, Result,
    disk_image::{
        ImportedFile, RawExpectation,
        virtual_image::chain::{self, Layer},
    },
};
use std::{collections::HashSet, ffi::OsString, fs::File, path::Path, sync::atomic::AtomicBool};

pub(super) fn import(
    paths: &[OsString],
    first: File,
    output: &Path,
    expected: RawExpectation,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    if paths.len() > 8 {
        return Err(Error::Limit(
            "VMDK snapshot chain exceeds eight files".into(),
        ));
    }
    let mut first = Some(first);
    let mut members: Vec<Layer<Vmdk>> = Vec::new();
    let mut handles = HashSet::new();
    let mut cids = HashSet::new();
    let mut uuids = HashSet::new();
    let mut child: Option<Lineage> = None;
    let mut table_budget = 128u64 << 20;
    for path in paths {
        let mut file = match first.take() {
            Some(file) => file,
            None => File::open(path)?,
        };
        if !handles.insert(same_file::Handle::from_file(file.try_clone()?)?) {
            return Err(invalid("repeated VMDK snapshot file"));
        }
        let before = file.metadata()?;
        let header = Header::read(&mut file, before.len())?;
        let identity = validate_descriptor(&mut file, &header, None, true)?
            .ok_or_else(|| invalid("VMDK snapshot member requires an embedded descriptor"))?;
        if header.compressed && identity.requires_parent() {
            return Err(Error::Unsupported(
                "streamOptimized VMDK is supported as a standalone base for snapshots".into(),
            ));
        }
        if !cids.insert(identity.cid) || identity.image.is_some_and(|id| !uuids.insert(id)) {
            return Err(invalid("repeated VMDK snapshot content ID or image UUID"));
        }
        if let Some(next) = &child
            && (!next.accepts(&identity, Path::new(path))
                || members
                    .last()
                    .is_none_or(|m| m.layout.size != header.capacity))
        {
            return Err(invalid(
                "selected VMDK parent filename, CID, UUID, modification or logical size disagrees with the child",
            ));
        }
        let table_bytes = header.capacity.div_ceil(header.grain_size) * size_of::<Grain>() as u64;
        table_budget = table_budget
            .checked_sub(table_bytes)
            .ok_or_else(|| Error::Limit("VMDK snapshot grain maps exceed 128 MiB".into()))?;
        let layout = Vmdk::read_maps(&mut file, before.len(), &header, cancel)?;
        members.push(Layer {
            file,
            before,
            layout,
        });
        child = Some(identity);
    }
    if child.is_some_and(|last| last.requires_parent()) {
        return Err(Error::Request(
            "select every VMDK parent through its standalone base".into(),
        ));
    }
    let size = members[0].layout.size;
    // At most one compressed member can occur, the standalone base. Different
    // child grain boundaries can split a compressed base grain across reads.
    let mut cached_at = None;
    let mut decoded = Vec::new();
    let mut encoded = Vec::new();
    chain::flatten(
        &mut members,
        size,
        output,
        expected,
        cancel,
        |member, offset, buf, limit| {
            let disk = &member.layout;
            let (block, span) = disk.overlay_block(offset);
            *limit = span.min(*limit as u64) as usize;
            match block {
                None => return Ok(false),
                Some(Block::Zero) => buf[..*limit].fill(0),
                Some(Block::Data(at)) => read_at(&mut member.file, at, &mut buf[..*limit])?,
                Some(Block::Compressed { at, bytes, codec }) => {
                    let within = offset % disk.grain_size;
                    if cached_at != Some(at) {
                        let count = disk.grain_size.min(disk.size - (offset - within));
                        encoded.resize(bytes, 0);
                        read_at(&mut member.file, at, &mut encoded)?;
                        decoded.resize(disk.grain_size as usize + 1, 0);
                        codec.decode(&encoded, &mut decoded, disk.grain_size, count)?;
                        cached_at = Some(at);
                    }
                    let start = within as usize;
                    buf[..*limit].copy_from_slice(&decoded[start..start + *limit]);
                }
            }
            Ok(true)
        },
    )
}
