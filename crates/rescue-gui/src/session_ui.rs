use super::*;

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum Action {
    Open,
    Copy,
}

impl App {
    pub(super) fn session_dialog_ui(&mut self, ctx: &egui::Context) {
        let Some(action) = self.session_dialog else {
            return;
        };
        let mut start = false;
        let mut close = false;
        let response = responsive_modal(ctx, "session-open-copy", 600.0).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 600.0));
            ui.heading(if action == Action::Copy {
                tr!("セッションをコピー移行", "Migrate a session copy")
            } else {
                tr!("保存したセッションを開く", "Open saved session")
            });
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 160.0).max(100.0))
                .show(ui, |ui| {
                    path_row(ui, tr!("セッションファイル（JSON / SQLite）", "Session file (JSON / SQLite)"), &mut self.session_path, Picker::Session);
                    if action == Action::Copy {
                        path_row(ui, tr!("新しい索引の作業フォルダー（任意）", "Working folder for the new index (optional)"), &mut self.work, Picker::Folder);
                        ui.label(tr!("元の記録を残したまま、新しいIDの索引を作成します。途中で取り消したコピーは再開用セッションとして公開しません。", "Create an index with a new ID while keeping the original session. A cancelled copy is not published as a resumable session."));
                        ui.small(tr!("作業フォルダーが空欄なら、元の記録と同じフォルダー内の rescue-session-copies に保存します。", "If the working folder is blank, use rescue-session-copies beside the original session."));
                    } else {
                        ui.label(tr!("保存した候補と検索条件を読み込みます。旧版の索引で移行が必要な場合は、コピー移行を選んでください。", "Load saved candidates and search settings. If an old index requires migration, choose Migrate a session copy."));
                    }
                });
            ui.separator();
            ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                if ui.add_enabled(
                    !self.session_path.trim().is_empty(),
                    egui::Button::new(if action == Action::Copy { tr!("コピー移行を開始", "Start copy migration") } else { tr!("開く", "Open") })
                        .min_size(egui::vec2(176.0, 36.0)),
                ).clicked() {
                    start = true;
                }
                if ui.button(tr!("閉じる", "Close")).clicked() {
                    close = true;
                }
            });
        });
        if response.should_close() || close || start {
            self.session_dialog = None;
        }
        if start {
            let path = PathBuf::from(&self.session_path);
            match action {
                Action::Open => self.open_session(path),
                Action::Copy => self.import_session(path),
            }
        }
    }
}
