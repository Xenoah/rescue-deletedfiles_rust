#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#[macro_use]
mod language;
#[cfg(test)]
mod accessibility;
mod acquisition_ui;
mod appearance;
mod columns;
mod discovery_ui;
mod hash_ui;
#[cfg(any(windows, test))]
mod ime;
mod index_ui;
mod preview_ui;
mod raid_ui;
#[cfg(windows)]
mod read_access_ui;
mod scrolling;
mod session_ui;
mod text_input;
mod theme;
mod validation;
mod view;
use eframe::egui;
use egui_extras::{Column, TableBuilder};
use language::{Language, Text};
use rescue_core::{
    AllocationStatus, Candidate, CollisionPolicy, DiskSource, FileSystem, Progress, RecoveryResult,
    RestoreOptions, RestoreTarget, ScanMethod, ScanOptions, SearchSettings, Session, VolumeRoot,
    guard_output_path, list_disks, restore_files, restore_zip, save_restore_report,
    scan_with_settings,
};
use scrolling::AccessibleScrollArea;
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};
use validation::Field;
use view::{GroupBy, Row, SortColumn, ViewStyle};
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum Stage {
    #[default]
    Settings,
    Scanning,
    Imaging,
    Migrating,
    Selecting,
    Recovering,
    Finished,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SaveMode {
    #[default]
    Directory,
    Zip,
    Original,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
enum SourceMode {
    #[default]
    Disk,
    Image,
}
enum Message {
    Indexed(Box<index_ui::Loaded>),
    IndexResumed(Box<index_ui::Loaded>),
    IndexProgress(rescue_core::IndexedProgress),
    RaidChecked(Box<rescue_core::RaidInspection>, Option<PathBuf>),
    RaidSearched(Box<rescue_core::RaidSearchResult>),
    ImagingProgress(rescue_core::AcquisitionProgress),
    Imaged(rescue_core::AcquisitionResult),
    Progress(Progress),
    Discovered(Box<rescue_core::DiscoveryResult>),
    Scanned(Box<Session>, PathBuf),
    Finished {
        results: Vec<RecoveryResult>,
        status: Text,
        detail: Text,
    },
    RawExported {
        status: Text,
        detail: Text,
    },
    Error(Text),
}
struct App {
    language: Language,
    appearance: egui::ThemePreference,
    appearance_path: Option<PathBuf>,
    show_validation: bool,
    path_issues: Vec<validation::PathIssue>,
    status_error: bool,
    session_dialog: Option<session_ui::Action>,
    session_path: String,
    #[cfg(windows)]
    read_access: read_access_ui::State,
    preview: preview_ui::State,
    index: index_ui::State,
    raid: raid_ui::State,
    discovery_dialog: bool,
    discovery_running: bool,
    discovery_source: String,
    discovery: Option<rescue_core::DiscoveryResult>,
    discovered_id: Option<String>,
    recovered_volume: Option<rescue_core::SourceRegion>,
    map_dialog: bool,
    map_source: PathBuf,
    map_page: Option<rescue_core::ReadMapPage>,
    map_error: String,
    map_messages: Option<mpsc::Receiver<Result<rescue_core::ReadMapPage, String>>>,
    image_dialog: bool,
    image_destination: String,
    image_resume: bool,
    image_original: Option<(String, String, String)>,
    image_pause_requested: bool,
    image_retries: u8,
    image_block_size: u32,
    image_strategy: Option<rescue_core::AcquisitionStrategy>,
    image_read_timeout_seconds: u32,
    image_reconnect_seconds: u32,
    image_progress: Option<rescue_core::AcquisitionProgress>,
    image_stop_reason: Option<String>,
    source: String,
    source_mode: SourceMode,
    disks: Vec<DiskSource>,
    disk_messages: Option<mpsc::Receiver<Result<Vec<DiskSource>, String>>>,
    disk_status: Text,
    work: String,
    destination: String,
    save_mode: SaveMode,
    collision: CollisionPolicy,
    original_root: String,
    carve: bool,
    carve_all: bool,
    fragments: bool,
    estimate_fat_contiguous: bool,
    fat_short_name_encoding: rescue_core::FatShortNameEncoding,
    exfat_allow_damaged_upcase: bool,
    extensions: String,
    path_prefix: String,
    depth: String,
    range_start: String,
    range_end: String,
    stage: Stage,
    session: Option<Session>,
    selected: BTreeSet<String>,
    focused: Option<String>,
    filter: String,
    #[cfg(any(windows, test))]
    ime: ime::Composition,
    view: ViewStyle,
    column_widths: [f32; 5],
    sort: SortColumn,
    descending: bool,
    group: GroupBy,
    closed: BTreeSet<String>,
    rows: Vec<Row>,
    view_dirty: bool,
    show_evidence: bool,
    messages: Option<mpsc::Receiver<Message>>,
    cancel: Arc<AtomicBool>,
    progress: Progress,
    status: Text,
    report: Text,
    show_report: bool,
    report_export: Option<(rescue_core::ReportFormat, String, String)>,
    hashes: hash_ui::State,
    focus_report: bool,
    results: Vec<RecoveryResult>,
    primary_rect: Option<egui::Rect>,
}
fn configure_style(ctx: &egui::Context) {
    theme::configure(ctx);
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>, session: Option<PathBuf>, language: Language) -> Self {
        #[cfg(windows)]
        if let Some(state) = &cc.wgpu_render_state {
            let info = state.adapter.get_info();
            eprintln!(
                "Rescue renderer: {:?}; device: {:?}; adapter: {}",
                info.backend, info.device_type, info.name
            );
        }
        #[cfg(not(windows))]
        if cc.gl.is_some() {
            eprintln!("Rescue renderer: OpenGL");
        }
        configure_style(&cc.egui_ctx);
        let mut paths = if cfg!(windows) {
            let win = std::env::var_os("WINDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            vec![
                win.join(r"Fonts\YuGothR.ttc"),
                win.join(r"Fonts\meiryo.ttc"),
            ]
        } else if cfg!(target_os = "macos") {
            vec![PathBuf::from(
                "/System/Library/Fonts/ヒラギノ角ゴシック W3.ttc",
            )]
        } else {
            vec![PathBuf::from(
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            )]
        };
        if let Some(font) = std::env::var_os("RESCUE_FONT") {
            paths.insert(0, PathBuf::from(font));
        }
        for path in paths {
            if let Ok(data) = std::fs::read(path) {
                let mut fonts = egui::FontDefinitions::default();
                fonts
                    .font_data
                    .insert("japanese".into(), egui::FontData::from_owned(data).into());
                fonts
                    .families
                    .entry(egui::FontFamily::Proportional)
                    .or_default()
                    .insert(0, "japanese".into());
                fonts
                    .families
                    .entry(egui::FontFamily::Monospace)
                    .or_default()
                    .push("japanese".into());
                cc.egui_ctx.set_fonts(fonts);
                break;
            }
        }
        let mut app = Self::blank();
        app.appearance_path = appearance::path();
        if let Some(path) = &app.appearance_path {
            app.appearance = appearance::load(path);
        }
        cc.egui_ctx.set_theme(app.appearance);
        app.language = language;
        app.refresh_disks();
        if let Some(path) = session {
            app.open_session(path);
        }
        app
    }
    fn blank() -> Self {
        Self {
            language: Language::Japanese,
            appearance: egui::ThemePreference::System,
            appearance_path: None,
            show_validation: false,
            path_issues: Vec::new(),
            status_error: false,
            session_dialog: None,
            session_path: String::new(),
            #[cfg(windows)]
            read_access: read_access_ui::State::default(),
            index: index_ui::State::default(),
            raid: raid_ui::State::default(),
            discovery_dialog: false,
            discovery_running: false,
            discovery_source: String::new(),
            discovery: None,
            discovered_id: None,
            recovered_volume: None,
            map_dialog: false,
            map_source: PathBuf::new(),
            map_page: None,
            map_error: String::new(),
            map_messages: None,
            image_dialog: false,
            image_destination: String::new(),
            image_resume: false,
            image_original: None,
            image_pause_requested: false,
            image_retries: 2,
            image_block_size: 4 * 1024 * 1024,
            image_strategy: None,
            image_read_timeout_seconds: 30,
            image_reconnect_seconds: 15,
            image_progress: None,
            image_stop_reason: None,
            source: String::new(),
            source_mode: SourceMode::Disk,
            disks: Vec::new(),
            disk_messages: None,
            disk_status: Text::default(),
            work: String::new(),
            destination: String::new(),
            save_mode: SaveMode::Directory,
            collision: CollisionPolicy::KeepBoth,
            original_root: String::new(),
            carve: false,
            carve_all: false,
            fragments: false,
            estimate_fat_contiguous: false,
            fat_short_name_encoding: rescue_core::FatShortNameEncoding::Ascii,
            exfat_allow_damaged_upcase: false,
            extensions: String::new(),
            path_prefix: String::new(),
            depth: String::new(),
            range_start: String::new(),
            range_end: String::new(),
            stage: Stage::Settings,
            session: None,
            selected: BTreeSet::new(),
            focused: None,
            filter: String::new(),
            #[cfg(any(windows, test))]
            ime: ime::Composition::default(),
            view: ViewStyle::Details,
            column_widths: columns::DEFAULT,
            sort: SortColumn::Name,
            descending: false,
            group: GroupBy::None,
            closed: BTreeSet::new(),
            rows: Vec::new(),
            view_dirty: true,
            show_evidence: false,
            preview: preview_ui::State::default(),
            messages: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Progress::default(),
            status: message!(
                "ディスクと保存方法を設定してください。",
                "Choose a disk and save options."
            ),
            report: Text::default(),
            show_report: false,
            report_export: None,
            hashes: hash_ui::State::default(),
            focus_report: false,
            results: Vec::new(),
            primary_rect: None,
        }
    }
    fn busy(&self) -> bool {
        matches!(
            self.stage,
            Stage::Scanning | Stage::Recovering | Stage::Imaging | Stage::Migrating
        )
    }
    fn settings(&self) -> Result<SearchSettings, Text> {
        let number = |s: &str| {
            if s.trim().is_empty() {
                Ok(None)
            } else {
                s.trim().parse::<u64>().map(Some).map_err(|_| {
                    message!(
                        "範囲・深度には整数を入力してください。",
                        "Enter whole numbers for the byte range and depth."
                    )
                })
            }
        };
        let s = SearchSettings {
            carve_scope: if self.carve_all {
                rescue_core::CarveScope::All
            } else {
                rescue_core::CarveScope::Unallocated
            },
            fragments: !self.carve && self.fragments,
            estimate_fat_contiguous: !self.carve && self.estimate_fat_contiguous,
            exfat_allow_damaged_upcase: !self.carve && self.exfat_allow_damaged_upcase,
            fat_short_name_encoding: if self.carve {
                rescue_core::FatShortNameEncoding::Ascii
            } else {
                self.fat_short_name_encoding
            },
            recovered_volume: if self.carve || self.discovery_source != self.source {
                None
            } else {
                self.recovered_volume.clone()
            },
            method: if self.carve {
                ScanMethod::Carve
            } else {
                ScanMethod::Quick
            },
            extensions: self
                .extensions
                .split([',', ';', ' ', '\n'])
                .filter(|s| !s.is_empty())
                .map(str::to_owned)
                .collect(),
            path_prefix: if self.carve {
                String::new()
            } else {
                self.path_prefix.clone()
            },
            max_depth: if self.carve {
                None
            } else {
                number(&self.depth)?
                    .map(u16::try_from)
                    .transpose()
                    .map_err(|_| {
                        message!(
                            "深度は0～256で指定してください。",
                            "Enter a depth between 0 and 256."
                        )
                    })?
            },
            range_start: if self.carve {
                number(&self.range_start)?.unwrap_or(0)
            } else {
                0
            },
            range_end: if self.carve {
                number(&self.range_end)?
            } else {
                None
            },
        };
        s.validate().map_err(|e| e.to_string())?;
        Ok(s)
    }
    fn work_path(&self) -> PathBuf {
        if !self.work.trim().is_empty() {
            return PathBuf::from(&self.work);
        }
        let out = PathBuf::from(&self.destination);
        let base = if self.save_mode == SaveMode::Zip {
            out.parent().unwrap_or_else(|| Path::new("."))
        } else {
            out.as_path()
        };
        base.join(".rescue-work")
    }
    fn install_session(&mut self, s: Session, path: &Path) {
        self.status_error = false;
        self.index = index_ui::State::default();
        self.source = s.source.to_string_lossy().into_owned();
        self.source_mode = if rescue_core::is_device(&s.source) {
            SourceMode::Disk
        } else {
            SourceMode::Image
        };
        let settings = s.search.clone().unwrap_or_else(|| SearchSettings {
            carve_scope: rescue_core::CarveScope::All,
            method: if s.filesystem == FileSystem::Raw {
                ScanMethod::Carve
            } else {
                ScanMethod::Quick
            },
            ..Default::default()
        });
        self.carve = settings.method == ScanMethod::Carve;
        self.carve_all = settings.carve_scope == rescue_core::CarveScope::All;
        self.fragments = settings.fragments;
        self.estimate_fat_contiguous = settings.estimate_fat_contiguous;
        self.fat_short_name_encoding = settings.fat_short_name_encoding;
        self.exfat_allow_damaged_upcase = settings.exfat_allow_damaged_upcase;
        self.recovered_volume = settings.recovered_volume.clone();
        self.discovery_source = self.source.clone();
        self.extensions = settings.extensions.join(", ");
        self.path_prefix = settings.path_prefix;
        self.depth = settings
            .max_depth
            .map(|v| v.to_string())
            .unwrap_or_default();
        self.range_start = if settings.range_start == 0 {
            String::new()
        } else {
            settings.range_start.to_string()
        };
        self.range_end = settings
            .range_end
            .map(|v| v.to_string())
            .unwrap_or_default();
        self.work = path
            .parent()
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.focused = s.candidates.first().map(|c| c.id.clone());
        self.session = Some(s);
        self.selected.clear();
        self.results.clear();
        self.hashes = hash_ui::State::default();
        self.filter.clear();
        self.closed.clear();
        self.rows.clear();
        self.view_dirty = true;
        self.stage = Stage::Selecting;
        self.report = if path.as_os_str().is_empty() {
            message!(
                "スキャン結果をメモリーに保持しています。",
                "Scan results are held in memory."
            )
        } else {
            message!("セッション: {}", "Session: {}", path.display())
        };
    }
    fn invalidate_changed_source(&mut self) {
        let changed = self.session.as_ref().is_some_and(|s| {
            let prev = s.search.clone().unwrap_or_else(|| SearchSettings {
                carve_scope: rescue_core::CarveScope::All,
                method: if s.filesystem == FileSystem::Raw {
                    ScanMethod::Carve
                } else {
                    ScanMethod::Quick
                },
                ..Default::default()
            });
            self.source != s.source.to_string_lossy() || self.settings().as_ref() != Ok(&prev)
        });
        if changed {
            self.status_error = false;
            self.index = index_ui::State::default();
            self.session = None;
            self.selected.clear();
            self.focused = None;
            self.results.clear();
            self.hashes = hash_ui::State::default();
            self.rows.clear();
            self.report.clear();
            self.view_dirty = true;
            self.status = message!(
                "入力または検索条件が変更されました。スキャンを実行してください。",
                "The source or search settings changed. Run a new scan."
            );
        }
    }
    fn start_scan(&mut self) {
        if !self.validate_configuration() {
            return;
        }
        let settings = match self.settings() {
            Ok(s) => s,
            Err(e) => {
                self.status = e;
                self.status_error = true;
                return;
            }
        };
        let source = PathBuf::from(&self.source);
        let work = self.work_path();
        let guarded = guard_output_path(&source, &work);
        if !self.work.trim().is_empty()
            && let Err(error) = &guarded
        {
            self.remember_path_issue(
                Field::Work,
                message!(
                    "再開用の作業フォルダーを使用できません: {error}",
                    "Cannot use the working folder for resume: {error}"
                ),
            );
            return;
        }
        let persist = guarded.is_ok()
            && (!self.work.trim().is_empty() || self.save_mode != SaveMode::Original);
        self.index = index_ui::State::default();
        self.filter.clear();
        let query = self.index_query();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        self.status_error = false;
        self.stage = Stage::Scanning;
        self.discovery_running = false;
        self.session = None;
        self.selected.clear();
        self.focused = None;
        self.results.clear();
        self.hashes = hash_ui::State::default();
        self.rows.clear();
        self.report.clear();
        self.view_dirty = true;
        self.progress = Progress::default();
        self.status = message!("スキャン中…", "Scanning…");
        thread::spawn(move || {
            let result = if persist {
                rescue_core::scan_indexed(
                    &source,
                    &work,
                    ScanOptions {
                        max_records: 100_000_000,
                        max_candidates: 10_000_000,
                        fragments: settings.fragments,
                        estimate_fat_contiguous: settings.estimate_fat_contiguous,
                        fat_short_name_encoding: settings.fat_short_name_encoding,
                        exfat_allow_damaged_upcase: settings.exfat_allow_damaged_upcase,
                    },
                    &settings,
                    &cancel,
                    |p| {
                        let _ = tx.try_send(Message::IndexProgress(p));
                    },
                )
                .map_err(|e| e.to_string())
                .and_then(|path| index_ui::load_scan_result(path, query))
                .map(|loaded| Message::Indexed(Box::new(loaded)))
            } else {
                scan_with_settings(&source, ScanOptions::default(), &settings, &cancel, |p| {
                    let _ = tx.try_send(Message::Progress(p));
                })
                .map(|session| Message::Scanned(Box::new(session), PathBuf::new()))
                .map_err(|e| e.to_string())
            };
            let _ = tx.send(result.unwrap_or_else(|e| Message::Error(e.into())));
        });
    }
    /// Candidates of the last recovery that did not complete (read errors on damaged
    /// media, validation failures). Their raw bytes can be exported unverified.
    fn unrecovered_ids(&self) -> Vec<String> {
        self.results
            .iter()
            .filter(|r| r.status != "completed" && r.status != "skipped")
            .map(|r| r.candidate_id.clone())
            .collect()
    }
    fn start_raw_export(&mut self) {
        let ids = self.unrecovered_ids();
        let Some(session) = self.session.clone() else {
            return;
        };
        if ids.is_empty() {
            return;
        }
        let out = PathBuf::from(&self.destination);
        let base = match self.save_mode {
            SaveMode::Directory => out,
            SaveMode::Zip => out.parent().map(Path::to_path_buf).unwrap_or_default(),
            SaveMode::Original => self.work_path(),
        };
        if base.as_os_str().is_empty() {
            self.status = message!(
                "書き出し先のフォルダーを設定画面で指定してください。",
                "Choose a destination folder in Settings."
            );
            return;
        }
        let stamp = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or_default();
        let dest = base.join(format!("rescue-raw-{stamp}"));
        let index_path = self.index.path.clone();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::sync_channel(2);
        self.messages = Some(rx);
        self.status_error = false;
        self.stage = Stage::Recovering;
        self.status = message!(
            "復元できなかったデータを書き出し中…",
            "Exporting unrecovered data…"
        );
        thread::spawn(move || {
            let result = (|| -> rescue_core::Result<Message> {
                let session = match index_path {
                    Some(path) => {
                        rescue_core::CandidateIndex::open(&path)?.selected_session(&ids)?
                    }
                    None => session,
                };
                let exports = rescue_core::export_unrecovered(&session, &ids, &dest, &cancel)?;
                let gaps = exports
                    .iter()
                    .filter(|e| e.status == "exported_with_gaps")
                    .count();
                let done = exports
                    .iter()
                    .filter(|e| e.status != "not_exportable")
                    .count();
                let failed = exports.len() - done;
                let folder = dest.display().to_string();
                Ok(Message::RawExported {
                    status: message!(
                        "未検証のバイナリを書き出し: {} 件（読めない部分を0で埋めたもの {} 件）/ 書き出せなかったもの: {} 件",
                        "Unverified raw data exported: {} ({} with unreadable parts zero-filled) / not exportable: {}",
                        done,
                        gaps,
                        failed
                    ),
                    detail: message!(
                        "書き出し先: {}\n各 .rescue-raw.bin の横の .rescue-raw.json に、0で埋めた範囲と理由を記録しました。元のファイルとして扱わないでください。",
                        "Folder: {}\nEach .rescue-raw.bin has a .rescue-raw.json listing zero-filled ranges and reasons. Do not treat these as the original files.",
                        folder
                    ),
                })
            })();
            let _ = tx.send(
                result.unwrap_or_else(|e| Message::Error(message!("{}", "{}", e.to_string()))),
            );
        });
    }
    fn start_recovery(&mut self) {
        self.invalidate_changed_source();
        if !self.validate_configuration() {
            self.stage = Stage::Settings;
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        if self.selected.is_empty() {
            return;
        }
        let index_path = self.index.path.clone();
        let ids = self.selected.iter().cloned().collect::<Vec<_>>();
        let dest = PathBuf::from(&self.destination);
        let mode = self.save_mode;
        let collision = self.collision;
        let mappings = if self.original_root.trim().is_empty() {
            Vec::new()
        } else {
            vec![VolumeRoot {
                partition: session.region.as_ref().map(|r| r.index),
                path: PathBuf::from(&self.original_root),
            }]
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        self.status_error = false;
        self.stage = Stage::Recovering;
        self.status = message!("選択したファイルを復元中…", "Recovering selected files…");
        thread::spawn(move || {
            let session = if let Some(path) = index_path {
                match rescue_core::CandidateIndex::open(&path)
                    .and_then(|index| index.selected_session(&ids))
                {
                    Ok(session) => session,
                    Err(error) => {
                        let _ = tx.send(Message::Error(error.to_string().into()));
                        return;
                    }
                }
            } else {
                session
            };
            let r = if mode == SaveMode::Zip {
                restore_zip(&session, &ids, &dest, collision, &cancel).map(|r| Message::Finished {
                    status: message!(
                        "ZIP保存: {} / {} ファイル",
                        "ZIP: {} / {} files",
                        status_text(&r.status),
                        r.results.iter().filter(|v| v.status == "completed").count()
                    ),
                    detail: message!(
                        "ZIP: {}\nSHA-256: {}\n{}{}{}",
                        "ZIP: {}\nSHA-256: {}\n{}{}{}",
                        r.archive
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        r.sha256.unwrap_or_default(),
                        r.error.unwrap_or_default(),
                        r.staging
                            .map(|p| message!(
                                "\n一時保存: {}",
                                "\nStaging folder: {}",
                                p.display()
                            ))
                            .unwrap_or_default(),
                        r.partial_archive
                            .map(|p| message!(
                                "\n未完了ZIP: {}",
                                "\nUnfinished ZIP: {}",
                                p.display()
                            ))
                            .unwrap_or_default()
                    ),
                    results: r.results,
                })
            } else {
                let options = RestoreOptions {
                    target: if mode == SaveMode::Original {
                        RestoreTarget::Original(mappings)
                    } else {
                        RestoreTarget::Directory(dest.clone())
                    },
                    collision,
                };
                restore_files(&session, &ids, &options, &cancel).and_then(|results| {
                    let detail = if mode == SaveMode::Original {
                        message!("元の場所への復元結果を下に表示しています。", "Results of restoring to original locations are shown below.")
                    } else {
                        message!("レポート: {}", "Report: {}",
                            save_restore_report(&dest, &session, &results)?.display()
                        )
                    };
                    let n = results.iter().filter(|r| r.status == "completed").count();
                    let skipped = results.iter().filter(|r| r.status == "skipped").count();
                    let skipped_label = if skipped == 0 {
                        Text::default()
                    } else {
                        message!(" / スキップ: {skipped}", " / Skipped: {skipped}")
                    };
                    Ok(Message::Finished {
                        status: message!("保存完了: {n}{skipped_label} / 不完全・失敗: {} / 取消: {}", "Saved: {n}{skipped_label} / Incomplete or failed: {} / Cancellation: {}",
                            results.len() - n - skipped,
                            if cancel.load(Ordering::Relaxed) {
                                message!("要求あり", "Requested")
                            } else {
                                message!("なし", "None")
                            }
                        ),
                        detail,
                        results,
                    })
                })
            };
            let _ = tx.send(r.unwrap_or_else(|e| Message::Error(e.to_string().into())));
        });
    }
    fn receive(&mut self) {
        self.receive_index();
        if let Some(result) = self.map_messages.as_ref().and_then(|rx| rx.try_recv().ok()) {
            self.map_messages = None;
            match result {
                Ok(page) => {
                    self.map_page = Some(page);
                    self.map_error.clear();
                }
                Err(error) => self.map_error = error,
            }
        }
        if let Some(result) = self
            .disk_messages
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        {
            self.disk_messages = None;
            match result {
                Ok(disks) => {
                    self.disk_status = if disks.is_empty() {
                        message!(
                            "ディスクを検出できませんでした。接続・アクセス権を確認してください。",
                            "No disks found. Check the connection and access permissions."
                        )
                    } else {
                        message!(
                            "{} 個のディスク／ボリューム",
                            "{} disks / volumes",
                            disks.len()
                        )
                    };
                    self.disks = disks;
                }
                Err(e) => {
                    self.disk_status = message!(
                        "ディスク一覧を取得できません: {e}",
                        "Cannot list disks: {e}"
                    )
                }
            }
        }
        let messages = self
            .messages
            .as_ref()
            .map(|rx| rx.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for m in messages {
            if !matches!(
                m,
                Message::Error(_)
                    | Message::Progress(_)
                    | Message::IndexProgress(_)
                    | Message::ImagingProgress(_)
            ) {
                self.status_error = false;
            }
            match m {
                Message::RaidChecked(report, path) => self.raid_checked(*report, path),
                Message::RaidSearched(report) => self.raid_searched(*report),
                Message::ImagingProgress(p) => {
                    self.progress = Progress {
                        scanned_records: p.completed_bytes,
                        total_records: p.total_bytes,
                        candidates: 0,
                        unit: rescue_core::ProgressUnit::Bytes,
                    };
                    self.image_progress = Some(p);
                }
                Message::Imaged(result) => {
                    if let Some(origin) = &mut self.image_original {
                        origin.2 = result.image.to_string_lossy().into_owned();
                    }
                    self.source = result.image.to_string_lossy().into_owned();
                    self.source_mode = SourceMode::Image;
                    self.stage = Stage::Settings;
                    self.report = message!(
                        "イメージ: {}\n読取マップ: {}",
                        "Image: {}\nRead map: {}",
                        result.image.display(),
                        result.read_map.display()
                    );
                    self.status = message!(
                        "イメージ取得{} · 読取済み {} · 読取失敗 {} · 未読 {}",
                        "Image acquisition {} · Read {} · Failed {} · Unread {}",
                        match result.state.as_str() {
                            "completed" => message!("完了", "complete"),
                            "cancelled" => message!("を中断", "paused"),
                            _ => message!("（一部取得）", "(partial)"),
                        },
                        view::size_text(Some(result.summary.copied_bytes)),
                        view::size_text(Some(result.summary.failed_bytes)),
                        view::size_text(Some(result.summary.unattempted_bytes))
                    );
                    self.image_resume = result.state != "completed";
                    self.image_stop_reason = result.stop_reason.clone();
                    if let Some(reason) = &result.stop_reason {
                        self.image_dialog = true;
                        self.report = message!(
                            "イメージ: {}\n読取マップ: {}\n停止理由: {}\n元媒体を確認して取得を再開できます。",
                            "Image: {}\nRead map: {}\nStopped: {}\nCheck the original source, then resume acquisition.",
                            result.image.display(),
                            result.read_map.display(),
                            reason
                        );
                    }
                    if result.state == "cancelled" && self.image_pause_requested {
                        self.image_resume = true;
                        self.image_dialog = true;
                        self.status = message!(
                            "イメージ取得を一時停止しました。保存済みブロックを照合して再開できます。",
                            "Image acquisition paused. Saved blocks will be verified before resuming."
                        );
                    }
                }
                Message::Indexed(loaded) => self.install_index(*loaded),
                Message::IndexResumed(loaded) => self.accept_resumed_index(*loaded),
                Message::IndexProgress(p) => {
                    self.report = message!("索引: {}", "Index: {}", p.index.display());
                    self.index.path = Some(p.index);
                    self.progress = p.progress;
                }
                Message::Progress(p) => self.progress = p,
                Message::Discovered(result) => {
                    self.discovery_running = false;
                    self.stage = Stage::Settings;
                    self.status = message!(
                        "領域探索{}: {} 候補{}",
                        "Volume search {}: {} candidates{}",
                        if result.cancelled {
                            message!("を中断", "paused")
                        } else {
                            message!("完了", "complete")
                        },
                        result.volumes.len(),
                        if result.truncated {
                            message!(" / 未探索の範囲あり", " / Some ranges remain unscanned")
                        } else {
                            Text::default()
                        }
                    );
                    self.discovery = Some(*result);
                    self.discovered_id = None;
                    self.discovery_dialog = true;
                }
                Message::Scanned(s, p) => {
                    self.status = message!(
                        "スキャン完了: {} 候補 / 解析不良: {} 件{}",
                        "Scan complete: {} candidates / {} invalid records{}",
                        s.candidates.len(),
                        s.invalid_records,
                        if s.cancelled {
                            message!(
                                " / 取消：未探索領域があります",
                                " / Cancelled: some ranges remain unscanned"
                            )
                        } else if s.truncated {
                            message!(
                                " / 一部未探索：警告を確認してください",
                                " / Partly scanned: check warnings"
                            )
                        } else {
                            Text::default()
                        }
                    );
                    self.install_session(*s, &p);
                }
                Message::Finished {
                    results,
                    status,
                    detail,
                } => {
                    self.hashes = hash_ui::State::default();
                    self.results = results;
                    self.status = status;
                    self.report = detail;
                    self.stage = Stage::Finished;
                    self.focus_report = true;
                }
                Message::RawExported { status, detail } => {
                    self.status = status;
                    self.report = detail;
                    self.stage = Stage::Finished;
                    self.focus_report = true;
                }
                Message::Error(e) => {
                    self.raid.running = false;
                    self.discovery_running = false;
                    self.status = message!("エラー: {e}", "Error: {e}");
                    self.status_error = true;
                    self.stage = if self.session.is_some() {
                        Stage::Selecting
                    } else {
                        Stage::Settings
                    };
                }
            }
        }
    }
    fn refresh_disks(&mut self) {
        if self.disk_messages.is_some() {
            return;
        }
        let (tx, rx) = mpsc::channel();
        self.disk_messages = Some(rx);
        self.disk_status = message!("ディスクを確認中…", "Checking disks…");
        thread::spawn(move || {
            let _ = tx.send(list_disks().map_err(|e| e.to_string()));
        });
    }
    fn source_ui(&mut self, ui: &mut egui::Ui) {
        let error = self.field_error(Field::Source);
        theme::section(
            ui,
            "01",
            tr!("スキャンするディスク", "Disk to scan"),
            tr!(
                "削除したファイルがあった場所を選びます。",
                "Choose where the deleted files were stored."
            ),
        );
        ui.horizontal_wrapped(|ui| {
            let previous = self.source_mode;
            ui.selectable_value(
                &mut self.source_mode,
                SourceMode::Disk,
                tr!("ディスク／ボリューム", "Disk / Volume"),
            );
            ui.selectable_value(
                &mut self.source_mode,
                SourceMode::Image,
                tr!("保存したイメージ", "Saved image"),
            );
            if ui.button(tr!("RAIDを構成", "Configure RAID")).clicked() {
                if rescue_core::is_raid_path(Path::new(&self.source)) {
                    self.raid.load_source(Path::new(&self.source));
                }
                self.raid.dialog = true;
            }
            if previous != self.source_mode {
                self.source.clear();
            }
        });
        if self.source_mode == SourceMode::Disk {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.disk_messages.is_none(),
                        egui::Button::new(tr!("ディスク一覧を更新", "Refresh disk list")),
                    )
                    .clicked()
                {
                    self.refresh_disks();
                }
                ui.weak(&self.disk_status);
            });
            egui::ScrollArea::vertical()
                .id_salt("disks")
                .max_height(145.0)
                .show_accessible(ui, tr!("ディスク一覧", "Disk list"), |ui| {
                    for disk in &self.disks {
                        let path = disk.path.to_string_lossy();
                        let kind = if disk.kind == "disk" {
                            tr!("ディスク全体", "Whole disk")
                        } else {
                            tr!("ボリューム", "Volume")
                        };
                        let label = format!(
                            "{kind}   {}   {}   {}",
                            disk.name,
                            view::size_text(Some(disk.bytes)),
                            path
                        );
                        if ui.selectable_label(self.source == path, label).clicked() {
                            self.source = path.into_owned();
                        }
                    }
                });
            checked_text_row(
                ui,
                tr!("選択したディスク", "Selected disk"),
                &mut self.source,
                if cfg!(windows) {
                    tr!(
                        r"例: \\.\PhysicalDrive1 または \\.\D:",
                        "Example: \\\\.\\PhysicalDrive1 or \\\\.\\D:"
                    )
                } else if cfg!(target_os = "macos") {
                    tr!(
                        "/dev/rdisk2 または /dev/rdisk2s1",
                        "/dev/rdisk2 or /dev/rdisk2s1"
                    )
                } else {
                    tr!("/dev/sdb または /dev/sdb1", "/dev/sdb or /dev/sdb1")
                },
                error.as_deref(),
            );
            ui.small(tr!("ディスク全体はMBR／GPTの各パーティションを検出します。読み取り専用でアクセスします。", "A whole-disk scan finds MBR / GPT partitions. The source is opened read-only."));
            #[cfg(windows)]
            self.read_access.show(ui, &self.source);
            #[cfg(not(windows))]
            ui.small(tr!(
                "ディスクを読み取れる権限が必要です。",
                "Permission to read the disk is required."
            ));
        } else {
            checked_path_row(
                ui,
                tr!("イメージファイル", "Image file"),
                &mut self.source,
                Picker::Open,
                error.as_deref(),
            );
            ui.small(tr!(
                "RAWボリュームまたはMBR／GPTディスクイメージを読み取ります。",
                "Read a RAW volume or an MBR / GPT disk image."
            ));
        }
    }
    fn source_card_ui(&mut self, ui: &mut egui::Ui) {
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            self.source_ui(ui);
            ui.collapsing(tr!("ディスクのツール", "Disk tools"), |ui| {
                if ui
                    .add_enabled(
                        !self.source.trim().is_empty(),
                        egui::Button::new(tr!(
                            "ディスクのイメージを作成・再開",
                            "Create or resume a disk image"
                        )),
                    )
                    .clicked()
                {
                    self.image_dialog = true;
                }
                if self.source_mode == SourceMode::Image
                    && ui
                        .add_enabled(
                            !self.source.trim().is_empty(),
                            egui::Button::new(tr!("読取マップを確認", "View read map")),
                        )
                        .clicked()
                {
                    self.map_source = PathBuf::from(&self.source);
                    self.map_page = None;
                    self.map_dialog = true;
                    self.load_map_page(0, None);
                }
                self.discovery_controls(ui);
            });
        });
    }
    fn save_card_ui(&mut self, ui: &mut egui::Ui) {
        let error = self.field_error(Field::Destination);
        theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            theme::section(ui, "02", tr!("復元したファイルの保存先", "Where to save recovered files"), tr!("別のディスクへの保存をおすすめします。", "A destination on another disk is recommended."));
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.save_mode, SaveMode::Original, tr!("元の場所へ戻す", "Original locations"));
                ui.selectable_value(&mut self.save_mode, SaveMode::Directory, tr!("フォルダーに保存", "Save to folder"));
                if ui.selectable_value(&mut self.save_mode, SaveMode::Zip, tr!("ZIPファイルに保存", "Save as ZIP")).changed()
                    && !self.destination.to_lowercase().ends_with(".zip") { self.destination.clear(); }
            });
            if self.save_mode != SaveMode::Original {
                let picker = if self.save_mode == SaveMode::Zip { Picker::Zip } else { Picker::Folder };
                checked_path_row(ui, tr!("保存先", "Destination"), &mut self.destination, picker, error.as_deref());
            } else {
                ui.label(tr!("確認できた元のフォルダーとファイル名で戻します。元の場所が不明な候補はフォルダー保存を選んでください。", "Restore verified original folders and filenames. Choose a destination folder for files whose original locations are unknown."));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(tr!("同じ名前がある場合", "When a name already exists"));
                ui.selectable_value(&mut self.collision, CollisionPolicy::KeepBoth, tr!("両方を残す（復元 1）", "Keep both (numbered copy)"));
                ui.selectable_value(&mut self.collision, CollisionPolicy::Skip, tr!("スキップ", "Skip"));
            });
            ui.small(tr!("既存ファイルは上書きしません。同名フォルダーは統合します。", "Existing files are kept. Folders with the same name are merged."));
            });
    }
    fn setup_ui(&mut self, ui: &mut egui::Ui) {
        let issues = self.configuration_issues();
        let advanced_invalid = issues.iter().any(|issue| issue.field.advanced());
        let error = |field| {
            issues
                .iter()
                .find(|issue| issue.field == field)
                .map(|issue| issue.message.to_string())
        };
        egui::ScrollArea::vertical().id_salt("settings-scroll").auto_shrink([false,false]).show_accessible(ui, tr!("設定", "Settings"), |ui| {
            ui.set_max_width(ui.available_width());
            ui.add_space(8.0);
            ui.heading(tr!("ファイルを取り戻しましょう", "Recover your files"));
            ui.label(tr!("対象と保存先を選んでスキャン。見つかったファイルを確認してから復元します。", "Choose a source and destination. Scan, review the results, then recover."));
            ui.add_space(16.0);
            if ui.available_width() >= 960.0 {
                ui.columns(2, |columns| {
                    self.source_card_ui(&mut columns[0]);
                    self.save_card_ui(&mut columns[1]);
                });
            } else {
                self.source_card_ui(ui);
                ui.add_space(12.0);
                self.save_card_ui(ui);
            }
            ui.add_space(12.0);
            theme::card(ui).show(ui, |ui| {
            ui.set_width(ui.available_width());
            egui::CollapsingHeader::new(tr!("詳細設定 · 検索方式・範囲・深度・拡張子", "Advanced · Method, range, depth and extensions")).id_salt("advanced-settings").open(if advanced_invalid { Some(true) } else { None }).show(ui, |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr!("検索方式", "Search method"));
                    ui.selectable_value(&mut self.carve, false, tr!("管理情報 / Quick", "Metadata / Quick"));
                    ui.selectable_value(&mut self.carve, true, tr!("写真・文書・動画 / Carving", "Photos, documents and videos / Carving"));
                });
                checked_text_row(ui, tr!("探す拡張子", "File extensions to find"), &mut self.extensions, tr!("空欄: すべて / 例: jpg, pdf, docx, zip", "Blank: all / Example: jpg, pdf, docx, zip"), error(Field::Extensions).as_deref());
                if self.carve {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr!("割当範囲", "Allocation scope"));
                        ui.selectable_value(&mut self.carve_all,false,tr!("未割当領域（推奨）", "Unallocated regions (recommended)"));
                        ui.selectable_value(&mut self.carve_all,true,tr!("全領域", "All regions"));
                    });
                    ui.small(tr!("割当情報が不明・破損している範囲は全領域へ切り替え、結果に理由を表示します。", "Regions with unavailable or damaged allocation data are searched in full, with the reason shown in the results."));
                    ui.label(tr!("PNG/JPEG、PDF、ZIP/Office、MP4/MOVを探索します。形式ごとに構造・CRC・参照位置を確認します。元の名前・削除状態は不明です。", "Find PNG/JPEG, PDF, ZIP/Office and MP4/MOV. Validate structure, CRC or references as appropriate. Original names and deletion status are unknown."));
                    checked_text_row(ui, tr!("開始バイト", "Start byte"), &mut self.range_start, tr!("空欄: 0（ディスク/イメージの先頭）", "Blank: 0 (start of the disk / image)"), error(Field::RangeStart).as_deref());
                    checked_text_row(ui, tr!("終了バイト", "End byte"), &mut self.range_end, tr!("空欄: 末尾 / この位置は含みません", "Blank: end of source / This position is excluded"), error(Field::RangeEnd).as_deref());
                } else {
                    ui.checkbox(&mut self.estimate_fat_contiguous, tr!("FATの連続配置を推定する（元の内容との一致は未確認）", "Estimate contiguous FAT files (original content unverified)"));
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr!("FATの短い名前の文字コード", "FAT short-name encoding"));
                        egui::ComboBox::from_id_salt("fat-short-name-encoding")
                            .selected_text(match self.fat_short_name_encoding {
                                rescue_core::FatShortNameEncoding::Ascii => tr!("ASCII（従来どおり）", "ASCII (legacy behavior)"),
                                rescue_core::FatShortNameEncoding::Cp932 => tr!("日本語 CP932", "Japanese CP932"),
                            }).show_ui(ui, |ui| {
                                ui.selectable_value(&mut self.fat_short_name_encoding, rescue_core::FatShortNameEncoding::Ascii, tr!("ASCII（従来どおり）", "ASCII (legacy behavior)"));
                                ui.selectable_value(&mut self.fat_short_name_encoding, rescue_core::FatShortNameEncoding::Cp932, tr!("日本語 CP932", "Japanese CP932"));
                            });
                    });
                    if self.fat_short_name_encoding == rescue_core::FatShortNameEncoding::Cp932 {
                        ui.small(tr!("FATの8.3名だけに適用します。消えた先頭文字は補わず、曖昧なバイトは {16進数} で示します。", "Applies only to FAT 8.3 names. Missing leading characters are not invented; ambiguous bytes appear as {hex}."));
                    }
                    ui.checkbox(&mut self.exfat_allow_damaged_upcase, tr!("exFATの大文字変換表が壊れていても探索する（名前の検証は不完全）", "Scan with a damaged exFAT upcase table (names incompletely verified)"));
                    if self.exfat_allow_damaged_upcase {
                        ui.small(tr!("表の内容・チェックサムの破損に限定します。配置・割当・エントリの検証は維持し、名前ハッシュの未検証を結果に残します。", "Limited to table content/checksum damage. Extents, allocation and entry checksums remain required; missing name-hash verification is recorded."));
                    }
                    ui.checkbox(&mut self.fragments,tr!("残存するMFTレコードも探索（クイックフォーマット後など）", "Also search for remnant MFT records (for example, after a quick format)"));
                    checked_text_row(ui, tr!("検索フォルダー", "Search folder"), &mut self.path_prefix, tr!("空欄: 全体 / 例: 写真/旅行", "Blank: all / Example: Photos/Travel"), error(Field::Folder).as_deref());
                    checked_text_row(ui, tr!("検索深度", "Search depth"), &mut self.depth, tr!("空欄: 上限なし / 0: 直下 / 最大256", "Blank: unlimited / 0: direct children / Maximum: 256"), error(Field::Depth).as_deref());
                    ui.small(tr!("範囲・深度・拡張子は結果を絞ります。割当の競合確認はボリューム全体を調べます。", "Range, depth and extensions filter the results. Allocation conflicts are checked across the full volume."));
                }
                checked_path_row(ui, tr!("再開用の作業フォルダー（任意）", "Working folder for resume (optional)"), &mut self.work, Picker::Folder, error(Field::Work).as_deref());
                ui.small(tr!("空欄では保存先の近くに索引を作ります。元の場所への復元・同じ物理ディスクの場合はメモリーに保持します。再開には安全な作業フォルダーを指定してください。", "If blank, an index is created near the destination. Original-location or same-physical-disk recovery holds results in memory. Set a suitable working folder to enable resume."));
                if self.save_mode == SaveMode::Original {
                    checked_path_row(ui, tr!("元ボリュームのルート（任意）", "Original volume root (optional)"), &mut self.original_root, Picker::Folder, error(Field::OriginalRoot).as_deref());
                    ui.small(tr!("通常は自動判定します。イメージなど判定できない場合は元ボリュームのルートを指定します。複数パーティションの個別指定はCLIを使います。", "Usually detected automatically. Specify the original volume root for images or when detection fails. Use the CLI to map multiple partitions individually."));
                }
            });
            });
            self.invalidate_changed_source();
            ui.add_space(16.0);
            ui.small(tr!("同じディスクにも保存できます。選択したファイルをすべて先に読み出します（合計512 MiBまで）。同じディスクへの保存は、未選択の削除データを上書きする可能性があります。", "You can save to the same disk. All selected files are read first (up to 512 MiB total). Saving to the same disk may overwrite deleted data you have not selected."));
        });
    }
    /// Drawn after content so Tab order follows the form, then the stable action row.
    fn actions_ui(&mut self, ui: &mut egui::Ui) {
        self.remember_selection_sizes();
        ui.separator();
        if self.stage == Stage::Settings {
            ui.horizontal_wrapped(|ui| {
                if ui.button(tr!("保存したセッションを開く", "Open saved session")).clicked() {
                    self.session_dialog = Some(session_ui::Action::Open);
                }
                if ui
                    .button(tr!("セッションをコピー移行", "Migrate a session copy"))
                    .on_hover_text(
                        tr!("旧JSON・SQLiteを新しい索引へコピーします。元の記録は変更しません。", "Copy an old JSON or SQLite session into a new index. Keep the original unchanged."),
                    )
                    .clicked()
                {
                    self.session_dialog = Some(session_ui::Action::Copy);
                }
                if self.session.is_some() && ui.button(tr!("結果へ戻る", "Return to results")).clicked() {
                    self.stage = Stage::Selecting;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = ui.add(theme::primary(ui, tr!("スキャン開始", "Start scan"), 176.0));
                self.primary_rect = Some(response.rect);
                if response.clicked() {
                    self.start_scan();
                    if !self.configuration_issues().is_empty() {
                        theme::reveal_errors(ui.ctx());
                    }
                }
                ui.weak(tr!(
                    "ファイルの選択はスキャン後",
                    "Select files after scanning"
                ));
            });
        } else if !self.busy() {
            let bytes = self.selected_bytes();
            ui.horizontal_wrapped(|ui| {
                ui.label(tr_format!(
                    "{} 件選択 · {}",
                    "{} selected · {}",
                    self.selected.len(),
                    view::size_text(Some(bytes))
                ));
                ui.label(match self.save_mode {
                    SaveMode::Original => tr!("元の場所へ戻す", "Original locations"),
                    SaveMode::Directory => tr!("フォルダーに保存", "Save to folder"),
                    SaveMode::Zip => tr!("ZIPに保存", "Save as ZIP"),
                });
                if self.save_mode != SaveMode::Original {
                    ui.add(egui::Label::new(&self.destination).truncate())
                        .on_hover_text(&self.destination);
                }
            });
            ui.horizontal(|ui| {
                if ui.button(tr!("設定に戻る", "Back to settings")).clicked() {
                    self.stage = Stage::Settings;
                }
                let unrecovered = self.unrecovered_ids().len();
                if self.stage == Stage::Finished
                    && unrecovered > 0
                    && ui
                        .button(tr_format!(
                            "復元できなかった {} 件をバイナリで書き出す",
                            "Export {} unrecovered as raw binary",
                            unrecovered
                        ))
                        .on_hover_text(tr!(
                            "読めた部分をそのまま書き出し、読めない部分は0で埋めて範囲を記録します。内容は検証されていません。",
                            "Writes the readable bytes as-is, fills unreadable parts with zeros and records the ranges. Content is not verified."
                        ))
                        .clicked()
                {
                    self.start_raw_export();
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let response = ui.add_enabled(
                        !self.selected.is_empty() && !self.index.loading(),
                        theme::primary(ui, tr!("選択したファイルを復元", "Recover selected files"), 224.0),
                    );
                    self.primary_rect = Some(response.rect);
                    if response.clicked() {
                        self.start_recovery();
                        if self.stage == Stage::Settings && !self.configuration_issues().is_empty() {
                            theme::reveal_errors(ui.ctx());
                        }
                    }
                });
            });
        } else {
            ui.label(tr!(
                "進行状況と取消は画面下に表示しています。",
                "Progress and cancellation controls are at the bottom of the window."
            ));
        }
        if !self.report.is_empty() && self.stage != Stage::Settings {
            // Keep the report after the main actions in both visual and Tab
            // order. Use the reserved height for this frame; resize next frame.
            let was_open = self.show_report;
            let report = ui.toggle_value(
                &mut self.show_report,
                tr!("保存場所・レポート", "Saved locations and reports"),
            );
            if self.focus_report && !self.busy() {
                report.request_focus();
                self.focus_report = false;
            }
            if report.changed() {
                ui.ctx().request_repaint();
            }
            if was_open {
                egui::ScrollArea::vertical()
                    .id_salt("saved-report-body")
                    .max_height(96.0)
                    .show_accessible(ui, tr!("保存レポート", "Saved reports"), |ui| {
                        if !self.results.is_empty() && !self.busy() {
                            ui.horizontal_wrapped(|ui| {
                                if ui
                                    .button(tr!("HTMLレポートを保存", "Save HTML report"))
                                    .clicked()
                                {
                                    self.export_report(rescue_core::ReportFormat::Html);
                                }
                                if ui
                                    .button(tr!("CSVレポートを保存", "Save CSV report"))
                                    .clicked()
                                {
                                    self.export_report(rescue_core::ReportFormat::Csv);
                                }
                                if ui
                                    .button(tr!("既知ハッシュと照合", "Compare known hashes"))
                                    .clicked()
                                    && let Some(session) = self.session.as_ref()
                                {
                                    let destination = PathBuf::from(&self.destination);
                                    let folder = if self.save_mode == SaveMode::Zip {
                                        destination.parent().unwrap_or(Path::new(".")).to_owned()
                                    } else if self.destination.is_empty() {
                                        PathBuf::from(".")
                                    } else {
                                        destination
                                    };
                                    self.hashes.start(
                                        rescue_core::recovery_report(session, &self.results),
                                        &folder,
                                    );
                                }
                            });
                        }
                        ui.add(egui::Label::new(&self.report).wrap());
                    });
            }
        }
    }
    fn export_report(&mut self, format: rescue_core::ReportFormat) {
        let destination = PathBuf::from(&self.destination);
        let folder = if self.save_mode == SaveMode::Zip {
            destination.parent().unwrap_or(Path::new(".")).to_owned()
        } else if self.destination.is_empty() {
            PathBuf::from(".")
        } else {
            destination
        };
        self.report_export = Some((
            format,
            folder
                .join(format!("rescue-report.{}", format.extension()))
                .display()
                .to_string(),
            String::new(),
        ));
    }
    fn report_export_ui(&mut self, ctx: &egui::Context) {
        let Some((format, mut path, mut error)) = self.report_export.take() else {
            return;
        };
        let mut close = false;
        let response = responsive_modal(ctx, "export-recovery-report", 640.0).show(ctx, |ui| {
            ui.heading(tr!("復元レポートを保存", "Save recovery report"));
            ui.label(tr!("保存結果・警告・元データとの一致が未確認であることを含めます。既存ファイルは上書きしません。", "Includes saved results, warnings and unverified content. Existing files are never replaced."));
            let label = ui.label(tr!("レポートの保存先", "Report output path"));
            text_input::singleline(ui, &mut path, egui::Id::new("report-output-path"), label.id, ui.available_width(), "");
            if !error.is_empty() { ui.colored_label(theme::palette(ui).warning, &error); }
            ui.horizontal(|ui| {
                if ui.button(tr!("参照", "Browse")).clicked()
                    && let Some(chosen) = rfd::FileDialog::new().set_file_name(format!("rescue-report.{}", format.extension())).add_filter(format.extension(), &[format.extension()]).save_file() {
                        path = chosen.display().to_string();
                    }
                if ui.button(tr!("保存", "Save")).clicked() && let Some(session) = self.session.as_ref() {
                    let report = rescue_core::recovery_report(session, &self.results);
                    match rescue_core::export_recovery_report(&report, format, Path::new(&path)) {
                        Ok(()) => { self.status = message!("レポートを保存: {path}", "Report saved: {path}"); close = true; }
                        Err(e) => error = e.to_string(),
                    }
                }
                if ui.button(tr!("取消", "Cancel")).clicked() { close = true; }
            });
        });
        if !close && !response.should_close() {
            self.report_export = Some((format, path, error));
        }
    }
    fn results_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label(
                egui::RichText::new(if self.stage == Stage::Finished {
                    tr!("復元結果", "Recovery results")
                } else {
                    tr!("見つかったファイル", "Discovered files")
                })
                .size(21.0)
                .strong(),
            );
            theme::badge(
                ui,
                tr_format!("{} 件選択", "{} selected", self.selected.len()),
                false,
            );
            let estimated = self.session.as_ref().map_or(0, |s| {
                s.candidates
                    .iter()
                    .filter(|c| {
                        c.extent_evidence == rescue_core::ExtentEvidence::EstimatedContiguous
                    })
                    .count()
            });
            if estimated > 0 {
                theme::badge(
                    ui,
                    tr_format!(
                        "表示中 {} 件は推定・内容未確認",
                        "{} shown are estimated; content unverified",
                        estimated
                    ),
                    true,
                );
            }
            if self
                .session
                .as_ref()
                .is_some_and(|s| s.cancelled || s.truncated)
            {
                theme::badge(ui, tr!("一部の結果", "Partial results"), true);
            }
        });
        if self.session.as_ref().is_some_and(|s| {
            s.warnings
                .iter()
                .any(|w| w.starts_with("ALLOCATION_FALLBACK:"))
        }) {
            ui.colored_label(theme::palette(ui).warning, tr!("一部の領域で割当情報を確認できないため、その領域は全体を検索しています。詳細は検証情報をご確認ください。", "Some regions have no verifiable allocation data and are being searched in full. See the evidence for details."));
        }
        let busy = self.busy();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr!("表示", "View"));
                ui.selectable_value(&mut self.view, ViewStyle::Details, tr!("詳細", "Details"));
                ui.selectable_value(&mut self.view, ViewStyle::Icons, tr!("アイコン", "Icons"));
                ui.separator();
                let group_label = ui.label(tr!("グループ", "Group"));
                egui::ComboBox::from_id_salt("group")
                    .selected_text(self.group.label())
                    .show_ui(ui, |ui| {
                        for g in GroupBy::ALL {
                            if ui.selectable_value(&mut self.group, g, g.label()).changed() {
                                self.closed.clear();
                                self.view_dirty = true;
                            }
                        }
                    })
                    .response
                    .labelled_by(group_label.id);
                ui.checkbox(
                    &mut self.show_evidence,
                    tr!("復元の根拠", "Recovery evidence"),
                );
                if ui
                    .add_enabled(
                        !self.index.loading() && self.focused.is_some(),
                        egui::Button::new(tr!("プレビュー", "Preview")),
                    )
                    .clicked()
                    && let (Some(session), Some(id)) = (&self.session, &self.focused)
                {
                    self.preview.start(session.clone(), id);
                }
            });
            ui.horizontal_wrapped(|ui| {
                let search_label = ui.label(tr!("結果を検索", "Search results"));
                if text_input::singleline(
                    ui,
                    &mut self.filter,
                    egui::Id::new("result-search"),
                    search_label.id,
                    280.0,
                    tr!("名前・元のパス", "Name or original path"),
                )
                .changed()
                {
                    self.view_dirty = true;
                }
                if ui
                    .add_enabled(
                        !self.index.loading()
                            && (self.index.path.is_none()
                                || self.index.query == self.index_query()),
                        egui::Button::new(tr!("表示中の候補を選択", "Select visible candidates")),
                    )
                    .clicked()
                    && let Some(s) = &self.session
                {
                    let needle = self.filter.to_lowercase();
                    for c in &s.candidates {
                        if c.supported && c.matches_path_text(&needle) {
                            set_selected(&mut self.selected, &c.id, true);
                        }
                    }
                }
                if ui.button(tr!("選択解除", "Clear selection")).clicked() {
                    self.selected.clear();
                }
            });
        });
        self.indexed_controls(ui);
        if self.view_dirty {
            if let Some(s) = &self.session {
                self.rows = if self.index.path.is_some() {
                    view::indexed_rows(&s.candidates, self.group, &self.closed)
                } else {
                    view::rows(
                        &s.candidates,
                        &self.filter,
                        self.sort,
                        self.descending,
                        self.group,
                        &self.closed,
                    )
                };
            }
            self.view_dirty = false;
        }
        ui.separator();
        let height = ui.available_height().max(32.0);
        ui.allocate_ui(egui::vec2(ui.available_width(), height), |ui| {
            if self.view == ViewStyle::Details {
                self.details_ui(ui);
            } else {
                self.icons_ui(ui);
            }
        });
    }
    fn details_ui(&mut self, ui: &mut egui::Ui) {
        ui.spacing_mut().item_spacing.y = 3.0;
        let busy = self.busy()
            || self.index.loading()
            || (self.index.path.is_some() && self.index.query != self.index_query());
        let mut sort_clicked = None;
        let mut toggle = None;
        let Some(session) = &self.session else {
            return;
        };
        egui::ScrollArea::horizontal().show_accessible(
            ui,
            tr!("ファイル一覧", "Files"),
            |ui| {
                ui.set_min_width((self.column_widths.iter().sum::<f32>() + 173.0).max(1030.0));
                let top = ui.cursor().top();
                let spacing = ui.spacing().item_spacing.x;
                // TableRow::col returns the contents' used rectangle, which can
                // be smaller than the column. Boundaries follow actual widths.
                let mut left = ui.cursor().left() + 27.0 + spacing;
                let edges = self.column_widths.map(|width| {
                    let edge = left + width + spacing * 0.5;
                    left += width + spacing;
                    edge
                });
                let mut table_scroll = TableBuilder::new(ui)
                    .id_salt("files-table")
                    .min_scrolled_height(0.0)
                    .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                    .column(Column::exact(27.0))
                    .column(Column::exact(self.column_widths[0]))
                    .column(Column::exact(self.column_widths[1]))
                    .column(Column::exact(self.column_widths[2]))
                    .column(Column::exact(self.column_widths[3]))
                    .column(Column::exact(self.column_widths[4]))
                    .column(Column::remainder().at_least(90.0))
                    .header(40.0, |mut row| {
                        row.col(|_| {});
                        for (index, (col, label)) in [
                            (SortColumn::Name, tr!("名前", "Name")),
                            (SortColumn::Created, tr!("作成日時", "Date created")),
                            (SortColumn::Modified, tr!("更新日時", "Date modified")),
                            (SortColumn::Type, tr!("種類", "Type")),
                            (SortColumn::Size, tr!("サイズ", "Size")),
                            (SortColumn::State, tr!("状態", "Status")),
                        ]
                        .into_iter()
                        .enumerate()
                        {
                            row.col(|ui| {
                                let text = format!(
                                    "{label}{}",
                                    if self.sort == col {
                                        if self.descending { "  ↓" } else { "  ↑" }
                                    } else {
                                        ""
                                    }
                                );
                                let response = ui.selectable_label(self.sort == col, text);
                                if let Some(width) = self.column_widths.get_mut(index) {
                                    columns::keyboard_width(ui, &response, width, index);
                                }
                                if response.clicked() {
                                    sort_clicked = Some(col);
                                }
                            });
                        }
                    })
                    .body(|body| {
                        body.rows(32.0, self.rows.len(), |mut row| {
                            match &self.rows[row.index()] {
                                Row::Group(name, count) => {
                                    row.col(|ui| {
                                        if ui
                                            .small_button(if self.closed.contains(name) {
                                                "+"
                                            } else {
                                                "−"
                                            })
                                            .clicked()
                                        {
                                            toggle = Some(name.clone());
                                        }
                                    });
                                    row.col(|ui| {
                                        ui.horizontal(|ui| {
                                            folder_icon(ui, 17.0);
                                            if ui
                                                .selectable_label(
                                                    false,
                                                    format!("{name}  ({count})"),
                                                )
                                                .clicked()
                                            {
                                                toggle = Some(name.clone());
                                            }
                                        });
                                    });
                                    for _ in 0..5 {
                                        row.col(|_| {});
                                    }
                                }
                                Row::File(i) => {
                                    let c = &session.candidates[*i];
                                    let mut checked = self.selected.contains(&c.id);
                                    row.set_selected(checked);
                                    row.col(|ui| {
                                        let response = ui.add_enabled(
                                            c.supported && !busy,
                                            egui::Checkbox::without_text(&mut checked),
                                        );
                                        response.widget_info(|| {
                                            egui::WidgetInfo::selected(
                                                egui::WidgetType::Checkbox,
                                                c.supported && !busy,
                                                checked,
                                                tr_format!(
                                                    "{}を復元対象に選択",
                                                    "Select {} for recovery",
                                                    c.name
                                                ),
                                            )
                                        });
                                        if response.changed() {
                                            set_selected(&mut self.selected, &c.id, checked);
                                        }
                                        if response.changed() || response.has_focus() {
                                            self.focused = Some(c.id.clone());
                                        }
                                    });
                                    row.col(|ui| {
                                        ui.horizontal(|ui| {
                                            file_icon(ui, 16.0, c);
                                            let label = ui
                                                .add(
                                                    egui::Label::new(&c.name)
                                                        .truncate()
                                                        .sense(egui::Sense::click()),
                                                )
                                                .on_hover_text(format!(
                                                    "{}\n{}",
                                                    c.original_path, c.id
                                                ));
                                            if label.has_focus() || label.clicked() {
                                                self.focused = Some(c.id.clone());
                                            }
                                            if label.clicked() && c.supported && !busy {
                                                set_selected(&mut self.selected, &c.id, !checked);
                                            }
                                        });
                                    });
                                    row.col(|ui| {
                                        ui.add(
                                            egui::Label::new(view::date_text(&c.created))
                                                .truncate(),
                                        )
                                        .on_hover_text(view::date_hint(&c.created));
                                    });
                                    row.col(|ui| {
                                        ui.add(
                                            egui::Label::new(view::date_text(&c.modified))
                                                .truncate(),
                                        )
                                        .on_hover_text(view::date_hint(&c.modified));
                                    });
                                    row.col(|ui| {
                                        ui.add(egui::Label::new(view::file_type(c)).truncate());
                                    });
                                    row.col(|ui| {
                                        ui.with_layout(
                                            egui::Layout::right_to_left(egui::Align::Center),
                                            |ui| {
                                                ui.label(view::size_text(c.size)).on_hover_text(
                                                    c.size
                                                        .map(|n| format!("{n} bytes"))
                                                        .unwrap_or_default(),
                                                );
                                            },
                                        );
                                    });
                                    row.col(|ui| {
                                        let saved = self
                                            .results
                                            .iter()
                                            .rev()
                                            .find(|r| r.candidate_id == c.id);
                                        ui.label(
                                            saved
                                                .map(|r| if r.status == "completed" && c.extent_evidence == rescue_core::ExtentEvidence::EstimatedContiguous { tr!("保存済み（推定）", "Saved (estimated)") } else { status_label(&r.status) })
                                                .unwrap_or_else(|| view::state(c)),
                                        )
                                        .on_hover_text(c.warnings.join("\n"));
                                    });
                                }
                            }
                        })
                    });
                scrolling::finish(ui, &mut table_scroll, tr!("詳細一覧", "Details list"));
                columns::boundaries(
                    ui,
                    &edges,
                    top,
                    ui.min_rect().bottom(),
                    &mut self.column_widths,
                );
            },
        );
        if let Some(col) = sort_clicked {
            if self.sort == col {
                self.descending = !self.descending;
            } else {
                self.sort = col;
                self.descending = false;
            }
            self.view_dirty = true;
        }
        if let Some(key) = toggle {
            if !self.closed.remove(&key) {
                self.closed.insert(key);
            }
            self.view_dirty = true;
        }
    }
    fn icons_ui(&mut self, ui: &mut egui::Ui) {
        let Some(s) = &self.session else {
            return;
        };
        let busy = self.busy()
            || self.index.loading()
            || (self.index.path.is_some() && self.index.query != self.index_query());
        let cols = (ui.available_width() / 132.0).floor().max(1.0) as usize;
        let mut tiles = Vec::<Vec<&Row>>::new();
        let mut chunk = Vec::new();
        for row in &self.rows {
            if matches!(row, Row::Group(..)) {
                if !chunk.is_empty() {
                    tiles.push(std::mem::take(&mut chunk));
                }
                tiles.push(vec![row]);
            } else {
                chunk.push(row);
                if chunk.len() == cols {
                    tiles.push(std::mem::take(&mut chunk));
                }
            }
        }
        if !chunk.is_empty() {
            tiles.push(chunk);
        }
        let mut toggle = None;
        let heights = tiles
            .iter()
            .map(|row| {
                if matches!(row[0], Row::Group(..)) {
                    30.0
                } else {
                    126.0
                }
            })
            .collect::<Vec<_>>();
        let mut table_scroll = TableBuilder::new(ui)
            .id_salt("icon-table")
            .column(Column::remainder())
            .body(|body| {
                body.heterogeneous_rows(heights.into_iter(), |mut table_row| {
                    let i = table_row.index();
                    table_row.col(|ui| {
                        ui.horizontal(|ui| {
                            for row in &tiles[i] {
                                match row {
                                    Row::Group(name, count) => {
                                        folder_icon(ui, 30.0);
                                        if ui
                                            .selectable_label(
                                                false,
                                                format!(
                                                    "{} {name}  ({count})",
                                                    if self.closed.contains(name) {
                                                        "+"
                                                    } else {
                                                        "−"
                                                    }
                                                ),
                                            )
                                            .clicked()
                                        {
                                            toggle = Some(name.clone());
                                        }
                                    }
                                    Row::File(index) => {
                                        let c = &s.candidates[*index];
                                        ui.allocate_ui(egui::vec2(124.0, 112.0), |ui| {
                                            egui::Frame::new()
                                                .fill(if self.selected.contains(&c.id) {
                                                    ui.visuals().selection.bg_fill
                                                } else {
                                                    egui::Color32::TRANSPARENT
                                                })
                                                .inner_margin(6.0)
                                                .show(ui, |ui| {
                                                    ui.set_width(108.0);
                                                    ui.vertical_centered(|ui| {
                                                        file_icon(ui, 40.0, c);
                                                        let label = ui
                                                            .add(
                                                                egui::Label::new(&c.name)
                                                                    .truncate()
                                                                    .sense(egui::Sense::click()),
                                                            )
                                                            .on_hover_text(&c.original_path);
                                                        if label.has_focus() || label.clicked() {
                                                            self.focused = Some(c.id.clone());
                                                        }
                                                        if label.clicked() && c.supported && !busy {
                                                            let selected =
                                                                !self.selected.contains(&c.id);
                                                            set_selected(
                                                                &mut self.selected,
                                                                &c.id,
                                                                selected,
                                                            );
                                                        }
                                                        ui.small(view::size_text(c.size));
                                                        let mut selected =
                                                            self.selected.contains(&c.id);
                                                        let checkbox = ui.add_enabled(
                                                            c.supported && !busy,
                                                            egui::Checkbox::without_text(
                                                                &mut selected,
                                                            ),
                                                        );
                                                        checkbox.widget_info(|| {
                                                            egui::WidgetInfo::selected(
                                                                egui::WidgetType::Checkbox,
                                                                c.supported && !busy,
                                                                selected,
                                                                tr_format!(
                                                                    "{}を復元対象に選択",
                                                                    "Select {} for recovery",
                                                                    c.name
                                                                ),
                                                            )
                                                        });
                                                        if checkbox.changed()
                                                            || checkbox.has_focus()
                                                        {
                                                            self.focused = Some(c.id.clone());
                                                        }
                                                        if checkbox.changed() {
                                                            set_selected(
                                                                &mut self.selected,
                                                                &c.id,
                                                                selected,
                                                            );
                                                        }
                                                    });
                                                });
                                        });
                                    }
                                }
                            }
                        });
                    });
                });
            });
        scrolling::finish(ui, &mut table_scroll, tr!("アイコン一覧", "Icons list"));
        if let Some(key) = toggle {
            if !self.closed.remove(&key) {
                self.closed.insert(key);
            }
            self.view_dirty = true;
        }
    }
    fn intelligence(&self, ui: &mut egui::Ui) {
        ui.add_space(10.0);
        ui.heading(tr!("復元の根拠", "Recovery evidence"));
        let Some(s) = &self.session else {
            return;
        };
        ui.label(tr_format!(
            "{}{} 候補 / {} 対応",
            "{}{} candidates / {} supported",
            if self.index.path.is_some() {
                tr!("表示中 ", "Showing ")
            } else {
                ""
            },
            s.candidates.len(),
            s.candidates.iter().filter(|c| c.supported).count()
        ));
        ui.collapsing(tr!("ソースの識別と範囲", "Source identity and range"), |ui| {
            ui.add(egui::Label::new(s.source.display().to_string()).wrap());
            if let Some(info) = &s.source_info {
                ui.label(match info.kind {
                    rescue_core::SourceKind::Device => tr!("ディスク／ボリューム", "Disk / Volume"),
                    rescue_core::SourceKind::Image => tr!("ディスクイメージ", "Disk image"),
                    rescue_core::SourceKind::Acquisition => tr!("読取マップ付きイメージ", "Image with read map"),
                    rescue_core::SourceKind::Raid => tr!("仮想RAID", "Virtual RAID"),
                });
                ui.label(tr_format!("容量 {} / セクター {} bytes", "Capacity {} / Sector {} bytes",
                    view::size_text(Some(info.bytes)),
                    info.sector_size
                ));
                ui.label(tr_format!("読取窓：{} から {} bytes", "Read window: {} + {} bytes",
                    info.offset, info.length
                ));
                ui.small(tr!("ソース参照ID", "Source reference ID"));
                ui.add(egui::Label::new(&info.id).wrap());
                if let Some(map) = &info.read_map {
                    ui.label(tr_format!("取得済み {} / 未読 {} / 読取失敗 {}", "Acquired {} / Unread {} / Failed {}",
                        view::size_text(Some(map.summary.copied_bytes)),
                        view::size_text(Some(map.summary.unattempted_bytes)),
                        view::size_text(Some(map.summary.failed_bytes))
                    ));
                }
                ui.small(tr!("IDは媒体の識別用です。復元時には指紋と管理情報も再照合します。", "The ID identifies the source. Fingerprints and metadata are also rechecked before recovery."));
            } else {
                ui.small(tr!("旧版の記録です。復元時には保存した指紋と管理情報を再照合します。", "This is a legacy session. Saved fingerprints and metadata are rechecked before recovery."));
            }
        });
        if let Some(raid) = &s.raid {
            ui.collapsing(tr!("仮想RAIDの構成", "Virtual RAID configuration"),|ui|{
                ui.label(tr_format!("{:?} / {:?} / ストライプ {}", "{:?} / {:?} / Stripe {}",raid.config.level,raid.config.layout,view::size_text(Some(u64::from(raid.config.stripe_bytes)))));
                ui.label(tr_format!("{}メンバー / 欠損 {}", "{} members / {} missing",raid.config.members.len(),raid.config.members.iter().filter(|m|m.path.is_none()).count()));
                ui.small(tr!("この構成で読めた内容です。パリティの一致は元データとの一致保証ではありません。使用メンバーと範囲は復元レポートに記録します。", "These bytes were read using this configuration. Matching parity does not prove equality with the original. Members and ranges used are recorded in the recovery report."));
            });
        }
        if s.cancelled || s.truncated {
            ui.colored_label(
                theme::palette(ui).warning,
                tr!(
                    "未探索の領域が残っています。",
                    "Some regions remain unscanned."
                ),
            );
        }
        ui.separator();
        if let Some(c) = s
            .candidates
            .iter()
            .find(|c| Some(&c.id) == self.focused.as_ref())
        {
            ui.strong(&c.name);
            ui.small(&c.original_path);
            if !c.alternate_names.is_empty() {
                ui.collapsing(
                    tr_format!(
                        "同じデータの別名（{}）",
                        "Other names for the same data ({})",
                        c.alternate_names.len()
                    ),
                    |ui| {
                        ui.label(tr!(
                            "名前が複数あっても、復元するデータは1件です。",
                            "Multiple names refer to one item of data to recover."
                        ));
                        for alias in &c.alternate_names {
                            ui.label(&alias.original_path);
                            ui.small(if alias.path_verified {
                                tr!("親参照を照合済み", "Parent references verified")
                            } else {
                                tr!("階層は未確認", "Folder hierarchy unverified")
                            });
                        }
                    },
                );
            }
            ui.add_space(10.0);
            ui.strong(tr!("配置の根拠", "Extent evidence"));
            ui.label(view::extent_label(c.extent_evidence));
            ui.label(
                match c.extent_evidence {
                    rescue_core::ExtentEvidence::MetadataRemnant => tr!("現行MFT外の残存レコード。選択したブートのクラスタサイズで位置を解釈しています。元の形式や削除状態は未確認です。", "A remnant record outside the current MFT. Locations use the cluster size of the selected boot region. Original format and deletion status remain unverified."),
                    rescue_core::ExtentEvidence::PngValidated => tr!("PNGのCRC・圧縮データ・画素を検証済み。", "PNG CRC, compressed data and pixels verified."),
                    rescue_core::ExtentEvidence::JpegValidated => tr!("JPEGの構造・圧縮データの過不足・画素を検証済み。削除前の内容との一致は不明です。", "JPEG structure, compressed-data completeness and pixels verified. Equality with the original content is unknown."),
                    rescue_core::ExtentEvidence::PdfStructure => tr!("PDFの参照表・ページ階層・ストリーム範囲を検証。描画内容と終端後の余白は未確認です。", "PDF reference tables, page tree and stream bounds verified. Rendering and whitespace after the terminator are unverified."),
                    rescue_core::ExtentEvidence::ZipCrcValidated => tr!("ZIPの管理情報・展開長・全エントリーCRCを検証。Officeの表示内容は未確認です。", "ZIP metadata, expanded lengths and every entry CRC verified. Office rendering is unverified."),
                    rescue_core::ExtentEvidence::MediaStructure => tr!("動画のコンテナー・サンプル位置を検証。映像・音声の全フレーム復号は未実施です。", "Media container and sample locations verified. Full video and audio decoding has not been performed."),
                    rescue_core::ExtentEvidence::Unknown => tr!("配置と形式の検証は未完了です。候補の警告を確認してください。", "Extent and format verification is incomplete. Check the candidate warnings."),
                    _ => tr!("管理情報から配置を取得。形式の内容検証は未実施。", "Extents come from metadata. File-format content has not been validated."),
                },
            );
            ui.separator();
            ui.strong(tr!("割当状態", "Allocation status"));
            ui.label(match c.allocation_status {
                AllocationStatus::Unknown => tr!("不明", "Unknown"),
                AllocationStatus::Unallocated => allocation_label(c, s.candidate_filesystem(c)),
                AllocationStatus::Unreferenced => tr!(
                    "探索した使用中エントリーからの参照なし",
                    "No reference from the live entries scanned"
                ),
                AllocationStatus::Conflict if s.candidate_filesystem(c) == FileSystem::Ntfs => {
                    tr!(
                        "現在は割当済みの領域を含む：削除前の内容か要確認",
                        "Includes currently allocated regions: original content needs verification"
                    )
                }
                AllocationStatus::Conflict => tr!(
                    "使用中領域と競合：復元不可",
                    "Conflicts with live data: recovery unavailable"
                ),
            });
            ui.label(if s.filesystem == FileSystem::Raw {
                tr!(
                    "元の名前・階層・削除状態は不明です。",
                    "Original name, hierarchy and deletion status are unknown."
                )
            } else if c.path_verified {
                tr!(
                    "親参照を照合したパス",
                    "Path with verified parent references"
                )
            } else {
                tr!(
                    "元のパスは未確認または推定を含みます。",
                    "The original path is unverified or partly inferred."
                )
            });
            ui.separator();
            ui.strong(tr!("保存結果", "Save result"));
            if let Some(r) = self.results.iter().rev().find(|r| r.candidate_id == c.id) {
                ui.label(format!("{} / {} bytes", r.status, r.bytes_written));
                if let Some(path) = &r.path {
                    ui.small(tr!("保存した場所", "Saved location"));
                    ui.add(egui::Label::new(path.display().to_string()).wrap());
                }
                if let Some(entry) = &r.archive_entry {
                    ui.small(tr!("ZIP内の名前", "Name inside ZIP"));
                    ui.add(egui::Label::new(entry).wrap());
                }
                if let Some(naming) = &r.naming {
                    ui.collapsing(
                        tr!("元の名前と変更履歴", "Original name and changes"),
                        |ui| {
                            ui.label(match naming.basis {
                                rescue_core::NameBasis::MetadataVerifiedPath => {
                                    tr!(
                                        "元媒体から名前と親参照を照合",
                                        "Name and parent references checked against the source"
                                    )
                                }
                                rescue_core::NameBasis::MetadataUnverifiedPath => {
                                    tr!(
                                        "名前は管理情報から取得・階層は未確認",
                                        "Name from metadata; hierarchy unverified"
                                    )
                                }
                                rescue_core::NameBasis::Generated => {
                                    tr!(
                                        "元の名前は不明・探索時に名前を生成",
                                        "Original name unknown; name generated during scanning"
                                    )
                                }
                            });
                            if let Some(original) = &naming.original_path {
                                ui.small(tr!("元媒体の名前", "Name on the source"));
                                ui.add(egui::Label::new(original).wrap());
                            }
                            if naming.changes.is_empty() {
                                ui.label(tr!("名前の変更なし", "Name unchanged"));
                            }
                            for change in &naming.changes {
                                ui.separator();
                                ui.strong(match change.reason {
                                    rescue_core::NameChangeReason::AlternateStream => {
                                        tr!(
                                            "代替ストリームを通常ファイルとして保存",
                                            "Alternate stream saved as a regular file"
                                        )
                                    }
                                    rescue_core::NameChangeReason::PortableName => {
                                        tr!(
                                            "保存に使えない文字や長さを調整",
                                            "Unsupported characters or length adjusted"
                                        )
                                    }
                                    rescue_core::NameChangeReason::UnverifiedHierarchy => {
                                        tr!("未確認の階層を省略", "Unverified hierarchy omitted")
                                    }
                                    rescue_core::NameChangeReason::HierarchyLimit => {
                                        tr!("階層の長さを調整", "Hierarchy length adjusted")
                                    }
                                    rescue_core::NameChangeReason::DirectoryCollision => {
                                        tr!(
                                            "同名フォルダーとの衝突を解決",
                                            "Folder-name conflict resolved"
                                        )
                                    }
                                    rescue_core::NameChangeReason::FileCollision => {
                                        tr!(
                                            "同名ファイルを両方保持",
                                            "Both files with the same name kept"
                                        )
                                    }
                                    rescue_core::NameChangeReason::ArchiveCollision => {
                                        tr!(
                                            "ZIP内の同名を両方保持",
                                            "Both entries with the same ZIP name kept"
                                        )
                                    }
                                    rescue_core::NameChangeReason::PartitionNamespace => {
                                        tr!(
                                            "パーティション別のフォルダーへ保存",
                                            "Saved in a partition-specific folder"
                                        )
                                    }
                                    rescue_core::NameChangeReason::StreamingFlatOutput => {
                                        tr!(
                                            "大容量保存で階層を省略",
                                            "Hierarchy omitted for streaming output"
                                        )
                                    }
                                    rescue_core::NameChangeReason::UniqueOutputName => {
                                        tr!("重複しない識別名を付加", "Unique identifier added")
                                    }
                                    rescue_core::NameChangeReason::PartialOutput => {
                                        tr!(
                                            "未完了のファイル名へ変更",
                                            "Renamed as an unfinished file"
                                        )
                                    }
                                });
                                ui.add(
                                    egui::Label::new(format!("{}\n↓\n{}", change.from, change.to))
                                        .wrap(),
                                );
                            }
                        },
                    );
                }
                if let Some(h) = &r.sha256 {
                    ui.small(tr!("出力SHA-256", "Output SHA-256"));
                    ui.add(egui::Label::new(h).wrap());
                }
                if let Some(e) = &r.error {
                    ui.label(e);
                }
                if !r.missing_ranges.is_empty() {
                    ui.collapsing(tr!("読めなかった範囲", "Unreadable ranges"), |ui| {
                        for gap in r.missing_ranges.iter().take(100) {
                            ui.label(tr_format!(
                                "元媒体 {} から {} bytes / ファイル位置 {}",
                                "Source {} + {} bytes / File offset {}",
                                gap.source_offset,
                                gap.length,
                                gap.logical_offset.map_or_else(
                                    || tr!("不明", "Unknown").into(),
                                    |at| at.to_string()
                                )
                            ));
                        }
                        if r.missing_ranges.len() > 100 {
                            ui.label(tr!(
                                "残りの範囲は復元レポートに記録しています。",
                                "The recovery report lists the remaining ranges."
                            ));
                        }
                    });
                }
            } else {
                ui.label(tr!("未保存", "Not saved"));
            }
            ui.small(tr!("出力ハッシュは保存バイトの識別値です。削除前の内容との一致は正解データとの照合が必要です。", "The output hash identifies the saved bytes. Equality with the deleted original requires comparison with known original data."));
            ui.collapsing(tr!("候補の警告", "Candidate warnings"), |ui| {
                for w in &c.warnings {
                    ui.label(w);
                }
            });
        }
        ui.collapsing(tr!("スキャンの警告", "Scan warnings"), |ui| {
            for w in &s.warnings {
                ui.label(w);
            }
        });
    }
}
fn status_text(status: &str) -> Text {
    Text::new(
        language::with(Language::Japanese, || status_label(status).to_owned()),
        language::with(Language::English, || status_label(status).to_owned()),
    )
}
fn status_label(status: &str) -> &str {
    match status {
        "completed" => tr!("保存完了", "Saved"),
        "partial" => tr!("一部保存", "Partly saved"),
        "cancelled" => tr!("取消", "Cancelled"),
        "failed" => tr!("失敗", "Failed"),
        "incomplete" => tr!("不完全", "Incomplete"),
        "skipped" => tr!("スキップ", "Skip"),
        _ => status,
    }
}
fn set_selected(ids: &mut BTreeSet<String>, id: &str, selected: bool) {
    if selected {
        if ids.len() < 100_000 {
            ids.insert(id.to_owned());
        }
    } else {
        ids.remove(id);
    }
}
fn allocation_label(c: &Candidate, fs: FileSystem) -> &'static str {
    if c.size == Some(0) {
        return tr!(
            "データクラスターの割当なし（空ファイル）",
            "No data clusters allocated (empty file)"
        );
    }
    match fs {
        FileSystem::Fat12 | FileSystem::Fat16 | FileSystem::Fat32 => {
            tr!("FAT値0：空きとして記録", "FAT value 0: recorded as free")
        }
        FileSystem::Exfat => tr!(
            "割当ビットマップ：空き領域",
            "Allocation bitmap: free regions"
        ),
        _ => tr!("割当なし", "Unallocated"),
    }
}
#[derive(Clone, Copy)]
enum Picker {
    Open,
    Session,
    Folder,
    Zip,
}
/// Top anchoring does not reuse the previous frame's height/width when centering.
/// This keeps an already-open modal inside the first resized frame.
fn responsive_modal(ctx: &egui::Context, name: &str, max_width: f32) -> egui::Modal {
    let id = egui::Id::new(name);
    let width = (ctx.content_rect().width() - 48.0).clamp(240.0, max_width);
    let margin = egui::Frame::popup(&ctx.style()).total_margin().sum().x;
    let x = ((ctx.content_rect().width() - width - margin) / 2.0).max(8.0);
    egui::Modal::new(id)
        .area(egui::Modal::default_area(id).anchor(egui::Align2::LEFT_TOP, egui::vec2(x, 24.0)))
}
fn path_row(ui: &mut egui::Ui, label: &str, value: &mut String, picker: Picker) {
    let label_id = ui.label(label).id;
    ui.horizontal(|ui| {
        text_input::singleline(
            ui,
            value,
            ui.make_persistent_id(label),
            label_id,
            (ui.available_width() - 90.0).max(40.0),
            "",
        );
        if ui
            .add_sized([80.0, 36.0], egui::Button::new(tr!("参照", "Browse")))
            .on_hover_text(tr_format!("{label}を選択", "Choose {label}"))
            .clicked()
        {
            let dialog = rfd::FileDialog::new();
            let path = match picker {
                Picker::Open => dialog.pick_file(),
                Picker::Session => dialog
                    .add_filter("Rescue session / index", &["json", "sqlite", "db"])
                    .pick_file(),
                Picker::Folder => dialog.pick_folder(),
                Picker::Zip => dialog
                    .add_filter("ZIP", &["zip"])
                    .set_file_name("recovered.zip")
                    .save_file(),
            };
            if let Some(p) = path {
                *value = p.to_string_lossy().into_owned();
            }
        }
    });
}
fn text_row(ui: &mut egui::Ui, label: &str, value: &mut String, hint: &str) {
    let label_id = ui.label(label).id;
    text_input::singleline(
        ui,
        value,
        ui.make_persistent_id(label),
        label_id,
        ui.available_width(),
        hint,
    );
}
fn checked_path_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    picker: Picker,
    error: Option<&str>,
) {
    theme::field(ui, error, |ui| path_row(ui, label, value, picker));
}
fn checked_text_row(
    ui: &mut egui::Ui,
    label: &str,
    value: &mut String,
    hint: &str,
    error: Option<&str>,
) {
    theme::field(ui, error, |ui| text_row(ui, label, value, hint));
}
fn grouped(value: impl ToString) -> String {
    let digits = value.to_string();
    let mut out = String::new();
    for (i, ch) in digits.chars().enumerate() {
        if i > 0 && (digits.len() - i).is_multiple_of(3) {
            out.push(',');
        }
        out.push(ch);
    }
    out
}
fn progress_text(progress: &Progress) -> String {
    if progress.unit == rescue_core::ProgressUnit::Bytes {
        tr_format!(
            "読み取り済み {} / {}",
            "Read {} / {}",
            view::size_text(Some(progress.scanned_records)),
            view::size_text(Some(progress.total_records))
        )
    } else {
        tr_format!(
            "解析済み {} / {} レコード",
            "Analyzed {} / {} records",
            grouped(progress.scanned_records),
            grouped(progress.total_records)
        )
    }
}
fn folder_icon(ui: &mut egui::Ui, size: f32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(size, size), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(
        egui::Rect::from_min_max(
            r.min + egui::vec2(0.0, size * 0.16),
            r.min + egui::vec2(size * 0.48, size * 0.45),
        ),
        2.0,
        egui::Color32::from_rgb(235, 176, 45),
    );
    p.rect_filled(
        egui::Rect::from_min_max(
            r.min + egui::vec2(0.0, size * 0.3),
            r.max - egui::vec2(0.0, size * 0.12),
        ),
        2.0,
        egui::Color32::from_rgb(255, 207, 81),
    );
}
fn file_icon(ui: &mut egui::Ui, size: f32, candidate: &Candidate) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(size * 0.78, size), egui::Sense::hover());
    let p = ui.painter();
    let extension = rescue_core::file_extension(&candidate.name, candidate.stream_name.as_deref())
        .to_ascii_uppercase();
    let color = match extension.as_str() {
        "PNG" | "JPG" | "JPEG" | "GIF" | "BMP" | "WEBP" | "TIFF" => {
            egui::Color32::from_rgb(114, 74, 157)
        }
        "MP4" | "MOV" | "AVI" | "MKV" | "M4V" => egui::Color32::from_rgb(178, 87, 33),
        "MP3" | "WAV" | "FLAC" | "AAC" | "M4A" => egui::Color32::from_rgb(44, 119, 99),
        "ZIP" | "7Z" | "RAR" | "GZ" | "BZ2" => egui::Color32::from_rgb(142, 106, 22),
        "PDF" | "DOCX" | "TXT" | "XLSX" | "CSV" | "PPTX" => egui::Color32::from_rgb(44, 100, 165),
        _ => theme::palette(ui).muted,
    };
    p.rect_filled(r.shrink(1.0), 3.0, color.gamma_multiply(0.16));
    p.rect_filled(
        egui::Rect::from_min_max(
            r.min + egui::vec2(1.0, 1.0),
            egui::pos2(r.left() + 4.0, r.bottom() - 1.0),
        ),
        2.0,
        color,
    );
    if size >= 32.0 {
        let short: String = extension.chars().take(4).collect();
        p.text(
            r.center(),
            egui::Align2::CENTER_CENTER,
            short,
            egui::FontId::proportional(10.0),
            color,
        );
        return;
    }
    for y in [0.45, 0.6, 0.75] {
        p.line_segment(
            [
                r.min + egui::vec2(size * 0.17, size * y),
                r.min + egui::vec2(size * 0.58, size * y),
            ],
            egui::Stroke::new(1.0_f32, egui::Color32::from_rgb(90, 118, 150)),
        );
    }
}
impl App {
    fn scan_status_ui(&self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("scan-status").show_accessible(ui, tr!("スキャンの進行状況", "Scan progress"), |ui| {
            ui.add_space(16.0);
            theme::card(ui).show(ui, |ui| {
                ui.set_width(ui.available_width());
                theme::badge(ui, tr!("処理中 · 読み取り専用", "In progress · Read-only"), false);
                ui.heading(if self.raid.running {
                    tr!("RAIDの構成を検査しています", "Checking the RAID configuration")
                } else if self.discovery_running {
                    tr!("失われた領域を探しています", "Searching for lost volumes")
                } else {
                    tr!("復元できるファイルを探しています", "Finding files to recover")
                });
                ui.label(tr!("完了後、見つかったファイルを確認して復元できます。", "When the scan finishes, review the files and choose what to recover."));
                ui.add_space(12.0);
                let known = self.progress.total_records > 0;
                let fraction = if known { self.progress.scanned_records as f32 / self.progress.total_records as f32 } else { 0.0 }.clamp(0.0, 1.0);
                ui.label(egui::RichText::new(if known {
                    format!("{:.1}%", fraction * 100.0)
                } else {
                    tr!("準備・解析中…", "Preparing / analyzing…").into()
                }).size(36.0).strong());
                ui.add(egui::ProgressBar::new(fraction).desired_width(ui.available_width()).desired_height(20.0).fill(theme::palette(ui).primary).animate(!known));
                ui.label(progress_text(&self.progress));
                ui.add_space(8.0);
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr!("見つかった候補", "Candidates found"));
                    ui.label(egui::RichText::new(grouped(self.progress.candidates)).size(26.0).strong().color(theme::palette(ui).accent));
                });
                ui.label(tr!("候補数は復元成功数ではありません。スキャン後に内容と警告を確認してください。", "Candidate counts are not successful recoveries. Review their contents and warnings after scanning."));
                ui.separator();
                ui.strong(tr!("読み取り元", "Source"));
                ui.add(egui::Label::new(&self.source).wrap());
                ui.add_space(4.0);
                ui.small(tr!("中断・取消は画面下のボタンから操作できます。", "Use the buttons below to pause or cancel."));
            });
        });
    }

    fn render(&mut self, ctx: &egui::Context) {
        let _language = self.language.enter();
        self.primary_rect = None;
        if matches!(self.stage, Stage::Selecting | Stage::Finished)
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F))
        {
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("result-search")));
        }
        if self.busy()
            || self.disk_messages.is_some()
            || self.map_messages.is_some()
            || self.index.loading()
        {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::TopBottomPanel::top("title").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal_wrapped(|ui| {
                ui.label(
                    egui::RichText::new("Rescue")
                        .size(25.0)
                        .strong()
                        .color(theme::palette(ui).accent),
                );
                ui.weak(rescue_core::VERSION);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let previous = self.appearance;
                    egui::ComboBox::from_label(tr!("配色", "Appearance"))
                        .selected_text(appearance::label(self.appearance))
                        .width(100.0)
                        .show_ui(ui, |ui| {
                            for choice in [
                                egui::ThemePreference::System,
                                egui::ThemePreference::Light,
                                egui::ThemePreference::Dark,
                            ] {
                                ui.selectable_value(
                                    &mut self.appearance,
                                    choice,
                                    appearance::label(choice),
                                );
                            }
                        })
                        .response
                        .on_hover_text(tr!(
                            "配色 · ライト／ダーク／システム",
                            "Appearance · Light / Dark / System"
                        ));
                    if self.appearance != previous {
                        ctx.set_theme(self.appearance);
                        ctx.request_repaint();
                        if let Some(path) = &self.appearance_path
                            && let Err(error) = appearance::save(path, self.appearance)
                        {
                            self.status = message!(
                                "配色を切り替えましたが、設定を保存できません: {error}",
                                "Appearance changed, but the preference could not be saved: {error}"
                            );
                            self.status_error = true;
                        }
                    }
                    let before = self.language;
                    egui::ComboBox::from_label(tr!("言語", "Language"))
                        .selected_text(self.language.name())
                        .width(110.0)
                        .show_ui(ui, |ui| {
                            for language in [Language::Japanese, Language::English] {
                                ui.selectable_value(&mut self.language, language, language.name());
                            }
                        });
                    if before != self.language {
                        self.view_dirty = true;
                        self.closed.clear();
                        ctx.request_repaint();
                    }
                });
            });
            let _language = self.language.enter();
            let current_step = match self.stage {
                Stage::Settings | Stage::Imaging | Stage::Migrating => 1,
                Stage::Scanning => 2,
                Stage::Selecting => 3,
                Stage::Recovering | Stage::Finished => 4,
            };
            ui.horizontal_wrapped(|ui| {
                for (n, label) in [
                    (1, tr!("設定", "Settings")),
                    (2, tr!("スキャン", "Scan")),
                    (3, tr!("ファイル選択", "Select files")),
                    (4, tr!("復元", "Recover")),
                ] {
                    theme::step(ui, n, label, n == current_step, n < current_step);
                }
            });
            ui.add_space(7.0);
        });
        let _language = self.language.enter();
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.add_space(6.0);
            if self.stage == Stage::Settings && self.show_validation && !self.status_error {
                let count = self.configuration_issues().len();
                if count > 0 {
                    theme::error_banner(
                        ui,
                        &tr_format!(
                            "設定を確認してください · {count} 件 — 赤枠の項目を修正してください。",
                            "Check settings · {count} issues — Correct the fields outlined in red."
                        ),
                    );
                } else {
                    ui.label(tr!(
                        "設定の不備を修正しました。スキャンを開始できます。",
                        "Settings corrected. You can start the scan."
                    ));
                }
            } else if self.status_error {
                theme::error_banner(ui, &self.status.to_string());
            } else {
                ui.label(&self.status);
            }
            if self.busy() {
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    let fraction = if self.progress.total_records > 0 {
                        self.progress.scanned_records as f32 / self.progress.total_records as f32
                    } else {
                        0.0
                    };
                    if self.stage != Stage::Scanning {
                        ui.add(
                            egui::ProgressBar::new(fraction)
                                .desired_width(ui.available_width().min(360.0))
                                .text(if self.stage == Stage::Recovering {
                                    tr!("ファイルを保存中…", "Saving files…").into()
                                } else if self.stage == Stage::Migrating {
                                    tr!("索引へコピー中…", "Copying into an index…").into()
                                } else {
                                    progress_text(&self.progress)
                                }),
                        );
                    }
                    if self.stage == Stage::Scanning
                        && self.index.path.is_some()
                        && ui
                            .add(theme::primary(
                                ui,
                                tr!("中断して保存", "Pause and save"),
                                160.0,
                            ))
                            .clicked()
                    {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.status = message!(
                            "中断を要求しました。再開位置の保存完了を待っています。",
                            "Pause requested. Waiting for the resume position to be saved."
                        );
                    }
                    if self.stage == Stage::Imaging
                        && self.image_progress.is_some()
                        && ui
                            .button(tr!("一時停止して保存", "Pause and save"))
                            .clicked()
                    {
                        self.image_pause_requested = true;
                        self.cancel.store(true, Ordering::Relaxed);
                        self.status = message!(
                            "一時停止を要求しました。読取マップの保存完了を待っています。",
                            "Pause requested. Waiting for the read map to be saved."
                        );
                    }
                    if ui.button(tr!("取消", "Cancel")).clicked() {
                        self.image_pause_requested = false;
                        self.cancel.store(true, Ordering::Relaxed);
                        self.status = message!(
                            "取消を要求しました。処理の終了を待っています。",
                            "Cancellation requested. Waiting for the operation to stop."
                        );
                    }
                });
            }
            ui.add_space(6.0);
        });
        let wide = ctx.content_rect().width() >= 1008.0;
        if self.show_evidence && wide && self.session.is_some() && self.stage != Stage::Settings {
            egui::SidePanel::right("evidence")
                .default_width(280.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show_accessible(
                        ui,
                        tr!("復元の根拠", "Recovery evidence"),
                        |ui| self.intelligence(ui),
                    );
                });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            let report_height = if self.report.is_empty() || self.stage == Stage::Settings || self.busy() { 0.0 } else {
                36.0 + if self.show_report {104.0} else {0.0}
            };
            let height = if self.busy() { ui.available_height() } else { (ui.available_height() - 124.0 - report_height).max(32.0) };
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_height(height);
                    match self.stage {
                        Stage::Settings => self.setup_ui(ui),
                        Stage::Scanning => self.scan_status_ui(ui),
                        Stage::Imaging => self.imaging_status(ui),
                        Stage::Migrating => {
                            ui.heading(tr!("セッションをコピー移行しています", "Migrating a session copy"));
                            ui.label(&self.session_path);
                            ui.label(tr!("元の記録を保持し、新しい索引を作成しています。取消は画面下から操作できます。", "Creating a new index while keeping the original session. Use the controls below to cancel."));
                        }
                        _ => {
                            egui::ScrollArea::vertical().id_salt("results-view").auto_shrink([false, false]).show_accessible(ui, tr!("復元候補", "Recovery candidates"), |ui| self.results_ui(ui));
                        }
                    }
                },
            );
            if !self.busy() { self.actions_ui(ui); }
        });
        if self.image_dialog && !self.busy() {
            self.imaging_dialog(ctx);
        }
        if self.session_dialog.is_some() && !self.busy() {
            self.session_dialog_ui(ctx);
        }
        if self.map_dialog && !self.busy() {
            self.read_map_dialog(ctx);
        }
        if self.raid.dialog && !self.busy() {
            self.raid_dialog_ui(ctx);
        }
        if self.discovery_dialog && !self.busy() {
            self.discovery_dialog_ui(ctx);
        }
        if self.show_evidence && !wide && self.session.is_some() && self.stage != Stage::Settings {
            let mut close = false;
            let response = responsive_modal(ctx, "recovery-evidence", 560.0).show(ctx, |ui| {
                ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 560.0));
                egui::ScrollArea::vertical()
                    .max_height((ctx.content_rect().height() - 160.0).max(100.0))
                    .show_accessible(ui, tr!("復元の根拠", "Recovery evidence"), |ui| {
                        self.intelligence(ui)
                    });
                ui.separator();
                close = ui.button(tr!("閉じる", "Close")).clicked();
            });
            if response.should_close() || close {
                self.show_evidence = false;
            }
        }
        self.preview.show(ctx);
        if !self.busy() {
            self.hashes.show(ctx, self.session.as_ref());
        }
        if self.report_export.is_some() && !self.busy() {
            self.report_export_ui(ctx);
        }
    }
}
impl eframe::App for App {
    #[cfg(windows)]
    fn raw_input_hook(&mut self, ctx: &egui::Context, input: &mut egui::RawInput) {
        self.ime
            .normalize(ctx.memory(|memory| memory.focused()), &mut input.events);
    }

    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive();
        self.render(ctx);
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
fn launch_options(
    args: &[std::ffi::OsString],
) -> Result<(Option<PathBuf>, Language), &'static str> {
    let mut session = None;
    let mut language = Language::Japanese;
    let mut language_set = false;
    let mut arguments = args.iter();
    while let Some(flag) = arguments.next() {
        if flag == "--session" && session.is_none() {
            session = Some(PathBuf::from(
                arguments.next().ok_or("--session requires a path")?,
            ));
        } else if flag == "--language" && !language_set {
            language = match arguments.next().and_then(|v| v.to_str()) {
                Some("ja") => Language::Japanese,
                Some("en") => Language::English,
                _ => return Err("--language requires ja or en"),
            };
            language_set = true;
        } else {
            return Err("Unknown or repeated option. Use --session FILE and/or --language ja|en.");
        }
    }
    Ok((session, language))
}
fn main() -> eframe::Result {
    if let Some(code) = rescue_core::acquisition_worker_entry() {
        std::process::exit(code);
    }
    if let Some(code) = rescue_core::read_broker_entry() {
        std::process::exit(code);
    }
    if let Some(code) = rescue_core::preview_worker_entry() {
        std::process::exit(code);
    }
    let args = std::env::args_os().skip(1).collect::<Vec<_>>();
    match args.as_slice() {
        [flag] if flag == "--version" => {
            println!("rescue-gui {}", rescue_core::VERSION);
            return Ok(());
        }
        [flag] if flag == "--help" => {
            println!(
                "rescue-gui [--session FILE] [--language ja|en]\nOpen a saved JSON session or SQLite scan index. Language can also be changed in the window."
            );
            return Ok(());
        }
        _ => {}
    };
    let (session, language) = launch_options(&args).unwrap_or_else(|error| {
        eprintln!("{error}");
        std::process::exit(2);
    });
    eframe::run_native(
        "Rescue — File Recovery",
        eframe::NativeOptions {
            // Windows' monitor-size clamp includes the taskbar and excludes
            // window decorations. A normal 1260x820 window can hide the footer
            // on a 1024x768 desktop. Let Windows fit its actual work area.
            #[cfg(windows)]
            window_builder: Some(Box::new(|viewport| viewport.with_maximized(true))),
            #[cfg(windows)]
            renderer: eframe::Renderer::Wgpu,
            #[cfg(not(windows))]
            renderer: eframe::Renderer::Glow,
            viewport: egui::ViewportBuilder::default()
                .with_inner_size([1260.0, 820.0])
                .with_min_inner_size([720.0, 520.0]),
            ..Default::default()
        },
        Box::new(move |cc| Ok(Box::new(App::new(cc, session, language)))),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn launch_language_keeps_unicode_session_paths_and_rejects_ambiguous_options() {
        let args = ["--language", "en", "--session", "資料/完了.json"].map(Into::into);
        let (path, language) = launch_options(&args).unwrap();
        assert_eq!(path, Some(PathBuf::from("資料/完了.json")));
        assert_eq!(language, Language::English);
        assert_eq!(launch_options(&[]).unwrap(), (None, Language::Japanese));
        for args in [
            vec!["--language"],
            vec!["--language", "de"],
            vec!["--language", "en", "--language", "ja"],
            vec!["--session"],
        ] {
            assert!(launch_options(&args.into_iter().map(Into::into).collect::<Vec<_>>()).is_err());
        }
    }
    #[test]
    fn primary_action_stays_inside_resized_viewports_and_scroll_content() {
        let ctx = egui::Context::default();
        configure_style(&ctx);
        let mut app = App::blank();
        app.source_mode = SourceMode::Image;
        for language in [Language::Japanese, Language::English] {
            app.language = language;
            for stage in [Stage::Settings, Stage::Selecting, Stage::Finished] {
                app.stage = stage;
                if stage != Stage::Settings {
                    app.session = Some(session(FileSystem::Ntfs));
                    app.index.path = Some(PathBuf::from("saved/session.sqlite"));
                    app.index.query = app.index_query();
                }
                for zoom in [1.0, 1.5, 2.0] {
                    ctx.set_zoom_factor(zoom);
                    for size in [[1260.0, 820.0], [720.0, 520.0], [1008.0, 620.0]] {
                        for _ in 0..3 {
                            let _ = ctx.run(
                                egui::RawInput {
                                    screen_rect: Some(egui::Rect::from_min_size(
                                        egui::Pos2::ZERO,
                                        egui::vec2(size[0] * zoom, size[1] * zoom),
                                    )),
                                    ..Default::default()
                                },
                                |ctx| app.render(ctx),
                            );
                        }
                        let rect = app.primary_rect.expect("primary action exists");
                        let viewport = ctx.content_rect();
                        assert!(
                            viewport.contains_rect(rect),
                            "{stage:?} {size:?} at {zoom}: button {rect:?}, viewport {viewport:?}"
                        );
                        assert!(rect.height() >= 36.0 && rect.width() >= 176.0);
                        assert!(
                            viewport.bottom() - rect.bottom() < 100.0,
                            "primary action must remain near the bottom"
                        );
                    }
                }
            }
        }
    }
    pub(super) fn session(fs: FileSystem) -> Session {
        Session {
            source_info: None,
            raid: None,
            schema_version: 4,
            region: None,
            volumes: Vec::new(),
            filesystem: fs,
            id: "test".into(),
            source: PathBuf::from("images/loaded.raw"),
            fingerprint: rescue_core::Fingerprint {
                length: 0,
                modified_ns: "0".into(),
                sample_sha256: String::new(),
            },
            candidates: vec![],
            scanned_records: 0,
            total_records: 0,
            cancelled: false,
            truncated: false,
            invalid_records: 0,
            warnings: vec![],
            search: None,
        }
    }
    #[test]
    fn selected_ids_and_sizes_survive_page_changes_without_retaining_candidates() {
        let mut app = App::blank();
        app.session = Some(session(FileSystem::Ntfs));
        let candidate = |id: &str, size| Candidate {
            id: id.into(),
            record: 0,
            fragment_offset: None,
            sequence: 1,
            stream_name: None,
            name: id.into(),
            original_path: id.into(),
            path_verified: false,
            alternate_names: vec![],
            size: Some(size),
            supported: true,
            warnings: vec![],
            record_sha256: String::new(),
            extent_evidence: Default::default(),
            allocation_status: Default::default(),
            created: None,
            modified: None,
        };
        app.session.as_mut().unwrap().candidates = vec![candidate("first", 100)];
        set_selected(&mut app.selected, "first", true);
        app.remember_selection_sizes();
        app.session.as_mut().unwrap().candidates = vec![candidate("second", 900)];
        set_selected(&mut app.selected, "second", true);
        app.remember_selection_sizes();
        assert_eq!(app.selected.len(), 2);
        assert_eq!(app.selected_bytes(), 1000);
        assert_eq!(app.session.as_ref().unwrap().candidates.len(), 1);
        set_selected(&mut app.selected, "first", false);
        app.remember_selection_sizes();
        assert_eq!(app.selected_bytes(), 900);
    }
    #[test]
    fn inventory_completion_is_received_after_leaving_settings() {
        let mut app = App::blank();
        let (tx, rx) = mpsc::channel();
        app.disk_messages = Some(rx);
        app.install_session(session(FileSystem::Ntfs), Path::new("work/session.json"));
        tx.send(Ok(vec![])).unwrap();
        app.receive();
        assert_eq!(app.stage, Stage::Selecting);
        assert!(app.session.is_some() && app.disk_messages.is_none());
        assert!(
            app.disk_status
                .get(Language::Japanese)
                .contains("検出できません")
        );
        assert!(
            app.disk_status
                .get(Language::English)
                .contains("No disks found")
        );
    }
    #[test]
    fn loaded_session_updates_search_source_and_clears_selection() {
        let mut a = App::blank();
        a.source = "old.raw".into();
        a.selected.insert("old".into());
        a.filter = "old".into();
        a.install_session(session(FileSystem::Raw), Path::new("work/session.json"));
        assert_eq!(a.source, "images/loaded.raw");
        assert_eq!(a.work, "work");
        assert!(a.carve && a.selected.is_empty() && a.filter.is_empty());
        a.invalidate_changed_source();
        assert!(a.session.is_some());
    }
    #[test]
    fn changed_source_method_or_scope_cannot_restore_old_candidates() {
        for change in 0..3 {
            let mut a = App::blank();
            a.install_session(session(FileSystem::Fat32), Path::new("work/session.json"));
            a.selected.insert("old".into());
            match change {
                0 => a.source = "new.raw".into(),
                1 => a.carve = true,
                _ => a.extensions = "png".into(),
            };
            a.start_recovery();
            assert!(a.session.is_none() && a.selected.is_empty() && a.report.is_empty());
            assert!(!a.busy());
        }
    }
    #[test]
    fn saved_scope_loads_and_output_changes_preserve_session() {
        let mut a = App::blank();
        let mut s = session(FileSystem::Fat32);
        s.search = Some(SearchSettings {
            extensions: vec!["png".into()],
            path_prefix: "photos".into(),
            max_depth: Some(2),
            ..Default::default()
        });
        a.install_session(s, Path::new("work/session.json"));
        a.destination = "elsewhere".into();
        a.save_mode = SaveMode::Zip;
        a.invalidate_changed_source();
        assert!(a.session.is_some());
        assert_eq!(a.depth, "2");
        assert_eq!(a.path_prefix, "photos");
    }
}
