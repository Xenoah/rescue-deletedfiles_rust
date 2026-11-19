//! Browse a retained read-only Btrfs snapshot and restore selected historical paths.
use super::*;
use rescue_core::disk_image::{BtrfsRequest, btrfs};
use serde_json::Value;
use std::collections::BTreeMap;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Action {
    Roots,
    List,
    Restore,
}
struct Job {
    action: Action,
    cancel: Arc<AtomicBool>,
    thread: thread::JoinHandle<rescue_core::Result<(Value, bool)>>,
}
#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    source: PathBuf,
    pairs: Vec<(u64, u64)>,
    pair: usize,
    candidates: Vec<Value>,
    selected: BTreeSet<u64>,
    listed: bool,
    destination: String,
    results: Vec<Value>,
    error: Option<String>,
    job: Option<Job>,
}
impl Drop for State {
    fn drop(&mut self) {
        if let Some(job) = self.job.take() {
            job.cancel.store(true, Ordering::Relaxed);
            let _ = job.thread.join();
        }
    }
}
impl State {
    pub fn show(&mut self, source: &str) {
        *self = Self::default();
        self.source = source.into();
        self.open = true;
        self.start(Action::Roots, false);
    }
    fn start(&mut self, action: Action, all: bool) {
        let pair = if action == Action::Roots {
            None
        } else {
            self.pairs.get(self.pair).copied()
        };
        let restore = action == Action::Restore;
        let request = BtrfsRequest {
            source: self.source.clone(),
            current_root: pair.map(|v| v.0),
            snapshot_root: pair.map(|v| v.1),
            id: if restore && !all {
                self.selected.iter().copied().collect()
            } else {
                vec![]
            },
            all: restore && all,
            destination: restore.then(|| PathBuf::from(&self.destination)),
        };
        let cancel = Arc::new(AtomicBool::new(false));
        let token = cancel.clone();
        let thread = thread::spawn(move || btrfs(request, &token));
        self.job = Some(Job {
            action,
            cancel,
            thread,
        });
        self.error = None;
        self.results.clear();
    }
    fn receive(&mut self) {
        if !self.job.as_ref().is_some_and(|j| j.thread.is_finished()) {
            return;
        }
        let job = self.job.take().unwrap();
        let mut report = match job.thread.join() {
            Ok(Ok((v, _))) => v,
            Ok(Err(e)) => {
                self.error = Some(e.to_string());
                return;
            }
            Err(_) => {
                self.error = Some(
                    tr!(
                        "スナップショットの処理が終了しました",
                        "Snapshot worker stopped unexpectedly"
                    )
                    .into(),
                );
                return;
            }
        };
        let rows = |v: &mut Value| v.as_array_mut().map(std::mem::take).unwrap_or_default();
        match job.action {
            Action::Roots => {
                let roots = rows(&mut report["roots"]);
                let parents: BTreeMap<_, _> = roots
                    .iter()
                    .filter_map(|r| Some((r["uuid"].as_str()?, r["id"].as_u64()?)))
                    .filter(|(uuid, _)| *uuid != "00000000000000000000000000000000")
                    .collect();
                self.pairs = roots
                    .iter()
                    .filter(|r| r["readonly"] == true)
                    .filter_map(|r| {
                        Some((*parents.get(r["parent_uuid"].as_str()?)?, r["id"].as_u64()?))
                    })
                    .filter(|(c, s)| c != s)
                    .collect();
            }
            Action::List => {
                self.candidates = rows(&mut report["candidates"]);
                self.listed = true;
            }
            Action::Restore => self.results = rows(&mut report["results"]),
        }
    }
    fn choose_pair(&mut self, ui: &mut egui::Ui) {
        let previous = self.pair;
        let label = |(current, snapshot): (u64, u64)| {
            tr_format!(
                "現行 {} / スナップショット {}",
                "Current {} / Snapshot {}",
                current,
                snapshot
            )
        };
        egui::ComboBox::from_id_salt("snapshot-root-pair")
            .selected_text(label(self.pairs[self.pair]))
            .show_ui(ui, |ui| {
                for (index, &pair) in self.pairs.iter().enumerate() {
                    ui.selectable_value(&mut self.pair, index, label(pair));
                }
            });
        if previous != self.pair {
            self.listed = false;
            self.selected.clear();
            self.candidates.clear();
            self.results.clear();
            self.error = None;
        }
    }

    fn select_files(&mut self, ui: &mut egui::Ui) -> Option<(Action, bool)> {
        ui.label(tr_format!(
            "復元候補 {} 件",
            "{} historical files",
            self.candidates.len()
        ));
        let height = (ui.ctx().content_rect().height() - 400.0).clamp(65.0, 230.0);
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("btrfs-candidates")
            .max_height(height)
            .show_rows(
                ui,
                ui.spacing().interact_size.y,
                self.candidates.len(),
                |ui, range| {
                    for row in &self.candidates[range] {
                        let Some(id) = row["id"].as_u64() else {
                            continue;
                        };
                        let name = row["original_paths"][0].as_str().unwrap_or("");
                        let size = view::size_text(row["size"].as_u64());
                        let mut checked = self.selected.contains(&id);
                        if ui
                            .checkbox(&mut checked, format!("{name}  ({size})"))
                            .changed()
                        {
                            if checked {
                                self.selected.insert(id);
                            } else {
                                self.selected.remove(&id);
                            }
                        }
                    }
                },
            );
        scrolling::finish(ui, &mut scroll, tr!("過去のファイル", "Historical files"));
        path_row(
            ui,
            tr!("保存先フォルダー", "Destination folder"),
            &mut self.destination,
            Picker::Folder,
        );
        let ready = !self.destination.trim().is_empty();
        ui.horizontal_wrapped(|ui| {
            let selected =
                egui::Button::new(tr!("選択したファイルを復元", "Restore selected files"));
            let all = egui::Button::new(tr!("すべて復元", "Restore all files"));
            if ui
                .add_enabled(ready && !self.selected.is_empty(), selected)
                .clicked()
            {
                Some((Action::Restore, false))
            } else if ui
                .add_enabled(ready && !self.candidates.is_empty(), all)
                .clicked()
            {
                Some((Action::Restore, true))
            } else {
                None
            }
        })
        .inner
    }

    fn show_results(&self, ui: &mut egui::Ui) {
        let saved = self
            .results
            .iter()
            .filter(|r| r.get("path").is_some())
            .count();
        ui.label(tr_format!(
            "保存 {} 件 / 失敗 {} 件",
            "{} saved / {} failed",
            saved,
            self.results.len() - saved
        ));
        let height = ui.text_style_height(&egui::TextStyle::Body);
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt("btrfs-results")
            .max_height(96.0)
            .show_rows(ui, height, self.results.len(), |ui, range| {
                for row in &self.results[range] {
                    let name = row["original_paths"][0].as_str().unwrap_or("");
                    let value = row["error"]
                        .as_str()
                        .or_else(|| row["path"].as_str())
                        .unwrap_or("");
                    let digest = row["sha256"].as_str().unwrap_or("");
                    ui.add(egui::Label::new(format!("{name}  {value}")).truncate())
                        .on_hover_text(format!("{value}\n{digest}"));
                }
            });
        scrolling::finish(ui, &mut scroll, tr!("復元結果", "Recovery results"));
    }

    fn controls(&mut self, ui: &mut egui::Ui) -> Option<(Action, bool)> {
        ui.label(self.source.display().to_string());
        if let Some(job) = &self.job {
            let task = match job.action {
                Action::Roots => tr!("サブボリュームを確認中", "Reading subvolumes"),
                Action::List => tr!("現在と過去のパスを比較中", "Comparing historical paths"),
                Action::Restore => tr!("データを検証して保存中", "Verifying and saving files"),
            };
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(task);
                if ui.button(tr!("キャンセル", "Cancel")).clicked() {
                    job.cancel.store(true, Ordering::Relaxed);
                }
            });
            ui.ctx()
                .request_repaint_after(std::time::Duration::from_millis(80));
            return None;
        }
        if let Some(error) = &self.error {
            theme::error_banner(ui, error);
        }
        if self.pairs.is_empty() {
            if self.error.is_none() {
                ui.label(tr!(
                    "現行のサブボリュームに対応する読み取り専用スナップショットがありません。",
                    "No retained read-only snapshot identifies a current subvolume as its parent."
                ));
            }
            return None;
        }
        self.choose_pair(ui);
        if ui
            .button(tr!(
                "過去にだけ存在するファイルを表示",
                "List historical files"
            ))
            .clicked()
        {
            self.selected.clear();
            return Some((Action::List, false));
        }
        let action = if self.listed {
            self.select_files(ui)
        } else {
            None
        };
        if !self.results.is_empty() {
            self.show_results(ui);
        }
        action
    }

    pub fn draw(&mut self, ctx: &egui::Context) {
        self.receive();
        let mut action = None;
        let mut close = false;
        let modal = responsive_modal(ctx, "btrfs-snapshots", 680.0).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 680.0));
            ui.heading(tr!(
                "Btrfsスナップショットから復元",
                "Restore from a Btrfs snapshot"
            ));
            egui::ScrollArea::vertical()
                .id_salt("btrfs-dialog")
                .max_height((ctx.content_rect().height() - 170.0).max(100.0))
                .show_accessible(
                    ui,
                    tr!("スナップショットの復元", "Snapshot recovery"),
                    |ui| {
                        action = self.controls(ui);
                    },
                );
            if self.job.is_none() {
                close = ui.button(tr!("閉じる", "Close")).clicked();
            }
        });
        if let Some((action, all)) = action {
            self.start(action, all);
        } else if (close || modal.should_close()) && self.job.is_none() {
            self.open = false;
        }
    }
}
