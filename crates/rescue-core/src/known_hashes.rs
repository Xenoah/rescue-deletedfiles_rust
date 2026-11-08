//! Compare hashes recorded during recovery with an explicitly supplied known list.
//! Manifest labels are opaque text: never opened, followed, or used to rename files.
use crate::{Error, RecoveryResult, Result, report::MAX_REPORT_BYTES};
use serde::{Deserialize, Serialize};
use serde_json::Value;
use sha2::{Digest, Sha256};
use std::{
    collections::{BTreeMap, BTreeSet},
    io::Read,
    path::Path,
};

pub const MAX_KNOWN_HASH_BYTES: usize = 4 * 1024 * 1024;
const MAX_ENTRIES: usize = 20_000;
const MAX_COMPARISON_BYTES: usize = 32 * 1024 * 1024;

#[derive(Clone, Debug, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum KnownHashOutcome {
    Matched,
    NotListed,
    NotComparable,
}
#[derive(Clone, Debug, Default, Serialize, Deserialize)]
pub struct KnownHashCounts {
    pub matched: usize,
    pub not_listed: usize,
    pub not_comparable: usize,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnownHashRow {
    pub candidate_id: String,
    pub displayed_name: String,
    pub outcome: KnownHashOutcome,
    pub reason: Option<String>,
    pub recorded_sha256: Option<String>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct KnownHashComparison {
    pub schema_version: u32,
    pub kind: String,
    pub comparison_basis: String,
    pub integrity_note: String,
    pub manifest_sha256: String,
    pub manifest_entries: usize,
    pub recovery_report_json_sha256: String,
    pub counts: KnownHashCounts,
    /// SHA-256 -> zero or more labels. Multiple labels do not establish a unique original name.
    pub known_contents: BTreeMap<String, Vec<String>>,
    pub results: Vec<KnownHashRow>,
    /// Preserve every original result, warning, missing range and naming decision.
    pub recovery_report: Value,
}

fn request(message: impl Into<String>) -> Error {
    Error::Request(message.into())
}
fn digest(value: &str) -> Option<String> {
    (value.len() == 64 && value.bytes().all(|b| b.is_ascii_hexdigit()))
        .then(|| value.to_ascii_lowercase())
}

pub fn read_known_hashes(path: &Path) -> Result<Vec<u8>> {
    let file = std::fs::File::open(path)?;
    let metadata = file.metadata()?;
    if !metadata.is_file() || metadata.len() > MAX_KNOWN_HASH_BYTES as u64 {
        return Err(request(
            "known SHA-256 list must be a regular UTF-8 file up to 4 MiB",
        ));
    }
    let mut bytes = Vec::new();
    file.take((MAX_KNOWN_HASH_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_KNOWN_HASH_BYTES {
        return Err(Error::Limit("known SHA-256 list exceeds 4 MiB".into()));
    }
    Ok(bytes)
}

fn parse(bytes: &[u8]) -> Result<(BTreeMap<String, Vec<String>>, usize)> {
    if bytes.len() > MAX_KNOWN_HASH_BYTES {
        return Err(Error::Limit("known SHA-256 list exceeds 4 MiB".into()));
    }
    let text =
        std::str::from_utf8(bytes).map_err(|_| request("known SHA-256 list must be UTF-8"))?;
    let text = text.strip_prefix('\u{feff}').unwrap_or(text);
    let mut contents: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut labels = BTreeMap::new();
    let mut entries = BTreeSet::new();
    for (number, line) in text.lines().enumerate() {
        let bad = |why: &str| request(format!("known SHA-256 list line {}: {why}", number + 1));
        if line.is_empty() {
            continue;
        }
        let hash = line
            .get(..64)
            .and_then(digest)
            .ok_or_else(|| bad("expected 64 hexadecimal SHA-256 digits"))?;
        let label = if line.len() == 64 {
            None
        } else {
            if !matches!(line.as_bytes().get(64..66), Some(b"  " | b" *")) {
                return Err(bad(
                    "expected two spaces or a space and '*' before the label; escaped checksum lines are not supported",
                ));
            }
            let label = &line[66..];
            if label.is_empty() || label.len() > 4096 || label.chars().any(char::is_control) {
                return Err(bad(
                    "label must contain 1..4096 UTF-8 bytes without control characters",
                ));
            }
            if let Some(previous) = labels.insert(label.to_owned(), hash.clone())
                && previous != hash
            {
                return Err(bad("one label has conflicting hashes"));
            }
            Some(label.to_owned())
        };
        if !entries.insert((hash.clone(), label.clone())) {
            return Err(bad("duplicate checksum entry"));
        }
        if entries.len() > MAX_ENTRIES {
            return Err(Error::Limit(
                "known SHA-256 list exceeds 20,000 entries".into(),
            ));
        }
        let names = contents.entry(hash).or_default();
        if let Some(label) = label {
            names.push(label);
        }
    }
    if entries.is_empty() {
        return Err(request("known SHA-256 list is empty"));
    }
    Ok((contents, entries.len()))
}

/// Compare recorded output hashes, without reading paths mentioned by either input.
/// A match confirms equality of the recorded digests, not the current disk contents,
/// manifest authenticity, original path, or the reliability of an external report.
pub fn compare_known_hashes(report: &Value, manifest: &[u8]) -> Result<KnownHashComparison> {
    let report_bytes = serde_json::to_vec(report)?;
    if report_bytes.len() > MAX_REPORT_BYTES {
        return Err(Error::Limit("recovery report exceeds 16 MiB".into()));
    }
    if report.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(request("expected recovery report schema_version 1"));
    }
    let rows = report
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| request("report must contain a results array"))?;
    if rows.len() > 100_000 {
        return Err(Error::Limit("report exceeds 100,000 results".into()));
    }
    let (known_contents, manifest_entries) = parse(manifest)?;
    let mut counts = KnownHashCounts::default();
    let mut results = Vec::with_capacity(rows.len());
    let mut ids = BTreeSet::new();
    for value in rows {
        let row: RecoveryResult = serde_json::from_value(value.clone())
            .map_err(|e| request(format!("invalid recovery result: {e}")))?;
        if row.candidate_id.is_empty() || !ids.insert(row.candidate_id.clone()) {
            return Err(request("recovery result IDs must be nonempty and unique"));
        }
        let recorded_sha256 = row
            .sha256
            .as_deref()
            .map(|hash| digest(hash).ok_or_else(|| request("invalid recorded SHA-256")))
            .transpose()?;
        let reason = if row.status != "completed" {
            Some("recovery_not_completed")
        } else if row.error.is_some()
            || row.error_details.is_some()
            || !row.missing_ranges.is_empty()
        {
            Some("recovery_has_errors_or_missing_ranges")
        } else if recorded_sha256.is_none() {
            Some("no_recorded_sha256")
        } else {
            None
        };
        let outcome = if reason.is_some() {
            counts.not_comparable += 1;
            KnownHashOutcome::NotComparable
        } else if known_contents.contains_key(recorded_sha256.as_ref().unwrap()) {
            counts.matched += 1;
            KnownHashOutcome::Matched
        } else {
            counts.not_listed += 1;
            KnownHashOutcome::NotListed
        };
        results.push(KnownHashRow {
            displayed_name: row
                .naming
                .as_ref()
                .map_or_else(|| row.candidate_id.clone(), |n| n.displayed_name.clone()),
            candidate_id: row.candidate_id,
            outcome,
            reason: reason.map(str::to_owned),
            recorded_sha256,
        });
    }
    let comparison = KnownHashComparison {
        schema_version: 1, kind: "known_sha256_comparison".into(),
        comparison_basis: "sha256_recorded_when_recovery_wrote_output".into(),
        integrity_note: "Compares recorded output digests with the supplied list. Output files are not re-read. A match does not authenticate the list/report, establish an original path, or remove recovery uncertainty. Not listed does not mean corrupt.".into(),
        manifest_sha256: format!("{:x}", Sha256::digest(manifest)), manifest_entries,
        recovery_report_json_sha256: format!("{:x}", Sha256::digest(&report_bytes)),
        counts, known_contents, results, recovery_report: report.clone(),
    };
    bounded_bytes(&comparison)?;
    Ok(comparison)
}
fn bounded_bytes(comparison: &KnownHashComparison) -> Result<Vec<u8>> {
    let bytes = serde_json::to_vec_pretty(comparison)?;
    if bytes.len() > MAX_COMPARISON_BYTES {
        return Err(Error::Limit("known hash comparison exceeds 32 MiB".into()));
    }
    Ok(bytes)
}
/// Save a separate record; never overwrite the recovery report, list, or any existing file.
pub fn save_known_hash_comparison(comparison: &KnownHashComparison, output: &Path) -> Result<()> {
    crate::report::write_new_report(&bounded_bytes(comparison)?, output)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn checksum_markers_unicode_and_duplicate_content() {
        let hash = "AB".repeat(32);
        let text = format!(
            "\u{feff}{hash}  日本/photo.jpg\r\n{hash} *another name.jpg\r\n{}\n",
            "01".repeat(32)
        );
        let (map, count) = parse(text.as_bytes()).unwrap();
        assert_eq!(count, 3);
        assert_eq!(
            map[&hash.to_lowercase()],
            vec!["日本/photo.jpg", "another name.jpg"]
        );
        assert!(map[&"01".repeat(32)].is_empty());
    }
    #[test]
    fn ambiguous_and_malformed_manifests_are_rejected() {
        let hash = "ab".repeat(32);
        for text in [
            String::new(),
            format!("{hash} file"),
            format!("{hash}  a\n{hash}  a"),
            format!("{hash}  a\n{}  a", "cd".repeat(32)),
            format!("\\{hash}  escaped\\nname"),
            format!("{hash}  tab\tname"),
            format!("{hash}  {}", "a".repeat(4097)),
        ] {
            assert!(parse(text.as_bytes()).is_err(), "{text}");
        }
        assert!(parse(&[0xff]).is_err());
        assert!(parse(&vec![b' '; MAX_KNOWN_HASH_BYTES + 1]).is_err());
        assert!(parse(format!("{hash}  ../../opaque-label").as_bytes()).is_ok());
    }
    #[test]
    fn partial_and_missing_hash_rows_never_become_matches() {
        use serde_json::json;
        let hash = "ab".repeat(32);
        let row = |id, status, hash: Option<&str>| json!({"candidate_id":id,"status":status,"path":null,"bytes_written":12,"sha256":hash,"error":null,"warnings":["ESTIMATED_CONTIGUOUS: unverified"],"extent_evidence":"estimated_contiguous","allocation_status":"unallocated"});
        let report = json!({"schema_version":1,"results":[row("complete","completed",Some(&hash)),row("partial","partial",Some(&hash)),row("missing","completed",None),row("unlisted","completed",Some(&"cd".repeat(32)))]});
        let result =
            compare_known_hashes(&report, format!("{hash}  original.txt").as_bytes()).unwrap();
        assert_eq!(
            (
                result.counts.matched,
                result.counts.not_listed,
                result.counts.not_comparable
            ),
            (1, 1, 2)
        );
        assert_eq!(result.recovery_report, report);
    }
}
