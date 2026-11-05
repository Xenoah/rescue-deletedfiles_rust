//! Bounded, read-only reconstruction from explicit member images. Never repairs a member.
use crate::{
    Error, Fingerprint, MissingRange, OutputRoot, ReadState, RecoveryDestination, Result,
    source::Image,
};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::{
    collections::BTreeSet,
    fs::File,
    io::{Read, Seek, SeekFrom, Write},
    path::{Path, PathBuf},
    sync::atomic::{AtomicBool, Ordering},
};

pub const RAID_SUFFIX: &str = ".rescue-raid.json";
const MAX_MANIFEST: u64 = 128 * 1024;
const MAX_MEMBERS: usize = 16;
#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RaidLevel {
    Jbod,
    Raid0,
    Raid1,
    Raid5,
    Raid6,
    Raid10,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RaidLayout {
    #[default]
    Fixed,
    LeftAsymmetric,
    RightAsymmetric,
    LeftSymmetric,
    RightSymmetric,
    PqLast,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RaidMember {
    /// None is an explicitly missing slot, never an empty/zero-filled replacement.
    pub path: Option<PathBuf>,
    pub offset: u64,
    pub length: u64,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub fingerprint: Option<Fingerprint>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RaidConfig {
    pub schema_version: u32,
    pub level: RaidLevel,
    #[serde(default)]
    pub layout: RaidLayout,
    pub sector_size: u32,
    pub stripe_bytes: u32,
    pub members: Vec<RaidMember>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaidEvidence {
    pub config: RaidConfig,
    pub logical_bytes: u64,
    pub identity_sha256: String,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RaidInspection {
    pub evidence: RaidEvidence,
    pub sampled_bytes: u64,
    pub unavailable_sample_bytes: u64,
    pub sampled_sha256: String,
    pub filesystem_hint: Option<String>,
    pub warnings: Vec<String>,
}
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RaidReadRange {
    pub logical_offset: u64,
    pub length: u64,
    /// Zero-based manifest slots actually read and used for consistency/reconstruction.
    pub used_members: Vec<usize>,
    pub missing_members: Vec<usize>,
    pub parity_members: Vec<usize>,
    pub unavailable: bool,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct RaidReadTrace {
    pub ranges: Vec<RaidReadRange>,
    pub omitted_ranges: u64,
}
pub(crate) struct RaidSource {
    pub evidence: RaidEvidence,
    members: Vec<Option<Box<Image>>>,
    manifest_hash: Option<String>,
    trace: RaidReadTrace,
}
pub fn is_raid_path(path: &Path) -> bool {
    path.file_name()
        .is_some_and(|s| s.to_string_lossy().ends_with(RAID_SUFFIX))
}
fn invalid(s: &str) -> Error {
    Error::Invalid(format!("RAID: {s}"))
}
impl RaidConfig {
    pub fn load(path: &Path) -> Result<Self> {
        let (config, _) = manifest(&mut File::open(path)?)?;
        config.length()?;
        Ok(config)
    }
    fn length(&self) -> Result<u64> {
        use RaidLevel::*;
        let n = self.members.len();
        if self.schema_version != 1
            || !(2..=MAX_MEMBERS).contains(&n)
            || ![512, 4096].contains(&self.sector_size)
            || self.stripe_bytes < self.sector_size
            || self.stripe_bytes > 1024 * 1024
            || !self.stripe_bytes.is_power_of_two()
        {
            return Err(invalid("version, member count, sector size or stripe size"));
        }
        match self.level {
            Raid5
                if n < 3
                    || !matches!(
                        self.layout,
                        RaidLayout::LeftAsymmetric
                            | RaidLayout::RightAsymmetric
                            | RaidLayout::LeftSymmetric
                            | RaidLayout::RightSymmetric
                    ) =>
            {
                return Err(invalid(
                    "RAID5 requires 3..16 members and an explicit rotating parity layout",
                ));
            }
            Raid6 if n < 4 || self.layout != RaidLayout::PqLast => {
                return Err(invalid(
                    "RAID6 supports only pq_last: fixed P/Q, GF(2^8) polynomial 0x11d, coefficient 2^data_index",
                ));
            }
            Raid10 if !n.is_multiple_of(2) => {
                return Err(invalid("RAID10 requires adjacent mirror pairs"));
            }
            Jbod | Raid0 | Raid1 | Raid10 if self.layout != RaidLayout::Fixed => {
                return Err(invalid("this level requires the fixed layout"));
            }
            _ => {}
        }
        for m in &self.members {
            if m.length == 0
                || !m.offset.is_multiple_of(u64::from(self.sector_size))
                || !m.length.is_multiple_of(u64::from(self.sector_size))
                || m.offset.checked_add(m.length).is_none()
                || (m.path.is_none() && m.fingerprint.is_some())
            {
                return Err(invalid("member geometry"));
            }
            if self.level != Jbod
                && (m.length != self.members[0].length
                    || !m.length.is_multiple_of(u64::from(self.stripe_bytes)))
            {
                return Err(invalid(
                    "striped/mirrored member usable lengths must match and end at a stripe boundary",
                ));
            }
        }
        if self.level == Jbod {
            return self.members.iter().try_fold(0u64, |a, m| {
                a.checked_add(m.length)
                    .ok_or_else(|| invalid("logical size overflow"))
            });
        }
        let data = match self.level {
            Raid0 => n,
            Raid1 => 1,
            Raid5 => n - 1,
            Raid6 => n - 2,
            Raid10 => n / 2,
            Jbod => unreachable!(),
        };
        self.members[0]
            .length
            .checked_mul(data as u64)
            .ok_or_else(|| invalid("logical size overflow"))
    }
}
fn manifest(file: &mut File) -> Result<(RaidConfig, String)> {
    if file.metadata()?.len() > MAX_MANIFEST {
        return Err(Error::Limit("RAID manifest exceeds 128 KiB".into()));
    }
    file.seek(SeekFrom::Start(0))?;
    let mut bytes = Vec::new();
    file.take(MAX_MANIFEST + 1).read_to_end(&mut bytes)?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(Error::Limit("RAID manifest exceeds 128 KiB".into()));
    }
    Ok((
        serde_json::from_slice(&bytes)?,
        format!("{:x}", Sha256::digest(&bytes)),
    ))
}
impl RaidSource {
    pub fn open(file: &mut File, path: &Path) -> Result<Self> {
        let (config, digest) = manifest(file)?;
        let mut source = Self::from_config(config, path.parent().unwrap_or(Path::new(".")))?;
        source.manifest_hash = Some(digest);
        Ok(source)
    }
    pub fn from_config(mut config: RaidConfig, relative_to: &Path) -> Result<Self> {
        let length = config.length()?;
        let mut members = Vec::new();
        let mut identities = Vec::new();
        for member in &mut config.members {
            let Some(path) = &member.path else {
                members.push(None);
                continue;
            };
            let path = if path.is_absolute() {
                path.clone()
            } else {
                relative_to.join(path)
            };
            let path = crate::canonical_source_path(&path)?;
            if is_raid_path(&path) || crate::is_device(&path) {
                return Err(invalid(
                    "members must be regular RAW images; nested arrays and live devices are not accepted",
                ));
            }
            let mut image = Image::open(&path)?;
            if image.sector_size() != config.sector_size && image.has_read_map() {
                return Err(invalid("member map sector size differs from the array"));
            }
            if member
                .offset
                .checked_add(member.length)
                .is_none_or(|e| e > image.length)
            {
                return Err(invalid("member range extends beyond image/map"));
            }
            let identity = image.acquisition_identity()?.identity;
            if identities.contains(&identity) {
                return Err(invalid("the same file is used as multiple members"));
            }
            identities.push(identity);
            let fingerprint = image.fingerprint()?;
            if member
                .fingerprint
                .as_ref()
                .is_some_and(|old| old != &fingerprint)
            {
                return Err(Error::SourceChanged);
            }
            member.fingerprint = Some(fingerprint);
            member.path = Some(path);
            members.push(Some(Box::new(image)));
        }
        let digest = format!("{:x}", Sha256::digest(serde_json::to_vec(&config)?));
        let warnings=vec!["Array configuration is explicit. Sample hashes and parity consistency do not establish that member order, stripe size or old data are correct.".into(),"Missing or inconsistent byte ranges are unavailable, never fabricated as zero. RAID1/10 require all readable copies of a range to agree; no copy is silently preferred.".into(),"When parity/copies are missing, readable data may lack independent redundancy. The manifest and member fingerprints identify the exact configuration used.".into()];
        Ok(Self {
            evidence: RaidEvidence {
                config,
                logical_bytes: length,
                identity_sha256: digest,
                warnings,
            },
            members,
            manifest_hash: None,
            trace: RaidReadTrace::default(),
        })
    }
    pub fn take_trace(&mut self) -> RaidReadTrace {
        std::mem::take(&mut self.trace)
    }
    fn trace_range(&mut self, range: RaidReadRange) {
        if let Some(last) = self.trace.ranges.last_mut()
            && last.logical_offset + last.length == range.logical_offset
            && last.used_members == range.used_members
            && last.missing_members == range.missing_members
            && last.parity_members == range.parity_members
            && last.unavailable == range.unavailable
        {
            last.length += range.length;
        } else if self.trace.ranges.len() < 128 {
            self.trace.ranges.push(range);
        } else {
            self.trace.omitted_ranges += 1;
        }
    }
    pub fn check(&mut self, file: &mut File) -> Result<()> {
        if let Some(expected) = &self.manifest_hash
            && manifest(file)?.1 != *expected
        {
            return Err(Error::SourceChanged);
        }
        self.check_members()
    }
    pub fn check_members(&mut self) -> Result<()> {
        for (member, expected) in self.members.iter_mut().zip(&self.evidence.config.members) {
            if let Some(member) = member
                && Some(member.fingerprint()?) != expected.fingerprint
            {
                return Err(Error::SourceChanged);
            }
        }
        Ok(())
    }
    pub fn identity(&self, file: &File) -> Result<String> {
        if let Some(expected) = &self.manifest_hash
            && manifest(&mut file.try_clone()?)?.1 != *expected
        {
            return Err(Error::SourceChanged);
        }
        let identities = self
            .members
            .iter()
            .map(|m| m.as_ref().map(|m| m.acquisition_identity()).transpose())
            .collect::<Result<Vec<_>>>()?;
        Ok(format!(
            "{:x}",
            Sha256::digest(serde_json::to_vec(&(
                self.evidence.identity_sha256.clone(),
                identities
            ))?)
        ))
    }
    pub fn reject_same_file(&self, file: &File) -> Result<()> {
        for member in self.members.iter().flatten() {
            member.reject_same_file(file)?;
        }
        Ok(())
    }
    fn member_read(
        &mut self,
        index: usize,
        offset: u64,
        length: usize,
    ) -> Result<(Vec<u8>, Vec<MissingRange>)> {
        if let Some(member) = &mut self.members[index] {
            let base = self.evidence.config.members[index].offset + offset;
            let (bytes, mut gaps) = member.read_available(base, length)?;
            for gap in &mut gaps {
                gap.source_offset = gap.source_offset.saturating_sub(base);
            }
            Ok((bytes, gaps))
        } else {
            Ok((vec![0; length], vec![gap(0, length as u64)]))
        }
    }
    fn parity_positions(&self, stripe: u64) -> (usize, Option<usize>, Vec<usize>) {
        let c = &self.evidence.config;
        let n = c.members.len();
        if c.level == RaidLevel::Raid6 {
            return (n - 2, Some(n - 1), (0..n - 2).collect());
        }
        let p = match c.layout {
            RaidLayout::LeftAsymmetric | RaidLayout::LeftSymmetric => {
                n - 1 - (stripe % n as u64) as usize
            }
            _ => (stripe % n as u64) as usize,
        };
        let data = if matches!(
            c.layout,
            RaidLayout::LeftSymmetric | RaidLayout::RightSymmetric
        ) {
            (1..n).map(|i| (p + i) % n).collect()
        } else {
            (0..n).filter(|i| *i != p).collect()
        };
        (p, None, data)
    }
    pub fn read_available(
        &mut self,
        offset: u64,
        length: usize,
    ) -> Result<(Vec<u8>, Vec<MissingRange>)> {
        if offset
            .checked_add(length as u64)
            .is_none_or(|e| e > self.evidence.logical_bytes)
        {
            return Err(invalid("read beyond array"));
        }
        let mut output = Vec::with_capacity(length);
        let mut gaps = Vec::new();
        while output.len() < length {
            let pos = offset + output.len() as u64;
            let c = &self.evidence.config;
            let n = c.members.len();
            let chunk = u64::from(c.stripe_bytes);
            let mut count = (length - output.len()).min((chunk - pos % chunk) as usize);
            let (indexes, member_offset, data_slot, parity) = match c.level {
                RaidLevel::Jbod => {
                    let mut start = 0;
                    let mut index = 0;
                    for (i, m) in c.members.iter().enumerate() {
                        if pos < start + m.length {
                            index = i;
                            break;
                        }
                        start += m.length;
                    }
                    count = count.min((c.members[index].length - (pos - start)) as usize);
                    (vec![index], pos - start, 0, None)
                }
                RaidLevel::Raid0 => {
                    let unit = pos / chunk;
                    (
                        vec![(unit % n as u64) as usize],
                        unit / n as u64 * chunk + pos % chunk,
                        0,
                        None,
                    )
                }
                RaidLevel::Raid1 => ((0..n).collect(), pos, 0, None),
                RaidLevel::Raid10 => {
                    let unit = pos / chunk;
                    let pair = (unit % (n / 2) as u64) as usize * 2;
                    (
                        vec![pair, pair + 1],
                        unit / (n / 2) as u64 * chunk + pos % chunk,
                        0,
                        None,
                    )
                }
                RaidLevel::Raid5 | RaidLevel::Raid6 => {
                    let data = n - if c.level == RaidLevel::Raid5 { 1 } else { 2 };
                    let unit = pos / chunk;
                    let stripe = unit / data as u64;
                    let (p, q, slots) = self.parity_positions(stripe);
                    (
                        (0..n).collect(),
                        stripe * chunk + pos % chunk,
                        (unit % data as u64) as usize,
                        Some((p, q, slots)),
                    )
                }
            };
            let mut reads = Vec::new();
            let mut boundaries = BTreeSet::from([0usize, count]);
            for &index in &indexes {
                let (bytes, missing) = self.member_read(index, member_offset, count)?;
                for g in &missing {
                    boundaries.insert((g.source_offset as usize).min(count));
                    boundaries
                        .insert((g.source_offset.saturating_add(g.length) as usize).min(count));
                }
                reads.push((bytes, missing));
            }
            let edges = boundaries.into_iter().collect::<Vec<_>>();
            let base = output.len();
            output.resize(base + count, 0);
            for range in edges.windows(2) {
                let start = range[0];
                let end = range[1];
                if start == end {
                    continue;
                }
                let available = reads
                    .iter()
                    .map(|(b, g)| {
                        if g.iter().any(|g| {
                            g.source_offset < end as u64
                                && g.source_offset + g.length > start as u64
                        }) {
                            None
                        } else {
                            Some(&b[start..end])
                        }
                    })
                    .collect::<Vec<_>>();
                let result = if let Some((p, q, slots)) = &parity {
                    reconstruct(&available, *p, *q, slots, data_slot)
                } else {
                    let mut good = available.iter().flatten();
                    let first = good.next();
                    first.filter(|a| good.all(|b| *a == b)).map(|b| b.to_vec())
                };
                self.trace_range(RaidReadRange {
                    logical_offset: pos + start as u64,
                    length: (end - start) as u64,
                    used_members: available
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.is_some())
                        .map(|(i, _)| indexes[i])
                        .collect(),
                    missing_members: available
                        .iter()
                        .enumerate()
                        .filter(|(_, v)| v.is_none())
                        .map(|(i, _)| indexes[i])
                        .collect(),
                    parity_members: parity
                        .as_ref()
                        .map(|(p, q, _)| std::iter::once(*p).chain(*q).collect())
                        .unwrap_or_default(),
                    unavailable: result.is_none(),
                });
                if let Some(bytes) = result {
                    output[base + start..base + end].copy_from_slice(&bytes);
                } else {
                    push_gap(&mut gaps, pos + start as u64, (end - start) as u64);
                }
            }
        }
        Ok((output, gaps))
    }
}
fn gap(offset: u64, length: u64) -> MissingRange {
    MissingRange {
        source_offset: offset,
        length,
        logical_offset: None,
        logical_length: None,
        reason: ReadState::Failed,
    }
}
fn push_gap(gaps: &mut Vec<MissingRange>, offset: u64, length: u64) {
    if let Some(last) = gaps.last_mut()
        && last.source_offset + last.length == offset
    {
        last.length += length;
    } else {
        gaps.push(gap(offset, length));
    }
}
const fn gf_tables() -> ([u8; 512], [u8; 256]) {
    let mut exp = [0u8; 512];
    let mut log = [0u8; 256];
    let mut value = 1u16;
    let mut i = 0;
    while i < 255 {
        exp[i] = value as u8;
        log[value as usize] = i as u8;
        value <<= 1;
        if value & 256 != 0 {
            value ^= 0x11d;
        }
        i += 1;
    }
    while i < 512 {
        exp[i] = exp[i - 255];
        i += 1;
    }
    (exp, log)
}
const GF: ([u8; 512], [u8; 256]) = gf_tables();
fn mul(a: u8, b: u8) -> u8 {
    if a == 0 || b == 0 {
        0
    } else {
        GF.0[usize::from(GF.1[a as usize]) + usize::from(GF.1[b as usize])]
    }
}
fn coefficient(index: usize) -> u8 {
    GF.0[index]
}
fn inv(a: u8) -> u8 {
    debug_assert_ne!(a, 0);
    GF.0[255 - usize::from(GF.1[a as usize])]
}
/// Return bytes only if all available redundancy agrees. A missing parity is never invented as evidence.
fn reconstruct(
    parts: &[Option<&[u8]>],
    p: usize,
    q: Option<usize>,
    data: &[usize],
    target: usize,
) -> Option<Vec<u8>> {
    let missing = parts.iter().filter(|p| p.is_none()).count();
    if missing > if q.is_some() { 2 } else { 1 } {
        return None;
    }
    let length = parts.iter().flatten().next()?.len();
    let absent = data
        .iter()
        .enumerate()
        .filter(|(_, i)| parts[**i].is_none())
        .map(|(j, _)| j)
        .collect::<Vec<_>>();
    let mut recovered = vec![vec![0u8; length]; absent.len()];
    if absent.len() == 1 {
        let index = absent[0];
        let use_p = parts[p].is_some();
        let parity = if use_p { parts[p]? } else { parts[q?]? };
        let scale = if use_p { 1 } else { inv(coefficient(index)) };
        for at in 0..length {
            let mut value = parity[at];
            for (j, i) in data.iter().enumerate() {
                if j != index {
                    value ^= if use_p {
                        parts[*i]?[at]
                    } else {
                        mul(parts[*i]?[at], coefficient(j))
                    };
                }
            }
            recovered[0][at] = mul(value, scale);
        }
    } else if absent.len() == 2 {
        let a = absent[0];
        let b = absent[1];
        let ca = coefficient(a);
        let cb = coefficient(b);
        let divide = inv(ca ^ cb);
        let pp = parts[p]?;
        let qq = parts[q?]?;
        for at in 0..length {
            let mut s = pp[at];
            let mut t = qq[at];
            for (j, i) in data.iter().enumerate() {
                if j != a && j != b {
                    let v = parts[*i]?[at];
                    s ^= v;
                    t ^= mul(v, coefficient(j));
                }
            }
            let av = mul(t ^ mul(cb, s), divide);
            recovered[0][at] = av;
            recovered[1][at] = s ^ av;
        }
    }
    let get = |j: usize| -> &[u8] {
        if let Some(bytes) = parts[data[j]] {
            bytes
        } else {
            &recovered[absent.iter().position(|i| *i == j).unwrap()]
        }
    };
    for at in 0..length {
        let mut pv = 0;
        let mut qv = 0;
        for j in 0..data.len() {
            let v = get(j)[at];
            pv ^= v;
            if q.is_some() {
                qv ^= mul(v, coefficient(j));
            }
        }
        if parts[p].is_some_and(|bytes| bytes[at] != pv)
            || q.is_some_and(|q| parts[q].is_some_and(|bytes| bytes[at] != qv))
        {
            return None;
        }
    }
    Some(get(target).to_vec())
}
pub fn inspect_raid(
    config: &RaidConfig,
    relative_to: &Path,
    cancel: &AtomicBool,
) -> Result<RaidInspection> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let mut source = RaidSource::from_config(config.clone(), relative_to)?;
    let size = source.evidence.logical_bytes.min(64 * 1024) as usize;
    let mut hash = Sha256::new();
    let mut sampled = 0;
    let mut missing = 0;
    let mut hint = None;
    for offset in [
        0,
        source.evidence.logical_bytes.saturating_sub(size as u64) / 2,
        source.evidence.logical_bytes.saturating_sub(size as u64),
    ] {
        if cancel.load(Ordering::Relaxed) {
            return Err(Error::Cancelled);
        }
        let (bytes, gaps) = source.read_available(offset, size)?;
        sampled += size as u64;
        missing += gaps.iter().map(|g| g.length).sum::<u64>();
        hash.update(offset.to_le_bytes());
        hash.update(&bytes);
        hash.update(serde_json::to_vec(&gaps)?);
        if offset == 0 && gaps.is_empty() {
            hint = if bytes.get(3..11) == Some(b"NTFS    ") {
                Some("ntfs".into())
            } else if bytes.get(3..11) == Some(b"EXFAT   ") {
                Some("exfat".into())
            } else if bytes.get(82..90) == Some(b"FAT32   ") {
                Some("fat32".into())
            } else if bytes.get(510..512) == Some(&[0x55, 0xaa]) {
                Some("boot_or_partition_signature".into())
            } else {
                None
            };
        }
    }
    source.check_members()?;
    Ok(RaidInspection{evidence:source.evidence,sampled_bytes:sampled,unavailable_sample_bytes:missing,sampled_sha256:format!("{:x}",hash.finalize()),filesystem_hint:hint,warnings:vec!["Only bounded samples were checked. A boot signature is a hint, not filesystem validation; scan the selected array to validate filesystem structures.".into()]})
}
/// Save a reproducible, fingerprint-bound configuration with exclusive creation.
pub fn create_raid_manifest(
    config: &RaidConfig,
    relative_to: &Path,
    output: &Path,
    roots: Option<&[OutputRoot]>,
    cancel: &AtomicBool,
) -> Result<RaidInspection> {
    if !is_raid_path(output) {
        return Err(Error::Request(format!(
            "array filename must end in {RAID_SUFFIX}"
        )));
    }
    let report = inspect_raid(config, relative_to, cancel)?;
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    let output = crate::canonical_output_path(output)?;
    let parent = output.parent().ok_or_else(|| invalid("output parent"))?;
    let destination = if let Some(roots) = roots {
        RecoveryDestination::within_create(parent, roots)?
    } else {
        RecoveryDestination::open(parent)?
    };
    destination.verify()?;
    let bytes = serde_json::to_vec_pretty(&report.evidence.config)?;
    if bytes.len() as u64 > MAX_MANIFEST {
        return Err(Error::Limit("RAID manifest".into()));
    }
    let mut file = destination.dir.open_with(
        output
            .file_name()
            .ok_or_else(|| invalid("output filename"))?,
        cap_std::fs::OpenOptions::new().write(true).create_new(true),
    )?;
    file.write_all(&bytes)?;
    file.write_all(b"\n")?;
    file.sync_all()?;
    destination.verify()?;
    Ok(report)
}

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct RaidSearchOptions {
    pub stripe_sizes: Vec<u32>,
    pub layouts: Vec<RaidLayout>,
    pub reorder_members: bool,
    pub max_configurations: u16,
    pub max_milliseconds: u32,
}
impl Default for RaidSearchOptions {
    fn default() -> Self {
        Self {
            stripe_sizes: (12..=20).map(|i| 1 << i).collect(),
            layouts: Vec::new(),
            reorder_members: false,
            max_configurations: 128,
            max_milliseconds: 5000,
        }
    }
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RaidAlternative {
    pub inspection: RaidInspection,
    pub filesystem_evidence: Vec<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct RaidSearchResult {
    pub candidates: Vec<RaidAlternative>,
    pub tested_configurations: u16,
    pub cancelled: bool,
    pub truncated: bool,
    pub elapsed_milliseconds: u64,
    pub warnings: Vec<String>,
}
fn next_order(order: &mut [usize]) -> bool {
    let Some(i) = (0..order.len() - 1)
        .rev()
        .find(|i| order[*i] < order[*i + 1])
    else {
        return false;
    };
    let j = (i + 1..order.len())
        .rev()
        .find(|j| order[*j] > order[i])
        .unwrap();
    order.swap(i, j);
    order[i + 1..].reverse();
    true
}
/// Bounded alternatives only: no automatically selected winner and no writes.
pub fn search_raid(
    template: &RaidConfig,
    relative_to: &Path,
    options: &RaidSearchOptions,
    cancel: &AtomicBool,
    mut progress: impl FnMut(crate::Progress),
) -> Result<RaidSearchResult> {
    if cancel.load(Ordering::Relaxed) {
        return Err(Error::Cancelled);
    }
    if options.stripe_sizes.is_empty()
        || options.stripe_sizes.len() > 9
        || options.layouts.len() > 4
        || !(1..=256).contains(&options.max_configurations)
        || !(1..=30000).contains(&options.max_milliseconds)
        || options
            .stripe_sizes
            .iter()
            .any(|s| !(4096..=1024 * 1024).contains(s) || !s.is_power_of_two())
    {
        return Err(Error::Request("RAID search requires 1..9 power-of-two stripe sizes (4 KiB..1 MiB), <=4 layouts, 1..256 probes, and 1..30000 ms".into()));
    }
    // Pin every member before varying any geometry/order. Each subsequent open
    // must match this snapshot, even when templates did not contain fingerprints.
    let pinned = RaidSource::from_config(template.clone(), relative_to)?
        .evidence
        .config;
    let layouts = if options.layouts.is_empty() {
        if template.level == RaidLevel::Raid5 {
            vec![
                RaidLayout::LeftSymmetric,
                RaidLayout::RightSymmetric,
                RaidLayout::LeftAsymmetric,
                RaidLayout::RightAsymmetric,
            ]
        } else {
            vec![template.layout]
        }
    } else {
        options.layouts.clone()
    };
    let start = std::time::Instant::now();
    let mut order = (0..pinned.members.len()).collect::<Vec<_>>();
    let mut result=RaidSearchResult{candidates:Vec::new(),tested_configurations:0,cancelled:false,truncated:false,elapsed_milliseconds:0,warnings:vec!["Candidates are alternatives, not a ranking or proof of the original array. Explicitly select one before scanning/recovery. Search keeps supplied member offsets and usable lengths; it does not infer controller metadata or arbitrary offsets.".into()]};
    'orders: loop {
        for size in &options.stripe_sizes {
            for layout in &layouts {
                if cancel.load(Ordering::Relaxed) {
                    result.cancelled = true;
                    break 'orders;
                }
                if result.tested_configurations >= options.max_configurations
                    || start.elapsed().as_millis() >= u128::from(options.max_milliseconds)
                    || result.candidates.len() >= 32
                {
                    result.truncated = true;
                    break 'orders;
                }
                result.tested_configurations += 1;
                let mut config = pinned.clone();
                config.stripe_bytes = *size;
                config.layout = *layout;
                config.members = order.iter().map(|i| pinned.members[*i].clone()).collect();
                let probe = (|| -> Result<RaidAlternative> {
                    let inspection = inspect_raid(&config, relative_to, cancel)?;
                    if inspection.unavailable_sample_bytes != 0 {
                        return Err(invalid("sample ranges unavailable or inconsistent"));
                    }
                    let mut image = Image::from_raid(RaidSource::from_config(
                        inspection.evidence.config.clone(),
                        relative_to,
                    )?)?;
                    image.limit_reads(8 * 1024 * 1024);
                    let regions = crate::partitions::inspect_image(&mut image)?.regions;
                    let mut evidence = Vec::new();
                    for region in regions.iter().filter(|r| r.filesystem.is_some()).take(4) {
                        if cancel.load(Ordering::Relaxed) {
                            return Err(Error::Cancelled);
                        }
                        image.select_window(region.offset, region.length)?;
                        if let Ok(mut checks) =
                            crate::discovery::probe_image(&mut image, region.filesystem)
                        {
                            evidence.push(format!(
                                "Region {} at {}: {:?}",
                                region.index, region.offset, region.filesystem
                            ));
                            evidence.append(&mut checks);
                        }
                    }
                    if evidence.is_empty() {
                        return Err(invalid("no independently validated filesystem structures"));
                    }
                    Ok(RaidAlternative {
                        inspection,
                        filesystem_evidence: evidence,
                    })
                })();
                match probe {
                    Ok(candidate) => result.candidates.push(candidate),
                    Err(Error::SourceChanged) => return Err(Error::SourceChanged),
                    Err(Error::Cancelled) => {
                        result.cancelled = true;
                        break 'orders;
                    }
                    Err(_) => {}
                }
                progress(crate::Progress {
                    scanned_records: u64::from(result.tested_configurations),
                    total_records: u64::from(options.max_configurations),
                    candidates: result.candidates.len(),
                    unit: crate::ProgressUnit::Records,
                });
            }
        }
        if !options.reorder_members || !next_order(&mut order) {
            break;
        }
    }
    result.elapsed_milliseconds = start.elapsed().as_millis() as u64;
    Ok(result)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn geometry_overflow_and_cancel_are_rejected_before_member_io() {
        let base = RaidConfig {
            schema_version: 1,
            level: RaidLevel::Raid6,
            layout: RaidLayout::PqLast,
            sector_size: 512,
            stripe_bytes: 4096,
            members: (0..4)
                .map(|i| RaidMember {
                    path: Some(PathBuf::from(format!("not-opened-{i}"))),
                    offset: 0,
                    length: 4096,
                    fingerprint: None,
                })
                .collect(),
        };
        let cancel = AtomicBool::new(true);
        assert!(matches!(
            search_raid(
                &base,
                Path::new("."),
                &RaidSearchOptions::default(),
                &cancel,
                |_| {}
            ),
            Err(Error::Cancelled)
        ));
        assert!(matches!(
            inspect_raid(&base, Path::new("."), &cancel),
            Err(Error::Cancelled)
        ));
        for value in [0, 1, 511, 513, u64::MAX, u64::MAX - 4095] {
            let mut config = base.clone();
            for member in &mut config.members {
                member.length = value;
            }
            assert!(config.length().is_err());
        }
        let mut config = base.clone();
        config.members[0].offset = u64::MAX - 511;
        assert!(config.length().is_err());
        config = base;
        config.members.resize(17, config.members[0].clone());
        assert!(config.length().is_err());
    }
    #[test]
    fn trace_is_bounded_and_scoped_grants_cannot_expand() {
        let root = tempfile::tempdir().unwrap();
        let mut members = Vec::new();
        for index in 0..2 {
            let path = root.path().join(format!("member-{index}.raw"));
            std::fs::write(&path, vec![index as u8 + 1; 96 * 1024]).unwrap();
            members.push(RaidMember {
                path: Some(path),
                offset: 0,
                length: 96 * 1024,
                fingerprint: None,
            });
        }
        let config = RaidConfig {
            schema_version: 1,
            level: RaidLevel::Raid0,
            layout: RaidLayout::Fixed,
            sector_size: 512,
            stripe_bytes: 512,
            members,
        };
        let mut source = RaidSource::from_config(config.clone(), root.path()).unwrap();
        let (bytes, gaps) = source.read_available(0, 192 * 1024).unwrap();
        assert!(gaps.is_empty());
        for (i, chunk) in bytes.chunks(512).enumerate() {
            assert!(chunk.iter().all(|b| *b == 1 + (i % 2) as u8));
        }
        let trace = source.take_trace();
        assert_eq!(trace.ranges.len(), 128);
        assert_eq!(trace.omitted_ranges, 256);
        assert_eq!(trace.ranges[1].used_members, vec![1]);
        assert!(trace.ranges.iter().all(|r| r.missing_members.is_empty()
            && r.parity_members.is_empty()
            && !r.unavailable));
        let only = config.members[0].path.clone().unwrap();
        {
            let _scope = crate::SourceScope::enter([only.clone()]);
            let _nested =
                crate::SourceScope::enter(config.members.iter().filter_map(|m| m.path.clone()));
            assert!(RaidSource::from_config(config.clone(), root.path()).is_err());
        }
        assert!(RaidSource::from_config(config, root.path()).is_ok());
    }
}
