//! On-disk Btrfs checksum encodings for superblocks, tree blocks and file sectors.
use crate::{Error, Result};
use blake2::{Blake2b, digest::consts::U32};
use sha2::{Digest, Sha256};
use twox_hash::XxHash64;

#[derive(Clone, Copy)]
pub(super) enum Checksum {
    Crc32c,
    XxHash64,
    Sha256,
    Blake2b256,
}
impl Checksum {
    pub fn from_code(code: u16) -> Result<Self> {
        match code {
            0 => Ok(Self::Crc32c),
            1 => Ok(Self::XxHash64),
            2 => Ok(Self::Sha256),
            3 => Ok(Self::Blake2b256),
            _ => Err(Error::Unsupported(format!(
                "unknown Btrfs checksum type {code}"
            ))),
        }
    }
    pub fn size(self) -> usize {
        match self {
            Self::Crc32c => 4,
            Self::XxHash64 => 8,
            Self::Sha256 | Self::Blake2b256 => 32,
        }
    }
    pub fn name(self) -> &'static str {
        match self {
            Self::Crc32c => "crc32c",
            Self::XxHash64 => "xxhash",
            Self::Sha256 => "sha256",
            Self::Blake2b256 => "blake2",
        }
    }
    pub fn matches(self, data: &[u8], expected: &[u8]) -> bool {
        match self {
            Self::Crc32c => crc32c(data).to_le_bytes() == expected,
            Self::XxHash64 => XxHash64::oneshot(0, data).to_le_bytes() == expected,
            Self::Sha256 => Sha256::digest(data).as_slice() == expected,
            Self::Blake2b256 => Blake2b::<U32>::digest(data).as_slice() == expected,
        }
    }
    pub fn block_matches(self, block: &[u8]) -> bool {
        self.matches(&block[32..], &block[..self.size()])
    }
}

const fn crc_table() -> [u32; 256] {
    let mut a = [0; 256];
    let mut i = 0;
    while i < 256 {
        let mut v = i as u32;
        let mut j = 0;
        while j < 8 {
            v = (v >> 1) ^ if v & 1 != 0 { 0x82f63b78 } else { 0 };
            j += 1;
        }
        a[i] = v;
        i += 1;
    }
    a
}
const CRC: [u32; 256] = crc_table();
fn crc32c(data: &[u8]) -> u32 {
    let mut c = u32::MAX;
    for &b in data {
        c = (c >> 8) ^ CRC[usize::from(c as u8 ^ b)];
    }
    !c
}
