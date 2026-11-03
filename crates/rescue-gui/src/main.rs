#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]

use eframe::egui;
use rescue_core::{
    Progress, RecoveryResult, ScanOptions, Session, recover, save_report, scan_image,
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
}
impl App {
    fn new(cc: &eframe::CreationContext<'_>) -> Self {
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
            status: "NTFSボリュームのRAWイメージを選択してください。".into(),
            report: String::new(),
        }
    }
    fn start_scan(&mut self) {
        if self.source.is_empty() || self.work.is_empty() {
            self.status = "イメージと作業フォルダーを指定してください。".into();
            return;
        }
        let source = PathBuf::from(&self.source);
        let work = PathBuf::from(&self.work);
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        self.busy = true;
        self.recovering = false;
        self.session = None;
        self.selected.clear();
        self.progress = Progress::default();
        self.status = "削除済みMFTレコードを解析中…".into();
        thread::spawn(move || {
            let result = scan_image(&source, ScanOptions::default(), &cancel, |p| {
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
                        "{}件の候補。破損レコード: {} / 打切り: {} / 取消: {}",
                        session.candidates.len(),
                        session.invalid_records,
                        session.truncated,
                        session.cancelled
                    );
                    self.report = format!("セッション: {}", path.display());
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
                "{} — NTFSイメージ復元 / Alpha",
                rescue_core::VERSION
            ));
            ui.label(
                "対応: 通常・圧縮ファイル、ADSの別ファイル保存。暗号化・実ディスク・FAT/exFAT・RAIDは未対応。",
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
                    ui.add(egui::ProgressBar::new(fraction).text(format!(
                        "MFT {} / {}",
                        self.progress.scanned_records, self.progress.total_records
                    )));
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
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.add_enabled_ui(!self.busy, |ui| {
                path_row(ui, "イメージ", &mut self.source, false);
                path_row(ui, "作業フォルダー", &mut self.work, true);
                ui.horizontal(|ui| {
                    if ui.button("スキャン開始").clicked() { self.start_scan(); }
                    #[cfg(windows)]
                    if ui.button("保存したセッションを開く").clicked()
                        && let Some(path) = rfd::FileDialog::new().pick_file() {
                        match Session::load(&path) {
                            Ok(session) => { self.session = Some(session); self.selected.clear(); self.status = "セッションを読み込みました。".into(); }
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
                                if ui.add_enabled(c.supported && !self.busy,
                                    egui::Checkbox::new(&mut selected, &c.original_path)).changed() {
                                    if selected { self.selected.insert(c.id.clone()); } else { self.selected.remove(&c.id); }
                                }
                                ui.label(c.size.map(|s|format!("{s} bytes")).unwrap_or_else(||"サイズ不明".into()));
                                if !c.supported { ui.label("未対応").on_hover_text(c.warnings.join("\n")); }
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
