use clap::{Parser, Subcommand};
use rescue_core::{
    CollisionPolicy, Error, RestoreOptions, RestoreTarget, ScanMethod, ScanOptions, SearchSettings,
    Session, VolumeRoot, recover, recover_zip, restore_files, restore_zip, save_report,
    save_restore_report, scan_with_settings,
};
use serde_json::json;
use std::{
    path::PathBuf,
    sync::{Arc, atomic::AtomicBool},
};

#[derive(Parser)]
#[command(
    name = "rescue",
    version,
    about = "Read-only NTFS/FAT32/exFAT disk and volume recovery (alpha)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Validate that output and source use different physical disks, without writing.
    CheckDestination {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// List native disks and volumes (read-only inventory).
    Sources {
        #[arg(long)]
        json: bool,
    },
    /// Inspect a disk partition table or a volume without scanning deleted files.
    Inspect {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Scan {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        work_dir: PathBuf,
        #[arg(long, default_value = "quick", value_parser = ["quick", "carve"])]
        mode: String,
        #[arg(long = "extension", value_delimiter = ',')]
        extensions: Vec<String>,
        #[arg(long, default_value = "")]
        path_prefix: String,
        #[arg(long)]
        max_depth: Option<u16>,
        #[arg(long, default_value_t = 0)]
        range_start: u64,
        #[arg(long)]
        range_end: Option<u64>,
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
        destination: Option<PathBuf>,
        /// Restore verified original paths on the mounted source volume.
        #[arg(long, conflicts_with_all = ["destination", "archive", "stream"])]
        original: bool,
        /// Override mounted volume root: PATH, or PARTITION=PATH for a disk scan.
        #[arg(long, requires = "original")]
        volume_root: Vec<String>,
        #[arg(long, default_value = "keep-both", value_parser = ["keep-both", "skip"])]
        collision: String,
        /// Legacy streaming into a separate physical disk, without the 512 MiB batch limit.
        #[arg(long)]
        stream: bool,
        #[arg(
            long,
            conflicts_with = "destination",
            required_unless_present_any = ["destination", "original"]
        )]
        archive: Option<PathBuf>,
        #[arg(long)]
        work_dir: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Scan and recover in memory, without saving a session on the source disk.
    Restore {
        #[arg(long)]
        source: PathBuf,
        #[arg(
            long,
            required_unless_present = "original",
            conflicts_with = "original"
        )]
        destination: Option<PathBuf>,
        #[arg(long)]
        original: bool,
        #[arg(long, requires = "original")]
        volume_root: Vec<String>,
        /// Exact filename (case-insensitive); repeat to select multiple names.
        #[arg(long, required_unless_present = "all", conflicts_with = "all")]
        name: Vec<String>,
        #[arg(long)]
        all: bool,
        #[arg(long, default_value = "keep-both", value_parser = ["keep-both", "skip"])]
        collision: String,
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
fn restore_options(
    original: bool,
    destination: Option<PathBuf>,
    mappings: Vec<String>,
    collision: &str,
) -> Result<RestoreOptions, Error> {
    let target = if original {
        let roots = mappings
            .into_iter()
            .map(|value| {
                let (partition, path) = if let Some((n, path)) = value.split_once('=') {
                    (
                        Some(n.parse::<u32>().map_err(|_| {
                            Error::Request("volume root must be PATH or PARTITION=PATH".into())
                        })?),
                        PathBuf::from(path),
                    )
                } else {
                    (None, PathBuf::from(value))
                };
                if path.as_os_str().is_empty() {
                    return Err(Error::Request("empty volume root".into()));
                }
                Ok(VolumeRoot { partition, path })
            })
            .collect::<Result<Vec<_>, Error>>()?;
        RestoreTarget::Original(roots)
    } else {
        RestoreTarget::Directory(
            destination
                .ok_or_else(|| Error::Request("choose a destination or --original".into()))?,
        )
    };
    Ok(RestoreOptions {
        target,
        collision: if collision == "skip" {
            CollisionPolicy::Skip
        } else {
            CollisionPolicy::KeepBoth
        },
    })
}
fn recovery_code(results: &[rescue_core::RecoveryResult], cancel: &AtomicBool) -> i32 {
    if cancel.load(std::sync::atomic::Ordering::Relaxed) {
        130
    } else if results
        .iter()
        .any(|r| !matches!(r.status.as_str(), "completed" | "skipped"))
    {
        5
    } else {
        0
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
        Command::CheckDestination {
            source,
            destination,
            json: compact,
        } => {
            rescue_core::guard_output_path(&source, &destination)?;
            print(
                json!({"safe":true,"source":source,"destination":destination}),
                compact,
            );
            Ok(0)
        }
        Command::Sources { json: compact } => {
            print(
                json!({"schema_version":1,"sources":rescue_core::list_disks()?}),
                compact,
            );
            Ok(0)
        }
        Command::Inspect {
            source,
            json: compact,
        } => {
            print(
                json!({"schema_version":1,"layout":rescue_core::inspect_source(&source)?}),
                compact,
            );
            Ok(0)
        }
        Command::Scan {
            source,
            work_dir,
            json: compact,
            mode,
            extensions,
            path_prefix,
            max_depth,
            range_start,
            range_end,
        } => {
            rescue_core::guard_output_path(&source, &work_dir)?;
            let settings = SearchSettings {
                method: if mode == "carve" {
                    ScanMethod::Carve
                } else {
                    ScanMethod::Quick
                },
                extensions,
                path_prefix,
                max_depth,
                range_start,
                range_end,
            };
            let session = scan_with_settings(
                &source,
                ScanOptions::default(),
                &settings,
                &cancel,
                |p: rescue_core::Progress| {
                    if !compact {
                        eprintln!(
                            "{:?} {} / {} ; candidates {}",
                            p.unit, p.scanned_records, p.total_records, p.candidates
                        );
                    }
                },
            )?;
            let path = session.save(&work_dir)?;
            print(
                json!({"event":"scan_finished","schema_version":1,"session":path,
                "session_id":session.id,"candidates":session.candidates.len(),
                "filesystem":session.filesystem,
                "progress_unit":if mode == "carve" { "bytes" } else { "records" },
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
            original,
            volume_root,
            collision,
            stream,
            archive,
            work_dir,
            json: compact,
        } => {
            let archive_work = work_dir.unwrap_or_else(|| {
                session
                    .parent()
                    .unwrap_or_else(|| std::path::Path::new("."))
                    .to_owned()
            });
            let session = Session::load(&session)?;
            if all {
                id = session
                    .candidates
                    .iter()
                    .filter(|c| c.supported)
                    .map(|c| c.id.clone())
                    .collect();
            }
            if let Some(archive) = archive {
                let result = if stream {
                    recover_zip(&session, &id, &archive_work, &archive, &cancel)?
                } else {
                    restore_zip(
                        &session,
                        &id,
                        &archive,
                        if collision == "skip" {
                            CollisionPolicy::Skip
                        } else {
                            CollisionPolicy::KeepBoth
                        },
                        &cancel,
                    )?
                };
                let code = if result.status == "cancelled" {
                    130
                } else if matches!(result.status.as_str(), "completed" | "skipped") {
                    0
                } else {
                    5
                };
                print(
                    json!({"event":"zip_recovery_finished","schema_version":1,"result":result}),
                    compact,
                );
                return Ok(code);
            }
            let options = restore_options(original, destination.clone(), volume_root, &collision)?;
            let results = if stream {
                recover(&session, &id, destination.as_ref().unwrap(), &cancel)?
            } else {
                restore_files(&session, &id, &options, &cancel)?
            };
            let report = destination
                .as_ref()
                .map(|path| {
                    if stream {
                        save_report(path, &session, &results)
                    } else {
                        save_restore_report(path, &session, &results)
                    }
                })
                .transpose()?;
            print(
                json!({"event":"recovery_finished","schema_version":1,"report":report,"results":results}),
                compact,
            );
            Ok(recovery_code(&results, &cancel))
        }
        Command::Restore {
            source,
            destination,
            original,
            volume_root,
            name,
            all,
            collision,
            json: compact,
        } => {
            let session = scan_with_settings(
                &source,
                ScanOptions::default(),
                &SearchSettings::default(),
                &cancel,
                |_| {},
            )?;
            let ids = session
                .candidates
                .iter()
                .filter(|c| {
                    c.supported && (all || name.iter().any(|n| c.name.eq_ignore_ascii_case(n)))
                })
                .map(|c| c.id.clone())
                .collect::<Vec<_>>();
            if ids.is_empty() {
                return Err(Error::Request(
                    "no supported deleted files match the requested names".into(),
                ));
            }
            let options = restore_options(original, destination.clone(), volume_root, &collision)?;
            let results = restore_files(&session, &ids, &options, &cancel)?;
            let report = destination
                .as_ref()
                .map(|path| save_restore_report(path, &session, &results))
                .transpose()?;
            print(
                json!({"event":"recovery_finished","schema_version":1,"report":report,"results":results}),
                compact,
            );
            Ok(recovery_code(&results, &cancel))
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
