use super::*;
use rescue_core::{
    RaidConfig, RaidInspection, RaidLayout, RaidLevel, RaidMember, RaidSearchResult,
};
#[derive(Default)]
struct MemberInput {
    path: String,
    missing: bool,
    offset: String,
    length: String,
}
pub(super) struct State {
    pub dialog: bool,
    pub running: bool,
    #[cfg(test)]
    dialog_rect: Option<egui::Rect>,
    level: RaidLevel,
    layout: RaidLayout,
    stripe: u32,
    sector: u32,
    members: Vec<MemberInput>,
    destination: String,
    error: String,
    inspection: Option<RaidInspection>,
    search: Option<RaidSearchResult>,
    pinned: Option<RaidConfig>,
    reorder: bool,
}
impl Default for State {
    fn default() -> Self {
        Self {
            dialog: false,
            running: false,
            #[cfg(test)]
            dialog_rect: None,
            level: RaidLevel::Raid5,
            layout: RaidLayout::LeftSymmetric,
            stripe: 65536,
            sector: 512,
            members: (0..4)
                .map(|_| MemberInput {
                    offset: "0".into(),
                    ..Default::default()
                })
                .collect(),
            destination: String::new(),
            error: String::new(),
            inspection: None,
            search: None,
            pinned: None,
            reorder: false,
        }
    }
}
impl State {
    pub(super) fn load_source(&mut self, path: &Path) {
        let result = RaidConfig::load(path).map_err(|e| e.to_string());
        match result {
            Ok(mut c) => {
                for m in &mut c.members {
                    if let Some(p) = &m.path
                        && !p.is_absolute()
                    {
                        m.path = Some(path.parent().unwrap_or(Path::new(".")).join(p));
                    }
                }
                self.level = c.level;
                self.layout = c.layout;
                self.stripe = c.stripe_bytes;
                self.sector = c.sector_size;
                self.members = c
                    .members
                    .iter()
                    .map(|m| MemberInput {
                        path: m
                            .path
                            .as_ref()
                            .map(|p| p.to_string_lossy().into_owned())
                            .unwrap_or_default(),
                        missing: m.path.is_none(),
                        offset: m.offset.to_string(),
                        length: m.length.to_string(),
                    })
                    .collect();
                self.pinned = Some(c);
                self.inspection = None;
                self.search = None;
                self.error.clear();
            }
            Err(e) => self.error = e,
        }
    }
    fn config(&self) -> Result<RaidConfig, String> {
        let members = self
            .members
            .iter()
            .map(|member| {
                let path = if member.missing {
                    None
                } else {
                    if member.path.trim().is_empty() {
                        return Err(
                            "各メンバーのイメージを指定してください。欠損はチェックで明示します。"
                                .into(),
                        );
                    }
                    Some(PathBuf::from(member.path.trim()))
                };
                Ok(RaidMember {
                    path,
                    offset: member
                        .offset
                        .parse()
                        .map_err(|_| "開始位置はバイト単位の整数です。")?,
                    length: member
                        .length
                        .parse()
                        .map_err(|_| "使用長を指定するか、長さを揃えてください。")?,
                    fingerprint: None,
                })
            })
            .collect::<Result<Vec<_>, String>>()?;
        let config = RaidConfig {
            schema_version: 1,
            level: self.level,
            layout: self.layout,
            sector_size: self.sector,
            stripe_bytes: self.stripe,
            members,
        };
        if let Some(pinned) = &self.pinned {
            let mut shape = pinned.clone();
            for m in &mut shape.members {
                m.fingerprint = None;
            }
            if shape == config {
                return Ok(pinned.clone());
            }
        }
        Ok(config)
    }
    fn accept(&mut self, inspection: RaidInspection) {
        let c = &inspection.evidence.config;
        self.level = c.level;
        self.layout = c.layout;
        self.stripe = c.stripe_bytes;
        self.sector = c.sector_size;
        self.members = c
            .members
            .iter()
            .map(|m| MemberInput {
                path: m
                    .path
                    .as_ref()
                    .map(|p| p.to_string_lossy().into_owned())
                    .unwrap_or_default(),
                missing: m.path.is_none(),
                offset: m.offset.to_string(),
                length: m.length.to_string(),
            })
            .collect();
        self.pinned = Some(c.clone());
        self.inspection = Some(inspection);
        self.error.clear();
    }
    fn fill_lengths(&mut self) -> Result<(), String> {
        let mut lengths = Vec::new();
        for m in &self.members {
            if !m.missing {
                let offset = m
                    .offset
                    .parse::<u64>()
                    .map_err(|_| "開始位置が不正です。")?;
                let meta = std::fs::metadata(&m.path).map_err(|e| e.to_string())?;
                if !meta.is_file() {
                    return Err("先にディスクのイメージを取得してください。".into());
                }
                lengths.push(
                    meta.len()
                        .checked_sub(offset)
                        .ok_or("開始位置がイメージの外です。")?,
                );
            }
        }
        let smallest = *lengths
            .iter()
            .min()
            .ok_or("読めるメンバーを1つ以上指定してください。")?;
        let mut available = lengths.into_iter();
        for m in &mut self.members {
            let length = if m.missing {
                smallest
            } else {
                available.next().unwrap()
            };
            let length = if self.level == RaidLevel::Jbod {
                length / u64::from(self.sector) * u64::from(self.sector)
            } else {
                smallest / u64::from(self.stripe) * u64::from(self.stripe)
            };
            m.length = length.to_string();
        }
        self.pinned = None;
        Ok(())
    }
}
impl App {
    fn raid_start(&mut self, search: bool, save: bool) {
        let config = match self.raid.config() {
            Ok(c) => c,
            Err(e) => {
                self.raid.error = e;
                return;
            }
        };
        let destination = PathBuf::from(&self.raid.destination);
        let reorder = self.raid.reorder;
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = self.cancel.clone();
        self.raid.running = true;
        self.raid.dialog = false;
        self.stage = Stage::Scanning;
        self.status = "RAIDのサンプルと構成を検査しています…".into();
        self.progress = Progress::default();
        let (tx, rx) = mpsc::sync_channel(8);
        self.messages = Some(rx);
        thread::spawn(move || {
            let base = std::env::current_dir().unwrap_or_default();
            if search {
                let options = rescue_core::RaidSearchOptions {
                    reorder_members: reorder,
                    ..Default::default()
                };
                let result = rescue_core::search_raid(&config, &base, &options, &cancel, |p| {
                    let _ = tx.try_send(Message::Progress(p));
                });
                let _ = tx.send(match result {
                    Ok(r) => Message::RaidSearched(Box::new(r)),
                    Err(e) => Message::Error(e.to_string()),
                });
            } else {
                let result = if save {
                    rescue_core::create_raid_manifest(&config, &base, &destination, None, &cancel)
                } else {
                    rescue_core::inspect_raid(&config, &base, &cancel)
                };
                let _ = tx.send(match result {
                    Ok(r) => Message::RaidChecked(
                        Box::new(r),
                        if save { Some(destination) } else { None },
                    ),
                    Err(e) => Message::Error(e.to_string()),
                });
            }
        });
    }
    pub(super) fn raid_checked(&mut self, report: RaidInspection, path: Option<PathBuf>) {
        self.stage = Stage::Settings;
        self.raid.running = false;
        self.status = format!(
            "RAID検査: {} / サンプル欠損 {}",
            view::size_text(Some(report.evidence.logical_bytes)),
            view::size_text(Some(report.unavailable_sample_bytes))
        );
        self.raid.accept(report);
        if let Some(path) = path {
            self.source = path.to_string_lossy().into_owned();
            self.source_mode = SourceMode::Image;
            self.recovered_volume = None;
            self.invalidate_changed_source();
            self.raid.dialog = false;
        } else {
            self.raid.dialog = true;
        }
    }
    pub(super) fn raid_searched(&mut self, report: RaidSearchResult) {
        self.stage = Stage::Settings;
        self.raid.running = false;
        self.status = format!(
            "RAID候補: {} / {}構成を検査{}",
            report.candidates.len(),
            report.tested_configurations,
            if report.truncated {
                "（探索上限）"
            } else {
                ""
            }
        );
        self.raid.search = Some(report);
        self.raid.dialog = true;
    }
    pub(super) fn raid_dialog_ui(&mut self, ctx: &egui::Context) {
        let mut close = false;
        let mut inspect = false;
        let mut search = false;
        let mut save = false;
        let mut selected = None;
        let response = responsive_modal(ctx, "raid-configuration", 1000.0).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 1000.0));
            ui.heading("RAIDのメンバーイメージを構成");
            ui.label("順序・欠損位置・使用範囲を指定します。元メンバーは読み取り専用です。");
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 230.0).max(100.0))
                .show(ui, |ui| {
                    self.raid.geometry_ui(ui);
                    self.raid.members_ui(ui);
                    text_row(ui, "構成の保存先", &mut self.raid.destination, "/path/array.rescue-raid.json");
                    ui.checkbox(&mut self.raid.reorder, "自動候補探索でメンバー順も試す");
                    ui.small("候補探索: ストライプ4 KiB〜1 MiB、最大128構成・5秒。開始位置と使用長は指定値を維持します。候補は選択後に使用します。");
                    selected = self.raid.evidence_ui(ui);
                    ui.horizontal_wrapped(|ui| {
                        inspect = ui.button("指定した構成を検査").clicked();
                        search = ui.button("構成の候補を探す").clicked();
                    });
                    if !self.raid.error.is_empty() {
                        ui.colored_label(egui::Color32::YELLOW, &self.raid.error);
                    }
                });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                close = ui.add_sized([64.0, 36.0], egui::Button::new("閉じる")).clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    save = ui.add_enabled(
                        !self.raid.destination.trim().is_empty(),
                        egui::Button::new("構成を保存して使用").min_size(egui::vec2(190.0, 36.0)),
                    ).clicked();
                });
            });
        });
        #[cfg(test)]
        {
            self.raid.dialog_rect = Some(response.response.rect);
        }
        if let Some(index) = selected {
            let report = self.raid.search.as_ref().unwrap().candidates[index]
                .inspection
                .clone();
            self.raid.accept(report);
        }
        if close || response.should_close() {
            self.raid.dialog = false;
        } else if inspect || search || save {
            self.raid_start(search, save);
        }
    }
}

impl State {
    fn geometry_ui(&mut self, ui: &mut egui::Ui) {
        ui.horizontal_wrapped(|ui| {
            ui.label("方式");
            let old = self.level;
            egui::ComboBox::from_id_salt("raid-level")
                .selected_text(format!("{:?}", self.level))
                .show_ui(ui, |ui| {
                    for value in [
                        RaidLevel::Jbod,
                        RaidLevel::Raid0,
                        RaidLevel::Raid1,
                        RaidLevel::Raid5,
                        RaidLevel::Raid6,
                        RaidLevel::Raid10,
                    ] {
                        ui.selectable_value(&mut self.level, value, format!("{value:?}"));
                    }
                });
            if old != self.level {
                self.layout = match self.level {
                    RaidLevel::Raid5 => RaidLayout::LeftSymmetric,
                    RaidLevel::Raid6 => RaidLayout::PqLast,
                    _ => RaidLayout::Fixed,
                };
            }
            ui.label("ストライプ");
            egui::ComboBox::from_id_salt("raid-stripe")
                .selected_text(view::size_text(Some(u64::from(self.stripe))))
                .show_ui(ui, |ui| {
                    for i in 9..=20 {
                        ui.selectable_value(
                            &mut self.stripe,
                            1 << i,
                            view::size_text(Some(1 << i)),
                        );
                    }
                });
            ui.label("セクター");
            egui::ComboBox::from_id_salt("raid-sector")
                .selected_text(self.sector.to_string())
                .show_ui(ui, |ui| {
                    for value in [512, 4096] {
                        ui.selectable_value(&mut self.sector, value, value.to_string());
                    }
                });
        });
        if self.level == RaidLevel::Raid5 {
            ui.horizontal_wrapped(|ui| {
                ui.label("Pの配置");
                egui::ComboBox::from_id_salt("raid-layout")
                    .selected_text(format!("{:?}", self.layout))
                    .show_ui(ui, |ui| {
                        for value in [
                            RaidLayout::LeftSymmetric,
                            RaidLayout::RightSymmetric,
                            RaidLayout::LeftAsymmetric,
                            RaidLayout::RightAsymmetric,
                        ] {
                            ui.selectable_value(&mut self.layout, value, format!("{value:?}"));
                        }
                    });
            });
        }
        if self.level == RaidLevel::Raid6 {
            ui.label("P/Qは末尾2メンバー固定、GF 0x11d、係数2^i。対応配置を確認してください。");
        }
        if self.level == RaidLevel::Raid10 {
            ui.label("メンバー1・2、3・4…をミラーペアとして扱います。");
        }
    }
    fn members_ui(&mut self, ui: &mut egui::Ui) {
        let mut remove = None;
        let mut shift = None;
        for (index, member) in self.members.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.strong(format!("メンバー {}", index + 1));
                    ui.checkbox(&mut member.missing, "この位置は欠損");
                    if ui
                        .add_enabled(index > 0, egui::Button::new("上へ"))
                        .clicked()
                    {
                        shift = Some(index);
                    }
                    if ui.button("削除").clicked() {
                        remove = Some(index);
                    }
                });
                if !member.missing {
                    path_row(ui, "メンバーイメージ", &mut member.path, Picker::Open);
                }
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label("開始バイト").id;
                    ui.add(egui::TextEdit::singleline(&mut member.offset).desired_width(130.0))
                        .labelled_by(label);
                    let label = ui.label("使用長").id;
                    ui.add(egui::TextEdit::singleline(&mut member.length).desired_width(160.0))
                        .labelled_by(label);
                });
            });
        }
        if let Some(index) = remove {
            self.members.remove(index);
        } else if let Some(index) = shift {
            self.members.swap(index, index - 1);
        }
        ui.horizontal_wrapped(|ui| {
            if ui
                .add_enabled(self.members.len() < 16, egui::Button::new("メンバーを追加"))
                .clicked()
            {
                self.members.push(MemberInput {
                    offset: "0".into(),
                    ..Default::default()
                });
            }
            if ui.button("使用できる長さを揃える").clicked() {
                self.error = self.fill_lengths().err().unwrap_or_default();
            }
        });
        ui.small("長さを揃える操作は最小メンバーの境界までを使います。表示した範囲以外は読みません。取得マップがある場合は宣言された範囲も検査します。");
    }
    fn evidence_ui(&self, ui: &mut egui::Ui) -> Option<usize> {
        let mut selected = None;
        if let Some(result) = &self.search {
            ui.separator();
            ui.strong(format!("構造を照合できた候補 {}", result.candidates.len()));
            if result.truncated {
                ui.label("探索上限に達しました。すべての構成を検査した結果ではありません。");
            }
            for (index, candidate) in result.candidates.iter().enumerate() {
                let config = &candidate.inspection.evidence.config;
                ui.horizontal_wrapped(|ui| {
                    if ui.button(format!("候補 {} を選択", index + 1)).clicked() {
                        selected = Some(index);
                    }
                    ui.label(format!(
                        "{:?} / {:?} / {}",
                        config.level,
                        config.layout,
                        view::size_text(Some(u64::from(config.stripe_bytes)))
                    ));
                });
                ui.small(candidate.filesystem_evidence.join(" · "));
                let order = config
                    .members
                    .iter()
                    .map(|m| {
                        m.path
                            .as_ref()
                            .map(|p| {
                                p.file_name()
                                    .unwrap_or_default()
                                    .to_string_lossy()
                                    .into_owned()
                            })
                            .unwrap_or_else(|| "欠損".into())
                    })
                    .collect::<Vec<_>>()
                    .join(" → ");
                ui.small(format!("順序: {order}"));
            }
        }
        if let Some(report) = &self.inspection {
            ui.separator();
            ui.strong("選択した構成のサンプル検査");
            ui.label(format!(
                "論理容量 {} / サンプル {} / 不一致・欠損 {}",
                view::size_text(Some(report.evidence.logical_bytes)),
                view::size_text(Some(report.sampled_bytes)),
                view::size_text(Some(report.unavailable_sample_bytes))
            ));
            ui.label("サンプルやパリティの一致だけで、元の配列や削除前の内容は確定しません。スキャン結果も確認してください。");
        }
        selected
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn open_dialog_stays_inside_first_frame_after_resize() {
        for zoom in [1.0, 1.5, 2.0] {
            let ctx = egui::Context::default();
            ctx.set_zoom_factor(zoom);
            // Configure the native scale before opening the dialog. Each subsequent
            // viewport resize is checked on its first layout, with no settling pass.
            for _ in 0..3 {
                let _ = ctx.run(
                    egui::RawInput {
                        screen_rect: Some(egui::Rect::from_min_size(
                            egui::Pos2::ZERO,
                            egui::vec2(1260.0 * zoom, 820.0 * zoom),
                        )),
                        ..Default::default()
                    },
                    |_| {},
                );
            }
            let mut app = App::blank();
            app.raid.dialog = true;
            for size in [
                [1260.0, 820.0],
                [720.0, 520.0],
                [1008.0, 620.0],
                [720.0, 520.0],
            ] {
                for _ in 0..3 {
                    let _ = ctx.run(
                        egui::RawInput {
                            screen_rect: Some(egui::Rect::from_min_size(
                                egui::Pos2::ZERO,
                                egui::vec2(size[0] * zoom, size[1] * zoom),
                            )),
                            ..Default::default()
                        },
                        |ctx| app.render(ctx),
                    );
                    let rect = app.raid.dialog_rect.expect("dialog shown");
                    assert!(
                        ctx.content_rect().contains_rect(rect),
                        "{size:?} {zoom}: {rect:?} outside {:?}",
                        ctx.content_rect()
                    );
                }
            }
        }
    }
}
