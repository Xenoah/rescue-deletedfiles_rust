//! Paged disk indexes. Database queries run off the UI thread; selection is by
//! stable candidate ID and survives page, sort and filter changes.
use super::*;
use rescue_core::{
    CandidateIndex, IndexCursor, IndexGroup, IndexPage, IndexQuery, IndexSort, IndexStatus,
};
use std::collections::{BTreeMap, VecDeque};
pub(super) const PAGE_SIZE: usize = 500;
#[derive(Default)]
pub(super) struct State {
    pub path: Option<PathBuf>,
    pub status: Option<IndexStatus>,
    pub query: IndexQuery,
    pub next: Option<IndexCursor>,
    current: Option<IndexCursor>,
    history: VecDeque<Option<IndexCursor>>,
    page: usize,
    receiver: Option<mpsc::Receiver<Reply>>,
    failed_query: Option<IndexQuery>,
    error: String,
    sizes: BTreeMap<String, Option<u64>>,
}
#[derive(Clone, Copy)]
enum Navigation {
    Reset,
    Next,
    Previous,
}
struct Reply {
    query: IndexQuery,
    cursor: Option<IndexCursor>,
    navigation: Navigation,
    result: Result<(IndexStatus, IndexPage), String>,
}
pub(super) struct Loaded {
    pub path: PathBuf,
    pub header: Session,
    pub status: IndexStatus,
    pub page: IndexPage,
    pub query: IndexQuery,
}
pub(super) fn load(path: PathBuf, query: IndexQuery) -> Result<Loaded, String> {
    let index = CandidateIndex::open(&path).map_err(|e| e.to_string())?;
    let header = index.session().map_err(|e| e.to_string())?;
    let mut status = index.status().map_err(|e| e.to_string())?;
    let page = index
        .page(&query, None, PAGE_SIZE)
        .map_err(|e| e.to_string())?;
    status.candidates = page.watermark;
    Ok(Loaded {
        path,
        header,
        status,
        page,
        query,
    })
}
impl State {
    pub fn loading(&self) -> bool {
        self.receiver.is_some()
    }
}
impl App {
    pub(super) fn index_query(&self) -> IndexQuery {
        IndexQuery {
            sort: match self.sort {
                SortColumn::Name => IndexSort::Name,
                SortColumn::Created => IndexSort::Created,
                SortColumn::Modified => IndexSort::Modified,
                SortColumn::Type => IndexSort::Type,
                SortColumn::Size => IndexSort::Size,
                SortColumn::State => IndexSort::State,
            },
            group: match self.group {
                GroupBy::None => IndexGroup::None,
                GroupBy::Folder => IndexGroup::Folder,
                GroupBy::Type => IndexGroup::Type,
                GroupBy::Created => IndexGroup::Created,
                GroupBy::Modified => IndexGroup::Modified,
                GroupBy::Size => IndexGroup::Size,
                GroupBy::Initial => IndexGroup::Initial,
                GroupBy::State => IndexGroup::State,
            },
            descending: self.descending,
            text: self.filter.clone(),
            supported_only: false,
        }
    }
    pub(super) fn install_index(&mut self, mut loaded: Loaded) {
        loaded.header.candidates = std::mem::take(&mut loaded.page.candidates);
        self.install_session(loaded.header, &loaded.path);
        self.filter = loaded.query.text.clone();
        self.work = loaded
            .path
            .parent()
            .and_then(Path::parent)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.status = format!(
            "スキャン{}: {} 候補 · {}",
            if loaded.status.state == "cancelled" {
                "を中断"
            } else {
                "を読込み"
            },
            loaded.status.candidates,
            if loaded.status.resumable {
                "再開できます"
            } else if loaded.status.state == "partial" {
                "一部未探索・警告を確認してください"
            } else {
                "探索完了"
            }
        );
        self.index = State {
            path: Some(loaded.path),
            status: Some(loaded.status),
            query: loaded.query,
            next: loaded.page.next_cursor,
            ..Default::default()
        };
    }
    pub(super) fn remember_selection_sizes(&mut self) {
        self.index.sizes.retain(|id, _| self.selected.contains(id));
        if let Some(session) = &self.session {
            for candidate in &session.candidates {
                if self.selected.contains(&candidate.id) {
                    self.index
                        .sizes
                        .insert(candidate.id.clone(), candidate.size);
                }
            }
        }
    }
    pub(super) fn selected_bytes(&self) -> u64 {
        self.index
            .sizes
            .iter()
            .filter(|(id, _)| self.selected.contains(*id))
            .filter_map(|(_, size)| *size)
            .fold(0u64, u64::saturating_add)
    }
    fn request_page(&mut self, navigation: Navigation) {
        if self.index.loading() || self.busy() {
            return;
        }
        let Some(path) = self.index.path.clone() else {
            return;
        };
        self.remember_selection_sizes();
        let cursor = match navigation {
            Navigation::Reset => None,
            Navigation::Next => self.index.next.clone(),
            Navigation::Previous => self.index.history.back().cloned().flatten(),
        };
        let query = self.index_query();
        let (tx, rx) = mpsc::sync_channel(1);
        self.index.receiver = Some(rx);
        self.index.failed_query = None;
        self.index.error.clear();
        thread::spawn(move || {
            let result = (|| {
                let index = CandidateIndex::open(&path)?;
                let mut status = index.status()?;
                let page = index.page(&query, cursor.as_ref(), PAGE_SIZE)?;
                status.candidates = page.watermark;
                Ok::<_, rescue_core::Error>((status, page))
            })()
            .map_err(|e| e.to_string());
            let _ = tx.send(Reply {
                query,
                cursor,
                navigation,
                result,
            });
        });
    }
    pub(super) fn receive_index(&mut self) {
        let Some(reply) = self
            .index
            .receiver
            .as_ref()
            .and_then(|rx| rx.try_recv().ok())
        else {
            return;
        };
        self.index.receiver = None;
        if reply.query != self.index_query() {
            return;
        }
        match reply.result {
            Err(error) => {
                self.index.error = error;
                self.index.failed_query = Some(reply.query);
            }
            Ok((status, page)) => {
                match reply.navigation {
                    Navigation::Reset => {
                        self.index.history.clear();
                        self.index.page = 0;
                    }
                    Navigation::Next => {
                        self.index.history.push_back(self.index.current.clone());
                        while self.index.history.len() > 128
                            || self
                                .index
                                .history
                                .iter()
                                .flatten()
                                .map(|c| c.last_key.len() + c.last_group.len() + 256)
                                .sum::<usize>()
                                > 16 * 1024 * 1024
                        {
                            self.index.history.pop_front();
                        }
                        self.index.page += 1;
                    }
                    Navigation::Previous => {
                        self.index.history.pop_back();
                        self.index.page = self.index.page.saturating_sub(1);
                    }
                }
                self.index.current = reply.cursor;
                self.index.next = page.next_cursor;
                self.index.status = Some(status);
                self.index.query = reply.query;
                if let Some(session) = &mut self.session {
                    session.candidates = page.candidates;
                    self.focused = session.candidates.first().map(|c| c.id.clone());
                }
                self.view_dirty = true;
            }
        }
    }
    pub(super) fn indexed_controls(&mut self, ui: &mut egui::Ui) {
        if self.index.path.is_none() {
            return;
        }
        let desired = self.index_query();
        if !self.index.loading()
            && self.index.query != desired
            && self.index.failed_query.as_ref() != Some(&desired)
        {
            self.request_page(Navigation::Reset);
        }
        let loading = self.index.loading();
        let can_query = !loading && !self.busy();
        let available = can_query && self.index.query == desired;
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(can_query, egui::Button::new("最初 / 最新の結果"))
                .clicked()
            {
                self.request_page(Navigation::Reset);
            }
            if ui
                .add_enabled(
                    available && !self.index.history.is_empty(),
                    egui::Button::new("前のページ"),
                )
                .clicked()
            {
                self.request_page(Navigation::Previous);
            }
            if ui
                .add_enabled(
                    available && self.index.next.is_some(),
                    egui::Button::new("次のページ"),
                )
                .clicked()
            {
                self.request_page(Navigation::Next);
            }
            ui.label(format!(
                "{} ページ · 表示 {} 件 / 保存済み {} 候補",
                self.index.page + 1,
                self.session.as_ref().map_or(0, |s| s.candidates.len()),
                self.index.status.as_ref().map_or(0, |s| s.candidates)
            ));
            if loading {
                ui.spinner();
                ui.label("検索中…");
            }
            if self.index.status.as_ref().is_some_and(|s| s.resumable)
                && ui
                    .add_enabled(available, egui::Button::new("スキャンを再開"))
                    .clicked()
            {
                self.resume_index();
            }
        });
        if self.group != GroupBy::None {
            ui.small(
                "グループ内の件数はこのページの表示件数です。選択はページを移動しても保持します。",
            );
        }
        if !self.index.error.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.index.error);
                if ui.button("再試行").clicked() {
                    self.request_page(Navigation::Reset);
                }
            });
        }
        if self.selected.len() >= 100_000 {
            ui.label("選択は1回100,000件までです。複数回に分けて復元してください。");
        }
    }
    fn resume_index(&mut self) {
        let Some(path) = self.index.path.clone() else {
            return;
        };
        self.remember_selection_sizes();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let query = self.index_query();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        self.stage = Stage::Scanning;
        self.status = "保存した位置からスキャンを再開中…".into();
        thread::spawn(move || {
            let result = rescue_core::resume_indexed_scan(&path, &cancel, |p| {
                let _ = tx.try_send(Message::IndexProgress(p));
            })
            .map_err(|e| e.to_string())
            .and_then(|p| load(p, query));
            let _ = tx.send(match result {
                Ok(loaded) => Message::IndexResumed(Box::new(loaded)),
                Err(e) => Message::Error(e),
            });
        });
    }
    pub(super) fn accept_resumed_index(&mut self, loaded: Loaded) {
        let selected = std::mem::take(&mut self.selected);
        let sizes = std::mem::take(&mut self.index.sizes);
        self.install_index(loaded);
        self.selected = selected;
        self.index.sizes = sizes;
    }
    pub(super) fn open_session(&mut self, path: PathBuf) {
        self.index = State::default();
        self.session = None;
        self.selected.clear();
        self.rows.clear();
        self.report.clear();
        self.filter.clear();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let query = self.index_query();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        self.stage = Stage::Scanning;
        self.progress = Progress::default();
        self.status = "保存したスキャンを読み込み中…".into();
        thread::spawn(move || {
            let result = (|| -> Result<Message, String> {
                if CandidateIndex::is_index(&path).map_err(|e| e.to_string())? {
                    Ok(Message::Indexed(Box::new(load(path, query)?)))
                } else {
                    Ok(Message::Scanned(
                        Box::new(Session::load(&path).map_err(|e| e.to_string())?),
                        path,
                    ))
                }
            })();
            let message = if cancel.load(Ordering::Relaxed) {
                Message::Error("保存したスキャンの読込みを取り消しました。".into())
            } else {
                result.unwrap_or_else(Message::Error)
            };
            let _ = tx.send(message);
        });
    }
}
