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
    requested_query: Option<IndexQuery>,
    error: Text,
    cancel: QueryCancel,
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
    pub deferred_page: bool,
}
pub(super) fn load(
    path: PathBuf,
    query: IndexQuery,
    cancel: &Arc<AtomicBool>,
) -> Result<Loaded, String> {
    let index = CandidateIndex::open(&path).map_err(|e| e.to_string())?;
    index
        .prepare_query(&query, cancel)
        .map_err(|e| e.to_string())?;
    let header = index.session().map_err(|e| e.to_string())?;
    let mut status = index.status().map_err(|e| e.to_string())?;
    let page = index
        .page_with_cancel(&query, None, PAGE_SIZE, cancel)
        .map_err(|e| e.to_string())?;
    status.candidates = page.watermark;
    Ok(Loaded {
        path,
        header,
        status,
        page,
        query,
        deferred_page: false,
    })
}

/// A finished or paused scan has already committed its checkpoint. Present
/// that state immediately, then query the saved candidates as a separate,
/// cancellable UI operation. The scan's cancellation flag must not cancel
/// its own result display or turn a saved pause into an error.
pub(super) fn load_scan_result(path: PathBuf, query: IndexQuery) -> Result<Loaded, String> {
    let index = CandidateIndex::open(&path).map_err(|e| e.to_string())?;
    let header = index.session().map_err(|e| e.to_string())?;
    let status = index.status().map_err(|e| e.to_string())?;
    let page = IndexPage {
        session_id: header.id.clone(),
        index_id: status.index_id.clone(),
        watermark: status.candidates,
        candidates: Vec::new(),
        next_cursor: None,
    };
    Ok(Loaded {
        path,
        header,
        status,
        page,
        query,
        deferred_page: true,
    })
}
#[derive(Default)]
struct QueryCancel(Arc<AtomicBool>);
impl Drop for QueryCancel {
    fn drop(&mut self) {
        self.0.store(true, Ordering::Relaxed);
    }
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
        let deferred_page = loaded.deferred_page;
        loaded.header.candidates = std::mem::take(&mut loaded.page.candidates);
        self.install_session(loaded.header, &loaded.path);
        self.filter = loaded.query.text.clone();
        self.work = loaded
            .path
            .parent()
            .and_then(Path::parent)
            .map(|p| p.to_string_lossy().into_owned())
            .unwrap_or_default();
        self.status = message!(
            "スキャン{}: {} 候補 · {}",
            "Scan {}: {} candidates · {}",
            if loaded.status.state == "cancelled" {
                message!("を中断", "paused")
            } else {
                message!("を読込み", "loaded")
            },
            loaded.status.candidates,
            if loaded.status.resumable {
                message!("再開できます", "Can resume")
            } else if loaded.status.state == "partial" {
                message!(
                    "一部未探索・警告を確認してください",
                    "Some ranges remain unscanned; check warnings"
                )
            } else {
                message!("探索完了", "Scan complete")
            }
        );
        self.index = State {
            path: Some(loaded.path),
            status: Some(loaded.status),
            query: loaded.query,
            next: loaded.page.next_cursor,
            ..Default::default()
        };
        if deferred_page {
            self.request_page(Navigation::Reset);
        }
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
        self.index.cancel = QueryCancel(Arc::new(AtomicBool::new(false)));
        let cancel = self.index.cancel.0.clone();
        self.index.failed_query = None;
        self.index.requested_query = Some(query.clone());
        self.index.error.clear();
        thread::spawn(move || {
            let result = (|| {
                let index = CandidateIndex::open(&path)?;
                index.prepare_query(&query, &cancel)?;
                let mut status = index.status()?;
                let page = index.page_with_cancel(&query, cursor.as_ref(), PAGE_SIZE, &cancel)?;
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
        if self.index.cancel.0.load(Ordering::Relaxed) {
            self.index.error = message!(
                "検索を取り消しました。条件を変えるか再試行してください。",
                "Search cancelled. Change the query or retry."
            );
            self.index.failed_query = Some(reply.query);
            return;
        }
        match reply.result {
            Err(error) => {
                self.index.error = error.into();
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
        if self.index.loading() && self.index.requested_query.as_ref() != Some(&desired) {
            self.index.cancel.0.store(true, Ordering::Relaxed);
        }
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
                .add_enabled(
                    can_query,
                    egui::Button::new(tr!("最初 / 最新の結果", "First / Latest results")),
                )
                .clicked()
            {
                self.request_page(Navigation::Reset);
            }
            if ui
                .add_enabled(
                    available && !self.index.history.is_empty(),
                    egui::Button::new(tr!("前のページ", "Previous page")),
                )
                .clicked()
            {
                self.request_page(Navigation::Previous);
            }
            if ui
                .add_enabled(
                    available && self.index.next.is_some(),
                    egui::Button::new(tr!("次のページ", "Next page")),
                )
                .clicked()
            {
                self.request_page(Navigation::Next);
            }
            ui.label(tr_format!(
                "{} ページ · 表示 {} 件 / 保存済み {} 候補",
                "Page {} · Showing {} / {} saved candidates",
                self.index.page + 1,
                self.session.as_ref().map_or(0, |s| s.candidates.len()),
                self.index.status.as_ref().map_or(0, |s| s.candidates)
            ));
            if loading {
                ui.spinner();
                ui.label(tr!(
                    "検索・並べ替えを準備中…",
                    "Preparing search and sorting…"
                ));
                if ui.button(tr!("検索を取消", "Cancel search")).clicked() {
                    self.index.cancel.0.store(true, Ordering::Relaxed);
                }
            }
            if self.index.status.as_ref().is_some_and(|s| s.resumable)
                && ui
                    .add_enabled(
                        available,
                        egui::Button::new(tr!("スキャンを再開", "Resume scan")),
                    )
                    .clicked()
            {
                self.resume_index();
            }
        });
        if self.group != GroupBy::None {
            ui.small(tr!(
                "グループ内の件数はこのページの表示件数です。選択はページを移動しても保持します。",
                "Group counts refer to this page. Selection is kept when you change pages."
            ));
        }
        if !self.index.error.is_empty() {
            ui.horizontal_wrapped(|ui| {
                ui.label(&self.index.error);
                if ui.button(tr!("再試行", "Retry")).clicked() {
                    self.request_page(Navigation::Reset);
                }
            });
        }
        if self.selected.len() >= 100_000 {
            ui.label(tr!("選択は1回100,000件までです。複数回に分けて復元してください。", "Select up to 100,000 files per batch. Recover larger selections in multiple batches."));
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
        self.status_error = false;
        self.stage = Stage::Scanning;
        self.status = message!(
            "保存した位置からスキャンを再開中…",
            "Resuming the scan from its saved position…"
        );
        thread::spawn(move || {
            let result = rescue_core::resume_indexed_scan(&path, &cancel, |p| {
                let _ = tx.try_send(Message::IndexProgress(p));
            })
            .map_err(|e| e.to_string())
            .and_then(|p| load_scan_result(p, query));
            let _ = tx.send(match result {
                Ok(loaded) => Message::IndexResumed(Box::new(loaded)),
                Err(e) => Message::Error(e.into()),
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
        self.status_error = false;
        self.stage = Stage::Scanning;
        self.progress = Progress::default();
        self.status = message!("保存したスキャンを読み込み中…", "Loading a saved scan…");
        thread::spawn(move || {
            let result = (|| -> Result<Message, String> {
                if CandidateIndex::is_index(&path).map_err(|e| e.to_string())? {
                    Ok(Message::Indexed(Box::new(load(path, query, &cancel)?)))
                } else {
                    Ok(Message::Scanned(
                        Box::new(Session::load(&path).map_err(|e| e.to_string())?),
                        path,
                    ))
                }
            })();
            let message = if cancel.load(Ordering::Relaxed) {
                Message::Error(message!(
                    "保存したスキャンの読込みを取り消しました。",
                    "Loading the saved scan was cancelled."
                ))
            } else {
                result.unwrap_or_else(|e| Message::Error(e.into()))
            };
            let _ = tx.send(message);
        });
    }
    pub(super) fn import_session(&mut self, path: PathBuf) {
        let work = if self.work.trim().is_empty() {
            path.parent()
                .unwrap_or(Path::new("."))
                .join("rescue-session-copies")
        } else {
            PathBuf::from(&self.work)
        };
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
        self.status_error = false;
        self.stage = Stage::Migrating;
        self.progress = Progress::default();
        self.status = message!(
            "新しい索引へコピー移行しています。元の記録は保持します…",
            "Copying the session into a new index. The original is kept…"
        );
        thread::spawn(move || {
            let result =
                rescue_core::IndexWriter::import_session_with_cancel(&path, &work, &cancel)
                    .map_err(|e| format!("{}: {e}", e.code()))
                    .and_then(|p| load(p, query, &cancel))
                    .map(|loaded| Message::Indexed(Box::new(loaded)))
                    .unwrap_or_else(|e| Message::Error(e.into()));
            let _ = tx.send(result);
        });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

    #[test]
    fn paused_scan_displays_committed_results_with_a_fresh_query_and_keeps_selection() {
        let root = std::env::temp_dir().join(format!(
            "rescue-paused-ui-{}-{}",
            std::process::id(),
            SystemTime::now()
                .duration_since(UNIX_EPOCH)
                .unwrap()
                .as_nanos()
        ));
        std::fs::create_dir(&root).unwrap();
        let mut header = crate::tests::session(FileSystem::Ntfs);
        header.id = "963596c3-c364-4bbb-906c-e66c4e557bfd".into();
        header.source = root.join("source.img");
        std::fs::write(&header.source, [0u8; 8192]).unwrap();
        header.fingerprint = rescue_core::inspect_image(&header.source).unwrap();
        let mut writer =
            rescue_core::IndexWriter::create(&root.join("work"), &header, "ntfs", &0).unwrap();
        let candidate = Candidate {
            id: "saved".into(),
            record: 1,
            fragment_offset: None,
            sequence: 1,
            stream_name: None,
            name: "saved.txt".into(),
            original_path: "saved.txt".into(),
            path_verified: true,
            alternate_names: vec![],
            size: Some(46),
            supported: true,
            warnings: vec![],
            record_sha256: String::new(),
            extent_evidence: Default::default(),
            allocation_status: Default::default(),
            created: None,
            modified: None,
        };
        header.cancelled = true;
        header.scanned_records = 1;
        header.total_records = 2;
        writer
            .commit(&header, &[candidate], "cancelled", true, &1)
            .unwrap();
        let path = writer.index().path().to_owned();
        drop(writer);
        let mut app = App::blank();
        app.cancel.store(true, Ordering::Relaxed);
        let query = app.index_query();
        // This was the post-scan path: it incorrectly rejected a saved pause.
        assert!(load(path.clone(), query.clone(), &app.cancel).is_err());
        let loaded = load_scan_result(path, query).unwrap();
        assert!(loaded.deferred_page && loaded.status.resumable);
        app.selected.insert("saved".into());
        app.index.sizes.insert("saved".into(), Some(46));
        app.accept_resumed_index(loaded);
        assert_eq!(app.stage, Stage::Selecting);
        assert!(app.cancel.load(Ordering::Relaxed));
        assert!(!app.index.cancel.0.load(Ordering::Relaxed));
        let until = Instant::now() + Duration::from_secs(5);
        while app.index.loading() {
            app.receive_index();
            assert!(Instant::now() < until, "paused result query did not finish");
            thread::sleep(Duration::from_millis(5));
        }
        assert_eq!(app.session.as_ref().unwrap().candidates[0].id, "saved");
        assert!(app.index.status.as_ref().unwrap().resumable);
        assert!(app.selected.contains("saved"));
        assert_eq!(app.selected_bytes(), 46);
        assert!(app.status.get(Language::English).contains("Can resume"));
        drop(app);
        std::fs::remove_dir_all(root).unwrap();
    }
}
