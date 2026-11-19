//! Resolve a flat or split VMDK descriptor exclusively against explicitly supplied files.
use super::{Descriptor, Storage, Vmdk, invalid, region};
use crate::{
    Error, Result,
    disk_image::{ImportedFile, publish_output, virtual_image::Disk},
};
use sha2::{Digest, Sha256};
use std::{
    collections::{HashMap, HashSet},
    ffi::{OsStr, OsString},
    fs::{File, Metadata},
    io::{Read, Seek, SeekFrom},
    ops::Range,
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

struct Source {
    file: File,
    before: Metadata,
    ranges: Vec<Range<u64>>,
}
enum Mapping {
    Flat(u64),
    Sparse,
    Zero,
}
struct Part {
    index: usize,
    size: u64,
    mapping: Mapping,
}

pub(in crate::disk_image) fn import_segments(
    paths: &[OsString],
    kind: Option<&str>,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if kind.is_some_and(|s| s != "vmdk") {
        return Ok(None);
    }
    let mut descriptor = File::open(&paths[0])?;
    let before = descriptor.metadata()?;
    let mut probe = [0; 21];
    if before.len() >= probe.len() as u64 {
        descriptor.read_exact(&mut probe)?;
    }
    if &probe != b"# Disk DescriptorFile" {
        return if paths.len() > 1 && kind == Some("vmdk") {
            Err(invalid(
                "expected a VMDK descriptor followed by its extent files",
            ))
        } else {
            Ok(None)
        };
    }
    if before.len() > 1 << 20 || paths.len() > 2049 {
        return Err(Error::Limit(
            "VMDK descriptor or component count exceeds limit".into(),
        ));
    }
    descriptor.seek(SeekFrom::Start(0))?;
    let mut data = vec![0; before.len() as usize];
    descriptor.read_exact(&mut data)?;
    let desc = Descriptor::parse(&data)?;
    if !matches!(
        desc.kind,
        "monolithicFlat" | "twoGbMaxExtentFlat" | "twoGbMaxExtentSparse"
    ) {
        return Err(Error::Unsupported(
            "requires a standalone flat or split VMDK descriptor".into(),
        ));
    }
    if desc.kind == "monolithicFlat" && desc.extents.len() != 1 {
        return Err(invalid("monolithicFlat requires one extent"));
    }
    let mut sources = vec![Source {
        file: descriptor,
        before,
        ranges: Vec::new(),
    }];
    let mut by_name = HashMap::new();
    let mut identities =
        HashSet::from([same_file::Handle::from_file(sources[0].file.try_clone()?)?]);
    for path in &paths[1..] {
        let basename = Path::new(path)
            .file_name()
            .ok_or_else(|| invalid("VMDK extent file name"))?;
        if by_name.insert(basename, sources.len()).is_some() {
            return Err(invalid("duplicate supplied VMDK extent name"));
        }
        let file = File::open(path)?;
        if !identities.insert(same_file::Handle::from_file(file.try_clone()?)?) {
            return Err(invalid("duplicate physical VMDK component"));
        }
        sources.push(Source {
            before: file.metadata()?,
            file,
            ranges: Vec::new(),
        });
    }
    let index = |name: &str| {
        by_name
            .get(OsStr::new(name))
            .copied()
            .ok_or_else(|| invalid("missing explicitly supplied VMDK extent file"))
    };
    let mut parts = Vec::new();
    let mut length = 0u64;
    for e in desc.extents {
        length = length
            .checked_add(e.size)
            .filter(|&s| s <= 64u64 << 40)
            .ok_or_else(|| invalid("VMDK total size limit"))?;
        if desc.kind.starts_with("twoGb") && e.size > 2u64 << 30 {
            return Err(invalid("split VMDK extent exceeds 2 GiB"));
        }
        let (i, mapping) = match e.storage {
            Storage::Flat(name, at) if desc.kind != "twoGbMaxExtentSparse" => {
                let i = index(name)?;
                let r = region(at, e.size, sources[i].before.len())?;
                sources[i].ranges.push(r);
                (i, Mapping::Flat(at))
            }
            Storage::Sparse(name) if desc.kind == "twoGbMaxExtentSparse" => {
                let i = index(name)?;
                let size = sources[i].before.len();
                sources[i].ranges.push(0..size);
                (i, Mapping::Sparse)
            }
            Storage::Zero if desc.kind != "monolithicFlat" => (0, Mapping::Zero),
            _ => return Err(invalid("VMDK extent type disagrees with descriptor")),
        };
        parts.push(Part {
            index: i,
            size: e.size,
            mapping,
        });
    }
    for source in &mut sources[1..] {
        if source.ranges.is_empty() {
            return Err(invalid("unreferenced VMDK extent file supplied"));
        }
        source.ranges.sort_unstable_by_key(|r| r.start);
        if source.ranges.windows(2).any(|r| r[0].end > r[1].start) {
            return Err(invalid("overlapping VMDK component ranges"));
        }
    }
    let result = publish_output(output, cancel, |out| {
        let mut offset = 0u64;
        let mut digest = Sha256::new();
        for part in parts {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let src = &mut sources[part.index];
            let mut disk = match part.mapping {
                Mapping::Flat(at) => Disk::Flat {
                    size: part.size,
                    at: Some(at),
                },
                Mapping::Zero => Disk::Flat {
                    size: part.size,
                    at: None,
                },
                Mapping::Sparse => {
                    let disk =
                        Vmdk::open(&mut src.file, src.before.len(), Some(part.size), cancel)?;
                    if !offset.is_multiple_of(disk.grain_size) {
                        return Err(invalid(
                            "split VMDK sparse extent starts between guest grain boundaries",
                        ));
                    }
                    Disk::Vmdk(disk)
                }
            };
            disk.write_at(&mut src.file, out, offset, &mut digest, cancel)?;
            offset += part.size;
        }
        for src in &sources {
            let after = src.file.metadata()?;
            if after.len() != src.before.len() || after.modified()? != src.before.modified()? {
                return Err(Error::SourceChanged);
            }
        }
        out.set_len(length)?;
        Ok((length, digest))
    })?;
    Ok(Some(result))
}
