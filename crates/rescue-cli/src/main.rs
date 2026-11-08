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
    /// Explicitly request Windows UAC permission for this command's source disk.
    /// The GUI/parser process keeps its existing token. Never implicit in MCP.
    #[arg(long, global = true)]
    request_read_access: bool,
    #[command(subcommand)]
    command: Command,
}
#[derive(Subcommand)]
enum Command {
    /// Decode a selected PNG/JPEG or text prefix in a resource-limited worker.
    /// Writes a derived PNG/text under destination; never opens a viewer.
    Preview {
        #[arg(long)]
        session: PathBuf,
        #[arg(long)]
        id: String,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Inspect or save an explicit read-only RAID configuration. Members remain unchanged.
    Raid {
        #[arg(long)]
        config: PathBuf,
        #[arg(long, conflicts_with = "output")]
        search: bool,
        #[arg(long, requires = "search", value_delimiter = ',')]
        stripe_sizes: Vec<u32>,
        #[arg(long, requires = "search")]
        reorder: bool,
        #[arg(long, default_value_t = 128)]
        max_configurations: u16,
        #[arg(long, default_value_t = 5000)]
        max_milliseconds: u32,
        /// Save a fingerprint-bound .rescue-raid.json source, using exclusive creation.
        #[arg(long)]
        output: Option<PathBuf>,
        #[arg(long)]
        json: bool,
    },
    /// Acquire a disk into a durable RAW image/read-map pair. The output directory must be empty.
    Image {
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long,default_value_t=4*1024*1024)]
        block_size: u32,
        #[arg(long, default_value_t = 2)]
        retries: u8,
        /// Auto uses adaptive passes for new images and the saved strategy on resume.
        #[arg(long, default_value = "auto", value_parser = ["auto", "adaptive", "linear"])]
        strategy: String,
        /// Read-helper wait limit in milliseconds; 0 uses direct, unbounded OS reads.
        #[arg(long, default_value_t = 30000)]
        read_timeout_ms: u32,
        /// Wait for the identical source to reconnect; 0 disables automatic reconnect.
        #[arg(long, default_value_t = 15000)]
        reconnect_wait_ms: u32,
        #[arg(long)]
        resume: bool,
        /// Emit durable progress as JSON lines on stderr, while the final result remains on stdout.
        #[arg(long)]
        progress: bool,
        #[arg(long)]
        json: bool,
    },
    /// Inspect acquisition coverage without reading damaged source media.
    ReadMap {
        #[arg(long)]
        image: PathBuf,
        #[arg(long, default_value_t = 0)]
        offset: u64,
        #[arg(long, default_value_t = 100)]
        limit: usize,
        #[arg(long)]
        map_digest: Option<String>,
        #[arg(long)]
        json: bool,
    },
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
    /// Find lost volumes and validated backup boots without repairing the source.
    Discover {
        #[arg(long)]
        source: PathBuf,
        #[arg(long, default_value_t = 0)]
        range_start: u64,
        #[arg(long)]
        range_end: Option<u64>,
        #[arg(long)]
        json: bool,
    },
    /// Resume a durable scan after validating source identity and volume boundaries.
    Resume {
        #[arg(long)]
        session: PathBuf,
        #[arg(long)]
        progress: bool,
        #[arg(long)]
        json: bool,
    },
    /// Copy a JSON session or older SQLite index into the current format; keep the original.
    IndexImport {
        #[arg(long)]
        session: PathBuf,
        #[arg(long)]
        work_dir: PathBuf,
        #[arg(long)]
        json: bool,
    },
    Scan {
        /// Persist candidates and resumable checkpoints in a disk index.
        #[arg(long)]
        indexed: bool,
        #[arg(long)]
        max_records: Option<u64>,
        #[arg(long)]
        max_candidates: Option<usize>,
        #[arg(long)]
        progress: bool,
        #[arg(long)]
        source: PathBuf,
        #[arg(long)]
        work_dir: PathBuf,
        #[arg(long, default_value = "quick", value_parser = ["quick", "carve"])]
        mode: String,
        /// Search unallocated clusters by default; unreadable maps fall back explicitly.
        #[arg(long, default_value = "unallocated", value_parser = ["unallocated", "all"])]
        carve_scope: String,
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
        /// Select one validated volume returned by discover (including overlaps).
        #[arg(long)]
        volume_id: Option<String>,
        /// Also search self-contained NTFS records outside the current MFT.
        #[arg(long)]
        fragments: bool,
        /// Estimate contiguous extents for deleted FAT files whose entire chain is free.
        #[arg(long)]
        estimate_fat_contiguous: bool,
        #[arg(long)]
        json: bool,
    },
    Results {
        #[arg(long)]
        cursor: Option<String>,
        #[arg(long, default_value="discovery", value_parser=["discovery","name","path","size","created","modified","type","state"])]
        sort: String,
        #[arg(long,default_value="none",value_parser=["none","folder","type","created","modified","size","initial","state"])]
        group: String,
        #[arg(long)]
        descending: bool,
        #[arg(long)]
        supported_only: bool,
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
    /// Write unrecoverable candidates' raw bytes (unreadable media zero-filled) with a JSON map.
    /// Output is unverified raw data, never presented as the original file.
    ExportRaw {
        #[arg(long)]
        session: PathBuf,
        #[arg(long, required = true)]
        id: Vec<String>,
        #[arg(long)]
        destination: PathBuf,
        #[arg(long)]
        json: bool,
    },
    /// Export an existing recovery JSON report as an offline HTML or CSV file.
    ExportReport {
        #[arg(long)]
        input: PathBuf,
        #[arg(long)]
        output: PathBuf,
        #[arg(long, value_parser = ["html", "csv"])]
        format: String,
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
    if cli.request_read_access {
        let source=match &cli.command {
            Command::Scan{source,..}|Command::Image{source,..}|Command::Inspect{source,..}|Command::Discover{source,..}|Command::CheckDestination{source,..}|Command::Restore{source,..}=>source.clone(),
            Command::Recover{session,..}|Command::Resume{session,..}|Command::Preview{session,..}|Command::ExportRaw{session,..}=> {
                if rescue_core::CandidateIndex::is_index(session)? {rescue_core::CandidateIndex::open(session)?.session()?.source}
                else {Session::load(session)?.source}
            },
            _=>return Err(Error::Request("--request-read-access requires a disk-reading command; MCP never requests elevation".into())),
        };
        let access = rescue_core::request_read_access(&source, &cancel)?;
        eprintln!("{}", json!({"read_access":access}));
    }
    match cli.command {
        Command::ExportRaw {
            session,
            id,
            destination,
            json: compact,
        } => {
            let session = if rescue_core::CandidateIndex::is_index(&session)? {
                rescue_core::CandidateIndex::open(&session)?.selected_session(&id)?
            } else {
                Session::load(&session)?
            };
            let exports = rescue_core::export_unrecovered(&session, &id, &destination, &cancel)?;
            let exported = exports
                .iter()
                .filter(|e| e.status != "not_exportable")
                .count();
            print(
                json!({"schema_version":1,"exported":exported,"not_exportable":exports.len()-exported,
                "verified_recovery":false,"exports":exports}),
                compact,
            );
            Ok(if exported == exports.len() { 0 } else { 5 })
        }
        Command::Preview {
            session,
            id,
            destination,
            json: compact,
        } => {
            let session = if rescue_core::CandidateIndex::is_index(&session)? {
                rescue_core::CandidateIndex::open(&session)?
                    .selected_session(std::slice::from_ref(&id))?
            } else {
                Session::load(&session)?
            };
            session.guard_output(&destination)?;
            let preview = rescue_core::preview_candidate(session.clone(), &id, &cancel)?;
            if cancel.load(std::sync::atomic::Ordering::Relaxed) {
                return Err(Error::Cancelled);
            }
            let result = rescue_core::save_preview(&session, &preview, &destination, None)?;
            if compact {
                println!("{}", serde_json::to_string(&result)?);
            } else {
                println!("{}", serde_json::to_string_pretty(&result)?);
            }
            Ok(0)
        }
        Command::Image {
            source,
            destination,
            block_size,
            retries,
            strategy,
            read_timeout_ms,
            reconnect_wait_ms,
            resume,
            progress,
            json: compact,
        } => {
            let result = rescue_core::acquire_image(
                &source,
                &destination,
                rescue_core::AcquisitionOptions {
                    block_size,
                    retries,
                    strategy: match strategy.as_str() {
                        "adaptive" => Some(rescue_core::AcquisitionStrategy::Adaptive),
                        "linear" => Some(rescue_core::AcquisitionStrategy::Linear),
                        _ => None,
                    },
                    read_timeout_ms,
                    reconnect_wait_ms,
                    resume,
                },
                &cancel,
                |p| {
                    if progress {
                        eprintln!("{}", serde_json::to_string(&p).unwrap());
                    }
                },
            )?;
            let code = match result.state.as_str() {
                "completed" => 0,
                "cancelled" => 130,
                _ => 5,
            };
            print(serde_json::to_value(result)?, compact);
            Ok(code)
        }
        Command::ReadMap {
            image,
            offset,
            limit,
            map_digest,
            json: compact,
        } => {
            print(
                serde_json::to_value(rescue_core::read_map_page(
                    &image,
                    offset,
                    limit,
                    map_digest.as_deref(),
                )?)?,
                compact,
            );
            Ok(0)
        }
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
        Command::Raid {
            config,
            output,
            search,
            stripe_sizes,
            reorder,
            max_configurations,
            max_milliseconds,
            json: compact,
        } => {
            use std::io::Read;
            let file = std::fs::File::open(&config)?;
            let mut bytes = Vec::new();
            file.take(128 * 1024 + 1).read_to_end(&mut bytes)?;
            if bytes.len() > 128 * 1024 {
                return Err(Error::Limit("RAID configuration exceeds 128 KiB".into()));
            }
            let settings: rescue_core::RaidConfig = serde_json::from_slice(&bytes)?;
            let parent = config.parent().unwrap_or(std::path::Path::new("."));
            if search {
                let defaults = rescue_core::RaidSearchOptions::default();
                let options = rescue_core::RaidSearchOptions {
                    stripe_sizes: if stripe_sizes.is_empty() {
                        defaults.stripe_sizes
                    } else {
                        stripe_sizes
                    },
                    reorder_members: reorder,
                    max_configurations,
                    max_milliseconds,
                    ..defaults
                };
                let result =
                    rescue_core::search_raid(&settings, parent, &options, &cancel, |_| {})?;
                let code = if result.cancelled { 130 } else { 0 };
                print(serde_json::to_value(result)?, compact);
                return Ok(code);
            }
            let report = if let Some(output) = output {
                rescue_core::create_raid_manifest(&settings, parent, &output, None, &cancel)?
            } else {
                rescue_core::inspect_raid(&settings, parent, &cancel)?
            };
            print(serde_json::to_value(report)?, compact);
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
        Command::Discover {
            source,
            range_start,
            range_end,
            json: compact,
        } => {
            let result = rescue_core::discover_volumes(
                &source,
                &rescue_core::DiscoveryOptions {
                    range_start,
                    range_end,
                },
                &cancel,
                |p| {
                    if !compact {
                        eprintln!(
                            "{} / {} bytes ; {} volume candidates",
                            p.scanned_records, p.total_records, p.candidates
                        );
                    }
                },
            )?;
            let code = if result.cancelled {
                130
            } else if result.truncated {
                5
            } else {
                0
            };
            print(serde_json::to_value(result)?, compact);
            Ok(code)
        }
        Command::Resume {
            session,
            progress,
            json: compact,
        } => {
            let path = rescue_core::resume_indexed_scan(&session, &cancel, |p| {
                if progress {
                    eprintln!("{}", serde_json::to_string(&p).unwrap());
                }
            })?;
            let index = rescue_core::CandidateIndex::open(&path)?;
            let status = index.status()?;
            print(
                json!({"event":"scan_finished","session":path,"index":status}),
                compact,
            );
            Ok(if status.state == "cancelled" {
                130
            } else if status.state == "partial" {
                5
            } else {
                0
            })
        }
        Command::IndexImport {
            session,
            work_dir,
            json: compact,
        } => {
            let path =
                rescue_core::IndexWriter::import_session_with_cancel(&session, &work_dir, &cancel)?;
            print(
                json!({"session":path,"index":rescue_core::CandidateIndex::open(&path)?.status()?}),
                compact,
            );
            Ok(0)
        }
        Command::Scan {
            indexed,
            max_records,
            max_candidates,
            progress: emit_progress,
            source,
            work_dir,
            json: compact,
            mode,
            carve_scope,
            extensions,
            path_prefix,
            max_depth,
            range_start,
            range_end,
            volume_id,
            fragments,
            estimate_fat_contiguous,
        } => {
            rescue_core::guard_output_path(&source, &work_dir)?;
            let recovered_volume = if let Some(id) = volume_id {
                if mode != "quick" {
                    return Err(Error::Request("--volume-id requires --mode quick".into()));
                }
                let found = rescue_core::discover_volumes(
                    &source,
                    &rescue_core::DiscoveryOptions::default(),
                    &cancel,
                    |_| {},
                )?;
                Some(
                    found
                        .volumes
                        .into_iter()
                        .find(|v| v.id == id)
                        .ok_or_else(|| {
                            Error::Request("selected discovered volume is unavailable".into())
                        })?
                        .region,
                )
            } else {
                None
            };
            let settings = SearchSettings {
                carve_scope: if carve_scope == "all" {
                    rescue_core::CarveScope::All
                } else {
                    rescue_core::CarveScope::Unallocated
                },
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
                recovered_volume,
                fragments,
                estimate_fat_contiguous,
            };
            if !indexed && (max_records.is_some() || max_candidates.is_some()) {
                return Err(Error::Request(
                    "explicit scan limits require --indexed".into(),
                ));
            }
            let options = ScanOptions {
                max_records: max_records.unwrap_or(if indexed { 100_000_000 } else { 2_000_000 }),
                max_candidates: max_candidates.unwrap_or(if indexed {
                    10_000_000
                } else {
                    100_000
                }),
                fragments,
                estimate_fat_contiguous,
            };
            if indexed {
                let path = rescue_core::scan_indexed(
                    &source,
                    &work_dir,
                    options,
                    &settings,
                    &cancel,
                    |p| {
                        if emit_progress {
                            eprintln!("{}", serde_json::to_string(&p).unwrap());
                        }
                    },
                )?;
                let status = rescue_core::CandidateIndex::open(&path)?.status()?;
                print(
                    json!({"event":"scan_finished","session":path,"index":status}),
                    compact,
                );
                return Ok(if status.state == "cancelled" {
                    130
                } else if status.state == "partial" {
                    5
                } else {
                    0
                });
            }
            let session = scan_with_settings(
                &source,
                options,
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
            cursor,
            sort,
            group,
            descending,
            supported_only,
            session,
            limit,
            offset,
            name,
            json: compact,
        } => {
            if limit == 0 || limit > 1000 {
                return Err(Error::Request("limit must be 1..1000".into()));
            }
            if rescue_core::CandidateIndex::is_index(&session)? {
                if offset != 0 {
                    return Err(Error::Request(
                        "indexed results use --cursor, not --offset".into(),
                    ));
                }
                let index = rescue_core::CandidateIndex::open(&session)?;
                let cursor: Option<rescue_core::IndexCursor> =
                    cursor.map(|s| serde_json::from_str(&s)).transpose()?;
                let query = rescue_core::IndexQuery {
                    sort: match sort.as_str() {
                        "name" => rescue_core::IndexSort::Name,
                        "path" => rescue_core::IndexSort::Path,
                        "size" => rescue_core::IndexSort::Size,
                        "created" => rescue_core::IndexSort::Created,
                        "modified" => rescue_core::IndexSort::Modified,
                        "type" => rescue_core::IndexSort::Type,
                        "state" => rescue_core::IndexSort::State,
                        _ => rescue_core::IndexSort::Discovery,
                    },
                    group: serde_json::from_value(json!(group))?,
                    descending,
                    text: name.unwrap_or_default(),
                    supported_only,
                };
                index.prepare_query(&query, &cancel)?;
                print(
                    serde_json::to_value(index.page_with_cancel(
                        &query,
                        cursor.as_ref(),
                        limit,
                        &cancel,
                    )?)?,
                    compact,
                );
                return Ok(0);
            }
            if cursor.is_some()
                || sort != "discovery"
                || group != "none"
                || descending
                || supported_only
            {
                return Err(Error::Request(
                    "cursor and indexed sorting require a SQLite index".into(),
                ));
            }
            let session = Session::load(&session)?;
            let matching = session
                .candidates
                .iter()
                .filter(|c| name.as_ref().is_none_or(|n| c.matches_name(n)))
                .collect::<Vec<_>>();
            print(
                json!({"schema_version":1,"session_id":session.id,"total":matching.len(),
                "filesystem":session.filesystem,
                "offset":offset,"candidates":matching.iter().skip(offset).take(limit).map(|c|c.in_session(&session.id)).collect::<Vec<_>>()}),
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
            let session = if rescue_core::CandidateIndex::is_index(&session)? {
                let index = rescue_core::CandidateIndex::open(&session)?;
                if all {
                    let q = rescue_core::IndexQuery {
                        supported_only: true,
                        ..Default::default()
                    };
                    let mut cursor = None;
                    loop {
                        let page = index.page(&q, cursor.as_ref(), 1000)?;
                        id.extend(page.candidates.into_iter().map(|c| c.id));
                        if id.len() > 100_000 {
                            return Err(Error::Limit(
                                "recover at most 100000 indexed candidates per batch".into(),
                            ));
                        }
                        cursor = page.next_cursor;
                        if cursor.is_none() {
                            break;
                        }
                    }
                }
                index.selected_session(&id)?
            } else {
                Session::load(&session)?
            };
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
            if session.cancelled {
                return Err(Error::Cancelled);
            }
            let ids = session
                .candidates
                .iter()
                .filter(|c| {
                    c.supported
                        && (all
                            || name.iter().any(|n| {
                                c.name.eq_ignore_ascii_case(n)
                                    || c.alternate_names
                                        .iter()
                                        .any(|a| a.name.eq_ignore_ascii_case(n))
                            }))
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
        Command::ExportReport {
            input,
            output,
            format,
            json: compact,
        } => {
            let report = rescue_core::read_recovery_report(&input)?;
            let kind = if format == "html" {
                rescue_core::ReportFormat::Html
            } else {
                rescue_core::ReportFormat::Csv
            };
            rescue_core::export_recovery_report(&report, kind, &output)?;
            print(
                json!({"event":"report_exported","format":format,"output":output,"results":report["results"].as_array().map(Vec::len)}),
                compact,
            );
            Ok(0)
        }
        Command::Report {
            session,
            json: compact,
        } => {
            if rescue_core::CandidateIndex::is_index(&session)? {
                let index = rescue_core::CandidateIndex::open(&session)?;
                print(
                    json!({"index":index.status()?,"session":index.session()?}),
                    compact,
                );
                return Ok(0);
            }
            let session = Session::load(&session)?;
            print(
                json!({"schema_version":1,"session_id":session.id,"source":session.source,
                "source_info":session.source_info,
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
    if let Some(code) = rescue_core::acquisition_worker_entry() {
        std::process::exit(code);
    }
    if let Some(code) = rescue_core::read_broker_entry() {
        std::process::exit(code);
    }
    if let Some(code) = rescue_core::preview_worker_entry() {
        std::process::exit(code);
    }
    let code = match run() {
        Ok(code) => code,
        Err(error) => {
            eprintln!(
                "{}",
                json!({"schema_version":1,"error":error.report("command")})
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
