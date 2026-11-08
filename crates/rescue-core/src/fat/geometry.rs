use super::{invalid, u16_at, u32_at, u64_at};
use crate::{Error, FileSystem, Result, source::Image};

pub(super) struct Geometry {
    pub filesystem: FileSystem,
    pub sector: u64,
    pub cluster: u64,
    pub fat_offsets: Vec<u64>,
    pub heap: u64,
    pub clusters: u32,
    pub root: u32,
    pub fixed_root: Option<(u64, u64)>,
    pub warnings: Vec<String>,
}

impl Geometry {
    pub fn open(image: &mut Image) -> Result<Self> {
        let boot = image.boot_read(512)?;
        if boot[510..512] != [0x55, 0xaa] {
            return Err(Error::Unsupported(
                "expected an NTFS/FAT32/exFAT volume at byte 0".into(),
            ));
        }
        if &boot[3..11] == b"EXFAT   " {
            Self::exfat(image, &boot)
        } else if u16_at(&boot, 22)? != 0 {
            Self::fat12_16(image, &boot)
        } else {
            Self::fat32(image, &boot)
        }
    }

    fn fat12_16(image: &mut Image, boot: &[u8]) -> Result<Self> {
        let sector = u64::from(u16_at(boot, 11)?);
        let spc = u64::from(boot[13]);
        let reserved = u64::from(u16_at(boot, 14)?);
        let copies = boot[16];
        let entries = u64::from(u16_at(boot, 17)?);
        let short_total = u64::from(u16_at(boot, 19)?);
        let long_total = u64::from(u32_at(boot, 32)?);
        let fat_sectors = u64::from(u16_at(boot, 22)?);
        if ![512, 1024, 2048, 4096].contains(&sector)
            || !spc.is_power_of_two()
            || spc > 128
            || reserved == 0
            || !(1..=2).contains(&copies)
            || entries == 0
            || fat_sectors == 0
            || (short_total != 0 && long_total != 0 && short_total != long_total)
        {
            return Err(invalid("FAT12/16 BPB geometry"));
        }
        let total = if short_total != 0 {
            short_total
        } else {
            long_total
        };
        let root_sector = reserved + fat_sectors * u64::from(copies);
        let root_sectors = (entries * 32).div_ceil(sector);
        let data_sector = root_sector + root_sectors;
        if data_sector >= total || total * sector > image.length {
            return Err(invalid("truncated FAT12/16 volume or overlapping regions"));
        }
        let clusters = (total - data_sector) / spc;
        let (filesystem, fat_bytes) = match clusters {
            1..4085 => (FileSystem::Fat12, ((clusters + 2) * 3).div_ceil(2)),
            4085..65525 => (FileSystem::Fat16, (clusters + 2) * 2),
            _ => return Err(invalid("FAT12/16 cluster count")),
        };
        if fat_bytes > fat_sectors * sector {
            return Err(invalid("FAT12/16 allocation table capacity"));
        }
        Ok(Self {
            filesystem,
            sector,
            cluster: sector * spc,
            fat_offsets: (0..copies)
                .map(|n| (reserved + u64::from(n) * fat_sectors) * sector)
                .collect(),
            heap: data_sector * sector,
            clusters: clusters as u32,
            root: 0,
            fixed_root: Some((root_sector * sector, entries * 32)),
            warnings: Vec::new(),
        })
    }

    fn fat32(image: &mut Image, boot: &[u8]) -> Result<Self> {
        let sector = u16_at(boot, 11)? as u64;
        let spc = boot[13] as u64;
        let reserved = u16_at(boot, 14)? as u64;
        let copies = boot[16];
        let fat_sectors = u32_at(boot, 36)? as u64;
        let total = u32_at(boot, 32)? as u64;
        if ![512, 1024, 2048, 4096].contains(&sector)
            || !spc.is_power_of_two()
            || spc > 128
            || reserved == 0
            || !(1..=2).contains(&copies)
            || fat_sectors == 0
            || u16_at(boot, 17)? != 0
            || u16_at(boot, 19)? != 0
            || u16_at(boot, 22)? != 0
        {
            return Err(Error::Unsupported("invalid FAT32 BPB geometry".into()));
        }
        let data_sector = reserved + fat_sectors * u64::from(copies);
        if data_sector >= total || total * sector > image.length {
            return Err(invalid("truncated FAT32 volume or overlapping regions"));
        }
        let clusters = (total - data_sector) / spc;
        if !(65525..=0x0fff_fff5).contains(&clusters) || (clusters + 2) * 4 > fat_sectors * sector {
            return Err(invalid("FAT32 cluster count or FAT capacity"));
        }
        let flags = u16_at(boot, 40)?;
        if u16_at(boot, 42)? != 0 || flags & !0x8f != 0 {
            return Err(Error::Unsupported("FAT32 version/extended flags".into()));
        }
        let mut warnings = Vec::new();
        if image.uses_backup_boot() {
            warnings.push(
                "Using a selected FAT32 backup boot copy; the primary was not repaired".into(),
            );
        }
        let fat_offsets = if flags & 0x80 == 0 {
            (0..copies)
                .map(|n| (reserved + u64::from(n) * fat_sectors) * sector)
                .collect()
        } else {
            let active = (flags & 15) as u8;
            if active >= copies {
                return Err(invalid("active FAT index"));
            }
            warnings.push(format!("FAT mirroring disabled; using active FAT {active}, inactive copies are not authoritative"));
            vec![(reserved + u64::from(active) * fat_sectors) * sector]
        };
        let backup_sector = u16_at(boot, 50)? as u64;
        if backup_sector == 0 || backup_sector == 0xffff {
            warnings.push("FAT32 backup boot sector is not configured".into());
        } else {
            if backup_sector >= reserved {
                return Err(invalid("backup boot outside reserved area"));
            }
            let backup = image.read(backup_sector * sector, 512)?;
            if backup[510..512] != [0x55, 0xaa] || backup[11..64] != boot[11..64] {
                return Err(invalid("FAT32 primary and backup BPB disagree"));
            }
        }
        let geometry = Self {
            filesystem: FileSystem::Fat32,
            sector,
            cluster: sector * spc,
            fat_offsets,
            heap: data_sector * sector,
            clusters: clusters as u32,
            root: u32_at(boot, 44)?,
            fixed_root: None,
            warnings,
        };
        geometry.address(geometry.root)?;
        Ok(geometry)
    }

    fn exfat(image: &mut Image, boot: &[u8]) -> Result<Self> {
        let shift = boot[108];
        let cluster_shift = boot[109];
        if !(9..=12).contains(&shift)
            || cluster_shift > 25 - shift
            || boot[11..64].iter().any(|b| *b != 0)
        {
            return Err(invalid("exFAT sector/cluster geometry or reserved BPB"));
        }
        if u16_at(boot, 104)? != 0x100 || boot[110] != 1 {
            return Err(Error::Unsupported(
                "exFAT revision or dual-FAT TexFAT".into(),
            ));
        }
        let sector = 1u64 << shift;
        let cluster = sector << cluster_shift;
        let primary = image.boot_read((12 * sector) as usize)?;
        let backup = image.read(12 * sector, (12 * sector) as usize)?;
        check_boot(&primary, sector as usize)?;
        check_boot(&backup, sector as usize)?;
        if backup[64..106] != boot[64..106] || backup[108..112] != boot[108..112] {
            return Err(invalid("exFAT primary and backup geometry disagree"));
        }
        let total = u64_at(boot, 72)?;
        let fat = u32_at(boot, 80)? as u64;
        let fat_sectors = u32_at(boot, 84)? as u64;
        let heap = u32_at(boot, 88)? as u64;
        let clusters = u32_at(boot, 92)?;
        if total.checked_mul(sector).is_none_or(|n| n > image.length)
            || fat < 24
            || fat_sectors == 0
            || fat + fat_sectors > heap
            || heap >= total
            || clusters == 0
            || clusters > 0xffff_fff5
            || (u64::from(clusters) + 2) * 4 > fat_sectors * sector
            || u64::from(clusters) * (1u64 << cluster_shift) > total - heap
        {
            return Err(invalid("exFAT volume/FAT/cluster-heap bounds"));
        }
        let flags = u16_at(boot, 106)?;
        if flags & !0x0e != 0 {
            return Err(invalid("exFAT volume flags"));
        }
        let mut warnings = Vec::new();
        if image.uses_backup_boot() {
            warnings.push(
                "Using a checksum-validated exFAT backup boot region; the primary was not repaired"
                    .into(),
            );
        }
        if flags & 6 != 0 {
            warnings.push(
                "exFAT volume is marked dirty or media-failure; metadata may be inconsistent"
                    .into(),
            );
        }
        let geometry = Self {
            filesystem: FileSystem::Exfat,
            sector,
            cluster,
            fat_offsets: vec![fat * sector],
            heap: heap * sector,
            clusters,
            root: u32_at(boot, 96)?,
            fixed_root: None,
            warnings,
        };
        geometry.address(geometry.root)?;
        Ok(geometry)
    }

    pub fn address(&self, cluster: u32) -> Result<u64> {
        if cluster < 2 || u64::from(cluster) >= u64::from(self.clusters) + 2 {
            return Err(invalid("cluster outside data heap"));
        }
        Ok(self.heap + u64::from(cluster - 2) * self.cluster)
    }
}

fn check_boot(region: &[u8], sector: usize) -> Result<()> {
    if &region[3..11] != b"EXFAT   " || region[510..512] != [0x55, 0xaa] {
        return Err(invalid("exFAT boot signature"));
    }
    for index in 1..=8 {
        if u32_at(region, (index + 1) * sector - 4)? != 0xaa55_0000 {
            return Err(invalid("exFAT extended boot signature"));
        }
    }
    let mut checksum = 0u32;
    for (index, byte) in region[..11 * sector].iter().enumerate() {
        if ![106, 107, 112].contains(&index) {
            checksum = checksum.rotate_right(1).wrapping_add(u32::from(*byte));
        }
    }
    for encoded in region[11 * sector..12 * sector].as_chunks::<4>().0 {
        if u32::from_le_bytes(*encoded) != checksum {
            return Err(invalid("exFAT boot checksum mismatch"));
        }
    }
    Ok(())
}
