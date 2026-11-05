//! Explicit one-device UAC consent; the application and its parsers stay ordinary.
use eframe::egui;
use std::{
    path::PathBuf,
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    time::Duration,
};

#[derive(Default)]
pub(super) struct State {
    pending: Option<mpsc::Receiver<Result<rescue_core::ReadAccess, String>>>,
    cancel: Arc<AtomicBool>,
    source: String,
    status: String,
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
impl State {
    pub fn show(&mut self, ui: &mut egui::Ui, source: &str) {
        if let Some(receiver) = &self.pending {
            match receiver.try_recv() {
                Ok(result) => {
                    self.pending = None;
                    self.status = match result {
                        Ok(access) => format!("{} の読取を許可しました。", access.source.display()),
                        Err(error) => format!("読取許可: {error}"),
                    };
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.status = "読取許可の処理が終了しました。もう一度実行できます。".into();
                }
                Err(mpsc::TryRecvError::Empty) => {
                    ui.ctx().request_repaint_after(Duration::from_millis(50))
                }
            }
        }
        if self.pending.is_none() && self.source != source {
            self.status.clear();
            self.source = source.into();
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    self.pending.is_none() && !source.trim().is_empty(),
                    egui::Button::new("ディスクの読取を許可"),
                )
                .clicked()
            {
                let path = PathBuf::from(source);
                self.source = source.into();
                self.cancel = Arc::new(AtomicBool::new(false));
                let cancel = self.cancel.clone();
                let (send, receive) = mpsc::channel();
                self.pending = Some(receive);
                self.status =
                    "Windowsの許可画面で、選択したディスクの読取を許可してください。".into();
                std::thread::spawn(move || {
                    let result =
                        rescue_core::request_read_access(&path, &cancel).map_err(|e| e.to_string());
                    let _ = send.send(result);
                });
                ui.ctx().request_repaint();
            }
            if self.pending.is_some() && ui.button("取消").clicked() {
                self.cancel.store(true, Ordering::Relaxed);
                self.status =
                    "取消待ちです。Windowsの許可画面が開いている場合は閉じてください。".into();
            }
        });
        ui.small("アクセス拒否の場合に使用します。画面は通常権限のまま、選択したディスクの読み取りだけを許可します。");
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }
}
