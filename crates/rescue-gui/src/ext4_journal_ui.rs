//! Select an unlinked inode from ext4 journal history, optionally checking an original digest.
use super::*;
use rescue_core::disk_image::{Ext4JournalRequest, ext4_journal};
use serde_json::Value;

struct Job {
    cancel: Arc<AtomicBool>,
    handle: thread::JoinHandle<rescue_core::Result<Value>>,
}
#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    source: PathBuf,
    rows: Vec<Value>,
    inode: Option<u32>,
    output: String,
    digest: String,
    unverified: bool,
    job: Option<Job>,
    saved: Option<Value>,
    error: Option<String>,
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            let _ = job.handle.join();
        }
    }
}
impl State {
    pub fn show(&mut self, source: &str) {
        *self = Self::default();
        self.open = true;
        self.source = source.into();
        self.start(false);
    }
    fn start(&mut self, save: bool) {
        let request = Ext4JournalRequest {
            source: self.source.clone(),
            inode: if save { self.inode } else { None },
            output: save.then(|| PathBuf::from(&self.output)),
            sha256: (save && !self.unverified).then(|| self.digest.trim().to_owned()),
            allow_unverified_content: save && self.unverified,
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        self.job = Some(Job {
            cancel,
            handle: thread::spawn(move || ext4_journal(request, &token)),
        });
        self.saved = None;
        self.error = None;
    }
    fn receive(&mut self) {
        if !self.job.as_ref().is_some_and(|j| j.handle.is_finished()) {
            return;
        }
        let job = self.job.take().unwrap();
        match job.handle.join() {
            Ok(Ok(mut report)) if report.get("saved").is_none() => {
                self.rows = report["candidates"]
                    .as_array_mut()
                    .map(std::mem::take)
                    .unwrap_or_default();
            }
            Ok(Ok(report)) => self.saved = Some(report),
            Ok(Err(err)) => self.error = Some(err.to_string()),
            Err(_) => {
                self.error = Some(
                    tr!(
                        "処理が中断されました",
                        "The recovery worker stopped unexpectedly"
                    )
                    .into(),
                )
            }
        }
    }
    fn controls(&mut self, ui: &mut egui::Ui) {
        ui.label(self.source.display().to_string());
        if let Some(job) = &self.job {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(tr!(
                    "ジャーナルとファイルを照合中",
                    "Checking the journal and file data"
                ));
                if ui.button(tr!("キャンセル", "Cancel")).clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
            return;
        }
        if let Some(error) = &self.error {
            theme::error_banner(ui, error);
        }
        if self.rows.is_empty() {
            if self.error.is_none() {
                ui.label(tr!(
                    "復元可能なinodeが見つかりませんでした。",
                    "No recoverable historical inodes were found."
                ));
            }
            return;
        }
        ui.label(tr_format!(
            "復元候補 {} 件",
            "{} historical files",
            self.rows.len()
        ));
        let height = (ui.ctx().content_rect().height() - 480.0).clamp(64.0, 200.0);
        let prior = self.inode;
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("ext4-journal-files")
            .max_height(height)
            .show_rows(
                ui,
                ui.spacing().interact_size.y,
                self.rows.len(),
                |ui, range| {
                    for r in &self.rows[range] {
                        let Some(id) = r["inode"].as_u64().and_then(|n| u32::try_from(n).ok())
                        else {
                            continue;
                        };
                        let name = r["historical_names"][0]["name"].as_str().unwrap_or("");
                        let size = view::size_text(r["size"].as_u64());
                        ui.selectable_value(
                            &mut self.inode,
                            Some(id),
                            format!("{name}    inode {id}   ({size})"),
                        );
                    }
                },
            );
        scrolling::finish(ui, &mut scroll, tr!("過去のファイル", "Historical files"));
        if prior != self.inode {
            self.output.clear();
            self.digest.clear();
            self.saved = None;
            self.error = None;
            self.unverified = false;
        }
        path_row(
            ui,
            tr!("保存先ファイル", "Output file"),
            &mut self.output,
            Picker::File,
        );
        ui.add_enabled_ui(!self.unverified, |ui| {
            text_row(
                ui,
                tr!("原本のSHA-256", "Original SHA-256"),
                &mut self.digest,
                "",
            )
        });
        ui.checkbox(
            &mut self.unverified,
            tr!(
                "原本ハッシュなしで未照合の内容を保存",
                "Save unverified content without the original hash"
            ),
        );
        let digest = self.digest.trim();
        let ready = self.inode.is_some()
            && !self.output.trim().is_empty()
            && (self.unverified
                || digest.len() == 64 && digest.bytes().all(|v| v.is_ascii_hexdigit()));
        if ui
            .add_enabled(
                ready,
                egui::Button::new(tr!("ファイルを復元", "Restore file")),
            )
            .clicked()
        {
            self.start(true);
        }
        if let Some(result) = &self.saved {
            let message = if result["original_sha256_matched"] == true {
                tr!(
                    "原本のSHA-256が一致しました。",
                    "The recovered bytes match the original SHA-256."
                )
            } else {
                tr!(
                    "未照合の内容を保存しました。",
                    "Saved with unverified content."
                )
            };
            ui.label(message);
            ui.label(result["saved"]["path"].as_str().unwrap_or(""));
            ui.monospace(result["saved"]["sha256"].as_str().unwrap_or(""));
        }
    }
    pub fn draw(&mut self, ctx: &egui::Context) {
        self.receive();
        let mut close = false;
        let modal = responsive_modal(ctx, "ext4-journal", 680.0).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 680.0));
            ui.heading(tr!(
                "ext4ジャーナルから復元",
                "Restore from the ext4 journal"
            ));
            egui::ScrollArea::vertical()
                .id_salt("ext4-journal-dialog")
                .max_height((ctx.content_rect().height() - 170.0).max(100.0))
                .show_accessible(ui, tr!("ext4ジャーナル", "ext4 journal"), |ui| {
                    self.controls(ui)
                });
            if self.job.is_none() {
                close = ui.button(tr!("閉じる", "Close")).clicked();
            }
        });
        if (close || modal.should_close()) && self.job.is_none() {
            self.open = false;
        }
    }
}
