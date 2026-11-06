use super::*;
impl App {
    fn imaging_source(&self) -> &str {
        if self.image_resume
            && let Some((source, destination, image)) = &self.image_original
            && destination == &self.image_destination
            && (&self.source == source || &self.source == image)
        {
            source
        } else {
            &self.source
        }
    }
    pub(super) fn load_map_page(&mut self, offset: u64, digest: Option<String>) {
        let path = self.map_source.clone();
        let (tx, rx) = mpsc::channel();
        self.map_messages = Some(rx);
        self.map_error.clear();
        thread::spawn(move || {
            let _ = tx.send(
                rescue_core::read_map_page(&path, offset, 100, digest.as_deref())
                    .map_err(|e| format!("{}: {e}", e.code())),
            );
        });
    }
    pub(super) fn read_map_dialog(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut request = None;
        let response = egui::Modal::new(egui::Id::new("read-map-dialog")).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 1000.0));
            ui.heading(tr!("読取マップ", "Read map"));
            ui.add(egui::Label::new(self.map_source.to_string_lossy()).wrap());
            if let Some(page) = &self.map_page {
                ui.label(tr_format!(
                    "読取済み {} · 失敗 {} · 未読 {}",
                    "Read {} · Failed {} · Unread {}",
                    view::size_text(Some(page.summary.copied_bytes)),
                    view::size_text(Some(page.summary.failed_bytes)),
                    view::size_text(Some(page.summary.unattempted_bytes))
                ));
                egui::ScrollArea::both()
                    .max_height((ctx.content_rect().height() - 280.0).max(100.0))
                    .show_accessible(ui, tr!("イメージ取得", "Disk imaging"), |ui| {
                        egui::Grid::new("read-map-ranges")
                            .striped(true)
                            .min_row_height(32.0)
                            .spacing([16.0, 8.0])
                            .show(ui, |ui| {
                                for label in [
                                    tr!("開始バイト", "Start byte"),
                                    tr!("サイズ", "Size"),
                                    tr!("状態", "Status"),
                                    tr!("試行回数", "Attempts"),
                                    tr!("読取時刻（UTC）", "Read time (UTC)"),
                                    tr!("OSエラー", "OS error"),
                                ] {
                                    ui.strong(label);
                                }
                                ui.end_row();
                                for range in &page.ranges {
                                    ui.label(range.offset.to_string());
                                    ui.label(view::size_text(Some(range.length)));
                                    ui.label(match range.state {
                                        rescue_core::ReadState::Copied => tr!("読取済み", "Read"),
                                        rescue_core::ReadState::Failed => {
                                            tr!("読取失敗", "Read failed")
                                        }
                                        rescue_core::ReadState::Unattempted => {
                                            tr!("未読", "Unread")
                                        }
                                    })
                                    .on_hover_text(
                                        range.sha256.as_deref().unwrap_or(tr!(
                                            "内容のハッシュは未取得",
                                            "Content hash unavailable"
                                        )),
                                    );
                                    ui.label(range.attempts.to_string());
                                    ui.label(range.read_utc.as_deref().unwrap_or("—"));
                                    ui.label(
                                        range
                                            .error_code
                                            .map_or_else(|| "—".into(), |n| n.to_string()),
                                    );
                                    ui.end_row();
                                }
                            });
                    });
            }
            if !self.map_error.is_empty() {
                ui.label(&self.map_error);
            }
            if self.map_messages.is_some() {
                ui.spinner();
            }
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                if ui
                    .add_enabled(
                        self.map_messages.is_none(),
                        egui::Button::new(tr!("最初のページ・更新", "First page / Refresh")),
                    )
                    .clicked()
                {
                    request = Some((0, None));
                }
                if let Some(page) = &self.map_page
                    && let Some(next) = page.next_offset
                    && ui
                        .add_enabled(
                            self.map_messages.is_none(),
                            egui::Button::new(tr!("次の100範囲", "Next 100 ranges")),
                        )
                        .clicked()
                {
                    request = Some((next, Some(page.map_digest.clone())));
                }
                if ui.button(tr!("閉じる", "Close")).clicked() {
                    close = true;
                }
            });
        });
        self.map_dialog = !response.should_close() && !close;
        if let Some((offset, digest)) = request {
            self.load_map_page(offset, digest);
        }
    }
    pub(super) fn imaging_status(&self, ui: &mut egui::Ui) {
        ui.add_space(24.0);
        ui.heading(tr!(
            "ディスクのイメージを取得しています",
            "Acquiring a disk image"
        ));
        ui.label(self.imaging_source());
        ui.label(&self.image_destination);
        if let Some(p) = &self.image_progress {
            ui.label(if p.phase == "verify_resume" {
                tr!(
                    "保存済みブロックのSHA-256を照合しています",
                    "Verifying saved blocks against their SHA-256 hashes"
                )
                .into()
            } else {
                tr_format!(
                    "読取パス {} · {}",
                    "Read pass {} · {}",
                    p.pass,
                    if p.phase == "retry" {
                        tr!("読めなかったセクターを再試行", "Retry unreadable sectors")
                    } else {
                        tr!("全体を取得", "Acquire the full source")
                    }
                )
            });
            ui.label(tr_format!(
                "読取済み {} / 読取失敗 {} / 未読 {}",
                "Read {} / Failed {} / Unread {}",
                view::size_text(Some(p.map.copied_bytes)),
                view::size_text(Some(p.map.failed_bytes)),
                view::size_text(Some(p.map.unattempted_bytes))
            ));
        }
        ui.label(
            tr!("取消後も読取マップを保持します。同じ元ディスクと保存先を指定すると再開できます。", "The read map is kept after cancellation. Select the same source disk and destination to resume."),
        );
        ui.small(tr!(
            "OSがディスクの応答を待っている場合、取消は読み取りが戻ってから反映されます。",
            "If the OS is waiting for the disk, cancellation takes effect when the read returns."
        ));
    }
    pub(super) fn imaging_dialog(&mut self, ctx: &egui::Context) {
        let mut start = false;
        let mut close = false;
        let response=egui::Modal::new(egui::Id::new("image-acquisition-dialog")).show(ctx,|ui|{
            ui.set_width((ctx.content_rect().width()-48.0).clamp(240.0,520.0));
            ui.heading(tr!("イメージの作成・再開", "Create or resume an image"));
            ui.label(tr_format!("取得元: {}", "Source: {}",self.imaging_source()));
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height()-160.0).max(100.0)).show_accessible(ui, tr!("読取マップ", "Read map"), |ui|{
            ui.label(tr!("元ディスクを読み取り専用で取得し、後からイメージをスキャンできます。", "Read the source disk without writing to it, then scan the acquired image."));
            path_row(ui,tr!("イメージ保存フォルダー", "Image destination folder"),&mut self.image_destination,Picker::Folder);
            ui.small(tr!("新規取得では空のフォルダーを指定します。物理ディスクからの取得は別の媒体へ保存します。", "Use an empty folder for a new acquisition. Save an image of a physical disk to a separate medium."));
            ui.checkbox(&mut self.image_resume,tr!("このフォルダーにある取得を再開する", "Resume the acquisition in this folder"));
            ui.horizontal_wrapped(|ui|{
                ui.label(tr!("読取ブロック", "Read block size"));
                egui::ComboBox::from_id_salt("image-block").selected_text(view::size_text(Some(self.image_block_size as u64))).show_ui(ui,|ui|{
                    for n in [1024*1024,4*1024*1024,16*1024*1024] {ui.selectable_value(&mut self.image_block_size,n,view::size_text(Some(n as u64)));}
                });
                ui.label(tr!("失敗セクターの追加試行", "Additional attempts for failed sectors"));ui.add(egui::DragValue::new(&mut self.image_retries).range(0..=3));
            });
            ui.label(tr!("RAW本体と .rmap を一緒に保管してください。読めなかった範囲を含むファイルは完全復元と表示しません。", "Keep the RAW file and .rmap together. Files that overlap unreadable ranges are not reported as fully recovered."));
            });
            ui.separator();ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui|{
                if ui.add_enabled(!self.image_destination.trim().is_empty(),egui::Button::new(if self.image_resume {tr!("取得を再開", "Resume acquisition")}else{tr!("取得を開始", "Start acquisition")}).min_size(egui::vec2(176.0,36.0))).clicked(){start=true;}
                if ui.button(tr!("閉じる", "Close")).clicked(){close=true;}
            });
        });
        self.image_dialog = !response.should_close() && !close && !start;
        if start {
            self.start_imaging();
        }
    }
    fn start_imaging(&mut self) {
        let source = PathBuf::from(self.imaging_source());
        let destination = PathBuf::from(&self.image_destination);
        self.image_original = Some((
            source.to_string_lossy().into_owned(),
            self.image_destination.clone(),
            destination
                .join("image.rescue.raw")
                .to_string_lossy()
                .into_owned(),
        ));
        self.image_pause_requested = false;
        let options = rescue_core::AcquisitionOptions {
            block_size: self.image_block_size,
            retries: self.image_retries,
            resume: self.image_resume,
        };
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        self.stage = Stage::Imaging;
        self.session = None;
        self.selected.clear();
        self.focused = None;
        self.results.clear();
        self.rows.clear();
        self.report.clear();
        self.progress = Progress::default();
        self.image_progress = None;
        self.status = message!("イメージを取得しています…", "Acquiring an image…");
        thread::spawn(move || {
            let result = rescue_core::acquire_image(&source, &destination, options, &cancel, |p| {
                let _ = tx.try_send(Message::ImagingProgress(p));
            });
            let _ = tx.send(match result {
                Ok(r) => Message::Imaged(r),
                Err(e) => Message::Error(format!("{}: {e}", e.code()).into()),
            });
        });
    }
}
