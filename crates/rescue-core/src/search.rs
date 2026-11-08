use crate::{Candidate, Error, Progress, Result, ScanOptions, Session};
use serde::{Deserialize, Serialize};
use std::{path::Path, sync::atomic::AtomicBool};

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ScanMethod {
    #[default]
    Quick,
    Carve,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CarveScope {
    #[default]
    Unallocated,
    All,
}
impl CarveScope {
    // Sessions written before this field existed scanned all source bytes.
    pub(crate) fn legacy_all() -> Self {
        Self::All
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSettings {
    pub method: ScanMethod,
    #[serde(default = "CarveScope::legacy_all")]
    pub carve_scope: CarveScope,
    pub extensions: Vec<String>,
    pub path_prefix: String,
    /// Maximum folder levels below the selected path. None has no extra limit.
    pub max_depth: Option<u16>,
    pub range_start: u64,
    pub range_end: Option<u64>,
    /// A specific, independently validated alternative from volume discovery.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub recovered_volume: Option<crate::SourceRegion>,
    #[serde(default)]
    pub fragments: bool,
    /// Opt in to clearly labelled contiguous guesses when every FAT entry is free.
    #[serde(default)]
    pub estimate_fat_contiguous: bool,
    #[serde(default)]
    pub fat_short_name_encoding: crate::FatShortNameEncoding,
}
impl SearchSettings {
    pub fn validate(&self) -> Result<()> {
        if self.fat_short_name_encoding != crate::FatShortNameEncoding::Ascii
            && self.method != ScanMethod::Quick
        {
            return Err(Error::Request(
                "FAT short-name encoding requires a metadata scan".into(),
            ));
        }
        if self.estimate_fat_contiguous && self.method != ScanMethod::Quick {
            return Err(Error::Request(
                "FAT contiguous estimation requires a metadata scan".into(),
            ));
        }
        if self.fragments && self.method != ScanMethod::Quick {
            return Err(Error::Request(
                "MFT remnants require a metadata scan".into(),
            ));
        }
        if self.recovered_volume.is_some() && self.method != ScanMethod::Quick {
            return Err(Error::Request(
                "a recovered volume requires metadata scanning".into(),
            ));
        }
        if self.extensions.len() > 64
            || self.extensions.iter().any(|e| {
                e.len() > 32
                    || e.contains(['/', '\\', ':', '\0'])
                    || e.trim()
                        .trim_start_matches("*.")
                        .trim_start_matches('.')
                        .is_empty()
            })
        {
            return Err(Error::Request(
                "extensions must contain 1..32 characters, at most 64 entries".into(),
            ));
        }
        if self.path_prefix.len() > 4096 || self.max_depth.is_some_and(|d| d > 256) {
            return Err(Error::Request("path/depth search limit".into()));
        }
        if self.method == ScanMethod::Quick && (self.range_start != 0 || self.range_end.is_some()) {
            return Err(Error::Request(
                "byte ranges are available for carving only".into(),
            ));
        }
        if self.method == ScanMethod::Carve
            && (!self.path_prefix.is_empty() || self.max_depth.is_some())
        {
            return Err(Error::Request(
                "folder scope/depth is available for metadata scans only".into(),
            ));
        }
        if self.range_end.is_some_and(|end| end < self.range_start) {
            return Err(Error::Request("range end precedes start".into()));
        }
        Ok(())
    }
    pub fn matches(&self, candidate: &Candidate) -> bool {
        self.matches_volume(candidate, false)
    }
    fn matches_volume(&self, candidate: &Candidate, disk: bool) -> bool {
        std::iter::once(candidate.original_path.as_str())
            .chain(
                candidate
                    .alternate_names
                    .iter()
                    .map(|n| n.original_path.as_str()),
            )
            .any(|path| {
                let path = if disk {
                    path.split_once('/').map_or(path, |(_, p)| p)
                } else {
                    path
                };
                self.matches_path(candidate, path)
            })
    }
    fn matches_path(&self, candidate: &Candidate, path: &str) -> bool {
        let path = path.replace('\\', "/").to_lowercase();
        let prefix = self
            .path_prefix
            .replace('\\', "/")
            .trim_matches('/')
            .to_lowercase();
        let relative = if prefix.is_empty() {
            path.as_str()
        } else if let Some(rest) = path.strip_prefix(&(prefix + "/")) {
            rest
        } else {
            return false;
        };
        if self
            .max_depth
            .is_some_and(|d| relative.matches('/').count() > usize::from(d))
        {
            return false;
        }
        if self.extensions.is_empty() {
            return true;
        }
        let basename = path.rsplit('/').next().unwrap_or(&path);
        let name = if let Some(stream) = &candidate.stream_name {
            basename
                .strip_suffix(&format!(":{}", stream.to_lowercase()))
                .unwrap_or(basename)
        } else {
            basename
        };
        self.extensions.iter().any(|ext| {
            let ext = ext
                .trim()
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .to_lowercase();
            (candidate.id.starts_with("jpeg-")
                && name.ends_with(".jpg")
                && matches!(ext.as_str(), "jpeg" | "jpe"))
                || ext == "*"
                || name.ends_with(&(".".to_owned() + &ext))
        })
    }
}

pub fn scan_with_settings(
    path: &Path,
    mut options: ScanOptions,
    settings: &SearchSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<Session> {
    settings.validate()?;
    options.fragments = settings.fragments;
    options.estimate_fat_contiguous = settings.estimate_fat_contiguous;
    options.fat_short_name_encoding = settings.fat_short_name_encoding;
    let mut session = match settings.method {
        ScanMethod::Quick => {
            if let Some(region) = &settings.recovered_volume {
                crate::scan_discovered_volume(path, region, options, cancel, progress)?
            } else {
                crate::scan_source(path, options, cancel, progress)?
            }
        }
        ScanMethod::Carve => crate::carve::scan_range(path, options, settings, cancel, progress)?,
    };
    let discovered = session.candidates.len();
    let disk = session.filesystem == crate::FileSystem::Disk;
    // P<n>/ is a volume label rather than a folder depth. Aliases refer to the
    // same candidate and must never multiply recovered payloads.
    session
        .candidates
        .retain(|c| settings.matches_volume(c, disk));
    if discovered != session.candidates.len() {
        session.warnings.push(format!("Search filters retained {} of {discovered} discovered candidates; ownership verification still examined metadata across the volume",session.candidates.len()));
    }
    session.search = Some(settings.clone());
    Ok(session)
}
