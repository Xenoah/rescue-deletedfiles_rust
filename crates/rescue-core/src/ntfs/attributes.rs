//! Bounded NTFS ATTRIBUTE_LIST resolution. No guessed record references or VCNs.
use super::*;
use std::collections::{BTreeMap, BTreeSet};

const MAX_LIST_BYTES: u64 = 1024 * 1024;
const MAX_LIST_ENTRIES: usize = 16_384;
const MAX_EXTENSION_RECORDS: usize = 1024;
const MAX_RECORD_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Debug)]
pub(super) struct Attribute {
    pub kind: u32,
    pub id: u16,
    pub name: Option<String>,
    pub lowest_vcn: u64,
    pub raw: Vec<u8>,
}
#[derive(Debug, PartialEq, Eq, PartialOrd, Ord)]
struct Entry {
    kind: u32,
    name: Option<String>,
    lowest_vcn: u64,
    reference: u64,
    id: u16,
}
impl Entry {
    fn index(&self) -> u64 {
        self.reference & 0x0000_ffff_ffff_ffff
    }
}
fn entries(raw: &[u8]) -> Result<Vec<Entry>> {
    if !raw.is_empty() && raw.iter().all(|b| *b == 0) {
        return Err(invalid(
            "attribute list is entirely zero; original metadata is unavailable (discard or overwrite possible)",
        ));
    }
    let mut at = 0;
    let mut entries = Vec::new();
    let mut unique = BTreeSet::new();
    while at < raw.len() {
        if entries.len() >= MAX_LIST_ENTRIES {
            return Err(Error::Limit("NTFS attribute-list entries".into()));
        }
        let length = u16_at(raw, at + 4)? as usize;
        if length < 26 || !length.is_multiple_of(8) {
            return Err(invalid("attribute-list entry length"));
        }
        let entry = bytes(raw, at, length)?;
        let name = if entry[6] == 0 {
            None
        } else {
            if entry[7] < 26 {
                return Err(invalid("attribute-list name overlaps header"));
            }
            let codes = bytes(entry, entry[7] as usize, entry[6] as usize * 2)?
                .as_chunks::<2>()
                .0
                .iter()
                .map(|b| u16::from_le_bytes(*b))
                .collect::<Vec<_>>();
            Some(String::from_utf16(&codes).map_err(|_| invalid("attribute-list UTF-16"))?)
        };
        let entry = Entry {
            kind: u32_at(entry, 0)?,
            name,
            lowest_vcn: u64_at(entry, 8)?,
            reference: u64_at(entry, 16)?,
            id: u16_at(entry, 24)?,
        };
        if entry.kind == 0
            || entry.kind == u32::MAX
            || entry.kind == 0x20
            || entry.reference >> 48 == 0
        {
            return Err(invalid("invalid attribute-list reference/type"));
        }
        if !unique.insert((
            entry.kind,
            entry.name.clone(),
            entry.lowest_vcn,
            entry.reference,
            entry.id,
        )) {
            return Err(invalid("duplicate attribute-list entry"));
        }
        entries.push(entry);
        at += length;
    }
    if entries.is_empty() {
        return Err(invalid("empty attribute list"));
    }
    Ok(entries)
}

fn list_bytes(image: &mut Image, record: &Record, geometry: &Geometry) -> Result<Vec<u8>> {
    let attrs = record
        .attributes
        .iter()
        .filter(|a| a.kind == 0x20)
        .collect::<Vec<_>>();
    if attrs.len() != 1
        || attrs[0].name.is_some()
        || attrs[0].lowest_vcn != 0
        || u16_at(&attrs[0].raw, 12)? != 0
    {
        return Err(invalid("invalid attribute-list storage"));
    }
    let stream = parse_stream(&attrs[0].raw, None, 0, geometry)?;
    let data = stream
        .data
        .ok_or_else(|| invalid("unsupported attribute-list storage"))?;
    if data.size() > MAX_LIST_BYTES {
        return Err(Error::Limit("NTFS attribute list exceeds 1 MiB".into()));
    }
    if let Data::NonResident {
        runs,
        size,
        initialized,
    } = &data
        && (initialized != size || runs.iter().any(|r| r.physical.is_none()))
    {
        return Err(invalid("sparse/uninitialized attribute list"));
    }
    data.read(image, 0, data.size() as usize)
}

/// Merge one stream's extents. The lowest VCN, not record order, determines order.
fn merge(attrs: &[&Attribute], geometry: &Geometry, complete: bool) -> Result<Stream> {
    let mut attrs = attrs.to_vec();
    attrs.sort_by_key(|a| a.lowest_vcn);
    let first = attrs[0];
    let flags = u16_at(&first.raw, 12)?;
    if first.lowest_vcn != 0 {
        return Err(invalid("stream has no first extent"));
    }
    if first.raw[8] == 0 {
        if attrs.len() != 1 {
            return Err(invalid("resident stream has multiple extents"));
        }
        return parse_stream(&first.raw, first.name.clone(), flags, geometry);
    }
    let compressed = flags & 1 != 0;
    let compression = u16_at(&first.raw, 34)?;
    if flags & !0x8001 != 0
        || (compressed && (compression != 4 || geometry.cluster > 4096))
        || (!compressed && compression != 0)
    {
        return Ok(Stream {
            name: first.name.clone(),
            data: None,
            unsupported: vec!["encrypted or unsupported extent compression".into()],
        });
    }
    let size = u64_at(&first.raw, 48)?;
    let initialized = u64_at(&first.raw, 56)?;
    if initialized > size {
        return Err(invalid("initialized extent size exceeds stream size"));
    }
    let mut runs = Vec::new();
    let mut end = 0;
    for attr in attrs {
        if attr.raw[8] != 1
            || u16_at(&attr.raw, 12)? != flags
            || u16_at(&attr.raw, 34)? != compression
        {
            return Err(invalid("extent storage/flags disagree"));
        }
        let start = attr
            .lowest_vcn
            .checked_mul(geometry.cluster)
            .ok_or_else(|| invalid("extent VCN overflow"))?;
        if start != end {
            if !complete && start > end {
                break;
            }
            return Err(invalid("overlapping or missing VCN extents"));
        }
        let highest = u64_at(&attr.raw, 24)?;
        // Sizes in extension headers are unspecified; only the base extent owns them.
        let relative_last = if highest == u64::MAX
            && start == 0
            && size == 0
            && initialized == 0
            && u64_at(&attr.raw, 40)? == 0
        {
            u64::MAX
        } else {
            highest
                .checked_sub(attr.lowest_vcn)
                .ok_or_else(|| invalid("inverted extent VCNs"))?
        };
        let run_offset = u16_at(&attr.raw, 32)? as usize;
        let header = if compressed { 72 } else { 64 };
        let name_end = if attr.name.is_some() {
            u16_at(&attr.raw, 10)? as usize + attr.raw[9] as usize * 2
        } else {
            0
        };
        if run_offset < header.max(name_end) {
            return Err(invalid("extent runlist overlaps header/name"));
        }
        let mut extent = runlist(
            bytes(
                &attr.raw,
                run_offset,
                attr.raw.len().saturating_sub(run_offset),
            )?,
            geometry,
            relative_last,
        )?;
        for run in &mut extent {
            run.logical = run
                .logical
                .checked_add(start)
                .ok_or_else(|| invalid("logical extent overflow"))?;
            run.logical
                .checked_add(run.length)
                .ok_or_else(|| invalid("logical extent end overflow"))?;
        }
        end = extent.last().map_or(start, |r| r.logical + r.length);
        runs.extend(extent);
    }
    if complete && size > end {
        return Err(invalid("stream larger than resolved extents"));
    }
    let mut physical = runs
        .iter()
        .filter_map(|r| r.physical.map(|p| (p, p + r.length)))
        .collect::<Vec<_>>();
    physical.sort_unstable();
    if physical.windows(2).any(|w| w[0].1 > w[1].0) {
        return Err(invalid("overlapping physical stream extents"));
    }
    Ok(Stream {
        name: first.name.clone(),
        data: Some(if compressed {
            Data::Compressed {
                runs,
                size,
                initialized,
                unit: geometry.cluster * 16,
            }
        } else {
            Data::NonResident {
                runs,
                size,
                initialized,
            }
        }),
        unsupported: Vec::new(),
    })
}
fn mft_prefix(records: &BTreeMap<u64, Record>, geometry: &Geometry) -> Result<Data> {
    let attrs = records
        .values()
        .flat_map(|r| r.attributes.iter())
        .filter(|a| a.kind == 0x80 && a.name.is_none())
        .collect::<Vec<_>>();
    if attrs.is_empty() {
        return Err(invalid("MFT DATA absent"));
    }
    let data = merge(&attrs, geometry, false)?
        .data
        .ok_or_else(|| invalid("unsupported MFT extents"))?;
    if let Data::NonResident {
        runs,
        size,
        initialized,
    } = &data
        && size == initialized
        && !runs.iter().any(|r| r.physical.is_none())
    {
        return Ok(data);
    }
    Err(invalid("MFT extents must be allocated and initialized"))
}

pub(super) fn resolve(
    image: &mut Image,
    geometry: &Geometry,
    index: u64,
    base: Record,
    mft: Option<&Data>,
) -> Result<Record> {
    if base.base != 0 || !base.attributes.iter().any(|a| a.kind == 0x20) {
        return Ok(base);
    }
    let list = list_bytes(image, &base, geometry)?;
    let entries = entries(&list)?;
    let current_reference = index | ((base.sequence as u64) << 48);
    let base_references = entries
        .iter()
        .filter(|e| e.index() == index)
        .map(|e| e.reference)
        .collect::<BTreeSet<_>>();
    if base_references.len() > 1 {
        return Err(invalid("attribute list has conflicting base references"));
    }
    let base_reference = base_references
        .first()
        .copied()
        .unwrap_or(current_reference);
    // Windows increments the FILE sequence when freeing a record; its surviving
    // attribute list still records the previous sequence. Require ONE consistent
    // mode for the base and every extension, so reused/mixed records stay invalid.
    let advanced = base_reference != current_reference;
    let next_sequence = |sequence: u16| sequence.wrapping_add(1).max(1);
    if advanced && (base.in_use || next_sequence((base_reference >> 48) as u16) != base.sequence) {
        return Err(invalid(
            "attribute-list base sequence mismatch (reused record)",
        ));
    }
    let matches_sequence = |current: u16, reference: u64| {
        current
            == if advanced {
                next_sequence((reference >> 48) as u16)
            } else {
                (reference >> 48) as u16
            }
    };
    let in_use = base.in_use;
    let mut records = BTreeMap::from([(index, base)]);
    let mut missing = entries
        .iter()
        .map(Entry::index)
        .filter(|i| *i != index)
        .collect::<BTreeSet<_>>();
    if missing.len() > MAX_EXTENSION_RECORDS
        || (missing.len() + 1) * geometry.record > MAX_RECORD_BYTES
    {
        return Err(Error::Limit("NTFS extension record budget".into()));
    }
    while !missing.is_empty() {
        let prefix;
        let mapping = if let Some(mft) = mft {
            mft
        } else {
            prefix = mft_prefix(&records, geometry)?;
            &prefix
        };
        let mut loaded = Vec::new();
        for &number in &missing {
            let offset = number
                .checked_mul(geometry.record as u64)
                .ok_or_else(|| invalid("extension record offset overflow"))?;
            if offset
                .checked_add(geometry.record as u64)
                .is_none_or(|e| e > mapping.size())
            {
                return Err(invalid("extension record outside MFT"));
            }
            if mft.is_none()
                && let Data::NonResident { runs, .. } = mapping
                && offset + geometry.record as u64 > runs.last().map_or(0, |r| r.logical + r.length)
            {
                continue;
            }
            let extension = parse_record(mapping.read(image, offset, geometry.record)?, geometry)?;
            if entries
                .iter()
                .filter(|e| e.index() == number)
                .any(|e| !matches_sequence(extension.sequence, e.reference))
                || extension.base != base_reference
                || extension.in_use != in_use
                || extension.attributes.iter().any(|a| a.kind == 0x20)
            {
                return Err(invalid(
                    "extension belongs to another record, was reused, or links another list",
                ));
            }
            loaded.push((number, extension));
        }
        if loaded.is_empty() {
            return Err(invalid(
                "MFT bootstrap has unreachable/cyclic extension references",
            ));
        }
        for (number, extension) in loaded {
            missing.remove(&number);
            records.insert(number, extension);
        }
    }
    let listed = entries
        .iter()
        .map(|e| (e.index(), e.kind, e.name.clone(), e.lowest_vcn, e.id))
        .collect::<BTreeSet<_>>();
    let mut attributes = BTreeMap::new();
    for (&number, record) in &records {
        for attr in record.attributes.iter().filter(|a| a.kind != 0x20) {
            let key = (
                number,
                attr.kind,
                attr.name.clone(),
                attr.lowest_vcn,
                attr.id,
            );
            if !listed.contains(&key) || attributes.insert(key, attr).is_some() {
                return Err(invalid(
                    "unlisted or duplicate attribute in referenced record",
                ));
            }
        }
    }
    let mut selected = Vec::new();
    for entry in &entries {
        if !matches_sequence(records[&entry.index()].sequence, entry.reference) {
            return Err(invalid("attribute-list sequence mismatch (reused record)"));
        }
        selected.push(
            *attributes
                .get(&(
                    entry.index(),
                    entry.kind,
                    entry.name.clone(),
                    entry.lowest_vcn,
                    entry.id,
                ))
                .ok_or_else(|| invalid("attribute-list attribute ID/type/name/VCN mismatch"))?,
        );
    }
    let mut streams = BTreeMap::<Option<String>, Vec<&Attribute>>::new();
    for attr in selected.iter().filter(|a| a.kind == 0x80) {
        streams.entry(attr.name.clone()).or_default().push(attr);
    }
    let mut streams = streams
        .values()
        .map(|attrs| merge(attrs, geometry, true))
        .collect::<Result<Vec<_>>>()?;
    // Windows may erase a nonresident stream's size and runlist when freeing
    // an attribute-list file. Its empty sentinel is indistinguishable from a
    // formerly empty stream. Do not publish zero bytes as recovered content.
    if advanced {
        for stream in &mut streams {
            if matches!(&stream.data, Some(Data::NonResident { size: 0, runs, .. }) if runs.is_empty())
            {
                stream.data = None;
                stream.unsupported.push("Freed NTFS attribute-list stream has an empty nonresident descriptor: deletion may have erased its original length and runlist; original contents are unavailable.".into());
            }
        }
    }
    let mut digest = Sha256::new();
    digest.update(b"rescue-ntfs-attribute-list-v1\0");
    digest.update(&list);
    for (&number, record) in &records {
        digest.update(number.to_le_bytes());
        digest.update(record.hash.as_bytes());
    }
    let standard = selected
        .iter()
        .filter(|a| a.kind == 0x10)
        .collect::<Vec<_>>();
    if standard.len() > 1 {
        return Err(invalid("duplicate STANDARD_INFORMATION across extensions"));
    }
    let times = if let Some(attr) = standard.first() {
        if attr.raw[8] != 0 {
            return Err(invalid("nonresident STANDARD_INFORMATION"));
        }
        let offset = u16_at(&attr.raw, 20)? as usize;
        let value = bytes(&attr.raw, offset, u32_at(&attr.raw, 16)? as usize)?;
        Some((
            crate::timestamps::filetime(u64_at(value, 0)?),
            crate::timestamps::filetime(u64_at(value, 8)?),
        ))
    } else {
        None
    };
    let names = records
        .values()
        .flat_map(|r| r.names.iter().cloned())
        .collect::<Vec<_>>();
    let name_record = records.values().find(|r| r.name.is_some()).cloned();
    let mut base = records.remove(&index).unwrap();
    if advanced {
        base.provenance.push("Deleted NTFS sequence increment verified consistently across the base, attribute-list references and every extension.".into());
    }
    base.names = names;
    if base.name.is_none()
        && let Some(named) = name_record
    {
        base.name = named.name;
        base.parent = named.parent;
        base.parent_sequence = named.parent_sequence;
        base.created = named.created;
        base.modified = named.modified;
    }
    if let Some((created, modified)) = times {
        base.created = created;
        base.modified = modified;
    }
    if base.names.iter().any(|n| n.namespace != 2)
        && !base.names.iter().any(|n| {
            n.namespace != 2
                && Some(&n.name) == base.name.as_ref()
                && n.parent == base.parent
                && n.sequence == base.parent_sequence
        })
        && let Some(name) = base.names.iter().find(|n| n.namespace != 2)
    {
        base.name = Some(name.name.clone());
        base.parent = name.parent;
        base.parent_sequence = name.sequence;
    }
    base.streams = streams;
    base.unsupported.clear();
    base.hash = format!("{:x}", digest.finalize());
    Ok(base)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn hostile_lists_are_bounded_and_do_not_panic() {
        let mut seed = 0x65ac_0721u32;
        let mut raw = vec![0u8; 4096];
        for length in 0..4096 {
            for byte in raw.iter_mut().take(length) {
                seed ^= seed << 13;
                seed ^= seed >> 17;
                seed ^= seed << 5;
                *byte = seed as u8;
            }
            let _ = entries(&raw[..length]);
        }
        let mut valid = vec![0u8; 32];
        valid[0..4].copy_from_slice(&0x80u32.to_le_bytes());
        valid[4..6].copy_from_slice(&32u16.to_le_bytes());
        valid[16..24].copy_from_slice(&(7u64 << 48).to_le_bytes());
        assert_eq!(entries(&valid).unwrap().len(), 1);
        for field in [4, 6, 7, 16, 22] {
            for byte in 0..=255 {
                let mut malformed = valid.clone();
                malformed[field] = byte;
                let _ = entries(&malformed);
            }
        }
        let mut over = Vec::new();
        for id in 0..=MAX_LIST_ENTRIES {
            let mut entry = valid.clone();
            entry[24..26].copy_from_slice(&(id as u16).to_le_bytes());
            over.extend(entry);
        }
        assert!(matches!(entries(&over), Err(Error::Limit(_))));
    }
    #[test]
    fn extent_coverage_cannot_wrap_logical_address_space() {
        let geometry = Geometry {
            sector: 512,
            cluster: 512,
            record: 1024,
            mft: 2048,
            volume_length: 1024 * 1024,
        };
        let clusters = u64::MAX / 512;
        let mut raw = vec![0u8; 80];
        raw[8] = 1;
        raw[24..32].copy_from_slice(&(clusters - 1).to_le_bytes());
        raw[32..34].copy_from_slice(&64u16.to_le_bytes());
        raw[48..56].copy_from_slice(&1u64.to_le_bytes());
        raw[56..64].copy_from_slice(&1u64.to_le_bytes());
        raw[64] = 0x08;
        raw[65..73].copy_from_slice(&clusters.to_le_bytes());
        let first = Attribute {
            kind: 0x80,
            id: 0,
            name: None,
            lowest_vcn: 0,
            raw: raw.clone(),
        };
        assert!(merge(&[&first], &geometry, true).is_ok());
        raw[16..24].copy_from_slice(&clusters.to_le_bytes());
        raw[24..32].copy_from_slice(&clusters.to_le_bytes());
        raw[64..].fill(0);
        raw[64] = 1;
        raw[65] = 1;
        let second = Attribute {
            kind: 0x80,
            id: 1,
            name: None,
            lowest_vcn: clusters,
            raw,
        };
        assert!(merge(&[&first, &second], &geometry, true).is_err());
    }
}
