//! Recover one known layout from the source selected on the settings screen.
use super::*;
use rescue_core::disk_image::ImportedFile;

#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    layout: String,
    pub output: String,
    error: Option<Text>,
    worker: Option<thread::JoinHandle<()>>,
}
impl State {
    pub fn show(&mut self) {
        self.error = None;
        self.open = true;
    }
    pub fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
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
            ui.heading(tr!("配置表からファイルを復元", "Recover a file from a layout"));
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 180.0).max(115.0))
                .show_accessible(ui, tr!("復元の設定", "Recovery settings"), |ui| {
                    ui.label(&self.source);
                    ui.small(tr!("元ファイルのSHA-256と、ディスク先頭からのバイト位置を記したJSON配置表を選んでください。", "Choose a JSON layout containing the original SHA-256 and byte offsets from the start of this source."));
                    path_row(ui,tr!("JSON配置表","JSON layout"),&mut state.layout,Picker::Open);
                    path_row(ui,tr!("復元ファイルの保存先","New output file"),&mut state.output,Picker::File);
                    if let Some(error) = &state.error { theme::error_banner(ui, &error.to_string()); }
                });
            ui.horizontal_wrapped(|ui| {
                start = ui.add_enabled(!state.layout.trim().is_empty() && !state.output.trim().is_empty(),
                    theme::primary(ui,tr!("ハッシュを照合して保存","Verify hash and save"),210.0)).clicked();
                close = ui.button(tr!("閉じる","Close")).clicked();
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
        self.status = message!(
            "配置表の領域を読み取っています。",
            "Reading the ranges in the layout."
        );
        let source = PathBuf::from(&self.source);
        let state = &mut self.extent_recovery;
        state.error = None;
        let layout = PathBuf::from(&state.layout);
        let output = PathBuf::from(&state.output);
        state.worker = Some(thread::spawn(move || {
            let result =
                rescue_core::disk_image::recover_extents(&source, &layout, &output, &cancel);
            let _ = tx.send(Message::ExtentsRecovered(result));
        }));
    }

    pub(super) fn finish_extent_recovery(&mut self, result: rescue_core::Result<ImportedFile>) {
        self.extent_recovery.join();
        self.stage = Stage::Settings;
        match result {
            Ok(file) => {
                self.status = message!(
                    "SHA-256照合済み。{} に {} バイトを保存しました。SHA-256 {}",
                    "Verified and saved {} ({} bytes). SHA-256 {}",
                    file.path.display(),
                    file.bytes,
                    file.sha256
                );
            }
            Err(rescue_core::Error::Cancelled) => {
                self.status = message!(
                    "配置表からの復元を取り消しました。",
                    "Layout recovery cancelled."
                );
            }
            Err(error) => {
                self.status_error = true;
                self.status = message!("復元できませんでした。{}", "Recovery failed. {}", error);
                self.extent_recovery.error = Some(self.status.clone());
                self.extent_recovery.open = true;
            }
        }
    }
}
