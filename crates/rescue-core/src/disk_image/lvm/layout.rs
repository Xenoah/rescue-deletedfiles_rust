//! Resolve LVM physical extents using authenticated VG metadata and supplied PV UUIDs.
use super::{
    Error, Result, invalid,
    pv::Physical,
    snapshot::{History, SnapshotInfo},
    text::{Fields, Section, Value},
};
use serde::Serialize;
use std::{
    collections::{BTreeMap, BTreeSet},
    io::{Read, Seek, SeekFrom},
    sync::Arc,
};

#[derive(Clone, Serialize)]
pub struct VolumeInfo {
    pub group: String,
    pub group_id: String,
    pub name: String,
    pub id: String,
    pub sequence: u64,
    pub bytes: u64,
    pub segments: usize,
    pub max_stripes: usize,
    #[serde(skip_serializing_if = "Option::is_none")]
    pub snapshot: Option<SnapshotInfo>,
}
pub(super) struct Group {
    pub volumes: Vec<Volume>,
}
pub(super) struct Volume {
    pub info: VolumeInfo,
    pub data: Data,
    pub history: Option<History>,
}
#[derive(Clone)]
pub(super) struct Data {
    pub bytes: u64,
    segments: Arc<[Segment]>,
}
impl Data {
    pub fn read(&self, disks: &mut [Physical], mut pos: u64, mut output: &mut [u8]) -> Result<()> {
        if pos > self.bytes || output.len() as u64 > self.bytes - pos {
            return Err(invalid("logical read exceeds volume"));
        }
        while !output.is_empty() {
            let i = self.segments.partition_point(|s| s.start <= pos);
            let s = &self.segments[i - 1];
            let local = pos - s.start;
            let stripe = local / s.stripe_bytes;
            let within = local % s.stripe_bytes;
            let lanes = s.lanes.len() as u64;
            let (slot, base) = s.lanes[(stripe % lanes) as usize];
            let offset = base + (stripe / lanes) * s.stripe_bytes + within;
            let count = (s.bytes - local)
                .min(s.stripe_bytes - within)
                .min(output.len() as u64) as usize;
            let (part, rest) = output.split_at_mut(count);
            disks[slot].file.seek(SeekFrom::Start(offset))?;
            disks[slot].file.read_exact(part)?;
            pos += count as u64;
            output = rest;
        }
        Ok(())
    }
}
pub(super) struct Segment {
    start: u64,
    pub bytes: u64,
    pub stripe_bytes: u64,
    pub lanes: Vec<(usize, u64)>,
}
#[derive(Clone, Copy)]
struct Device {
    slot: usize,
    base: u64,
    extents: u64,
}

fn product(a: u64, b: u64) -> Result<u64> {
    a.checked_mul(b).ok_or_else(|| invalid("size overflow"))
}
fn uuid(s: &str) -> Result<String> {
    let parts: Vec<_> = s.split('-').collect();
    if parts.iter().map(|p| p.len()).ne([6, 4, 4, 4, 4, 4, 6])
        || !parts
            .iter()
            .all(|p| p.bytes().all(|b| b.is_ascii_alphanumeric()))
    {
        return Err(invalid("invalid metadata UUID"));
    }
    Ok(parts.concat())
}
fn strings(value: &[Value]) -> Result<Vec<&str>> {
    value
        .iter()
        .map(|v| {
            if let Value::Text(s) = v {
                Ok(s.as_str())
            } else {
                Err(invalid("invalid flags or status"))
            }
        })
        .collect()
}
fn supported(section: &Section, required: &str, allowed: &[&str]) -> Result<()> {
    let status = strings(section.list("status")?)?;
    if !status.contains(&required)
        || status.iter().any(|item| !allowed.contains(item))
        || !section.list("flags")?.is_empty()
    {
        return Err(Error::Unsupported("unsupported LVM status or flags".into()));
    }
    Ok(())
}

fn devices(group: &Section, extent: u64, pvs: &[Physical]) -> Result<BTreeMap<String, Device>> {
    let mut used = BTreeSet::new();
    let mut out = BTreeMap::new();
    let entries = group.section("physical_volumes")?;
    if entries.len() != pvs.len() {
        return Err(invalid(
            "supply every physical volume in this group exactly once",
        ));
    }
    for key in entries.keys() {
        let p = entries.section(key)?;
        // An existing read-only or non-allocatable PV can still hold allocated extents.
        let status = strings(p.list("status")?)?;
        if !p.list("flags")?.is_empty()
            || status
                .iter()
                .any(|s| !matches!(*s, "ALLOCATABLE" | "EXPORTED"))
        {
            return Err(Error::Unsupported(
                "unavailable or unsupported LVM physical volume".into(),
            ));
        }
        let id = uuid(p.text("id")?)?;
        let slot = pvs
            .iter()
            .position(|v| v.id == id)
            .ok_or_else(|| invalid("required physical UUID missing"))?;
        let disk = &pvs[slot];
        let base = product(p.num("pe_start")?, 512)?;
        let extents = p.num("pe_count")?;
        let bytes = product(extents, extent)?;
        let end = base
            .checked_add(bytes)
            .ok_or_else(|| invalid("PV extent overflow"))?;
        if !used.insert(slot)
            || product(p.num("dev_size")?, 512)? != disk.size
            || base != disk.data.0
            || extents == 0
            || end > disk.size
            || (disk.data.1 != 0 && bytes > disk.data.1)
            || disk
                .areas
                .iter()
                .any(|&(start, len)| base < start + len && start < end)
        {
            return Err(invalid(
                "physical extents disagree with PV labels or overlap metadata",
            ));
        }
        out.insert(
            key.clone(),
            Device {
                slot,
                base,
                extents,
            },
        );
    }
    Ok(out)
}

fn segment(
    s: &Section,
    extent: u64,
    devices: &BTreeMap<String, Device>,
    claims: &mut [Vec<(u64, u64)>],
) -> Result<Segment> {
    if s.text("type")? != "striped"
        || s.keys().any(|k| {
            !matches!(
                k.as_str(),
                "start_extent"
                    | "extent_count"
                    | "type"
                    | "stripe_count"
                    | "stripe_size"
                    | "stripes"
                    | "tags"
            )
        })
    {
        return Err(Error::Unsupported(
            "LVM supports ordinary linear and striped segments".into(),
        ));
    }
    let count = s.num("stripe_count")?;
    let extents = s.num("extent_count")?;
    let list = s.list("stripes")?;
    if !(1..=32).contains(&count)
        || list.len() != count as usize * 2
        || extents == 0
        || !extents.is_multiple_of(count)
    {
        return Err(invalid("invalid stripe count or extent count"));
    }
    let bytes = product(extents, extent)?;
    let stripe_bytes = if count == 1 && !s.contains_key("stripe_size") {
        bytes
    } else {
        product(s.num("stripe_size")?, 512)?
    };
    if stripe_bytes == 0
        || (count > 1 && (!stripe_bytes.is_power_of_two() || !extent.is_multiple_of(stripe_bytes)))
        || !(bytes / count).is_multiple_of(stripe_bytes)
    {
        return Err(invalid("invalid LVM stripe size"));
    }
    let mut lanes = Vec::with_capacity(count as usize);
    for pair in list.as_chunks::<2>().0 {
        let [Value::Text(name), Value::Number(start)] = pair else {
            return Err(invalid("malformed stripe entry"));
        };
        let dev = devices
            .get(name)
            .ok_or_else(|| invalid("unknown PV in stripe table"))?;
        let end = start
            .checked_add(extents / count)
            .ok_or_else(|| invalid("stripe end overflow"))?;
        if end > dev.extents {
            return Err(invalid("stripe exceeds physical extents"));
        }
        let offset = dev.base + product(*start, extent)?;
        claims[dev.slot].push((*start, end));
        lanes.push((dev.slot, offset));
    }
    Ok(Segment {
        start: product(s.num("start_extent")?, extent)?,
        bytes,
        stripe_bytes,
        lanes,
    })
}

impl Group {
    pub fn parse(text: &Section, pvs: &[Physical]) -> Result<Self> {
        if text.num("version")? != 1 || text.text("contents")? != "Text Format Volume Group" {
            return Err(invalid("wrong metadata type or version"));
        }
        let mut groups = text.iter().filter_map(|(k, v)| {
            if let Value::Section(s) = v {
                Some((k, s))
            } else {
                None
            }
        });
        let (name, group) = groups
            .next()
            .ok_or_else(|| invalid("volume group missing"))?;
        if groups.next().is_some() || group.text("format")? != "lvm2" || group.num("seqno")? == 0 {
            return Err(invalid("ambiguous or invalid volume group"));
        }
        if group.contains_key("historical_logical_volumes") || group.contains_key("lock_type") {
            return Err(Error::Unsupported(
                "historical or clustered LVM group".into(),
            ));
        }
        supported(group, "READ", &["READ", "WRITE", "RESIZEABLE"])?;
        let group_id = group.text("id")?;
        uuid(group_id)?;
        let extent = product(group.num("extent_size")?, 512)?;
        if extent == 0 {
            return Err(invalid("empty physical extent size"));
        }
        let devices = devices(group, extent, pvs)?;
        let lvs = group.section("logical_volumes")?;
        let mut claims = vec![Vec::new(); pvs.len()];
        let mut normal = BTreeMap::new();
        let mut snapshots = BTreeMap::new();
        let mut hidden = BTreeSet::new();
        let mut ids = BTreeSet::new();
        for lv_name in lvs.keys() {
            let lv = lvs.section(lv_name)?;
            supported(lv, "READ", &["READ", "WRITE", "VISIBLE"])?;
            let visible = strings(lv.list("status")?)?.contains(&"VISIBLE");
            if !strings(lv.list("status")?)?.contains(&"READ")
                || lv.contains_key("origin")
                || lv.contains_key("external_origin")
            {
                return Err(Error::Unsupported(
                    "unreadable or dependent LVM volume".into(),
                ));
            }
            let id = lv.text("id")?;
            if !ids.insert(uuid(id)?) {
                return Err(invalid("duplicate logical volume UUID"));
            }
            let n = lv.num("segment_count")?;
            if !(1..=65536).contains(&n)
                || lv
                    .keys()
                    .filter(|k| {
                        k.strip_prefix("segment")
                            .is_some_and(|v| v.bytes().next().is_some_and(|b| b.is_ascii_digit()))
                    })
                    .count()
                    != n as usize
            {
                return Err(invalid("segment count mismatch"));
            }
            let first = lv.section("segment1")?;
            if first.text("type")? == "snapshot" {
                if !visible
                    || n != 1
                    || first.num("start_extent")? != 0
                    || first.keys().any(|k| {
                        !matches!(
                            k.as_str(),
                            "type"
                                | "start_extent"
                                | "extent_count"
                                | "chunk_size"
                                | "origin"
                                | "cow_store"
                                | "tags"
                        )
                    })
                {
                    return Err(Error::Unsupported(
                        "merging or unsupported LVM snapshot segment".into(),
                    ));
                }
                let bytes = product(first.num("extent_count")?, extent)?;
                let chunk = product(first.num("chunk_size")?, 512)?;
                let origin = first.text("origin")?.to_string();
                let store = first.text("cow_store")?.to_string();
                if !chunk.is_power_of_two()
                    || !(4096..=4 * 1024 * 1024).contains(&chunk)
                    || bytes == 0
                    || origin == store
                    || snapshots.insert(store, (origin, bytes, chunk)).is_some()
                {
                    return Err(invalid(
                        "invalid snapshot dependencies, length or chunk size",
                    ));
                }
                continue;
            }
            if !visible {
                hidden.insert(lv_name.to_string());
            }
            let mut bytes = 0u64;
            let mut segments = Vec::new();
            for index in 1..=n {
                let s = lv.section(&format!("segment{index}"))?;
                if product(s.num("start_extent")?, extent)? != bytes {
                    return Err(invalid("logical extents overlap or leave a hole"));
                }
                let seg = segment(s, extent, &devices, &mut claims)?;
                bytes = bytes
                    .checked_add(seg.bytes)
                    .ok_or_else(|| invalid("LV length overflow"))?;
                segments.push(seg);
            }
            let max_stripes = segments.iter().map(|s| s.lanes.len()).max().unwrap_or(0);
            let info = VolumeInfo {
                group: name.clone(),
                group_id: group_id.into(),
                name: lv_name.clone(),
                id: id.into(),
                sequence: group.num("seqno")?,
                bytes,
                segments: segments.len(),
                max_stripes,
                snapshot: None,
            };
            normal.insert(
                lv_name.clone(),
                Volume {
                    info,
                    data: Data {
                        bytes,
                        segments: segments.into(),
                    },
                    history: None,
                },
            );
        }
        for allocated in &mut claims {
            allocated.sort_unstable();
            if allocated.windows(2).any(|x| x[0].1 > x[1].0) {
                return Err(invalid(
                    "logical volumes or stripe legs share physical extents",
                ));
            }
        }
        if snapshots.keys().ne(hidden.iter()) {
            return Err(Error::Unsupported(
                "unmapped hidden LVM volumes or snapshot stores".into(),
            ));
        }
        let mut historical = BTreeMap::new();
        for (store, (origin, bytes, chunk)) in snapshots {
            let cow = normal
                .remove(&store)
                .ok_or_else(|| invalid("missing snapshot storage"))?;
            if hidden.contains(&origin) {
                return Err(Error::Unsupported("nested LVM snapshot origin".into()));
            }
            let base = normal
                .get(&origin)
                .ok_or_else(|| invalid("missing snapshot origin"))?;
            if base.info.bytes != bytes || cow.data.bytes < 3 * chunk {
                return Err(invalid("snapshot origin or store length"));
            }
            let mut info = base.info.clone();
            info.name = store.clone();
            info.id = cow.info.id;
            info.snapshot = Some(SnapshotInfo {
                origin,
                origin_uuid: base.info.id.clone(),
                chunk_bytes: chunk,
                cow_bytes: cow.data.bytes,
            });
            historical.insert(
                store,
                Volume {
                    info,
                    data: base.data.clone(),
                    history: Some(History {
                        cow: cow.data,
                        chunk,
                    }),
                },
            );
        }
        normal.append(&mut historical);
        Ok(Self {
            volumes: normal.into_values().collect(),
        })
    }
}
