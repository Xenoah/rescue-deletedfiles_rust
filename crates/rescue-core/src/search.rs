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

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchSettings {
    pub method: ScanMethod,
    pub extensions: Vec<String>,
    pub path_prefix: String,
    /// Maximum folder levels below the selected path. None has no extra limit.
    pub max_depth: Option<u16>,
    pub range_start: u64,
    pub range_end: Option<u64>,
}
impl SearchSettings {
    pub fn validate(&self) -> Result<()> {
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
                "byte ranges are available for PNG carving only".into(),
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
        let path = candidate.original_path.replace('\\', "/").to_lowercase();
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
        let name = if let Some(stream) = &candidate.stream_name {
            candidate
                .name
                .strip_suffix(&format!(":{stream}"))
                .unwrap_or(&candidate.name)
        } else {
            &candidate.name
        }
        .to_lowercase();
        self.extensions.iter().any(|ext| {
            let ext = ext
                .trim()
                .trim_start_matches("*.")
                .trim_start_matches('.')
                .to_lowercase();
            ext == "*" || name.ends_with(&(".".to_owned() + &ext))
        })
    }
}

pub fn scan_with_settings(
    path: &Path,
    options: ScanOptions,
    settings: &SearchSettings,
    cancel: &AtomicBool,
    progress: impl FnMut(Progress),
) -> Result<Session> {
    settings.validate()?;
    let mut session = match settings.method {
        ScanMethod::Quick => crate::scan_image(path, options, cancel, progress)?,
        ScanMethod::Carve => crate::carve::scan_range(
            path,
            options,
            settings.range_start,
            settings.range_end,
            cancel,
            progress,
        )?,
    };
    let discovered = session.candidates.len();
    session.candidates.retain(|c| settings.matches(c));
    if discovered != session.candidates.len() {
        session.warnings.push(format!("Search filters retained {} of {discovered} discovered candidates; ownership verification still examined metadata across the volume",session.candidates.len()));
    }
    session.search = Some(settings.clone());
    Ok(session)
}
