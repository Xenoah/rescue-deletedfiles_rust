//! Naming evidence comes from freshly validated metadata, never saved output paths.
use crate::{Candidate, FileSystem};
use serde::{Deserialize, Serialize};

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameBasis {
    MetadataVerifiedPath,
    MetadataUnverifiedPath,
    Generated,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum NameChangeReason {
    AlternateStream,
    PortableName,
    UnverifiedHierarchy,
    HierarchyLimit,
    DirectoryCollision,
    FileCollision,
    ArchiveCollision,
    PartitionNamespace,
    StreamingFlatOutput,
    UniqueOutputName,
    PartialOutput,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct NameChange {
    pub reason: NameChangeReason,
    pub from: String,
    pub to: String,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveryNaming {
    /// Metadata may retain only part of a name/path; check basis and warnings.
    /// A carved/generated name is never described as an original name.
    pub original_name: Option<String>,
    pub original_path: Option<String>,
    pub displayed_name: String,
    pub basis: NameBasis,
    pub stream_name: Option<String>,
    pub changes: Vec<NameChange>,
}
impl RecoveryNaming {
    pub(crate) fn from_candidate(candidate: &Candidate, filesystem: FileSystem) -> Self {
        let generated = filesystem == FileSystem::Raw;
        Self {
            original_name: (!generated).then(|| candidate.name.clone()),
            original_path: (!generated).then(|| candidate.original_path.clone()),
            displayed_name: candidate.name.clone(),
            basis: if generated {
                NameBasis::Generated
            } else if candidate.path_verified {
                NameBasis::MetadataVerifiedPath
            } else {
                NameBasis::MetadataUnverifiedPath
            },
            stream_name: candidate.stream_name.clone(),
            changes: vec![],
        }
    }
    pub(crate) fn changed(&mut self, reason: NameChangeReason, from: String, to: String) {
        if from != to {
            self.changes.push(NameChange { reason, from, to });
        }
    }
}
