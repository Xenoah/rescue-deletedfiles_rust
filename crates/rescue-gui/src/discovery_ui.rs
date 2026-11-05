use super::*;
impl App {
    fn start_discovery(&mut self) {
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        let path = PathBuf::from(&self.source);
        self.discovery_source = self.source.clone();
        self.recovered_volume = None;
        self.discovery = None;
        self.session = None;
        self.selected.clear();
        self.rows.clear();
        self.results.clear();
        self.report.clear();
        self.discovery_running = true;
        self.stage = Stage::Scanning;
        self.status = "ブート領域・管理情報を照合しています…".into();
        self.progress = Progress::default();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        thread::spawn(move || {
            let result = rescue_core::discover_volumes(
                &path,
                &rescue_core::DiscoveryOptions::default(),
                &cancel,
                |p| {
                    let _ = tx.try_send(Message::Progress(p));
                },
            );
            let _ = tx.send(match result {
                Ok(result) => Message::Discovered(Box::new(result)),
                Err(e) => Message::Error(e.to_string()),
            });
        });
    }
    pub(super) fn discovery_controls(&mut self, ui: &mut egui::Ui) {
        if self.discovery_source != self.source {
            self.recovered_volume = None;
            self.discovery = None;
            self.discovered_id = None;
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(
                    !self.source.trim().is_empty(),
                    egui::Button::new("失われた領域・破損ブートを探す"),
                )
                .clicked()
            {
                self.start_discovery();
            }
            if self.discovery.is_some() && ui.button("検出した領域を選ぶ").clicked() {
                self.discovery_dialog = true;
            }
        });
        if let Some(region) = &self.recovered_volume {
            ui.label(format!(
                "選択した領域: {:?} · 開始 {} · {}",
                region.filesystem.unwrap(),
                region.offset,
                view::size_text(Some(region.length))
            ));
            if ui.button("通常のディスク全体へ戻す").clicked() {
                self.recovered_volume = None;
                self.invalidate_changed_source();
            }
        }
    }
    pub(super) fn discovery_dialog_ui(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut use_selected = false;
        let response=egui::Modal::new(egui::Id::new("volume-discovery")).show(ctx,|ui| {
            ui.set_width((ctx.content_rect().width()-48.0).clamp(240.0,1000.0));
            ui.heading("検出した領域を選択");
            ui.label("範囲と管理情報を確認し、解析する領域を1つ選びます。元のパーティション表は変更しません。");
            egui::ScrollArea::both().max_height((ctx.content_rect().height()-280.0).max(100.0)).show(ui,|ui| {
                if let Some(result)=&self.discovery {
                    for warning in &result.warnings {ui.colored_label(egui::Color32::YELLOW,warning);}
                    if result.volumes.is_empty() {ui.label("整合性を確認できる領域は見つかりませんでした。写真・文書・動画のCarvingも利用できます。");}
                    egui::Grid::new("discovered-volumes").striped(true).min_row_height(32.0).spacing([16.0,8.0]).show(ui,|ui| {
                        for label in ["選択","形式","開始バイト","サイズ","ブート","現行の領域表","重複"] {ui.strong(label);}ui.end_row();
                        for volume in &result.volumes {
                            ui.radio_value(&mut self.discovered_id,Some(volume.id.clone()),format!("領域 {}",volume.region.index));
                            ui.label(format!("{:?}",volume.region.filesystem.unwrap()));ui.label(volume.region.offset.to_string());
                            ui.label(view::size_text(Some(volume.region.length)));
                            ui.label(match volume.region.recovery_boot.as_ref().unwrap().role {rescue_core::BootRole::Primary=>"主ブート",rescue_core::BootRole::Backup=>"バックアップ"});
                            ui.label(if volume.matches_current_table {"一致"} else {"表から確認できず"});
                            ui.label(if volume.overlaps.is_empty() {"なし"} else {"別候補と重なる"});ui.end_row();
                        }
                    });
                    if let Some(volume)=result.volumes.iter().find(|v|Some(&v.id)==self.discovered_id.as_ref()) {
                        ui.separator();ui.strong("この領域の根拠");for evidence in &volume.evidence {ui.label(evidence);}
                        if !volume.overlaps.is_empty() {ui.colored_label(egui::Color32::YELLOW,"重なる領域は別の解釈です。必要に応じて1つずつスキャンしてください。");}
                        ui.small("構造の整合性は、各ファイルの削除前の内容や完全性の証明とは異なります。");
                    }
                }
            });
            ui.separator();ui.horizontal(|ui| {
                if ui.add_sized([72.0,36.0],egui::Button::new("閉じる")).clicked() {close=true;}
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center),|ui| {
                    if ui.add_enabled(self.discovered_id.is_some(),egui::Button::new("この領域を設定に使用").min_size(egui::vec2(220.0,36.0))).clicked() {use_selected=true;}
                });
            });
        });
        if use_selected {
            self.recovered_volume = self
                .discovery
                .as_ref()
                .and_then(|d| {
                    d.volumes
                        .iter()
                        .find(|v| Some(&v.id) == self.discovered_id.as_ref())
                })
                .map(|v| v.region.clone());
            self.carve = false;
            self.invalidate_changed_source();
            self.discovery_dialog = false;
        } else if close || response.should_close() {
            self.discovery_dialog = false;
        }
    }
}
