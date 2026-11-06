#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
#[macro_use]
mod language;
mod acquisition_ui;
mod discovery_ui;
mod index_ui;
mod preview_ui;
mod raid_ui;
#[cfg(windows)]
mod read_access_ui;
mod session_ui;
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
    Error(Text),
}
struct App {
    language: Language,
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
    image_progress: Option<rescue_core::AcquisitionProgress>,
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
    view: ViewStyle,
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
    results: Vec<RecoveryResult>,
    primary_rect: Option<egui::Rect>,
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
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(8.0, 8.0);
            s.spacing.interact_size.y = 28.0;
            s.spacing.button_padding = egui::vec2(8.0, 4.0);
            s.visuals.panel_fill = egui::Color32::from_rgb(25, 25, 25);
            s.visuals.extreme_bg_color = egui::Color32::from_rgb(20, 20, 20);
            s.visuals.override_text_color = Some(egui::Color32::from_gray(224));
            s.visuals.selection.bg_fill = egui::Color32::from_rgb(38, 80, 110);
        });
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
            image_progress: None,
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
            view: ViewStyle::Details,
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
            self.index = index_ui::State::default();
            self.session = None;
            self.selected.clear();
            self.focused = None;
            self.results.clear();
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
        if self.source.trim().is_empty()
            || (self.save_mode != SaveMode::Original && self.destination.trim().is_empty())
        {
            self.status = message!(
                "ディスクと保存先を指定してください。",
                "Choose a disk and destination."
            );
            return;
        }
        let settings = match self.settings() {
            Ok(s) => s,
            Err(e) => {
                self.status = e;
                return;
            }
        };
        let source = PathBuf::from(&self.source);
        let work = self.work_path();
        let guarded = guard_output_path(&source, &work);
        if !self.work.trim().is_empty()
            && let Err(error) = &guarded
        {
            self.status = message!(
                "再開用の作業フォルダーを使用できません: {error}",
                "Cannot use the working folder for resume: {error}"
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
        self.stage = Stage::Scanning;
        self.discovery_running = false;
        self.session = None;
        self.selected.clear();
        self.focused = None;
        self.results.clear();
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
                    },
                    &settings,
                    &cancel,
                    |p| {
                        let _ = tx.try_send(Message::IndexProgress(p));
                    },
                )
                .map_err(|e| e.to_string())
                .and_then(|path| index_ui::load(path, query, &cancel))
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
    fn start_recovery(&mut self) {
        self.invalidate_changed_source();
        if self.save_mode != SaveMode::Original && self.destination.trim().is_empty() {
            self.status = message!(
                "設定画面で保存先を指定してください。",
                "Choose a destination in Settings."
            );
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
                    self.results = results;
                    self.status = status;
                    self.report = detail;
                    self.stage = Stage::Finished;
                }
                Message::Error(e) => {
                    self.raid.running = false;
                    self.discovery_running = false;
                    self.status = message!("エラー: {e}", "Error: {e}");
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
        ui.strong(tr!("1  スキャンするディスク", "1  Disk to scan"));
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
                .show(ui, |ui| {
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
            text_row(
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
            path_row(
                ui,
                tr!("イメージファイル", "Image file"),
                &mut self.source,
                Picker::Open,
            );
            ui.small(tr!(
                "RAWボリュームまたはMBR／GPTディスクイメージを読み取ります。",
                "Read a RAW volume or an MBR / GPT disk image."
            ));
        }
    }
    fn setup_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("settings-scroll").auto_shrink([false,false]).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(960.0));
            ui.add_space(8.0);
            ui.heading(tr!("保存方法と検索条件", "Save options and search settings"));
            ui.label(tr!("ディスクをスキャンしてから、復元するファイルを選択します。", "Scan the disk, then select files to recover."));
            ui.add_space(16.0);
            self.source_ui(ui);
            if ui.add_enabled(!self.source.trim().is_empty(),egui::Button::new(tr!("ディスクのイメージを作成・再開", "Create or resume a disk image"))).clicked(){self.image_dialog=true;}
            if self.source_mode==SourceMode::Image && ui.add_enabled(!self.source.trim().is_empty(),egui::Button::new(tr!("読取マップを確認", "View read map"))).clicked(){self.map_source=PathBuf::from(&self.source);self.map_page=None;self.map_dialog=true;self.load_map_page(0,None);}
            self.discovery_controls(ui);
            ui.add_space(16.0);
            ui.strong(tr!("2  保存方法", "2  Save options"));
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.save_mode, SaveMode::Original, tr!("元の場所へ戻す", "Original locations"));
                ui.selectable_value(&mut self.save_mode, SaveMode::Directory, tr!("フォルダーに保存", "Save to folder"));
                if ui.selectable_value(&mut self.save_mode, SaveMode::Zip, tr!("ZIPファイルに保存", "Save as ZIP")).changed()
                    && !self.destination.to_lowercase().ends_with(".zip") { self.destination.clear(); }
            });
            if self.save_mode != SaveMode::Original {
                let picker = if self.save_mode == SaveMode::Zip { Picker::Zip } else { Picker::Folder };
                path_row(ui, tr!("保存先", "Destination"), &mut self.destination, picker);
            } else {
                ui.label(tr!("確認できた元のフォルダーとファイル名で戻します。元の場所が不明な候補はフォルダー保存を選んでください。", "Restore verified original folders and filenames. Choose a destination folder for files whose original locations are unknown."));
            }
            ui.horizontal_wrapped(|ui| {
                ui.label(tr!("同じ名前がある場合", "When a name already exists"));
                ui.selectable_value(&mut self.collision, CollisionPolicy::KeepBoth, tr!("両方を残す（復元 1）", "Keep both (numbered copy)"));
                ui.selectable_value(&mut self.collision, CollisionPolicy::Skip, tr!("スキップ", "Skip"));
            });
            ui.small(tr!("既存ファイルは上書きしません。同名フォルダーは統合します。", "Existing files are kept. Folders with the same name are merged."));
            ui.add_space(16.0);
            ui.collapsing(tr!("詳細設定 · 検索方式・範囲・深度・拡張子", "Advanced · Method, range, depth and extensions"), |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label(tr!("検索方式", "Search method"));
                    ui.selectable_value(&mut self.carve, false, tr!("管理情報 / Quick", "Metadata / Quick"));
                    ui.selectable_value(&mut self.carve, true, tr!("写真・文書・動画 / Carving", "Photos, documents and videos / Carving"));
                });
                text_row(ui, tr!("探す拡張子", "File extensions to find"), &mut self.extensions, tr!("空欄: すべて / 例: jpg, pdf, docx, zip", "Blank: all / Example: jpg, pdf, docx, zip"));
                if self.carve {
                    ui.horizontal_wrapped(|ui| {
                        ui.label(tr!("割当範囲", "Allocation scope"));
                        ui.selectable_value(&mut self.carve_all,false,tr!("未割当領域（推奨）", "Unallocated regions (recommended)"));
                        ui.selectable_value(&mut self.carve_all,true,tr!("全領域", "All regions"));
                    });
                    ui.small(tr!("割当情報が不明・破損している範囲は全領域へ切り替え、結果に理由を表示します。", "Regions with unavailable or damaged allocation data are searched in full, with the reason shown in the results."));
                    ui.label(tr!("PNG/JPEG、PDF、ZIP/Office、MP4/MOVを探索します。形式ごとに構造・CRC・参照位置を確認します。元の名前・削除状態は不明です。", "Find PNG/JPEG, PDF, ZIP/Office and MP4/MOV. Validate structure, CRC or references as appropriate. Original names and deletion status are unknown."));
                    text_row(ui, tr!("開始バイト", "Start byte"), &mut self.range_start, tr!("空欄: 0（ディスク/イメージの先頭）", "Blank: 0 (start of the disk / image)"));
                    text_row(ui, tr!("終了バイト", "End byte"), &mut self.range_end, tr!("空欄: 末尾 / この位置は含みません", "Blank: end of source / This position is excluded"));
                } else {
                    ui.checkbox(&mut self.fragments,tr!("残存するMFTレコードも探索（クイックフォーマット後など）", "Also search for remnant MFT records (for example, after a quick format)"));
                    text_row(ui, tr!("検索フォルダー", "Search folder"), &mut self.path_prefix, tr!("空欄: 全体 / 例: 写真/旅行", "Blank: all / Example: Photos/Travel"));
                    text_row(ui, tr!("検索深度", "Search depth"), &mut self.depth, tr!("空欄: 上限なし / 0: 直下 / 最大256", "Blank: unlimited / 0: direct children / Maximum: 256"));
                    ui.small(tr!("範囲・深度・拡張子は結果を絞ります。割当の競合確認はボリューム全体を調べます。", "Range, depth and extensions filter the results. Allocation conflicts are checked across the full volume."));
                }
                path_row(ui, tr!("再開用の作業フォルダー（任意）", "Working folder for resume (optional)"), &mut self.work, Picker::Folder);
                ui.small(tr!("空欄では保存先の近くに索引を作ります。元の場所への復元・同じ物理ディスクの場合はメモリーに保持します。再開には安全な作業フォルダーを指定してください。", "If blank, an index is created near the destination. Original-location or same-physical-disk recovery holds results in memory. Set a suitable working folder to enable resume."));
                if self.save_mode == SaveMode::Original {
                    path_row(ui, tr!("元ボリュームのルート（任意）", "Original volume root (optional)"), &mut self.original_root, Picker::Folder);
                    ui.small(tr!("通常は自動判定します。イメージなど判定できない場合は元ボリュームのルートを指定します。複数パーティションの個別指定はCLIを使います。", "Usually detected automatically. Specify the original volume root for images or when detection fails. Use the CLI to map multiple partitions individually."));
                }
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
                let response = ui.add_enabled(
                    !self.source.trim().is_empty(),
                    egui::Button::new(tr!("スキャン開始", "Start scan"))
                        .min_size(egui::vec2(176.0, 36.0)),
                );
                self.primary_rect = Some(response.rect);
                if response.clicked() {
                    self.start_scan();
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
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let response = ui.add_enabled(
                        !self.selected.is_empty() && !self.index.loading(),
                        egui::Button::new(tr!("選択したファイルを復元", "Recover selected files"))
                            .min_size(egui::vec2(224.0, 36.0)),
                    );
                    self.primary_rect = Some(response.rect);
                    if response.clicked() {
                        self.start_recovery();
                    }
                });
            });
        } else {
            ui.label(tr!(
                "進行状況と取消は画面下に表示しています。",
                "Progress and cancellation controls are at the bottom of the window."
            ));
        }
    }
    fn results_ui(&mut self, ui: &mut egui::Ui) {
        if self.session.as_ref().is_some_and(|s| {
            s.warnings
                .iter()
                .any(|w| w.starts_with("ALLOCATION_FALLBACK:"))
        }) {
            ui.colored_label(egui::Color32::YELLOW, tr!("一部の領域で割当情報を確認できないため、その領域は全体を検索しています。詳細は検証情報をご確認ください。", "Some regions have no verifiable allocation data and are being searched in full. See the evidence for details."));
        }
        let busy = self.busy();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label(tr!("表示", "View"));
                ui.selectable_value(&mut self.view, ViewStyle::Details, tr!("詳細", "Details"));
                ui.selectable_value(&mut self.view, ViewStyle::Icons, tr!("アイコン", "Icons"));
                ui.separator();
                ui.label(tr!("グループ", "Group"));
                egui::ComboBox::from_id_salt("group")
                    .selected_text(self.group.label())
                    .show_ui(ui, |ui| {
                        for g in GroupBy::ALL {
                            if ui.selectable_value(&mut self.group, g, g.label()).changed() {
                                self.closed.clear();
                                self.view_dirty = true;
                            }
                        }
                    });
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
                ui.label(tr!("結果を検索", "Search results"));
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .id(egui::Id::new("result-search"))
                            .desired_width(280.0)
                            .hint_text(tr!("名前・元のパス", "Name or original path")),
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
        egui::ScrollArea::horizontal().show(ui, |ui| {
            ui.set_min_width(1030.0);
            TableBuilder::new(ui)
                .id_salt("files-table")
                .resizable(true)
                .cell_layout(egui::Layout::left_to_right(egui::Align::Center))
                .column(Column::exact(27.0))
                .column(Column::initial(250.0).at_least(140.0))
                .column(Column::initial(178.0).at_least(110.0))
                .column(Column::initial(178.0).at_least(110.0))
                .column(Column::initial(105.0).at_least(70.0))
                .column(Column::initial(90.0).at_least(65.0))
                .column(Column::remainder().at_least(90.0))
                .header(32.0, |mut row| {
                    row.col(|_| {});
                    for (col, label) in [
                        (SortColumn::Name, tr!("名前", "Name")),
                        (SortColumn::Created, tr!("作成日時", "Date created")),
                        (SortColumn::Modified, tr!("更新日時", "Date modified")),
                        (SortColumn::Type, tr!("種類", "Type")),
                        (SortColumn::Size, tr!("サイズ", "Size")),
                        (SortColumn::State, tr!("状態", "Status")),
                    ] {
                        row.col(|ui| {
                            let text = format!(
                                "{label}{}",
                                if self.sort == col {
                                    if self.descending { "  ↓" } else { "  ↑" }
                                } else {
                                    ""
                                }
                            );
                            if ui.selectable_label(self.sort == col, text).clicked() {
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
                                            .selectable_label(false, format!("{name}  ({count})"))
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
                                });
                                row.col(|ui| {
                                    ui.horizontal(|ui| {
                                        file_icon(ui, 16.0);
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
                                        if label.clicked() {
                                            self.focused = Some(c.id.clone());
                                            if c.supported && !busy {
                                                set_selected(&mut self.selected, &c.id, !checked);
                                            }
                                        }
                                    });
                                });
                                row.col(|ui| {
                                    ui.add(
                                        egui::Label::new(view::date_text(&c.created)).truncate(),
                                    )
                                    .on_hover_text(view::date_hint(&c.created));
                                });
                                row.col(|ui| {
                                    ui.add(
                                        egui::Label::new(view::date_text(&c.modified)).truncate(),
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
                                    let saved =
                                        self.results.iter().rev().find(|r| r.candidate_id == c.id);
                                    ui.label(
                                        saved
                                            .map(|r| status_label(&r.status))
                                            .unwrap_or_else(|| view::state(c)),
                                    )
                                    .on_hover_text(c.warnings.join("\n"));
                                });
                            }
                        }
                    })
                });
        });
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
        TableBuilder::new(ui)
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
                                                        file_icon(ui, 40.0);
                                                        let label = ui
                                                            .add(
                                                                egui::Label::new(&c.name)
                                                                    .truncate()
                                                                    .sense(egui::Sense::click()),
                                                            )
                                                            .on_hover_text(&c.original_path);
                                                        if label.clicked() {
                                                            self.focused = Some(c.id.clone());
                                                            if c.supported && !busy {
                                                                let selected =
                                                                    !self.selected.contains(&c.id);
                                                                set_selected(
                                                                    &mut self.selected,
                                                                    &c.id,
                                                                    selected,
                                                                );
                                                            }
                                                        }
                                                        ui.small(view::size_text(c.size));
                                                        let mut selected =
                                                            self.selected.contains(&c.id);
                                                        if ui
                                                            .add_enabled(
                                                                c.supported && !busy,
                                                                egui::Checkbox::without_text(
                                                                    &mut selected,
                                                                ),
                                                            )
                                                            .changed()
                                                        {
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
                egui::Color32::YELLOW,
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
            ui.label(format!("{:?}", c.extent_evidence));
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
        FileSystem::Fat32 => tr!("FAT値0：空きとして記録", "FAT value 0: recorded as free"),
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
        ui.add_sized(
            [(ui.available_width() - 72.0).max(40.0), 28.0],
            egui::TextEdit::singleline(value).id_salt(label),
        )
        .labelled_by(label_id);
        if ui
            .add_sized([64.0, 28.0], egui::Button::new(tr!("参照", "Browse")))
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
    ui.add_sized(
        [ui.available_width(), 28.0],
        egui::TextEdit::singleline(value)
            .id_salt(label)
            .hint_text(hint),
    )
    .labelled_by(label_id);
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
fn file_icon(ui: &mut egui::Ui, size: f32) {
    let (r, _) = ui.allocate_exact_size(egui::vec2(size * 0.78, size), egui::Sense::hover());
    let p = ui.painter();
    p.rect_filled(r.shrink(1.0), 2.0, egui::Color32::from_rgb(212, 220, 231));
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
            ui.horizontal(|ui| {
                ui.heading("Rescue");
                ui.weak(rescue_core::VERSION);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let before = self.language;
                    egui::ComboBox::from_label("Language")
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
            ui.horizontal_wrapped(|ui| {
                for (n, label, active) in [
                    (
                        1,
                        tr!("設定", "Settings"),
                        matches!(
                            self.stage,
                            Stage::Settings | Stage::Imaging | Stage::Migrating
                        ),
                    ),
                    (2, tr!("スキャン", "Scan"), self.stage == Stage::Scanning),
                    (
                        3,
                        tr!("ファイル選択", "Select files"),
                        self.stage == Stage::Selecting,
                    ),
                    (
                        4,
                        tr!("復元", "Recover"),
                        matches!(self.stage, Stage::Recovering | Stage::Finished),
                    ),
                ] {
                    if n > 1 {
                        ui.weak("›");
                    }
                    ui.colored_label(
                        if active {
                            egui::Color32::from_rgb(112, 199, 255)
                        } else {
                            egui::Color32::GRAY
                        },
                        format!("{n}  {label}"),
                    );
                }
            });
            ui.add_space(7.0);
        });
        let _language = self.language.enter();
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(&self.status);
            if self.busy() {
                ui.horizontal_wrapped(|ui| {
                    ui.spinner();
                    let fraction = if self.progress.total_records > 0 {
                        self.progress.scanned_records as f32 / self.progress.total_records as f32
                    } else {
                        0.0
                    };
                    ui.add(
                        egui::ProgressBar::new(fraction)
                            .desired_width(ui.available_width().min(360.0))
                            .text(if self.stage == Stage::Recovering {
                                tr!("ファイルを保存中…", "Saving files…").into()
                            } else if self.stage == Stage::Migrating {
                                tr!("索引へコピー中…", "Copying into an index…").into()
                            } else {
                                format!(
                                    "{} / {} {:?}",
                                    self.progress.scanned_records,
                                    self.progress.total_records,
                                    self.progress.unit
                                )
                            }),
                    );
                    if self.stage == Stage::Scanning
                        && self.index.path.is_some()
                        && ui.button(tr!("中断して保存", "Pause and save")).clicked()
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
            if !self.report.is_empty() {
                ui.collapsing(
                    tr!("保存場所・レポート", "Saved locations and reports"),
                    |ui| {
                        ui.add(egui::Label::new(&self.report).wrap());
                    },
                );
            }
        });
        let wide = ctx.content_rect().width() >= 1008.0;
        if self.show_evidence && wide && self.session.is_some() && self.stage != Stage::Settings {
            egui::SidePanel::right("evidence")
                .default_width(280.0)
                .show(ctx, |ui| {
                    egui::ScrollArea::vertical().show(ui, |ui| self.intelligence(ui));
                });
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            let height = (ui.available_height() - 100.0).max(32.0);
            ui.allocate_ui_with_layout(
                egui::vec2(ui.available_width(), height),
                egui::Layout::top_down(egui::Align::Min),
                |ui| {
                    ui.set_min_height(height);
                    match self.stage {
                        Stage::Settings => self.setup_ui(ui),
                        Stage::Scanning => {
                            ui.add_space(24.0);
                            ui.heading(if self.raid.running {
                                tr!("RAIDの構成を検査しています", "Checking the RAID configuration")
                            } else if self.discovery_running {
                                tr!("失われた領域を探しています", "Searching for lost volumes")
                            } else {
                                tr!("スキャンしています", "Scanning")
                            });
                            ui.label(&self.source);
                            ui.label(tr!("完了後、復元するファイルを選択できます。", "Select files to recover when the scan finishes."));
                        }
                        Stage::Imaging => self.imaging_status(ui),
                        Stage::Migrating => {
                            ui.heading(tr!("セッションをコピー移行しています", "Migrating a session copy"));
                            ui.label(&self.session_path);
                            ui.label(tr!("元の記録を保持し、新しい索引を作成しています。取消は画面下から操作できます。", "Creating a new index while keeping the original session. Use the controls below to cancel."));
                        }
                        _ => self.results_ui(ui),
                    }
                },
            );
            self.actions_ui(ui);
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
                    .show(ui, |ui| self.intelligence(ui));
                ui.separator();
                close = ui.button(tr!("閉じる", "Close")).clicked();
            });
            if response.should_close() || close {
                self.show_evidence = false;
            }
        }
        self.preview.show(ctx);
    }
}
impl eframe::App for App {
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
    fn session(fs: FileSystem) -> Session {
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
