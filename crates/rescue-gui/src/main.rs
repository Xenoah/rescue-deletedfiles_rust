#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use rescue_core::{
    AllocationStatus, ExtentEvidence, FileSystem, Progress, ProgressUnit, RecoveryResult,
    ScanOptions, Session, recover, save_report, scan_carving, scan_image,
};
use std::{
    collections::BTreeSet,
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
};

enum Message {
    Progress(Progress),
    Scanned(Session, PathBuf),
    Recovered(Vec<RecoveryResult>, PathBuf),
    Error(String),
}
struct App {
    source: String,
    work: String,
    destination: String,
    filter: String,
    session: Option<Session>,
    selected: BTreeSet<String>,
    messages: Option<mpsc::Receiver<Message>>,
    cancel: Arc<AtomicBool>,
    busy: bool,
    recovering: bool,
    progress: Progress,
    status: String,
    report: String,
    carve: bool,
    focused: Option<String>,
    results: Vec<RecoveryResult>,
}
impl App {
    fn intelligence(&self, ui: &mut egui::Ui) {
        let accent = egui::Color32::from_rgb(101, 215, 190);
        ui.add_space(8.0);
        ui.colored_label(accent, "RECOVERY INTELLIGENCE");
        ui.heading("復元の根拠を確認");
        ui.label("検出・検証・保存の状態を分けて表示します。");
        ui.separator();
        let Some(session) = &self.session else {
            ui.strong("01  探索方式を選ぶ");
            ui.label("Quick：NTFS / FAT32 / exFATの管理情報をたどります。");
            ui.label("Carving：画像全体からPNGを探索し、CRCと画像データを検証します。");
            ui.separator();
            ui.strong("02  候補の根拠を確認");
            ui.label(
                "ファイル名をクリックすると、ここに配置根拠・割当状態・保存結果を表示します。",
            );
            ui.separator();
            ui.strong("03  別の媒体に保存");
            ui.label(
                "出力のSHA-256を記録します。削除前の内容との一致は、正解データとの照合が必要です。",
            );
            return;
        };
        ui.horizontal(|ui| {
            ui.strong(format!("{} 候補", session.candidates.len()));
            ui.colored_label(
                accent,
                format!(
                    "{} 対応",
                    session.candidates.iter().filter(|c| c.supported).count()
                ),
            );
        });
        ui.label(format!(
            "{} 保存完了 / {} 選択",
            self.results
                .iter()
                .filter(|r| r.status == "completed")
                .count(),
            self.selected.len()
        ));
        ui.label(if session.filesystem == FileSystem::Raw {
            "探索方式：PNG carving / 連続配置"
        } else {
            "探索方式：ファイルシステムの管理情報"
        });
        if session.truncated || session.cancelled {
            ui.colored_label(
                egui::Color32::YELLOW,
                "探索は途中です。未探索領域が残っています。",
            );
        }
        ui.separator();
        if let Some(c) = session
            .candidates
            .iter()
            .find(|c| Some(&c.id) == self.focused.as_ref())
        {
            ui.strong(&c.name);
            ui.small(&c.id);
            ui.add_space(8.0);
            ui.colored_label(accent, "01  配置・形式の根拠");
            ui.label(match c.extent_evidence {
                ExtentEvidence::PngValidated => {
                    "PNG：全チャンクのCRC・画素の展開を通過。補助チャンクの内容の意味は未検証。"
                }
                ExtentEvidence::Metadata => {
                    "ファイルシステムの管理情報から配置を取得。ファイル形式の内容検証は未実施。"
                }
                ExtentEvidence::ResidualFatChain => "残存FATチェーンから配置を取得。",
                ExtentEvidence::SingleCluster => "既知の先頭クラスター内に収まる範囲。",
                ExtentEvidence::NoFatChain => "exFATのNoFatChainで示された連続配置。",
                ExtentEvidence::Unknown => {
                    "検証未通過または配置情報が不足。警告を確認してください。"
                }
            });
            ui.add_space(8.0);
            ui.colored_label(accent, "02  割当と元の名前");
            ui.label(match c.allocation_status {
                AllocationStatus::Unknown => "割当状態：不明",
                AllocationStatus::Unallocated => "割当ビットマップ：空き領域",
                AllocationStatus::Unreferenced => "探索した使用中エントリーからの参照なし",
                AllocationStatus::Conflict => "使用中領域と競合：復元不可",
            });
            ui.label(if session.filesystem == FileSystem::Raw {
                "元の名前・階層・削除状態：不明。使用中ファイルや埋込画像も候補になります。"
            } else if c.path_verified {
                "親参照を照合したパス"
            } else {
                "元のパスは未確認または推定を含みます。"
            });
            ui.add_space(8.0);
            ui.colored_label(accent, "03  保存と照合");
            if let Some(r) = self.results.iter().rev().find(|r| r.candidate_id == c.id) {
                ui.label(format!("{} / {} bytes", r.status, r.bytes_written));
                if let Some(hash) = &r.sha256 {
                    ui.small("出力 SHA-256");
                    ui.add(egui::Label::new(hash).wrap());
                }
                if let Some(error) = &r.error {
                    ui.label(error);
                }
            } else {
                ui.label("この実行での保存結果：未記録");
            }
            ui.label("保存完了・形式検証済みでも、削除前の内容との一致は保証しません。");
            ui.collapsing("候補の詳細な警告", |ui| {
                for warning in &c.warnings {
                    ui.label(warning);
                }
            });
        }
        ui.collapsing("スキャンの制限・警告", |ui| {
            for warning in &session.warnings {
                ui.label(warning);
            }
        });
    }

    fn new(cc: &eframe::CreationContext<'_>) -> Self {
        cc.egui_ctx.set_visuals(egui::Visuals::dark());
        cc.egui_ctx.style_mut(|s| {
            s.spacing.item_spacing = egui::vec2(10.0, 8.0);
            s.visuals.selection.bg_fill = egui::Color32::from_rgb(22, 94, 110);
        });
        let mut font_paths = if cfg!(windows) {
            let windows = std::env::var_os("WINDIR")
                .map(PathBuf::from)
                .unwrap_or_else(|| PathBuf::from(r"C:\Windows"));
            vec![
                windows.join(r"Fonts\YuGothR.ttc"),
                windows.join(r"Fonts\meiryo.ttc"),
            ]
        } else {
            vec![PathBuf::from(
                "/usr/share/fonts/opentype/noto/NotoSansCJK-Regular.ttc",
            )]
        };
        if let Some(font) = std::env::var_os("RESCUE_FONT") {
            font_paths.insert(0, PathBuf::from(font));
        }
        for path in font_paths {
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
        Self {
            source: String::new(),
            work: String::new(),
            destination: String::new(),
            filter: String::new(),
            session: None,
            selected: BTreeSet::new(),
            messages: None,
            cancel: Arc::new(AtomicBool::new(false)),
            busy: false,
            recovering: false,
            progress: Progress::default(),
            status: "NTFS・FAT32・exFATボリュームのRAWイメージを選択してください。".into(),
            report: String::new(),
            carve: false,
            focused: None,
            results: Vec::new(),
        }
    }
    fn start_scan(&mut self) {
        if self.source.is_empty() || self.work.is_empty() {
            self.status = "イメージと作業フォルダーを指定してください。".into();
            return;
        }
        let source = PathBuf::from(&self.source);
        let work = PathBuf::from(&self.work);
        let carve = self.carve;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        self.busy = true;
        self.recovering = false;
        self.session = None;
        self.selected.clear();
        self.focused = None;
        self.results.clear();
        self.report.clear();
        self.progress = Progress::default();
        self.status = if carve {
            "PNG署名を探索し、CRC・画像データを検証中…"
        } else {
            "ファイルシステムの管理情報を解析中…"
        }
        .into();
        thread::spawn(move || {
            let scanner = if carve { scan_carving } else { scan_image };
            let result = scanner(&source, ScanOptions::default(), &cancel, |p| {
                let _ = tx.try_send(Message::Progress(p));
            })
            .and_then(|session| {
                let path = session.save(&work)?;
                Ok((session, path))
            });
            let message = match result {
                Ok((session, path)) => Message::Scanned(session, path),
                Err(e) => Message::Error(e.to_string()),
            };
            let _ = tx.send(message);
        });
    }
    fn start_recovery(&mut self) {
        if self.destination.is_empty() {
            self.status = "復元先フォルダーを指定してください。".into();
            return;
        }
        let Some(session) = self.session.clone() else {
            return;
        };
        let ids = self.selected.iter().cloned().collect::<Vec<_>>();
        let destination = PathBuf::from(&self.destination);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        self.busy = true;
        self.recovering = true;
        self.status = "選択したファイルを復元中…".into();
        thread::spawn(move || {
            let result = recover(&session, &ids, &destination, &cancel).and_then(|results| {
                let path = save_report(&destination, &session, &results)?;
                Ok((results, path))
            });
            let message = match result {
                Ok((results, path)) => Message::Recovered(results, path),
                Err(e) => Message::Error(e.to_string()),
            };
            let _ = tx.send(message);
        });
    }
    fn receive(&mut self) {
        let messages = self
            .messages
            .as_ref()
            .map(|rx| rx.try_iter().collect::<Vec<_>>())
            .unwrap_or_default();
        for message in messages {
            match message {
                Message::Progress(progress) => self.progress = progress,
                Message::Scanned(session, path) => {
                    self.busy = false;
                    self.status = format!(
                        "{}件の候補。検証未通過: {} / 打切り: {} / 取消: {}",
                        session.candidates.len(),
                        session.invalid_records,
                        session.truncated,
                        session.cancelled
                    );
                    self.report = format!("セッション: {}", path.display());
                    self.focused = session.candidates.first().map(|c| c.id.clone());
                    self.session = Some(session);
                }
                Message::Recovered(results, path) => {
                    self.busy = false;
                    let complete = results.iter().filter(|r| r.status == "completed").count();
                    self.status = format!(
                        "保存完了: {complete} / 不完全・失敗: {} / 取消: {}",
                        results.len() - complete,
                        self.cancel.load(Ordering::Relaxed)
                    );
                    self.report = format!(
                        "レポート: {}\n{}",
                        path.display(),
                        results
                            .iter()
                            .map(|r| format!(
                                "{}: {} {}",
                                r.candidate_id,
                                r.status,
                                r.error.as_deref().unwrap_or("")
                            ))
                            .collect::<Vec<_>>()
                            .join("\n")
                    );
                    self.results = results;
                }
                Message::Error(error) => {
                    self.busy = false;
                    self.status = format!("エラー: {error}");
                }
            }
        }
    }
}
fn path_row(ui: &mut egui::Ui, label: &str, value: &mut String, folder: bool) {
    ui.horizontal(|ui| {
        let label_id = ui.label(label).id;
        let reserved = if cfg!(windows) { 64.0 } else { 0.0 };
        let width = (ui.available_width() - reserved).max(80.0);
        ui.add(egui::TextEdit::singleline(value).desired_width(width))
            .labelled_by(label_id);
        #[cfg(windows)]
        if ui.button("選択").clicked() {
            let path = if folder {
                rfd::FileDialog::new().pick_folder()
            } else {
                rfd::FileDialog::new().pick_file()
            };
            if let Some(path) = path {
                *value = path.to_string_lossy().into_owned();
            }
        }
        #[cfg(not(windows))]
        let _ = folder;
    });
}
impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.receive();
        if self.busy {
            ctx.request_repaint_after(std::time::Duration::from_millis(100));
        }
        egui::TopBottomPanel::top("title").show(ctx, |ui| {
            ui.heading("Rescue");
            ui.label(format!(
                "{} — NTFS / FAT32 / exFAT / PNG carving",
                rescue_core::VERSION
            ));
            ui.label(
                "Quick：ボリュームの管理情報 / Carving：PNGの署名と内容を検証。元イメージは読み取り専用です。",
            );
        });
        egui::TopBottomPanel::bottom("status").show(ctx, |ui| {
            ui.label(&self.status);
            if self.busy {
                if self.recovering {
                    ui.horizontal(|ui| {
                        ui.spinner();
                        ui.label("ファイルを保存中…");
                    });
                } else {
                    let fraction = if self.progress.total_records > 0 {
                        self.progress.scanned_records as f32 / self.progress.total_records as f32
                    } else {
                        0.0
                    };
                    let text = if self.progress.total_records == 0 {
                        format!("解析済み {} 件", self.progress.scanned_records)
                    } else {
                        format!(
                            "{} {} / {}",
                            if self.progress.unit == ProgressUnit::Bytes {
                                "バイト"
                            } else {
                                "レコード"
                            },
                            self.progress.scanned_records,
                            self.progress.total_records
                        )
                    };
                    ui.add(egui::ProgressBar::new(fraction).text(text));
                }
                if ui.button("取消").clicked() {
                    self.cancel.store(true, Ordering::Relaxed);
                    self.status = "取消を要求しました。現在の処理の終了を待っています。".into();
                }
            }
            ui.collapsing("保存場所・結果", |ui| {
                ui.label(&self.report);
            });
        });
        egui::SidePanel::right("intelligence")
            .exact_width(305.0)
            .show(ctx, |ui| {
                egui::ScrollArea::vertical().show(ui, |ui| self.intelligence(ui));
            });
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_enabled_ui(!self.busy, |ui| {
                ui.horizontal(|ui| {
                    ui.label("探索方式");
                    ui.selectable_value(&mut self.carve, false, "管理情報 / Quick");
                    ui.selectable_value(&mut self.carve, true, "PNG / Carving");
                });
                path_row(ui, "イメージ", &mut self.source, false);
                path_row(ui, "作業フォルダー", &mut self.work, true);
                ui.horizontal(|ui| {
                    if ui.button("スキャン開始").clicked() { self.start_scan(); }
                    #[cfg(windows)]
                    if ui.button("保存したセッションを開く").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file() {
                        match Session::load(&path) {
                            Ok(session) => { self.carve = session.filesystem == FileSystem::Raw; self.focused = session.candidates.first().map(|c|c.id.clone()); self.session = Some(session); self.selected.clear(); self.results.clear(); self.status = "セッションを読み込みました。".into(); }
                            Err(e) => self.status = format!("エラー: {e}"),
                        }
                    }
                });
            });
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("ファイル名で検索");
                ui.text_edit_singleline(&mut self.filter);
            });
            if let Some(session) = &self.session {
                let filter = self.filter.to_lowercase();
                let visible = session.candidates.iter().filter(|c|c.name.to_lowercase().contains(&filter)).collect::<Vec<_>>();
                ui.horizontal(|ui| {
                    if ui.add_enabled(!self.busy, egui::Button::new("表示中の対応候補を選択")).clicked() {
                        for c in &visible { if c.supported { self.selected.insert(c.id.clone()); } }
                    }
                    if ui.add_enabled(!self.busy, egui::Button::new("選択解除")).clicked() { self.selected.clear(); }
                    ui.label(format!("{}件表示 / {}件選択",visible.len(),self.selected.len()));
                });
                egui::ScrollArea::vertical().max_height((ui.available_height()-120.0).max(100.0))
                    .show_rows(ui, 28.0, visible.len(), |ui, range| {
                        for index in range {
                            let c = visible[index];
                            ui.horizontal(|ui| {
                                let mut selected = self.selected.contains(&c.id);
                                let name_width = (ui.available_width() - 240.0).max(80.0);
                                let checkbox = ui.add_enabled(c.supported && !self.busy,
                                    egui::Checkbox::without_text(&mut selected));
                                let label = ui.allocate_ui_with_layout(egui::vec2(name_width, 24.0),
                                    egui::Layout::left_to_right(egui::Align::Center), |ui|
                                    ui.add(egui::Label::new(&c.original_path).truncate().sense(egui::Sense::click()))).inner
                                    .on_hover_text(&c.original_path);
                                let changed = checkbox.changed();
                                checkbox.labelled_by(label.id);
                                if label.clicked() { self.focused = Some(c.id.clone()); }
                                let name_clicked = label.clicked() && c.supported && !self.busy;
                                if name_clicked { selected = !selected; }
                                if changed || name_clicked {
                                    if selected { self.selected.insert(c.id.clone()); } else { self.selected.remove(&c.id); }
                                }
                                ui.label(c.size.map(|s|format!("{s} bytes")).unwrap_or_else(||"サイズ不明".into()));
                                if c.extent_evidence == ExtentEvidence::PngValidated { ui.colored_label(egui::Color32::from_rgb(101, 215, 190), "PNG検証済"); }
                                if !c.supported { ui.label("復元不可").on_hover_text(c.warnings.join("\n")); }
                                else if !c.warnings.is_empty() && c.extent_evidence != ExtentEvidence::PngValidated { ui.label("要確認").on_hover_text(c.warnings.join("\n")); }
                            });
                        }
                    });
            }
            ui.separator();
            ui.add_enabled_ui(!self.busy, |ui| {
                path_row(ui, "復元先", &mut self.destination, true);
                ui.label("保存先はNTFS等のハードリンク対応媒体を使用してください。既存ファイルは上書きしません。");
                if ui.add_enabled(!self.selected.is_empty(), egui::Button::new("選択したファイルを復元")).clicked() {
                    self.start_recovery();
                }
            });
        });
    }
}
impl Drop for App {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
fn main() -> eframe::Result {
    eframe::run_native(
        "Rescue — Recovery Alpha",
        eframe::NativeOptions {
            viewport: egui::ViewportBuilder::default().with_inner_size([1100.0, 760.0]),
            ..Default::default()
        },
        Box::new(|cc| Ok(Box::new(App::new(cc)))),
    )
}
