//! Opaque source references are independent of display names and drive letters.
//! They identify a local OS object or an acquisition/RAID description, not the
//! authenticity or complete contents of a disk. Existing revalidation remains
//! mandatory before reads and writes.
use crate::{AcquisitionIdentity, ReadMapSummary};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SourceKind {
    Device,
    Image,
    Acquisition,
    Raid,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceReadMap {
    pub original_source: AcquisitionIdentity,
    pub summary: ReadMapSummary,
    pub sha256: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct SourceInfo {
    pub id: String,
    pub kind: SourceKind,
    pub bytes: u64,
    pub sector_size: u32,
    /// Absolute byte window available to this session/view of the source.
    pub offset: u64,
    pub length: u64,
    /// Local OS object identity, sealed acquisition UUID, or RAID evidence ID.
    /// Never a replacement for fingerprints, range or metadata validation.
    pub identity: String,
    pub read_map: Option<SourceReadMap>,
}

impl SourceInfo {
    pub(crate) fn identifier(kind: SourceKind, identity: &str) -> String {
        let mut hash = Sha256::new();
        hash.update(b"rescue-source-id-v1\0");
        hash.update(match kind {
            SourceKind::Device => b"device\0".as_slice(),
            SourceKind::Image => b"image\0".as_slice(),
            SourceKind::Acquisition => b"acquisition\0".as_slice(),
            SourceKind::Raid => b"raid\0".as_slice(),
        });
        hash.update(identity.as_bytes());
        format!("src-{:x}", hash.finalize())
    }

    pub(crate) fn same_object(&self, other: &Self) -> bool {
        self.id == other.id
            && self.kind == other.kind
            && self.bytes == other.bytes
            && self.sector_size == other.sector_size
            && self.identity == other.identity
            && self.read_map == other.read_map
    }
}

/// Public candidate rows carry their session scope without duplicating it in
/// every on-disk index row. IDs supplied to restore still require that session.
#[derive(Serialize)]
pub struct ScopedCandidate<'a> {
    pub session_id: &'a str,
    #[serde(flatten)]
    pub candidate: &'a crate::Candidate,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct CandidateReference {
    pub session_id: String,
    pub id: String,
}

impl crate::Candidate {
    pub fn in_session<'a>(&'a self, session_id: &'a str) -> ScopedCandidate<'a> {
        ScopedCandidate {
            session_id,
            candidate: self,
        }
    }
}
