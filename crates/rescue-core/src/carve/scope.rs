//! Allocation-aware candidate search, with explicit fallback for unknown maps.
//! Read buffers can span allocated bytes; only signatures and complete payloads
//! allowed by the chosen scope are retained. No inference of deletion is made.
use crate::{
    AllocationStatus, CarveScope, Error, FileSystem, Result, Session, SourceRegion, fat,
    ntfs::{Data, Ntfs, Run},
    source::Image,
};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::atomic::{AtomicBool, Ordering},
};

enum Backend {
    Ntfs(Box<Ntfs>),
    Fat(Box<fat::AllocationReader>),
}
pub(super) struct Scope {
    mode: CarveScope,
    path: PathBuf,
    regions: Vec<SourceRegion>,
    current: Option<(usize, Option<(Image, Backend)>)>,
    warnings: BTreeSet<String>,
    length: u64,
}
impl Scope {
    pub fn new(image: &mut Image, mode: CarveScope) -> Result<Self> {
        let mut result = Self {
            mode,
            path: image.path.clone(),
            regions: Vec::new(),
            current: None,
            warnings: BTreeSet::new(),
            length: image.length,
        };
        if mode == CarveScope::All {
            return Ok(result);
        }
        match crate::partitions::inspect_image(image) {
            Ok(layout) => {
                result.regions = layout.regions;
                result.regions.sort_by_key(|r| r.offset);
                result.warnings.extend(layout.warnings);
            }
            Err(Error::Cancelled) => return Err(Error::Cancelled),
            Err(error) => result.warning(format!(
                "source layout unavailable ({error}); all selected source bytes are searched"
            )),
        }
        // Explicit gap regions prevent a warning per signature and make every
        // cross-partition payload check cover both known and unknown allocations.
        let mut complete = Vec::new();
        let mut at = 0;
        for region in &result.regions {
            if region.offset > at {
                complete.push(unknown(at, region.offset - at));
            }
            complete.push(region.clone());
            at = region.offset + region.length;
        }
        if at < image.length {
            complete.push(unknown(at, image.length - at));
        }
        result.regions = complete;
        Ok(result)
    }
    fn warning(&mut self, message: String) {
        if self.warnings.len() < 512 {
            self.warnings
                .insert(format!("ALLOCATION_FALLBACK: {message}"));
        } else {
            self.warnings.insert("ALLOCATION_FALLBACK: additional unreadable allocation ranges are searched without an allocation filter; detail limit reached".into());
        }
    }
    pub fn append_warnings(&self, session: &mut Session) {
        for warning in &self.warnings {
            if !session.warnings.contains(warning) {
                session.warnings.push(warning.clone());
            }
        }
    }
    pub fn status(
        &mut self,
        offset: u64,
        length: u64,
        cancel: &AtomicBool,
    ) -> Result<AllocationStatus> {
        let end = offset
            .checked_add(length)
            .filter(|v| *v <= self.length)
            .ok_or_else(|| Error::Invalid("carving allocation range".into()))?;
        if self.mode == CarveScope::All || length == 0 {
            return Ok(AllocationStatus::Unknown);
        }
        let mut at = offset;
        let mut uncertain = false;
        while at < end {
            if cancel.load(Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let index = self.regions.partition_point(|r| r.offset + r.length <= at);
            let region = self.regions.get(index).ok_or(Error::SourceChanged)?.clone();
            let stop = end.min(region.offset + region.length);
            if self
                .current
                .as_ref()
                .is_none_or(|(loaded, _)| *loaded != index)
            {
                let load = (|| -> Result<(Image, Backend)> {
                    let mut image = Image::open_region(&self.path, Some(&region))?;
                    let backend = match region.filesystem {
                        Some(FileSystem::Ntfs) => Backend::Ntfs(Box::new(Ntfs::open(&mut image)?)),
                        Some(
                            FileSystem::Fat12
                            | FileSystem::Fat16
                            | FileSystem::Fat32
                            | FileSystem::Exfat,
                        ) => {
                            Backend::Fat(Box::new(fat::AllocationReader::open(&mut image, cancel)?))
                        }
                        _ => {
                            return Err(Error::Unsupported(
                                "no recognized allocation metadata".into(),
                            ));
                        }
                    };
                    Ok((image, backend))
                })();
                self.current = Some((
                    index,
                    match load {
                        Ok(backend) => Some(backend),
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        Err(error) => {
                            self.warning(format!(
                                "bytes {}..{}: {error}; this region is searched in full",
                                region.offset,
                                region.offset + region.length
                            ));
                            None
                        }
                    },
                ));
            }
            let status = match self
                .current
                .as_mut()
                .and_then(|(_, backend)| backend.as_mut())
            {
                Some((image, Backend::Ntfs(ntfs))) => {
                    let first =
                        (at - region.offset) / ntfs.geometry.cluster * ntfs.geometry.cluster;
                    let end = (stop - region.offset)
                        .div_ceil(ntfs.geometry.cluster)
                        .checked_mul(ntfs.geometry.cluster)
                        .ok_or(Error::SourceChanged)?;
                    let size = end - first;
                    let data = Data::NonResident {
                        runs: vec![Run {
                            logical: 0,
                            physical: Some(first),
                            length: size,
                        }],
                        size,
                        initialized: size,
                    };
                    let (status, reasons) = ntfs.allocation(image, &data);
                    if status == AllocationStatus::Unknown {
                        self.warning(format!("region {}: {}; signatures in unreadable allocation ranges are included",region.offset,reasons.join("; ")));
                    }
                    status
                }
                Some((image, Backend::Fat(fat))) => {
                    match fat.status(image, at - region.offset, stop - at, cancel) {
                        Ok(status) => status,
                        Err(Error::Cancelled) => return Err(Error::Cancelled),
                        Err(error) => {
                            self.warning(format!("region {}: {error}; signatures in unreadable allocation ranges are included",region.offset));
                            AllocationStatus::Unknown
                        }
                    }
                }
                None => AllocationStatus::Unknown,
            };
            if status == AllocationStatus::Conflict {
                return Ok(status);
            }
            uncertain |= status != AllocationStatus::Unallocated;
            at = stop;
        }
        Ok(if uncertain {
            AllocationStatus::Unknown
        } else {
            AllocationStatus::Unallocated
        })
    }
}
fn unknown(offset: u64, length: u64) -> SourceRegion {
    SourceRegion {
        index: 0,
        name: "allocation unknown".into(),
        offset,
        length,
        filesystem: None,
        recovery_boot: None,
    }
}

pub(crate) fn validate_selection(
    image: &mut Image,
    session: &Session,
    candidate: &crate::Candidate,
    cancel: &AtomicBool,
) -> Result<AllocationStatus> {
    if session
        .search
        .as_ref()
        .is_none_or(|s| s.carve_scope == CarveScope::All)
    {
        return Ok(AllocationStatus::Unknown);
    }
    let size = candidate
        .size
        .filter(|n| *n > 0 && *n <= 64 * 1024 * 1024)
        .ok_or(Error::SourceChanged)?;
    let mut scope = Scope::new(image, CarveScope::Unallocated)?;
    let status = scope.status(candidate.record, size, cancel)?;
    if status == AllocationStatus::Conflict
        || (candidate.allocation_status == AllocationStatus::Unallocated
            && status != AllocationStatus::Unallocated)
    {
        return Err(Error::SourceChanged);
    }
    Ok(status)
}
