//! Explicit one-device UAC consent; the application and its parsers stay ordinary.
use crate::language::Text;
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
    status: Text,
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
                        Ok(access) => message!(
                            "{} の読取を許可しました。",
                            "Read access granted for {}.",
                            access.source.display()
                        ),
                        Err(error) => message!("読取許可: {error}", "Read access: {error}"),
                    };
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.pending = None;
                    self.status = message!(
                        "読取許可の処理が終了しました。もう一度実行できます。",
                        "The read-access operation ended. You can try again."
                    );
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
                    egui::Button::new(tr!("ディスクの読取を許可", "Allow disk reading")),
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
                    message!("Windowsの許可画面で、選択したディスクの読取を許可してください。", "Allow access to the selected disk in the Windows permission prompt.");
                std::thread::spawn(move || {
                    let result =
                        rescue_core::request_read_access(&path, &cancel).map_err(|e| e.to_string());
                    let _ = send.send(result);
                });
                ui.ctx().request_repaint();
            }
            if self.pending.is_some() && ui.button(tr!("取消", "Cancel")).clicked() {
                self.cancel.store(true, Ordering::Relaxed);
                self.status =
                    message!("取消待ちです。Windowsの許可画面が開いている場合は閉じてください。", "Waiting for cancellation. Close the Windows permission prompt if it is still open.");
            }
        });
        ui.small(tr!("アクセス拒否の場合に使用します。画面は通常権限のまま、選択したディスクの読み取りだけを許可します。", "Use this when access is denied. The interface keeps its normal privileges; only reading the selected disk is allowed."));
        if !self.status.is_empty() {
            ui.label(&self.status);
        }
    }
}
