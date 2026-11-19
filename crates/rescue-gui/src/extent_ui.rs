//! Restore known file layouts or use position-independent chunk fingerprints.
use super::*;
use rescue_core::disk_image::{self, FingerprintedFile, ImportedFile};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Layout,
    FindChunks,
    CreateManifest,
}
pub(super) enum Outcome {
    File(ImportedFile),
    Manifest(FingerprintedFile),
}
#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    mode: Mode,
    layout: String,
    manifest: String,
    original: String,
    chunk_size: Option<u32>,
    sector_aligned: bool,
    output: String,
    error: Option<Text>,
    worker: Option<thread::JoinHandle<()>>,
}
impl State {
    pub fn show(&mut self) {
        self.mode = Mode::Layout;
        self.error = None;
        self.open = true;
    }
    pub fn show_fingerprints(&mut self, has_source: bool) {
        self.mode = if has_source {
            Mode::FindChunks
        } else {
            Mode::CreateManifest
        };
        self.error = None;
        self.open = true;
    }
    pub fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    pub fn working_title(&self) -> &'static str {
        match self.mode {
            Mode::Layout => tr!("配置表からファイルを復元中", "Recovering from the layout"),
            Mode::FindChunks => tr!(
                "一致する断片をディスク上で検索中",
                "Finding matching chunks on the disk"
            ),
            Mode::CreateManifest => {
                tr!("原本のハッシュを計算中", "Fingerprinting the original file")
            }
        }
    }
    pub fn working_output(&self) -> &str {
        if self.mode == Mode::CreateManifest {
            &self.manifest
        } else {
            &self.output
        }
    }
    fn ready(&self, source: &str) -> bool {
        match self.mode {
            Mode::CreateManifest => {
                !self.original.trim().is_empty() && !self.manifest.trim().is_empty()
            }
            Mode::FindChunks => {
                !source.trim().is_empty()
                    && !self.manifest.trim().is_empty()
                    && !self.output.trim().is_empty()
            }
            Mode::Layout => {
                !source.trim().is_empty()
                    && !self.layout.trim().is_empty()
                    && !self.output.trim().is_empty()
            }
        }
    }
    fn inputs(&mut self, ui: &mut egui::Ui, source: &str) {
        if self.mode != Mode::Layout {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(
                    &mut self.mode,
                    Mode::FindChunks,
                    tr!("断片を探して復元", "Find and restore"),
                );
                ui.selectable_value(
                    &mut self.mode,
                    Mode::CreateManifest,
                    tr!("原本から記録を作成", "Create from original"),
                );
            });
        }
        match self.mode {
            Mode::Layout => {
                ui.label(source);
                ui.small(tr!("元ファイルのSHA-256と、ディスク先頭からのバイト位置を記したJSON配置表を選んでください。", "Choose a JSON layout containing the original SHA-256 and byte offsets from the start of this source."));
                path_row(
                    ui,
                    tr!("JSON配置表", "JSON layout"),
                    &mut self.layout,
                    Picker::Open,
                );
            }
            Mode::FindChunks => {
                if source.trim().is_empty() {
                    ui.label(tr!(
                        "設定画面で復元元のディスクまたはイメージを選択してください。",
                        "Select a disk or image in Settings to search for the file."
                    ));
                } else {
                    ui.label(source);
                }
                ui.small(tr!(
                    "削除前に記録した断片のハッシュでディスクを検索します。",
                    "Search the disk with chunk hashes recorded before the file was lost."
                ));
                path_row(
                    ui,
                    tr!("断片ハッシュのJSON", "Fingerprint manifest"),
                    &mut self.manifest,
                    Picker::Open,
                );
                ui.checkbox(
                    &mut self.sector_aligned,
                    tr!("512バイト境界のみを検索", "Search only 512-byte boundaries"),
                );
            }
            Mode::CreateManifest => {
                path_row(
                    ui,
                    tr!(
                        "削除前の原本または同一のコピー",
                        "Original file or identical copy"
                    ),
                    &mut self.original,
                    Picker::Open,
                );
                let size = self.chunk_size.get_or_insert(4096);
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tr!("断片のバイト数", "Chunk size in bytes")).id;
                    egui::ComboBox::from_id_salt("fingerprint-chunk-size")
                        .selected_text(size.to_string())
                        .show_ui(ui, |ui| {
                            for n in [512, 1024, 4096, 65536, 1048576] {
                                ui.selectable_value(size, n, n.to_string());
                            }
                        })
                        .response
                        .labelled_by(label);
                });
                path_row(
                    ui,
                    tr!("新しい断片ハッシュのJSON", "New fingerprint manifest"),
                    &mut self.manifest,
                    Picker::File,
                );
            }
        }
        if self.mode != Mode::CreateManifest {
            path_row(
                ui,
                tr!("復元ファイルの保存先", "New output file"),
                &mut self.output,
                Picker::File,
            );
        }
    }
}
impl App {
    pub(super) fn extent_dialog(&mut self, ctx: &egui::Context) {
        let mut start = false;
        let mut close = false;
        let state = &mut self.extent_recovery;
        let modal = responsive_modal(ctx, "recover-known-layout", 620.0).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 620.0));
            ui.heading(if state.mode == Mode::Layout {
                tr!("配置表からファイルを復元", "Recover a file from a layout")
            } else {
                tr!("断片ハッシュによる復元", "File fingerprints")
            });
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 180.0).max(115.0))
                .show_accessible(ui, tr!("復元の設定", "Recovery settings"), |ui| {
                    state.inputs(ui, &self.source);
                    if let Some(error) = &state.error {
                        theme::error_banner(ui, &error.to_string());
                    }
                });
            ui.horizontal_wrapped(|ui| {
                let action = if state.mode == Mode::CreateManifest {
                    tr!("断片ハッシュを保存", "Save fingerprint manifest")
                } else {
                    tr!("ハッシュを照合して保存", "Verify hash and save")
                };
                start = ui
                    .add_enabled(state.ready(&self.source), theme::primary(ui, action, 210.0))
                    .clicked();
                close = ui.button(tr!("閉じる", "Close")).clicked();
            });
        });
        if modal.should_close() || close || start {
            state.open = false;
        }
        if start {
            self.start_extent_recovery();
        }
    }

    fn start_extent_recovery(&mut self) {
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancel);
        let (tx, rx) = mpsc::channel();
        self.messages = Some(rx);
        self.stage = Stage::RestoringExtents;
        self.status_error = false;
        let state = &mut self.extent_recovery;
        state.error = None;
        self.status = match state.mode {
            Mode::Layout => message!(
                "配置表の領域を読み取っています。",
                "Reading the ranges in the layout."
            ),
            Mode::FindChunks => message!(
                "一致する断片を探しています。",
                "Searching for matching chunks."
            ),
            Mode::CreateManifest => message!(
                "原本のハッシュを計算しています。",
                "Fingerprinting the original file."
            ),
        };
        let source = PathBuf::from(&self.source);
        let layout = PathBuf::from(&state.layout);
        let manifest = PathBuf::from(&state.manifest);
        let original = PathBuf::from(&state.original);
        let output = PathBuf::from(&state.output);
        let mode = state.mode;
        let alignment = if state.sector_aligned { 512 } else { 1 };
        let size = state.chunk_size.unwrap_or(4096);
        state.worker = Some(thread::spawn(move || {
            let result = match mode {
                Mode::Layout => disk_image::recover_extents(&source, &layout, &output, &cancel)
                    .map(Outcome::File),
                Mode::FindChunks => disk_image::recover_fingerprinted(
                    &source, &manifest, &output, alignment, 0, None, &cancel,
                )
                .map(|result| Outcome::File(result.file)),
                Mode::CreateManifest => {
                    disk_image::fingerprint_file(&original, &manifest, size, &cancel)
                        .map(Outcome::Manifest)
                }
            };
            let _ = tx.send(Message::ExtentsRecovered(result));
        }));
    }

    pub(super) fn finish_extent_recovery(&mut self, result: rescue_core::Result<Outcome>) {
        self.extent_recovery.join();
        self.stage = Stage::Settings;
        self.status_error = false;
        match result {
            Ok(Outcome::File(file)) => {
                self.status = message!(
                    "照合したファイルを {} に保存しました（{} バイト）。SHA-256 {}",
                    "Verified and saved {} ({} bytes). SHA-256 {}",
                    file.path.display(),
                    file.bytes,
                    file.sha256
                );
            }
            Ok(Outcome::Manifest(proof)) => {
                self.status = message!(
                    "{} に {} 断片の記録を保存しました。原本のSHA-256 {}",
                    "Saved {} with {} chunk fingerprints. Original SHA-256 {}",
                    proof.manifest.path.display(),
                    proof.chunks,
                    proof.sha256
                );
            }
            Err(rescue_core::Error::Cancelled) => {
                self.status = message!("処理を取り消しました。", "Operation cancelled.");
            }
            Err(error) => {
                self.status_error = true;
                self.status = message!("処理に失敗しました。{}", "Operation failed. {}", error);
                self.extent_recovery.error = Some(self.status.clone());
                self.extent_recovery.open = true;
            }
        }
    }
}
