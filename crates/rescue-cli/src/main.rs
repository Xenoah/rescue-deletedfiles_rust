use clap::{Parser, Subcommand};
use rescue_core::{Error, ScanOptions, Session, recover, save_report, scan_image};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Parser)]
#[command(
    name = "rescue",
    version,
    about = "Read-only NTFS/FAT32/exFAT RAW volume recovery (alpha)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    Scan {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        work_dir: PathBuf,
        #[arg(long, default_value = "quick", value_parser = ["quick"])]
        mode: String,
        #[arg(long)]
        json: bool,
    },
    Results {
        #[arg(long)]
        session: PathBuf,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long, default_value_t = 0)]
        offset: usize,
        #[arg(long)]
        name: Option<String>,
        #[arg(long)]
        json: bool,
    },
    Recover {
        #[arg(long)]
        session: PathBuf,
        #[arg(long, conflicts_with = "all", required_unless_present = "all")]
        id: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Report {
        #[arg(long)]
        session: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Mcp {
        #[arg(long, required = true)]
        stdio: bool,
        #[arg(long, required = true)]
        allow_source: Vec<PathBuf>,
        #[arg(long)]
        work_dir: PathBuf,
        #[arg(long)]
        allow_output_root: Vec<PathBuf>,
    },
}
fn print(value: serde_json::Value, compact: bool) {
    if compact {
        println!("{value}");
    } else {
        println!("{}", serde_json::to_string_pretty(&value).unwrap());
    }
}
fn run() -> Result<i32, Error> {
    let cli = Cli::parse();
    let cancel = Arc::new(AtomicBool::new(false));
    let signal = cancel.clone();
    // MCP clients close stdin for a graceful shutdown. Do not intercept SIGINT
    // with the unrelated cancellation flag used by interactive CLI operations.
    if !matches!(cli.command, Command::Mcp { .. }) {
        ctrlc::set_handler(move || signal.store(true, std::sync::atomic::Ordering::Relaxed))
            .map_err(|e| Error::Request(e.to_string()))?;
    }
    match cli.command {
        Command::Scan {
            source,
            work_dir,
            json: compact,
            ..
        } => {
            let session = scan_image(&source, ScanOptions::default(), &cancel, |p| {
                if !compact {
                    eprintln!(
                        "Records {} / {} ; candidates {}",
                        p.scanned_records, p.total_records, p.candidates
                    );
                }
            })?;
            let path = session.save(&work_dir)?;
            print(
                json!({"event":"scan_finished","schema_version":1,"session":path,
                "session_id":session.id,"candidates":session.candidates.len(),
                "filesystem":session.filesystem,
                "scanned_records":session.scanned_records,"total_records":session.total_records,
                "cancelled":session.cancelled,"truncated":session.truncated,
                "invalid_records":session.invalid_records,"warnings":session.warnings}),
                compact,
            );
            Ok(if session.cancelled {
                130
            } else if session.truncated || session.invalid_records > 0 {
                5
            } else {
                0
            })
        }
        Command::Results {
            session,
            limit,
            offset,
            name,
            json: compact,
        } => {
            if limit == 0 || limit > 1000 {
                return Err(Error::Request("limit must be 1..1000".into()));
            }
            let session = Session::load(&session)?;
            let matching = session
                .candidates
                .iter()
                .filter(|c| {
                    name.as_ref()
                        .is_none_or(|n| c.name.to_lowercase().contains(&n.to_lowercase()))
                })
                .collect::<Vec<_>>();
            print(
                json!({"schema_version":1,"session_id":session.id,"total":matching.len(),
                "filesystem":session.filesystem,
                "offset":offset,"candidates":matching.iter().skip(offset).take(limit).collect::<Vec<_>>()}),
                compact,
            );
            Ok(0)
        }
        Command::Recover {
            session,
            mut id,
            all,
            destination,
            json: compact,
        } => {
            let session = Session::load(&session)?;
            if all {
                id = session
                    .candidates
                    .iter()
                    .filter(|c| c.supported)
                    .map(|c| c.id.clone())
                    .collect();
            }
            let results = recover(&session, &id, &destination, &cancel)?;
            let report = save_report(&destination, &session, &results)?;
            print(
                json!({"event":"recovery_finished","schema_version":1,"report":report,"results":results}),
                compact,
            );
            Ok(if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                130
            } else if results.iter().any(|r| r.status != "completed") {
                5
            } else {
                0
            })
        }
        Command::Report {
            session,
            json: compact,
        } => {
            let session = Session::load(&session)?;
            print(
                json!({"schema_version":1,"session_id":session.id,"source":session.source,
                "filesystem":session.filesystem,
                "source_fingerprint":session.fingerprint,"candidates":session.candidates.len(),
                "supported":session.candidates.iter().filter(|c|c.supported).count(),
                "cancelled":session.cancelled,"truncated":session.truncated,
                "invalid_records":session.invalid_records,"warnings":session.warnings}),
                compact,
            );
            Ok(0)
        }
        Command::Mcp {
            allow_source,
            work_dir,
            allow_output_root,
            ..
        } => {
            rescue_mcp::serve(rescue_mcp::Config {
                sources: allow_source,
                work_dir,
                output_roots: allow_output_root,
            })?;
            Ok(0)
        }
    }
}
fn main() {
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!(
                "{}",
                json!({"error":{"code":error.code(),"message":error.to_string()}})
            );
            match error {
                Error::Request(_) | Error::Json(_) => 2,
                Error::Cancelled => 130,
                _ => 4,
            }
        }
    };
    std::process::exit(code);
}
