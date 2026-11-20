//! Resolve only the QCOW2 and RAW files explicitly supplied by the caller.
use super::{Block, Qcow, be32, be64, invalid, read_at};
use crate::{
    Error, Result,
    disk_image::{
        ImportedFile,
        virtual_image::chain::{self, Layer},
    },
};
use std::{
    collections::HashSet,
    ffi::{OsStr, OsString},
    fs::File,
    path::Path,
    sync::atomic::AtomicBool,
};

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Raw,
    Qcow2,
}

pub(super) struct Reference {
    name: String,
    kind: Kind,
}

pub(super) fn reference(
    source: &mut File,
    length: u64,
    header: &[u8],
    cluster: u64,
    allow: bool,
) -> Result<Option<Reference>> {
    let at = be64(header, 8);
    let len = u64::from(be32(header, 16));
    if at == 0 && len == 0 {
        return Ok(None);
    }
    if !allow {
        return Err(Error::Unsupported(
            "QCOW2 requires its explicitly selected backing files".into(),
        ));
    }
    let header_size = if be32(header, 4) == 2 {
        72
    } else {
        u64::from(be32(header, 100))
    };
    let end = at
        .checked_add(len)
        .filter(|&e| e <= length.min(cluster))
        .ok_or_else(|| invalid("QCOW2 backing name exceeds first cluster"))?;
    if !(1..=1023).contains(&len) || at < header_size + 8 {
        return Err(invalid("QCOW2 backing name or header length"));
    }
    let mut bytes = vec![0; (end - header_size) as usize];
    read_at(source, header_size, &mut bytes)?;
    let (mut extensions, name) = bytes.split_at((at - header_size) as usize);
    let mut kind = None;
    loop {
        let Some((field, rest)) = extensions.split_first_chunk::<8>() else {
            return Err(invalid("missing QCOW2 extension terminator"));
        };
        let id = be32(field, 0);
        let size = be32(field, 4) as usize;
        if id == 0 {
            if size != 0 {
                return Err(invalid("invalid QCOW2 extension terminator"));
            }
            break;
        }
        let padded = size
            .checked_next_multiple_of(8)
            .ok_or_else(|| invalid("QCOW2 extension length overflow"))?;
        let (value, remaining) = rest
            .split_at_checked(padded)
            .ok_or_else(|| invalid("QCOW2 extension exceeds header"))?;
        if id == 0xe2792aca {
            if kind.is_some() {
                return Err(invalid("duplicate QCOW2 backing format"));
            }
            kind = Some(match &value[..size] {
                b"raw" => Kind::Raw,
                b"qcow2" => Kind::Qcow2,
                _ => {
                    return Err(Error::Unsupported(
                        "QCOW2 parent format requires a different reader".into(),
                    ));
                }
            });
        }
        extensions = remaining;
    }
    let full = std::str::from_utf8(name).map_err(|_| invalid("non-UTF-8 QCOW2 backing name"))?;
    let base = full.rsplit(['/', '\\']).next().unwrap_or("");
    let is_url = full.split_once(':').is_some_and(|(prefix, rest)| {
        prefix.len() != 1
            || !prefix.as_bytes()[0].is_ascii_alphabetic()
            || !rest.starts_with(['/', '\\'])
    });
    if base.is_empty() || matches!(base, "." | "..") || is_url || full.chars().any(char::is_control)
    {
        return Err(invalid("QCOW2 backing reference is not a local file name"));
    }
    Ok(Some(Reference {
        name: base.into(),
        kind: kind
            .ok_or_else(|| Error::Unsupported("QCOW2 parent has no recorded format".into()))?,
    }))
}

struct QcowLayer {
    map: Option<Qcow>,
    decoded: Vec<u8>,
    cached_guest: Option<u64>,
}

impl Layer<QcowLayer> {
    fn size(&self) -> u64 {
        self.layout
            .map
            .as_ref()
            .map_or(self.before.len(), |q| q.size)
    }

    // Overlay data ends at the first allocation boundary in any younger layer.
    fn read(
        &mut self,
        guest: u64,
        buf: &mut [u8],
        limit: &mut usize,
        encoded: &mut Vec<u8>,
    ) -> Result<bool> {
        let size = self.size();
        if guest >= size {
            buf[..*limit].fill(0);
            return Ok(true);
        }
        *limit = (*limit as u64).min(size - guest) as usize;
        let Some(map) = &mut self.layout.map else {
            read_at(&mut self.file, guest, &mut buf[..*limit])?;
            return Ok(true);
        };
        let unit = if map.entry_bytes == 16 {
            map.cluster / 32
        } else {
            map.cluster
        };
        let skip = guest % unit;
        let (block, span) = map.mapping(&mut self.file, guest - skip)?;
        if let Some(Block::Compressed { at, bytes, codec }) = block {
            let within = guest % map.cluster;
            *limit = (*limit as u64).min(map.cluster - within) as usize;
            let start = guest - within;
            if self.layout.cached_guest != Some(start) {
                encoded.resize(bytes, 0);
                read_at(&mut self.file, at, encoded)?;
                self.layout.decoded.resize(map.cluster as usize + 1, 0);
                codec.decode(encoded, &mut self.layout.decoded, map.cluster, map.cluster)?;
                self.layout.cached_guest = Some(start);
            }
            let off = within as usize;
            buf[..*limit].copy_from_slice(&self.layout.decoded[off..off + *limit]);
            return Ok(true);
        }
        *limit = (*limit as u64).min(span - skip) as usize;
        match block {
            Some(Block::Zero) => buf[..*limit].fill(0),
            Some(Block::Data(at)) => read_at(&mut self.file, at + skip, &mut buf[..*limit])?,
            None => return Ok(false),
            Some(Block::Compressed { .. }) => unreachable!(),
        }
        Ok(true)
    }
}

pub(in crate::disk_image) fn import_chain(
    paths: &[OsString],
    requested: Option<&str>,
    output: &Path,
    expected: crate::disk_image::RawExpectation,
    cancel: &AtomicBool,
) -> Result<Option<ImportedFile>> {
    if requested.is_some_and(|v| !matches!(v, "qcow" | "qcow2")) {
        return Ok(None);
    }
    let mut child = File::open(&paths[0])?;
    let mut magic = [0; 8];
    if child.metadata()?.len() < 8 {
        return Ok(None);
    }
    read_at(&mut child, 0, &mut magic)?;
    if &magic[..4] != b"QFI\xfb" || !matches!(be32(&magic, 4), 2 | 3) {
        return Ok(None);
    }
    if paths.len() > 8 {
        return Err(Error::Limit("QCOW2 chain exceeds eight files".into()));
    }
    let mut members = Vec::with_capacity(paths.len());
    let mut identities = HashSet::new();
    let mut input = Some(child);
    let mut kind = Kind::Qcow2;
    for (i, path) in paths.iter().enumerate() {
        let mut file = match input.take() {
            Some(file) => file,
            None => File::open(path)?,
        };
        if !identities.insert(same_file::Handle::from_file(file.try_clone()?)?) {
            return Err(invalid("repeated QCOW2 chain member"));
        }
        let before = file.metadata()?;
        let map = if kind == Kind::Qcow2 {
            Some(Qcow::open_member(
                &mut file,
                before.len(),
                true,
                None,
                cancel,
            )?)
        } else {
            None
        };
        match (
            map.as_ref().and_then(|m| m.backing.as_ref()),
            paths.get(i + 1),
        ) {
            (Some(reference), Some(next)) => {
                if Path::new(next).file_name() != Some(OsStr::new(&reference.name)) {
                    return Err(invalid("selected QCOW2 parent name disagrees with header"));
                }
                kind = reference.kind;
            }
            (Some(_), None) => {
                return Err(Error::Request(
                    "select every parent in the QCOW2 backing chain".into(),
                ));
            }
            (None, Some(_)) => return Err(invalid("extra parent after standalone disk")),
            (None, None) => {}
        }
        members.push(Layer {
            file,
            before,
            layout: QcowLayer {
                map,
                decoded: Vec::new(),
                cached_guest: None,
            },
        });
    }
    let size = members[0].size();
    let mut encoded = Vec::new();
    chain::flatten(
        &mut members,
        size,
        output,
        expected,
        cancel,
        |m, guest, buf, n| m.read(guest, buf, n, &mut encoded),
    )
    .map(Some)
}
