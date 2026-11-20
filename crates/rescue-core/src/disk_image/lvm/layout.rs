//! Resolve LVM physical extents using authenticated VG metadata and supplied PV UUIDs.
use super::{
    Error, Result, invalid,
    pv::Physical,
    text::{Fields, Section, Value},
};
use serde::Serialize;
use std::collections::{BTreeMap, BTreeSet};

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
}
pub(super) struct Group {
    pub volumes: Vec<Volume>,
}
pub(super) struct Volume {
    pub info: VolumeInfo,
    pub segments: Vec<Segment>,
}
pub(super) struct Segment {
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
        let mut volumes = Vec::new();
        let mut ids = BTreeSet::new();
        for lv_name in lvs.keys() {
            let lv = lvs.section(lv_name)?;
            supported(lv, "VISIBLE", &["READ", "WRITE", "VISIBLE"])?;
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
            };
            volumes.push(Volume { info, segments });
        }
        for allocated in &mut claims {
            allocated.sort_unstable();
            if allocated.windows(2).any(|x| x[0].1 > x[1].0) {
                return Err(invalid(
                    "logical volumes or stripe legs share physical extents",
                ));
            }
        }
        Ok(Self { volumes })
    }
}
