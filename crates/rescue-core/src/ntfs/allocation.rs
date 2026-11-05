//! $Bitmap is evidence about current ownership, never proof of original bytes.
use super::*;
use crate::AllocationStatus;

pub(super) struct Bitmap {
    data: Option<Data>,
    reason: Option<String>,
    page: Vec<u8>,
    page_offset: u64,
}
impl Bitmap {
    pub fn load(ntfs: &Ntfs, image: &mut Image) -> Self {
        let result = (|| {
            let record = ntfs
                .record(image, 6)?
                .ok_or_else(|| invalid("$Bitmap record absent"))?;
            if !record.in_use
                || record.directory
                || record.base != 0
                || !record.unsupported.is_empty()
            {
                return Err(invalid("$Bitmap record is not a valid active base record"));
            }
            let stream = record
                .streams
                .into_iter()
                .find(|s| s.name.is_none())
                .ok_or_else(|| invalid("$Bitmap DATA absent"))?;
            let data = stream
                .data
                .ok_or_else(|| invalid("$Bitmap DATA unsupported"))?;
            let required = ntfs
                .geometry
                .volume_length
                .div_ceil(ntfs.geometry.cluster)
                .div_ceil(8);
            if data.size() < required {
                return Err(invalid("$Bitmap is shorter than the volume allocation map"));
            }
            match &data {
                Data::Resident(_) => (),
                Data::NonResident {
                    runs, initialized, ..
                } if *initialized >= required && !runs.iter().any(|r| r.physical.is_none()) => (),
                _ => return Err(invalid("$Bitmap is compressed, sparse or uninitialized")),
            }
            Ok(data)
        })();
        match result {
            Ok(data) => Self {
                data: Some(data),
                reason: None,
                page: Vec::new(),
                page_offset: 0,
            },
            Err(e) => Self {
                data: None,
                reason: Some(format!("Allocation unknown: {e}")),
                page: Vec::new(),
                page_offset: 0,
            },
        }
    }
    pub fn unknown() -> Self {
        Self {
            data: None,
            reason: Some("$Bitmap not loaded".into()),
            page: Vec::new(),
            page_offset: 0,
        }
    }
    pub fn status(
        &mut self,
        image: &mut Image,
        geometry: &Geometry,
        data: &Data,
    ) -> (AllocationStatus, Vec<String>) {
        let (runs, needed) = match data {
            Data::Resident(_) => return (AllocationStatus::Unknown, Vec::new()),
            Data::NonResident {
                runs, initialized, ..
            } => (runs, *initialized),
            Data::Compressed {
                runs,
                initialized,
                unit,
                ..
            } => (runs, initialized.saturating_add(unit - 1) / unit * unit),
        };
        let Some(bitmap) = &self.data else {
            return (
                AllocationStatus::Unknown,
                self.reason.iter().cloned().collect(),
            );
        };
        let result = (|| {
            let mut visited = 0u64;
            for run in runs.iter().filter(|r| r.logical < needed) {
                let Some(address) = run.physical else {
                    continue;
                };
                let first = address / geometry.cluster;
                let last = first
                    + run
                        .length
                        .min(needed - run.logical)
                        .div_ceil(geometry.cluster);
                let mut byte_offset = first / 8;
                let end = last.div_ceil(8);
                while byte_offset < end {
                    let page_offset = byte_offset / 65536 * 65536;
                    if self.page.is_empty() || self.page_offset != page_offset {
                        if page_offset >= bitmap.size() {
                            return Err(invalid("cluster outside $Bitmap"));
                        }
                        let count = (bitmap.size() - page_offset).min(65536) as usize;
                        visited += count as u64;
                        if visited > 64 * 1024 * 1024 {
                            return Err(Error::Limit(
                                "$Bitmap read budget per candidate (64 MiB)".into(),
                            ));
                        }
                        self.page = bitmap.read(image, page_offset, count)?;
                        self.page_offset = page_offset;
                    }
                    let stop = end.min(self.page_offset + self.page.len() as u64);
                    for at in byte_offset..stop {
                        let low = if at == first / 8 { first % 8 } else { 0 };
                        let high = if at + 1 == end && !last.is_multiple_of(8) {
                            last % 8
                        } else {
                            8
                        };
                        let mask = (((1u16 << high) - 1) & !((1u16 << low) - 1)) as u8;
                        if self.page[(at - self.page_offset) as usize] & mask != 0 {
                            return Ok(AllocationStatus::Conflict);
                        }
                    }
                    byte_offset = stop;
                }
            }
            Ok(AllocationStatus::Unallocated)
        })();
        match result {
            Ok(AllocationStatus::Conflict) => (AllocationStatus::Conflict,vec!["$Bitmap marks payload clusters as allocated. Current bytes may belong to another file; original content is not established.".into()]),
            Ok(status) => (status,Vec::new()),
            Err(e) => (AllocationStatus::Unknown,vec![format!("Allocation unknown: {e}")]),
        }
    }
}
