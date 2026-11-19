//! Restore a file from an externally supplied layout and the SHA-256 of its original bytes.
use super::{ImportedFile, publish_output};
use crate::{Error, Image, Result, canonical_output_path, guard_output_path};
use serde::Deserialize;
use sha2::{Digest, Sha256};
use std::{
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::Path,
    sync::atomic::{AtomicBool, Ordering},
};

const CHUNK: usize = 256 * 1024;
const MAX_OUTPUT: u64 = 16 << 40;
const MAX_MANIFEST: u64 = 4 << 20;

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
pub(super) struct Layout {
    pub(super) schema_version: u32,
    pub(super) sha256: String,
    pub(super) extents: Vec<Extent>,
}

#[derive(Deserialize)]
#[serde(untagged, deny_unknown_fields)]
pub(super) enum Extent {
    Source { offset: u64, length: u64 },
    Hole { zero_bytes: u64 },
}

impl Layout {
    fn read(path: &Path) -> Result<Self> {
        let meta = std::fs::metadata(path)?;
        if !meta.is_file() || meta.len() > MAX_MANIFEST {
            return Err(Error::Request(
                "expected a regular layout file of at most 4 MiB".into(),
            ));
        }
        let mut bytes = Vec::new();
        File::open(path)?
            .take(MAX_MANIFEST + 1)
            .read_to_end(&mut bytes)?;
        if bytes.len() as u64 > MAX_MANIFEST {
            return Err(Error::Limit("extent manifest exceeds 4 MiB".into()));
        }
        let layout: Self = serde_json::from_slice(&bytes)?;
        if layout.schema_version != 1 || layout.extents.len() > 65536 {
            return Err(Error::Request(
                "expected layout version 1 and at most 65536 extents".into(),
            ));
        }
        if layout.sha256.len() != 64 || !layout.sha256.bytes().all(|b| b.is_ascii_hexdigit()) {
            return Err(Error::Request(
                "supply the 64 hexadecimal digits of the original SHA-256".into(),
            ));
        }
        Ok(layout)
    }

    fn size(&self, image: &Image) -> Result<u64> {
        let mut size = 0u64;
        for extent in &self.extents {
            let n = match *extent {
                Extent::Source { offset, length } => {
                    if offset
                        .checked_add(length)
                        .is_none_or(|end| end > image.length)
                    {
                        return Err(Error::Invalid(
                            "specified extent exceeds source length".into(),
                        ));
                    }
                    length
                }
                Extent::Hole { zero_bytes } => zero_bytes,
            };
            if n == 0 {
                return Err(Error::Request("extents must have nonzero lengths".into()));
            }
            size = size
                .checked_add(n)
                .filter(|n| *n <= MAX_OUTPUT)
                .ok_or_else(|| Error::Limit("specified output exceeds 16 TiB".into()))?;
        }
        Ok(size)
    }
}

pub fn recover_extents(
    source: &Path,
    manifest: &Path,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    let layout = Layout::read(manifest)?;
    let mut image = Image::open(source)?;
    let output = canonical_output_path(output)?;
    guard_image_output(&image, &output)?;
    restore_layout(&mut image, layout, &output, cancel)
}

pub(super) fn guard_image_output(image: &Image, output: &Path) -> Result<()> {
    for path in image
        .backing_paths()
        .iter()
        .chain(std::iter::once(&image.path))
    {
        guard_file_output(path, output)?;
    }
    Ok(())
}

pub(super) fn guard_file_output(path: &Path, output: &Path) -> Result<()> {
    // A new .rmap beside an existing source changes how future opens interpret it.
    // Resolve its stem to handle hard links and case-insensitive filesystems.
    let map_owner = output
        .extension()
        .filter(|ext| ext.eq_ignore_ascii_case("rmap"))
        .and_then(|_| output.file_stem())
        .map(|stem| output.with_file_name(stem));
    guard_output_path(path, output)?;
    if crate::source::map_path(path) == output
        || map_owner
            .as_ref()
            .is_some_and(|p| same_file::is_same_file(p, path).unwrap_or(false))
    {
        return Err(Error::UnsafeDestination(
            "output would replace the source read-map path".into(),
        ));
    }
    Ok(())
}

pub(super) fn restore_layout(
    image: &mut Image,
    layout: Layout,
    output: &Path,
    cancel: &AtomicBool,
) -> Result<ImportedFile> {
    let size = layout.size(image)?;
    publish_output(output, cancel, |file| {
        let mut digest = Sha256::new();
        let zeros = vec![0; CHUNK];
        for extent in layout.extents {
            let (offset, length) = match extent {
                Extent::Source { offset, length } => (Some(offset), length),
                Extent::Hole { zero_bytes } => (None, zero_bytes),
            };
            let mut done = 0;
            while done < length {
                if cancel.load(Ordering::Relaxed) {
                    return Err(Error::Cancelled);
                }
                let n = (length - done).min(CHUNK as u64) as usize;
                if let Some(offset) = offset {
                    let data = image.read(offset + done, n)?;
                    digest.update(&data);
                    file.write_all(&data)?;
                } else {
                    digest.update(&zeros[..n]);
                    file.seek(SeekFrom::Current(n as i64))?;
                }
                done += n as u64;
            }
        }
        let actual = format!("{:x}", digest.clone().finalize());
        if !layout.sha256.eq_ignore_ascii_case(&actual) {
            return Err(Error::Invalid(
                "file SHA-256 differs from the supplied original digest".into(),
            ));
        }
        file.set_len(size)?;
        Ok((size, digest))
    })
}
