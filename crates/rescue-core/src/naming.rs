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

/// Extension of the final path component; strip only an explicitly known ADS suffix.
pub fn file_extension<'a>(name: &'a str, stream_name: Option<&str>) -> &'a str {
    let base = stream_name
        .and_then(|stream| name.strip_suffix(stream))
        .and_then(|base| base.strip_suffix(':'))
        .unwrap_or(name);
    let component = base.rsplit(['/', '\\']).next().unwrap_or(base);
    component
        .rsplit_once('.')
        .filter(|(stem, extension)| !stem.is_empty() && !extension.is_empty())
        .map_or("", |(_, extension)| extension)
}

#[cfg(test)]
mod extension_tests {
    use super::file_extension;
    #[test]
    fn literal_colons_path_components_and_explicit_streams() {
        for (name, stream, expected) in [
            ("../../CON:escape.txt", None, "txt"),
            ("dir.with.dot/plain", None, ""),
            ("dir.with.dot\\file.bin", None, "bin"),
            ("original:colon.txt:stream.dat", Some("stream.dat"), "txt"),
            ("file.txt:stream.dat", None, "dat"),
            (".hidden", None, ""),
            ("file.", None, ""),
        ] {
            assert_eq!(file_extension(name, stream), expected, "{name}");
        }
    }
}
