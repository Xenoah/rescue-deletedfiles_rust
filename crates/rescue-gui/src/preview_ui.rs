use eframe::egui;
use std::{
    sync::{
        Arc,
        atomic::{AtomicBool, Ordering},
        mpsc,
    },
    thread,
    time::Duration,
};

#[derive(Default)]
pub(super) struct State {
    open: bool,
    name: String,
    receiver: Option<mpsc::Receiver<Result<rescue_core::Preview, String>>>,
    cancel: Arc<AtomicBool>,
    preview: Option<rescue_core::Preview>,
    texture: Option<egui::TextureHandle>,
    error: String,
}
impl State {
    pub(super) fn start(&mut self, mut session: rescue_core::Session, id: &str) {
        self.cancel.store(true, Ordering::Relaxed);
        session.candidates.retain(|c| c.id == id);
        *self = Self::default();
        self.open = true;
        self.name = session
            .candidates
            .first()
            .map(|c| c.name.clone())
            .unwrap_or_default();
        let (tx, rx) = mpsc::channel();
        self.receiver = Some(rx);
        let cancel = self.cancel.clone();
        let id = id.to_owned();
        thread::spawn(move || {
            let _ = tx.send(
                rescue_core::preview_candidate(session, &id, &cancel)
                    .map_err(|e| format!("{}: {e}", e.code())),
            );
        });
    }
    pub(super) fn show(&mut self, ctx: &egui::Context) {
        if !self.open {
            return;
        }
        if let Some(receiver) = &self.receiver
            && let Ok(result) = receiver.try_recv()
        {
            match result {
                Ok(preview) => {
                    if !preview.rgba.is_empty() {
                        self.texture = Some(ctx.load_texture(
                            "candidate-preview",
                            egui::ColorImage::from_rgba_unmultiplied(
                                [preview.info.width as usize, preview.info.height as usize],
                                &preview.rgba,
                            ),
                            egui::TextureOptions::LINEAR,
                        ));
                    }
                    self.preview = Some(preview);
                }
                Err(error) => self.error = error,
            }
            self.receiver = None;
        }
        let mut close = false;
        let response=egui::Modal::new(egui::Id::new("candidate-preview")).show(ctx,|ui| {
            ui.set_width((ctx.content_rect().width()-48.0).clamp(240.0,640.0));
            ui.heading(tr!("ファイルのプレビュー", "File preview"));
            ui.add(egui::Label::new(&self.name).truncate());
            egui::ScrollArea::both().max_height((ctx.content_rect().height()-230.0).max(100.0)).show(ui,|ui| {
                if self.receiver.is_some() {ui.spinner();ui.label(tr!("元データを確認して表示を作成しています…", "Checking the source and preparing a preview…"));}
                if !self.error.is_empty() {ui.label(tr!("このファイルはプレビューできません。復元できるかは候補の状態で確認してください。", "This file cannot be previewed. Check the candidate status to see whether it can be recovered."));ui.label(&self.error);}
                if let Some(preview)=&self.preview {
                    if let Some(texture)=&self.texture {
                        let size=texture.size_vec2();let ratio=(ui.available_width()/size.x).min(1.0);
                        ui.add(egui::Image::new(texture).fit_to_exact_size(size*ratio));
                        ui.label(tr_format!("元画像 {} × {} / 表示 {} × {}", "Original {} × {} / Display {} × {}",preview.info.source_width,preview.info.source_height,preview.info.width,preview.info.height));
                    } else {
                        ui.add(egui::Label::new(egui::RichText::new(&preview.text).monospace()).wrap().selectable(true));
                        if preview.info.truncated {ui.label(tr!("先頭64 KiBを表示しています。", "Showing the first 64 KiB."));}
                    }
                    if !preview.info.warnings.is_empty() {
                        ui.collapsing(tr!("検証の詳細", "Verification details"),|ui| {for warning in &preview.info.warnings {ui.weak(warning);}});
                    }
                }
            });
            ui.separator();ui.label(tr!("表示用に変換した内容です。元ファイルの完全性を保証するものではありません。", "This content was converted for display. It does not guarantee that the original file is intact."));
            if ui.button(if self.receiver.is_some(){tr!("取消して閉じる", "Cancel and close")}else{tr!("閉じる", "Close")}).clicked(){close=true;}
        });
        if response.should_close() || close {
            self.cancel.store(true, Ordering::Relaxed);
            self.open = false;
            self.receiver = None;
            self.texture = None;
            self.preview = None;
        }
        if self.receiver.is_some() {
            ctx.request_repaint_after(Duration::from_millis(20));
        }
    }
}
impl Drop for State {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}
