//! Discover stopped, clean Linux MD mirror and parity-array geometry from version 1 superblocks.
use super::{MAX_MEMBERS, RaidConfig, RaidLayout, RaidLevel, RaidMember, invalid, is_raid_path};
use crate::{Error, Result, source::Image};
use serde::Serialize;
use std::{collections::HashSet, path::PathBuf};

const MD_MAGIC: u32 = 0xa92b4efc;
const HEADER: usize = 256;
const SUPERBLOCK: usize = 4096;
const SECTOR: u64 = 512;

#[derive(Serialize)]
pub struct MdDiscovery {
    pub metadata_version: String,
    pub array_uuid: String,
    pub events: u64,
    pub missing_slots: Vec<usize>,
    pub config: RaidConfig,
}

#[derive(PartialEq, Eq)]
struct Geometry {
    version: &'static str,
    uuid: [u8; 16],
    created: u64,
    events: u64,
    level: RaidLevel,
    layout: RaidLayout,
    stripe_bytes: u32,
    component_bytes: u64,
    count: usize,
    roles: Vec<u16>,
}

struct Member {
    geometry: Geometry,
    offset: u64,
    slot: usize,
    device_uuid: [u8; 16],
}

fn u32_at(data: &[u8], offset: usize) -> u32 {
    u32::from_le_bytes(data[offset..offset + 4].try_into().unwrap())
}
fn u64_at(data: &[u8], offset: usize) -> u64 {
    u64::from_le_bytes(data[offset..offset + 8].try_into().unwrap())
}
fn bytes_from_sectors(count: u64) -> Result<u64> {
    count
        .checked_mul(SECTOR)
        .ok_or_else(|| invalid("MD sector count overflow"))
}
fn array_layout(value: u32, level: RaidLevel) -> Result<RaidLayout> {
    use RaidLayout::*;
    if level == RaidLevel::Raid1 || level == RaidLevel::Raid10 {
        // Linux RAID10 encodes near copies in bits 0..7 and far copies in bits 8..15.
        return match (level, value) {
            (RaidLevel::Raid1, 0) | (RaidLevel::Raid10, 0x102) => Ok(Fixed),
            _ => Err(Error::Unsupported(
                "requires a plain mirror or Linux RAID10 near=2, far=1 without offset".into(),
            )),
        };
    }
    let layout = match value {
        0 => LeftAsymmetric,
        1 => RightAsymmetric,
        2 => LeftSymmetric,
        3 => RightSymmetric,
        4 => PqFirst,
        5 => PqLast,
        8 => DdfZeroRestart,
        9 => DdfNRestart,
        10 => DdfNContinue,
        16 => LeftAsymmetric6,
        17 => RightAsymmetric6,
        18 => LeftSymmetric6,
        19 => RightSymmetric6,
        20 => ParityFirst6,
        _ => return Err(invalid("unsupported Linux MD parity layout")),
    };
    if level == RaidLevel::Raid5 && !RaidLayout::RAID5_LAYOUTS.contains(&layout) {
        return Err(invalid("unsupported Linux MD RAID5 layout"));
    }
    Ok(layout)
}

fn checksum(bytes: &[u8]) -> u32 {
    let (words, tail) = bytes.as_chunks::<4>();
    let mut sum = 0u64;
    for (index, &word) in words.iter().enumerate() {
        if index * 4 != 216 {
            sum += u64::from(u32::from_le_bytes(word));
        }
    }
    if let [lo, hi] = tail {
        sum += u64::from(u16::from_le_bytes([*lo, *hi]));
    }
    (sum as u32).wrapping_add((sum >> 32) as u32)
}

impl Member {
    fn read(image: &mut Image) -> Result<Self> {
        if image.length < 16384 || !image.length.is_multiple_of(SECTOR) {
            return Err(invalid(
                "MD member length is too small or not sector aligned",
            ));
        }
        let tail = ((image.length / SECTOR - 16) & !7) * SECTOR;
        let mut located = None;
        for (offset, version) in [(0, "1.1"), (4096, "1.2"), (tail, "1.0")] {
            if u32_at(&image.read(offset, 4)?, 0) == MD_MAGIC {
                if located.is_some() {
                    return Err(invalid("multiple Linux MD superblocks on one member"));
                }
                located = Some((offset, version));
            }
        }
        let (offset, version) =
            located.ok_or_else(|| invalid("no Linux MD version 1 superblock found"))?;
        let head = image.read(offset, HEADER)?;
        if u32_at(&head, 4) != 1 || u64_at(&head, 144) != offset / SECTOR {
            return Err(invalid(
                "MD superblock version or recorded location differs",
            ));
        }
        let max_dev = u32_at(&head, 220) as usize;
        if !(1..=(SUPERBLOCK - HEADER) / 2).contains(&max_dev) {
            return Err(invalid("MD device role table exceeds its superblock"));
        }
        let data = image.read(offset, HEADER + 2 * max_dev)?;
        if head != data[..HEADER] || checksum(&data) != u32_at(&data, 216) {
            return Err(invalid("MD superblock checksum or header differs"));
        }
        if u32_at(&data, 8) != 0 || u32_at(&data, 12) != 0 || u64_at(&data, 208) != u64::MAX {
            return Err(Error::Unsupported("requires clean, fully synchronized MD members without feature flags (bitmap, reshape, recovery, bad blocks, journal or PPL)".into()));
        }
        let level = match u32_at(&data, 72) {
            1 => RaidLevel::Raid1,
            10 => RaidLevel::Raid10,
            5 => RaidLevel::Raid5,
            6 => RaidLevel::Raid6,
            _ => {
                return Err(Error::Unsupported(
                    "Linux MD metadata discovery requires RAID1, RAID5, RAID6 or near-two RAID10"
                        .into(),
                ));
            }
        };
        let count = u32_at(&data, 92) as usize;
        let minimum = match level {
            RaidLevel::Raid1 | RaidLevel::Raid10 => 2,
            RaidLevel::Raid5 => 3,
            _ => 4,
        };
        if !(minimum..=MAX_MEMBERS).contains(&count) {
            return Err(invalid("MD member count exceeds supported geometry"));
        }
        let roles: Vec<_> = data[HEADER..]
            .as_chunks::<2>()
            .0
            .iter()
            .map(|&v| u16::from_le_bytes(v))
            .collect();
        let mut active = HashSet::new();
        for &role in &roles {
            if role < 0xff00 {
                if usize::from(role) >= count || !active.insert(role) {
                    return Err(invalid("invalid or duplicate MD device role"));
                }
            } else if !matches!(role, 0xffff | 0xfffe) {
                return Err(invalid("unsupported MD journal or reserved device role"));
            }
        }
        let number = u32_at(&data, 160) as usize;
        let slot = roles
            .get(number)
            .map(|&role| usize::from(role))
            .filter(|slot| *slot < count)
            .ok_or_else(|| invalid("selected MD device is not an active data or parity member"))?;
        let component_bytes = bytes_from_sectors(u64_at(&data, 80))?;
        let start = bytes_from_sectors(u64_at(&data, 128))?;
        let available = bytes_from_sectors(u64_at(&data, 136))?;
        if component_bytes == 0
            || component_bytes > available
            || start
                .checked_add(available)
                .is_none_or(|end| end > image.length)
            || (start < offset + SUPERBLOCK as u64 && offset < start + component_bytes)
        {
            return Err(invalid(
                "MD component bounds exceed the image or overlap its superblock",
            ));
        }
        let chunk_sectors = u32_at(&data, 88);
        let stripe_bytes = if level == RaidLevel::Raid1 {
            if chunk_sectors != 0 {
                return Err(invalid("unexpected chunk size on a Linux MD mirror"));
            }
            // No on-disk stripe exists for a mirror. Choose a read unit
            // dividing the component exactly, including non-64-KiB-aligned tails.
            1u32 << component_bytes.trailing_zeros().min(16)
        } else {
            u32::try_from(bytes_from_sectors(u64::from(chunk_sectors))?)
                .map_err(|_| invalid("MD stripe size overflow"))?
        };
        let uuid: [u8; 16] = data[16..32].try_into().unwrap();
        let device_uuid: [u8; 16] = data[168..184].try_into().unwrap();
        if uuid == [0; 16] || device_uuid == [0; 16] {
            return Err(invalid("missing Linux MD array or device UUID"));
        }
        Ok(Self {
            geometry: Geometry {
                version,
                uuid,
                created: u64_at(&data, 64),
                events: u64_at(&data, 200),
                level,
                layout: array_layout(u32_at(&data, 76), level)?,
                stripe_bytes,
                component_bytes,
                count,
                roles,
            },
            slot,
            offset: start,
            device_uuid,
        })
    }
}

/// Read only explicitly selected member images. Slots without a selected current member remain missing.
pub fn discover_md_members(paths: &[PathBuf], sector_size: u32) -> Result<MdDiscovery> {
    if !(1..=MAX_MEMBERS).contains(&paths.len()) || !matches!(sector_size, 512 | 4096) {
        return Err(invalid(
            "select 1..16 regular MD member images and a sector size of 512 or 4096",
        ));
    }
    let mut geometry = None;
    let mut selected = Vec::new();
    let mut devices = HashSet::new();
    let mut slots = HashSet::new();
    for path in paths {
        let path = crate::canonical_source_path(path)?;
        if crate::is_device(&path) || is_raid_path(&path) || !path.is_file() {
            return Err(invalid("MD member must be a regular image file"));
        }
        let mut image = Image::open(&path)?;
        if image.has_read_map() && image.sector_size() != sector_size {
            return Err(invalid(
                "MD member read-map sector size differs from the request",
            ));
        }
        let fingerprint = image.fingerprint()?;
        let member = Member::read(&mut image)?;
        if image.fingerprint()? != fingerprint {
            return Err(Error::SourceChanged);
        }
        if geometry.as_ref().is_some_and(|g| *g != member.geometry) {
            return Err(invalid(
                "MD array identity, events, geometry or role table differs between members",
            ));
        }
        if !devices.insert(member.device_uuid) || !slots.insert(member.slot) {
            return Err(invalid("duplicate MD member UUID or role"));
        }
        selected.push((
            member.slot,
            RaidMember {
                path: Some(path),
                offset: member.offset,
                length: member.geometry.component_bytes,
                fingerprint: Some(fingerprint),
            },
        ));
        geometry.get_or_insert(member.geometry);
    }
    let g = geometry.ok_or_else(|| invalid("missing MD members"))?;
    let missing_slots: Vec<_> = (0..g.count).filter(|s| !slots.contains(s)).collect();
    let can_reconstruct = match g.level {
        RaidLevel::Raid1 => !slots.is_empty(),
        RaidLevel::Raid10 => {
            g.count.is_multiple_of(2)
                && (0..g.count)
                    .step_by(2)
                    .all(|i| slots.contains(&i) || slots.contains(&(i + 1)))
        }
        RaidLevel::Raid5 => missing_slots.len() <= 1,
        RaidLevel::Raid6 => missing_slots.len() <= 2,
        _ => false,
    };
    if !can_reconstruct {
        return Err(invalid(
            "too few current members to reconstruct the MD array",
        ));
    }
    let mut members = vec![
        RaidMember {
            path: None,
            offset: 0,
            length: g.component_bytes,
            fingerprint: None
        };
        g.count
    ];
    for (slot, member) in selected {
        members[slot] = member;
    }
    let config = RaidConfig {
        schema_version: 1,
        level: g.level,
        layout: g.layout,
        sector_size,
        stripe_bytes: g.stripe_bytes,
        members,
    };
    config.length()?;
    let array_uuid = g
        .uuid
        .as_chunks::<4>()
        .0
        .iter()
        .map(|w| format!("{:08x}", u32::from_be_bytes(*w)))
        .collect::<Vec<_>>()
        .join(":");
    Ok(MdDiscovery {
        metadata_version: g.version.into(),
        array_uuid,
        events: g.events,
        missing_slots,
        config,
    })
}
