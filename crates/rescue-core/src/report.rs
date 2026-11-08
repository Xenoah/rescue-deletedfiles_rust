//! Offline, escaped human-readable views of an existing recovery report.
use crate::{Error, RecoveryDestination, RecoveryResult, Result, Session};
use cap_std::fs::OpenOptions;
use serde_json::{Value, json};
use std::{
    io::{Read, Write},
    path::Path,
};

pub const MAX_REPORT_BYTES: usize = 16 * 1024 * 1024;

#[derive(Clone, Copy)]
pub enum ReportFormat {
    Html,
    Csv,
}

impl ReportFormat {
    pub fn extension(self) -> &'static str {
        match self {
            Self::Html => "html",
            Self::Csv => "csv",
        }
    }
}

pub fn recovery_report(session: &Session, results: &[RecoveryResult]) -> Value {
    json!({"schema_version":1,"session_id":session.id,"source":session.source,
        "source_info":session.source_info,"filesystem":session.filesystem,
        "source_fingerprint":session.fingerprint,"raid":session.raid,"results":results,
        "integrity_note":"Output hashes do not prove equality with the deleted originals."})
}

pub fn read_recovery_report(path: &Path) -> Result<Value> {
    let metadata = std::fs::metadata(path)?;
    if !metadata.is_file() || metadata.len() > MAX_REPORT_BYTES as u64 {
        return Err(Error::Request(
            "recovery report must be a regular JSON file up to 16 MiB".into(),
        ));
    }
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take((MAX_REPORT_BYTES + 1) as u64)
        .read_to_end(&mut bytes)?;
    if bytes.len() > MAX_REPORT_BYTES {
        return Err(Error::Limit("report exceeds 16 MiB".into()));
    }
    Ok(serde_json::from_slice(&bytes)?)
}

fn escape(text: &str) -> String {
    text.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
        .replace('\'', "&#39;")
}

fn cell(value: &str) -> String {
    // Quote every cell and neutralize formula prefixes, including hidden leading whitespace.
    let dangerous = value.starts_with(['\t', '\r', '\n'])
        || value.trim_start().starts_with(['=', '+', '-', '@']);
    format!(
        "\"{}{}\"",
        if dangerous { "'" } else { "" },
        value.replace('"', "\"\"")
    )
}

fn display(value: &Value) -> String {
    match value {
        Value::Null => String::new(),
        Value::String(s) => s.clone(),
        _ => value.to_string(),
    }
}

pub fn render_recovery_report(report: &Value, format: ReportFormat) -> Result<Vec<u8>> {
    if serde_json::to_vec(report)?.len() > MAX_REPORT_BYTES {
        return Err(Error::Limit("report exceeds 16 MiB".into()));
    }
    if report.get("schema_version").and_then(Value::as_u64) != Some(1) {
        return Err(Error::Request(
            "expected recovery report schema_version 1".into(),
        ));
    }
    let rows = report
        .get("results")
        .and_then(Value::as_array)
        .ok_or_else(|| Error::Request("report must contain a results array".into()))?;
    if rows.len() > 100_000 {
        return Err(Error::Limit("report exceeds 100,000 results".into()));
    }
    for row in rows {
        serde_json::from_value::<RecoveryResult>(row.clone())
            .map_err(|e| Error::Request(format!("invalid recovery result: {e}")))?;
    }
    let text = match format {
        ReportFormat::Csv => {
            let fields = [
                "candidate_id",
                "status",
                "path",
                "archive_entry",
                "bytes_written",
                "sha256",
                "extent_evidence",
                "allocation_status",
                "warnings",
                "error",
                "error_details",
                "naming",
                "missing_ranges",
                "raid_reads",
            ];
            let mut out = String::from("\u{feff}");
            let mut headers = vec![
                "session_id",
                "source",
                "integrity_note",
                "report_metadata_json",
            ];
            headers.extend(fields);
            headers.push("result_json");
            out.push_str(
                &headers
                    .iter()
                    .map(|s| cell(s))
                    .collect::<Vec<_>>()
                    .join(","),
            );
            out.push_str("\r\n");
            let mut metadata = report.clone();
            metadata.as_object_mut().unwrap().remove("results");
            for row in rows {
                let mut values = vec![display(&report["session_id"]),display(&report["source"]),
                    "Saved bytes and their hashes do not prove equality with the deleted originals. Estimated extents remain unverified.".into(),metadata.to_string()];
                values.extend(fields.iter().map(|field| display(&row[*field])));
                values.push(row.to_string());
                out.push_str(&values.iter().map(|s| cell(s)).collect::<Vec<_>>().join(","));
                out.push_str("\r\n");
                if out.len() > MAX_REPORT_BYTES * 8 {
                    return Err(Error::Limit("rendered report exceeds 128 MiB".into()));
                }
            }
            out
        }
        ReportFormat::Html => {
            let saved = rows.iter().filter(|r| r["status"] == "completed").count();
            let estimated = rows
                .iter()
                .filter(|r| r["extent_evidence"] == "estimated_contiguous")
                .count();
            let mut out = String::from(
                "<!doctype html><html lang=\"ja\"><meta charset=\"utf-8\"><meta name=\"viewport\" content=\"width=device-width,initial-scale=1\"><meta http-equiv=\"Content-Security-Policy\" content=\"default-src 'none'; style-src 'unsafe-inline'\"><title>Rescue 復元レポート / Recovery report</title><style>body{font:16px system-ui,sans-serif;background:#f1f5f9;color:#172d44;margin:0;padding:24px}main{max-width:1200px;margin:auto}h1{color:#00686b}header,section{background:white;border:1px solid #cbd5e1;border-radius:12px;padding:20px;margin:16px 0}.note{background:#fff3d7;padding:14px;border-radius:8px}table{border-collapse:collapse;width:100%;min-width:900px;table-layout:fixed}td,th{padding:12px;text-align:left;border-bottom:1px solid #dce4ed;vertical-align:top;overflow-wrap:anywhere}th{background:#e0f2f1}pre{white-space:pre-wrap;overflow-wrap:anywhere;font-size:13px}.scroll{overflow-x:auto}.estimated{color:#8a4b00;font-weight:bold}summary{cursor:pointer}.warnings{white-space:pre-line}@media print{body{background:white;padding:0}section{break-inside:avoid}details{display:block}}</style><main><header><h1>復元レポート / Recovery report</h1>",
            );
            out.push_str(&format!("<p>{} 件 / results · 保存処理完了 / saved: {saved} · 推定 / estimated: {estimated}</p><p>Source: <code>{}</code></p>", rows.len(), escape(&display(&report["source"]))));
            out.push_str("<p class=\"note\">保存完了・出力ハッシュは元ファイルとの一致を証明しません。推定は内容未確認です。<br>Saved bytes and their hashes do not prove equality with deleted originals. Estimated extents remain unverified.</p></header><section><div class=\"scroll\"><table><colgroup><col style=\"width:28%\"><col style=\"width:19%\"><col style=\"width:8%\"><col style=\"width:45%\"></colgroup><thead><tr><th>名前・保存先 / File</th><th>結果 / Result</th><th>バイト / Bytes</th><th>根拠・警告 / Evidence</th></tr></thead><tbody>");
            for row in rows {
                let name = row
                    .pointer("/naming/displayed_name")
                    .filter(|v| v.is_string())
                    .unwrap_or(&row["candidate_id"]);
                let estimate = row["extent_evidence"] == "estimated_contiguous";
                let state = if estimate && row["status"] == "completed" {
                    "保存済み（推定・内容未確認） / Saved (estimated, unverified)".to_string()
                } else {
                    match row["status"].as_str().unwrap_or_default() {
                        "completed" => "保存完了 / Saved".into(),
                        "failed" => "失敗 / Failed".into(),
                        "partial" => "不完全 / Partial".into(),
                        "cancelled" => "取消 / Cancelled".into(),
                        "skipped" => "スキップ / Skipped".into(),
                        _ => display(&row["status"]),
                    }
                };
                let warnings = row["warnings"]
                    .as_array()
                    .unwrap()
                    .iter()
                    .filter_map(Value::as_str)
                    .collect::<Vec<_>>()
                    .join("\n\n");
                out.push_str(&format!("<tr><td><strong>{}</strong><br>{}<br>{}</td><td{}>{}<br>{}</td><td>{}</td><td>{}<p class=\"warnings\">{}</p><details><summary>詳細 / Full result</summary><pre>{}</pre></details></td></tr>",
                    escape(&display(name)),escape(&display(&row["path"])),escape(&display(&row["archive_entry"])),
                    if estimate { " class=\"estimated\"" } else { "" },escape(&state),escape(&display(&row["error"])),escape(&display(&row["bytes_written"])),
                    escape(&display(&row["extent_evidence"])),escape(&warnings),escape(&serde_json::to_string_pretty(row)?)));
                if out.len() > MAX_REPORT_BYTES * 8 {
                    return Err(Error::Limit("rendered report exceeds 128 MiB".into()));
                }
            }
            let mut metadata = report.clone();
            metadata.as_object_mut().unwrap().remove("results");
            out.push_str(&format!("</tbody></table></div></section><section><h2>入力・識別情報 / Source metadata</h2><pre>{}</pre></section></main></html>\n", escape(&serde_json::to_string_pretty(&metadata)?)));
            out
        }
    };
    if text.len() > MAX_REPORT_BYTES * 8 {
        return Err(Error::Limit("rendered report exceeds 128 MiB".into()));
    }
    Ok(text.into_bytes())
}

/// Create a new file under a pinned directory. Never replace an existing path.
pub fn export_recovery_report(report: &Value, format: ReportFormat, output: &Path) -> Result<()> {
    let bytes = render_recovery_report(report, format)?;
    write_new_report(&bytes, output)
}

pub(crate) fn write_new_report(bytes: &[u8], output: &Path) -> Result<()> {
    let name = output
        .file_name()
        .ok_or_else(|| Error::Request("report filename is required".into()))?;
    let parent = output
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    let destination = RecoveryDestination::open_existing(parent)?;
    destination.verify()?;
    let mut file = destination
        .dir
        .open_with(name, OpenOptions::new().write(true).create_new(true))?;
    file.write_all(bytes)?;
    file.sync_all()?;
    Ok(())
}
