#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod acquisition_ui;
mod discovery_ui;
mod view;
use eframe::egui;
use egui_extras::{Column, TableBuilder};
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
    ImagingProgress(rescue_core::AcquisitionProgress),
    Imaged(rescue_core::AcquisitionResult),
    Progress(Progress),
    Discovered(Box<rescue_core::DiscoveryResult>),
    Scanned(Box<Session>, PathBuf),
    Finished {
        results: Vec<RecoveryResult>,
        status: String,
        detail: String,
    },
    Error(String),
}
struct App {
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
    image_retries: u8,
    image_block_size: u32,
    image_progress: Option<rescue_core::AcquisitionProgress>,
    source: String,
    source_mode: SourceMode,
    disks: Vec<DiskSource>,
    disk_messages: Option<mpsc::Receiver<Result<Vec<DiskSource>, String>>>,
    disk_status: String,
    work: String,
    destination: String,
    save_mode: SaveMode,
    collision: CollisionPolicy,
    original_root: String,
    carve: bool,
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
    status: String,
    report: String,
    results: Vec<RecoveryResult>,
    primary_rect: Option<egui::Rect>,
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
                cc.egui_ctx.set_fonts(fonts);
                break;
            }
        }
        let mut app = Self::blank();
        app.refresh_disks();
        app
    }
    fn blank() -> Self {
        Self {
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
            image_retries: 2,
            image_block_size: 4 * 1024 * 1024,
            image_progress: None,
            source: String::new(),
            source_mode: SourceMode::Disk,
            disks: Vec::new(),
            disk_messages: None,
            disk_status: String::new(),
            work: String::new(),
            destination: String::new(),
            save_mode: SaveMode::Directory,
            collision: CollisionPolicy::KeepBoth,
            original_root: String::new(),
            carve: false,
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
            messages: None,
            cancel: Arc::new(AtomicBool::new(false)),
            progress: Progress::default(),
            status: "ディスクと保存方法を設定してください。".into(),
            report: String::new(),
            results: Vec::new(),
            primary_rect: None,
        }
    }
    fn busy(&self) -> bool {
        matches!(
            self.stage,
            Stage::Scanning | Stage::Recovering | Stage::Imaging
        )
    }
    fn settings(&self) -> Result<SearchSettings, String> {
        let number = |s: &str| {
            if s.trim().is_empty() {
                Ok(None)
            } else {
                s.trim()
                    .parse::<u64>()
                    .map(Some)
                    .map_err(|_| "範囲・深度には整数を入力してください。".to_owned())
            }
        };
        let s = SearchSettings {
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
                    .map_err(|_| "深度は0～256で指定してください。".to_owned())?
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
        self.source = s.source.to_string_lossy().into_owned();
        self.source_mode = if rescue_core::is_device(&s.source) {
            SourceMode::Disk
        } else {
            SourceMode::Image
        };
        let settings = s.search.clone().unwrap_or_else(|| SearchSettings {
            method: if s.filesystem == FileSystem::Raw {
                ScanMethod::Carve
            } else {
                ScanMethod::Quick
            },
            ..Default::default()
        });
        self.carve = settings.method == ScanMethod::Carve;
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
            "スキャン結果をメモリーに保持しています。".into()
        } else {
            format!("セッション: {}", path.display())
        };
    }
    fn invalidate_changed_source(&mut self) {
        let changed = self.session.as_ref().is_some_and(|s| {
            let prev = s.search.clone().unwrap_or_else(|| SearchSettings {
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
            self.session = None;
            self.selected.clear();
            self.focused = None;
            self.results.clear();
            self.rows.clear();
            self.report.clear();
            self.view_dirty = true;
            self.status = "入力または検索条件が変更されました。スキャンを実行してください。".into();
        }
    }
    fn start_scan(&mut self) {
        if self.source.trim().is_empty()
            || (self.save_mode != SaveMode::Original && self.destination.trim().is_empty())
        {
            self.status = "ディスクと保存先を指定してください。".into();
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
        let persist = !self.work.trim().is_empty() && guard_output_path(&source, &work).is_ok();
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
        self.status = "スキャン中…".into();
        thread::spawn(move || {
            let r = scan_with_settings(&source, ScanOptions::default(), &settings, &cancel, |p| {
                let _ = tx.try_send(Message::Progress(p));
            })
            .and_then(|s| {
                let path = if persist {
                    s.save(&work)?
                } else {
                    PathBuf::new()
                };
                Ok((s, path))
            });
            let _ = tx.send(match r {
                Ok((s, p)) => Message::Scanned(Box::new(s), p),
                Err(e) => Message::Error(e.to_string()),
            });
        });
    }
    fn start_recovery(&mut self) {
        self.invalidate_changed_source();
        if self.save_mode != SaveMode::Original && self.destination.trim().is_empty() {
            self.status = "設定画面で保存先を指定してください。".into();
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        if self.selected.is_empty() {
            return;
        }
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
        self.status = "選択したファイルを復元中…".into();
        thread::spawn(move || {
            let r = if mode == SaveMode::Zip {
                restore_zip(&session, &ids, &dest, collision, &cancel).map(|r| Message::Finished {
                    status: format!(
                        "ZIP保存: {} / {} ファイル",
                        status_label(&r.status),
                        r.results.iter().filter(|v| v.status == "completed").count()
                    ),
                    detail: format!(
                        "ZIP: {}\nSHA-256: {}\n{}{}{}",
                        r.archive
                            .map(|p| p.display().to_string())
                            .unwrap_or_default(),
                        r.sha256.unwrap_or_default(),
                        r.error.unwrap_or_default(),
                        r.staging
                            .map(|p| format!("\n一時保存: {}", p.display()))
                            .unwrap_or_default(),
                        r.partial_archive
                            .map(|p| format!("\n未完了ZIP: {}", p.display()))
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
                        "元の場所への復元結果を下に表示しています。".into()
                    } else {
                        format!(
                            "レポート: {}",
                            save_restore_report(&dest, &session, &results)?.display()
                        )
                    };
                    let n = results.iter().filter(|r| r.status == "completed").count();
                    let skipped = results.iter().filter(|r| r.status == "skipped").count();
                    let skipped_label = if skipped == 0 {
                        String::new()
                    } else {
                        format!(" / スキップ: {skipped}")
                    };
                    Ok(Message::Finished {
                        status: format!(
                            "保存完了: {n}{skipped_label} / 不完全・失敗: {} / 取消: {}",
                            results.len() - n - skipped,
                            if cancel.load(Ordering::Relaxed) {
                                "要求あり"
                            } else {
                                "なし"
                            }
                        ),
                        detail,
                        results,
                    })
                })
            };
            let _ = tx.send(r.unwrap_or_else(|e| Message::Error(e.to_string())));
        });
    }
    fn receive(&mut self) {
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
                        "ディスクを検出できませんでした。接続・アクセス権を確認してください。"
                            .into()
                    } else {
                        format!("{} 個のディスク／ボリューム", disks.len())
                    };
                    self.disks = disks;
                }
                Err(e) => self.disk_status = format!("ディスク一覧を取得できません: {e}"),
            }
        }
        let messages = self
            .messages
            .as_ref()
            .map(|rx| rx.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for m in messages {
            match m {
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
                    self.source = result.image.to_string_lossy().into_owned();
                    self.source_mode = SourceMode::Image;
                    self.stage = Stage::Settings;
                    self.report = format!(
                        "イメージ: {}\n読取マップ: {}",
                        result.image.display(),
                        result.read_map.display()
                    );
                    self.status = format!(
                        "イメージ取得{} · 読取済み {} · 読取失敗 {} · 未読 {}",
                        match result.state.as_str() {
                            "completed" => "完了",
                            "cancelled" => "を中断",
                            _ => "（一部取得）",
                        },
                        view::size_text(Some(result.summary.copied_bytes)),
                        view::size_text(Some(result.summary.failed_bytes)),
                        view::size_text(Some(result.summary.unattempted_bytes))
                    );
                }
                Message::Progress(p) => self.progress = p,
                Message::Discovered(result) => {
                    self.discovery_running = false;
                    self.stage = Stage::Settings;
                    self.status = format!(
                        "領域探索{}: {} 候補{}",
                        if result.cancelled {
                            "を中断"
                        } else {
                            "完了"
                        },
                        result.volumes.len(),
                        if result.truncated {
                            " / 未探索の範囲あり"
                        } else {
                            ""
                        }
                    );
                    self.discovery = Some(*result);
                    self.discovered_id = None;
                    self.discovery_dialog = true;
                }
                Message::Scanned(s, p) => {
                    self.status = format!(
                        "スキャン完了: {} 候補 / 解析不良: {} 件{}",
                        s.candidates.len(),
                        s.invalid_records,
                        if s.cancelled {
                            " / 取消：未探索領域があります"
                        } else if s.truncated {
                            " / 一部未探索：警告を確認してください"
                        } else {
                            ""
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
                    self.discovery_running = false;
                    self.status = format!("エラー: {e}");
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
        self.disk_status = "ディスクを確認中…".into();
        thread::spawn(move || {
            let _ = tx.send(list_disks().map_err(|e| e.to_string()));
        });
    }
    fn source_ui(&mut self, ui: &mut egui::Ui) {
        ui.strong("1  スキャンするディスク");
        ui.horizontal_wrapped(|ui| {
            let previous = self.source_mode;
            ui.selectable_value(
                &mut self.source_mode,
                SourceMode::Disk,
                "ディスク／ボリューム",
            );
            ui.selectable_value(&mut self.source_mode, SourceMode::Image, "保存したイメージ");
            if previous != self.source_mode {
                self.source.clear();
            }
        });
        if self.source_mode == SourceMode::Disk {
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.disk_messages.is_none(),
                        egui::Button::new("ディスク一覧を更新"),
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
                            "ディスク全体"
                        } else {
                            "ボリューム"
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
                "選択したディスク",
                &mut self.source,
                if cfg!(windows) {
                    r"例: \\.\PhysicalDrive1 または \\.\D:"
                } else if cfg!(target_os = "macos") {
                    "/dev/rdisk2 または /dev/rdisk2s1"
                } else {
                    "/dev/sdb または /dev/sdb1"
                },
            );
            ui.small("ディスク全体はMBR／GPTの各パーティションを検出します。読み取り専用でアクセスします。");
            ui.small(if cfg!(windows) {
                "アクセス拒否の場合は管理者として起動してください。"
            } else {
                "ディスクを読み取れる権限が必要です。"
            });
        } else {
            path_row(ui, "イメージファイル", &mut self.source, Picker::Open);
            ui.small("RAWボリュームまたはMBR／GPTディスクイメージを読み取ります。");
        }
    }
    fn setup_ui(&mut self, ui: &mut egui::Ui) {
        egui::ScrollArea::vertical().id_salt("settings-scroll").auto_shrink([false,false]).show(ui, |ui| {
            ui.set_max_width(ui.available_width().min(960.0));
            ui.add_space(8.0);
            ui.heading("保存方法と検索条件");
            ui.label("ディスクをスキャンしてから、復元するファイルを選択します。");
            ui.add_space(16.0);
            self.source_ui(ui);
            if ui.add_enabled(!self.source.trim().is_empty(),egui::Button::new("ディスクのイメージを作成・再開")).clicked(){self.image_dialog=true;}
            if self.source_mode==SourceMode::Image && ui.add_enabled(!self.source.trim().is_empty(),egui::Button::new("読取マップを確認")).clicked(){self.map_source=PathBuf::from(&self.source);self.map_page=None;self.map_dialog=true;self.load_map_page(0,None);}
            self.discovery_controls(ui);
            ui.add_space(16.0);
            ui.strong("2  保存方法");
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(&mut self.save_mode, SaveMode::Original, "元の場所へ戻す");
                ui.selectable_value(&mut self.save_mode, SaveMode::Directory, "フォルダーに保存");
                if ui.selectable_value(&mut self.save_mode, SaveMode::Zip, "ZIPファイルに保存").changed()
                    && !self.destination.to_lowercase().ends_with(".zip") { self.destination.clear(); }
            });
            if self.save_mode != SaveMode::Original {
                let picker = if self.save_mode == SaveMode::Zip { Picker::Zip } else { Picker::Folder };
                path_row(ui, "保存先", &mut self.destination, picker);
            } else {
                ui.label("確認できた元のフォルダーとファイル名で戻します。元の場所が不明な候補はフォルダー保存を選んでください。");
            }
            ui.horizontal_wrapped(|ui| {
                ui.label("同じ名前がある場合");
                ui.selectable_value(&mut self.collision, CollisionPolicy::KeepBoth, "両方を残す（復元 1）");
                ui.selectable_value(&mut self.collision, CollisionPolicy::Skip, "スキップ");
            });
            ui.small("既存ファイルは上書きしません。同名フォルダーは統合します。");
            ui.add_space(16.0);
            ui.collapsing("詳細設定 · 検索方式・範囲・深度・拡張子", |ui| {
                ui.horizontal_wrapped(|ui| {
                    ui.label("検索方式");
                    ui.selectable_value(&mut self.carve, false, "管理情報 / Quick");
                    ui.selectable_value(&mut self.carve, true, "写真・文書・動画 / Carving");
                });
                text_row(ui, "探す拡張子", &mut self.extensions, "空欄: すべて / 例: jpg, pdf, docx, zip");
                if self.carve {
                    ui.label("PNG/JPEG、PDF、ZIP/Office、MP4/MOVを探索します。形式ごとに構造・CRC・参照位置を確認します。元の名前・削除状態は不明です。");
                    text_row(ui, "開始バイト", &mut self.range_start, "空欄: 0（ディスク/イメージの先頭）");
                    text_row(ui, "終了バイト", &mut self.range_end, "空欄: 末尾 / この位置は含みません");
                } else {
                    text_row(ui, "検索フォルダー", &mut self.path_prefix, "空欄: 全体 / 例: 写真/旅行");
                    text_row(ui, "検索深度", &mut self.depth, "空欄: 上限なし / 0: 直下 / 最大256");
                    ui.small("範囲・深度・拡張子は結果を絞ります。割当の競合確認はボリューム全体を調べます。");
                }
                path_row(ui, "セッション保存先（任意）", &mut self.work, Picker::Folder);
                ui.small("空欄・同じディスクの場合、スキャン結果はメモリーに保持します。");
                if self.save_mode == SaveMode::Original {
                    path_row(ui, "元ボリュームのルート（任意）", &mut self.original_root, Picker::Folder);
                    ui.small("通常は自動判定します。イメージなど判定できない場合は元ボリュームのルートを指定します。複数パーティションの個別指定はCLIを使います。");
                }
            });
            self.invalidate_changed_source();
            ui.add_space(16.0);
            ui.small("同じディスクにも保存できます。選択したファイルをすべて先に読み出します（合計512 MiBまで）。同じディスクへの保存は、未選択の削除データを上書きする可能性があります。");
        });
    }
    /// Drawn after content so Tab order follows the form, then the stable action row.
    fn actions_ui(&mut self, ui: &mut egui::Ui) {
        ui.separator();
        if self.stage == Stage::Settings {
            ui.horizontal_wrapped(|ui| {
                if ui.button("保存したセッションを開く").clicked()
                    && let Some(p) = rfd::FileDialog::new()
                        .add_filter("Rescue session", &["json"])
                        .pick_file()
                {
                    match Session::load(&p) {
                        Ok(session) => {
                            self.install_session(session, &p);
                            self.status =
                                "セッションを読み込みました。保存先は設定で確認してください。"
                                    .into();
                        }
                        Err(e) => self.status = e.to_string(),
                    }
                }
                if self.session.is_some() && ui.button("結果へ戻る").clicked() {
                    self.stage = Stage::Selecting;
                }
            });
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                let response = ui.add_enabled(
                    !self.source.trim().is_empty(),
                    egui::Button::new("スキャン開始").min_size(egui::vec2(176.0, 36.0)),
                );
                self.primary_rect = Some(response.rect);
                if response.clicked() {
                    self.start_scan();
                }
                ui.weak("ファイルの選択はスキャン後");
            });
        } else if !self.busy() {
            let bytes = self
                .session
                .as_ref()
                .map(|s| {
                    s.candidates
                        .iter()
                        .filter(|c| self.selected.contains(&c.id))
                        .filter_map(|c| c.size)
                        .fold(0u64, u64::saturating_add)
                })
                .unwrap_or(0);
            ui.horizontal_wrapped(|ui| {
                ui.label(format!(
                    "{} 件選択 · {}",
                    self.selected.len(),
                    view::size_text(Some(bytes))
                ));
                ui.label(match self.save_mode {
                    SaveMode::Original => "元の場所へ戻す",
                    SaveMode::Directory => "フォルダーに保存",
                    SaveMode::Zip => "ZIPに保存",
                });
                if self.save_mode != SaveMode::Original {
                    ui.add(egui::Label::new(&self.destination).truncate())
                        .on_hover_text(&self.destination);
                }
            });
            ui.horizontal(|ui| {
                if ui.button("設定に戻る").clicked() {
                    self.stage = Stage::Settings;
                }
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    let response = ui.add_enabled(
                        !self.selected.is_empty(),
                        egui::Button::new("選択したファイルを復元")
                            .min_size(egui::vec2(224.0, 36.0)),
                    );
                    self.primary_rect = Some(response.rect);
                    if response.clicked() {
                        self.start_recovery();
                    }
                });
            });
        } else {
            ui.label("進行状況と取消は画面下に表示しています。");
        }
    }
    fn results_ui(&mut self, ui: &mut egui::Ui) {
        let busy = self.busy();
        ui.add_enabled_ui(!busy, |ui| {
            ui.horizontal_wrapped(|ui| {
                ui.label("表示");
                ui.selectable_value(&mut self.view, ViewStyle::Details, "詳細");
                ui.selectable_value(&mut self.view, ViewStyle::Icons, "アイコン");
                ui.separator();
                ui.label("グループ");
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
                ui.checkbox(&mut self.show_evidence, "復元の根拠");
            });
            ui.horizontal_wrapped(|ui| {
                ui.label("結果を検索");
                if ui
                    .add(
                        egui::TextEdit::singleline(&mut self.filter)
                            .id(egui::Id::new("result-search"))
                            .desired_width(280.0)
                            .hint_text("名前・元のパス"),
                    )
                    .changed()
                {
                    self.view_dirty = true;
                }
                if ui.button("表示中の候補を選択").clicked()
                    && let Some(s) = &self.session
                {
                    let needle = self.filter.to_lowercase();
                    for c in &s.candidates {
                        if c.supported && c.matches_path_text(&needle) {
                            self.selected.insert(c.id.clone());
                        }
                    }
                }
                if ui.button("選択解除").clicked() {
                    self.selected.clear();
                }
            });
        });
        if self.view_dirty {
            if let Some(s) = &self.session {
                self.rows = view::rows(
                    &s.candidates,
                    &self.filter,
                    self.sort,
                    self.descending,
                    self.group,
                    &self.closed,
                );
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
        let busy = self.busy();
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
                        (SortColumn::Name, "名前"),
                        (SortColumn::Created, "作成日時"),
                        (SortColumn::Modified, "更新日時"),
                        (SortColumn::Type, "種類"),
                        (SortColumn::Size, "サイズ"),
                        (SortColumn::State, "状態"),
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
                                            format!("{}を復元対象に選択", c.name),
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
        let busy = matches!(self.stage, Stage::Recovering | Stage::Scanning);
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
        ui.heading("復元の根拠");
        let Some(s) = &self.session else {
            return;
        };
        ui.label(format!(
            "{} 候補 / {} 対応",
            s.candidates.len(),
            s.candidates.iter().filter(|c| c.supported).count()
        ));
        if s.cancelled || s.truncated {
            ui.colored_label(egui::Color32::YELLOW, "未探索の領域が残っています。");
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
                    format!("同じデータの別名（{}）", c.alternate_names.len()),
                    |ui| {
                        ui.label("名前が複数あっても、復元するデータは1件です。");
                        for alias in &c.alternate_names {
                            ui.label(&alias.original_path);
                            ui.small(if alias.path_verified {
                                "親参照を照合済み"
                            } else {
                                "階層は未確認"
                            });
                        }
                    },
                );
            }
            ui.add_space(10.0);
            ui.strong("配置の根拠");
            ui.label(format!("{:?}", c.extent_evidence));
            ui.label(
                match c.extent_evidence {
                    rescue_core::ExtentEvidence::PngValidated => "PNGのCRC・圧縮データ・画素を検証済み。",
                    rescue_core::ExtentEvidence::JpegValidated => "JPEGの構造・圧縮データの過不足・画素を検証済み。削除前の内容との一致は不明です。",
                    rescue_core::ExtentEvidence::PdfStructure => "PDFの参照表・ページ階層・ストリーム範囲を検証。描画内容と終端後の余白は未確認です。",
                    rescue_core::ExtentEvidence::ZipCrcValidated => "ZIPの管理情報・展開長・全エントリーCRCを検証。Officeの表示内容は未確認です。",
                    rescue_core::ExtentEvidence::MediaStructure => "動画のコンテナー・サンプル位置を検証。映像・音声の全フレーム復号は未実施です。",
                    rescue_core::ExtentEvidence::Unknown => "配置と形式の検証は未完了です。候補の警告を確認してください。",
                    _ => "管理情報から配置を取得。形式の内容検証は未実施。",
                },
            );
            ui.separator();
            ui.strong("割当状態");
            ui.label(match c.allocation_status {
                AllocationStatus::Unknown => "不明",
                AllocationStatus::Unallocated => allocation_label(c, s.candidate_filesystem(c)),
                AllocationStatus::Unreferenced => "探索した使用中エントリーからの参照なし",
                AllocationStatus::Conflict if s.candidate_filesystem(c) == FileSystem::Ntfs => {
                    "現在は割当済みの領域を含む：削除前の内容か要確認"
                }
                AllocationStatus::Conflict => "使用中領域と競合：復元不可",
            });
            ui.label(if s.filesystem == FileSystem::Raw {
                "元の名前・階層・削除状態は不明です。"
            } else if c.path_verified {
                "親参照を照合したパス"
            } else {
                "元のパスは未確認または推定を含みます。"
            });
            ui.separator();
            ui.strong("保存結果");
            if let Some(r) = self.results.iter().rev().find(|r| r.candidate_id == c.id) {
                ui.label(format!("{} / {} bytes", r.status, r.bytes_written));
                if let Some(h) = &r.sha256 {
                    ui.small("出力SHA-256");
                    ui.add(egui::Label::new(h).wrap());
                }
                if let Some(e) = &r.error {
                    ui.label(e);
                }
                if !r.missing_ranges.is_empty() {
                    ui.collapsing("読めなかった範囲", |ui| {
                        for gap in r.missing_ranges.iter().take(100) {
                            ui.label(format!(
                                "元媒体 {} から {} bytes / ファイル位置 {}",
                                gap.source_offset,
                                gap.length,
                                gap.logical_offset
                                    .map_or_else(|| "不明".into(), |at| at.to_string())
                            ));
                        }
                        if r.missing_ranges.len() > 100 {
                            ui.label("残りの範囲は復元レポートに記録しています。");
                        }
                    });
                }
            } else {
                ui.label("未保存");
            }
            ui.small("出力ハッシュは保存バイトの識別値です。削除前の内容との一致は正解データとの照合が必要です。");
            ui.collapsing("候補の警告", |ui| {
                for w in &c.warnings {
                    ui.label(w);
                }
            });
        }
        ui.collapsing("スキャンの警告", |ui| {
            for w in &s.warnings {
                ui.label(w);
            }
        });
    }
}
fn status_label(status: &str) -> &str {
    match status {
        "completed" => "保存完了",
        "partial" => "一部保存",
        "cancelled" => "取消",
        "failed" => "失敗",
        "incomplete" => "不完全",
        "skipped" => "スキップ",
        _ => status,
    }
}
fn set_selected(ids: &mut BTreeSet<String>, id: &str, selected: bool) {
    if selected {
        ids.insert(id.to_owned());
    } else {
        ids.remove(id);
    }
}
fn allocation_label(c: &Candidate, fs: FileSystem) -> &'static str {
    if c.size == Some(0) {
        return "データクラスターの割当なし（空ファイル）";
    }
    match fs {
        FileSystem::Fat32 => "FAT値0：空きとして記録",
        FileSystem::Exfat => "割当ビットマップ：空き領域",
        _ => "割当なし",
    }
}
#[derive(Clone, Copy)]
enum Picker {
    Open,
    Folder,
    Zip,
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
            .add_sized([64.0, 28.0], egui::Button::new("参照"))
            .on_hover_text(format!("{label}を選択"))
            .clicked()
        {
            let dialog = rfd::FileDialog::new();
            let path = match picker {
                Picker::Open => dialog.pick_file(),
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
        self.primary_rect = None;
        if matches!(self.stage, Stage::Selecting | Stage::Finished)
            && ctx.input_mut(|i| i.consume_key(egui::Modifiers::COMMAND, egui::Key::F))
        {
            ctx.memory_mut(|m| m.request_focus(egui::Id::new("result-search")));
        }
        if self.busy() || self.disk_messages.is_some() || self.map_messages.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::TopBottomPanel::top("title").show(ctx, |ui| {
            ui.add_space(8.0);
            ui.horizontal(|ui| {
                ui.heading("Rescue");
                ui.weak(rescue_core::VERSION);
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    for (n, label, active) in [
                        (
                            4,
                            "復元",
                            matches!(self.stage, Stage::Recovering | Stage::Finished),
                        ),
                        (3, "ファイル選択", self.stage == Stage::Selecting),
                        (2, "スキャン", self.stage == Stage::Scanning),
                        (
                            1,
                            "設定",
                            matches!(self.stage, Stage::Settings | Stage::Imaging),
                        ),
                    ] {
                        ui.colored_label(
                            if active {
                                egui::Color32::from_rgb(112, 199, 255)
                            } else {
                                egui::Color32::GRAY
                            },
                            format!("{n}  {label}"),
                        );
                        if n > 1 {
                            ui.weak("›");
                        }
                    }
                });
            });
            ui.add_space(7.0);
        });
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
                                "ファイルを保存中…".into()
                            } else {
                                format!(
                                    "{} / {} {:?}",
                                    self.progress.scanned_records,
                                    self.progress.total_records,
                                    self.progress.unit
                                )
                            }),
                    );
                    if ui.button("取消").clicked() {
                        self.cancel.store(true, Ordering::Relaxed);
                        self.status = "取消を要求しました。処理の終了を待っています。".into();
                    }
                });
            }
            if !self.report.is_empty() {
                ui.collapsing("保存場所・レポート", |ui| {
                    ui.add(egui::Label::new(&self.report).wrap());
                });
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
                            ui.heading(if self.discovery_running {
                                "失われた領域を探しています"
                            } else {
                                "スキャンしています"
                            });
                            ui.label(&self.source);
                            ui.label("完了後、復元するファイルを選択できます。");
                        }
                        Stage::Imaging => self.imaging_status(ui),
                        _ => self.results_ui(ui),
                    }
                },
            );
            self.actions_ui(ui);
        });
        if self.image_dialog && !self.busy() {
            self.imaging_dialog(ctx);
        }
        if self.map_dialog && !self.busy() {
            self.read_map_dialog(ctx);
        }
        if self.discovery_dialog && !self.busy() {
            self.discovery_dialog_ui(ctx);
        }
        if self.show_evidence && !wide && self.session.is_some() && self.stage != Stage::Settings {
            let mut open = true;
            egui::Window::new("復元の根拠")
                .open(&mut open)
                .default_width(320.0)
                .vscroll(true)
                .show(ctx, |ui| self.intelligence(ui));
            if !open {
                self.show_evidence = false;
            }
        }
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
fn main() -> eframe::Result {
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
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn primary_action_stays_inside_resized_viewports_and_scroll_content() {
        let ctx = egui::Context::default();
        let mut app = App::blank();
        app.source_mode = SourceMode::Image;
        for stage in [Stage::Settings, Stage::Selecting, Stage::Finished] {
            app.stage = stage;
            if stage != Stage::Settings {
                app.session = Some(session(FileSystem::Ntfs));
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
    fn session(fs: FileSystem) -> Session {
        Session {
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
    fn inventory_completion_is_received_after_leaving_settings() {
        let mut app = App::blank();
        let (tx, rx) = mpsc::channel();
        app.disk_messages = Some(rx);
        app.install_session(session(FileSystem::Ntfs), Path::new("work/session.json"));
        tx.send(Ok(vec![])).unwrap();
        app.receive();
        assert_eq!(app.stage, Stage::Selecting);
        assert!(app.session.is_some() && app.disk_messages.is_none());
        assert!(app.disk_status.contains("検出できません"));
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
