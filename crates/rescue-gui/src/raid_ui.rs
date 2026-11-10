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
    error: Text,
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
            error: Text::default(),
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
            Err(e) => self.error = e.into(),
        }
    }
    fn config(&self) -> Result<RaidConfig, Text> {
        let members = self
            .members
            .iter()
            .map(|member| {
                let path = if member.missing {
                    None
                } else {
                    if member.path.trim().is_empty() {
                        return Err(message!(
                            "各メンバーのイメージを指定してください。欠損はチェックで明示します。",
                            "Specify each member image. Mark missing members explicitly."
                        ));
                    }
                    Some(PathBuf::from(member.path.trim()))
                };
                Ok(RaidMember {
                    path,
                    offset: member.offset.parse().map_err(|_| {
                        message!(
                            "開始位置はバイト単位の整数です。",
                            "Start positions must be whole numbers of bytes."
                        )
                    })?,
                    length: member.length.parse().map_err(|_| {
                        message!(
                            "使用長を指定するか、長さを揃えてください。",
                            "Specify a usable length or align member lengths."
                        )
                    })?,
                    fingerprint: None,
                })
            })
            .collect::<Result<Vec<_>, Text>>()?;
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
    fn fill_lengths(&mut self) -> Result<(), Text> {
        let mut lengths = Vec::new();
        for m in &self.members {
            if !m.missing {
                let offset = m
                    .offset
                    .parse::<u64>()
                    .map_err(|_| message!("開始位置が不正です。", "Invalid start position."))?;
                let meta = std::fs::metadata(&m.path).map_err(|e| e.to_string())?;
                if !meta.is_file() {
                    return Err(message!(
                        "先にディスクのイメージを取得してください。",
                        "Acquire a disk image first."
                    ));
                }
                lengths.push(meta.len().checked_sub(offset).ok_or(message!(
                    "開始位置がイメージの外です。",
                    "Start position is outside the image."
                ))?);
            }
        }
        let smallest = *lengths.iter().min().ok_or(message!(
            "読めるメンバーを1つ以上指定してください。",
            "Specify at least one readable member."
        ))?;
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
        self.status_error = false;
        self.stage = Stage::Scanning;
        self.status = message!(
            "RAIDのサンプルと構成を検査しています…",
            "Checking RAID samples and configuration…"
        );
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
                    Err(e) => Message::Error(e.to_string().into()),
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
                    Err(e) => Message::Error(e.to_string().into()),
                });
            }
        });
    }
    pub(super) fn raid_checked(&mut self, report: RaidInspection, path: Option<PathBuf>) {
        self.stage = Stage::Settings;
        self.raid.running = false;
        self.status = message!(
            "RAID検査: {} / サンプル欠損 {}",
            "RAID check: {} / {} missing samples",
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
        self.status = message!(
            "RAID候補: {} / {}構成を検査{}",
            "RAID candidates: {} / {} configurations checked{}",
            report.candidates.len(),
            report.tested_configurations,
            if report.truncated {
                message!("（探索上限）", " (search limit)")
            } else {
                Text::default()
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
            ui.heading(tr!("RAIDのメンバーイメージを構成", "Configure RAID member images"));
            ui.label(tr!("順序・欠損位置・使用範囲を指定します。元メンバーは読み取り専用です。", "Specify member order, missing positions and usable ranges. Source members are read-only."));
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 230.0).max(100.0))
                .show_accessible(ui, tr!("RAID設定", "RAID settings"), |ui| {
                    self.raid.geometry_ui(ui);
                    self.raid.members_ui(ui);
                    text_row(ui, tr!("構成の保存先", "Configuration destination"), &mut self.raid.destination, "/path/array.rescue-raid.json");
                    ui.checkbox(&mut self.raid.reorder, tr!("自動候補探索でメンバー順も試す", "Also try member order in automatic discovery"));
                    ui.small(tr!("候補探索: ストライプ4 KiB〜1 MiB、最大128構成・5秒。開始位置と使用長は指定値を維持します。候補は選択後に使用します。", "Discovery tries stripes from 4 KiB to 1 MiB, up to 128 configurations and 5 seconds. Specified offsets and lengths are kept. Select a candidate before using it."));
                    selected = self.raid.evidence_ui(ui);
                    ui.horizontal_wrapped(|ui| {
                        inspect = ui.button(tr!("指定した構成を検査", "Check this configuration")).clicked();
                        search = ui.button(tr!("構成の候補を探す", "Find configuration candidates")).clicked();
                    });
                    if !self.raid.error.is_empty() {
                        ui.colored_label(crate::theme::palette(ui).warning, &self.raid.error);
                    }
                });
            ui.separator();
            ui.horizontal_wrapped(|ui| {
                close = ui.add_sized([64.0, 36.0], egui::Button::new(tr!("閉じる", "Close"))).clicked();
                ui.with_layout(egui::Layout::right_to_left(egui::Align::Center), |ui| {
                    save = ui.add_enabled(
                        !self.raid.destination.trim().is_empty(),
                        egui::Button::new(tr!("構成を保存して使用", "Save and use configuration")).min_size(egui::vec2(190.0, 36.0)),
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
            ui.label(tr!("方式", "Level"));
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
            ui.label(tr!("ストライプ", "Stripe"));
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
            ui.label(tr!("セクター", "Sector"));
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
                ui.label(tr!("Pの配置", "P layout"));
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
            ui.label(tr!("P/Qは末尾2メンバー固定、GF 0x11d、係数2^i。対応配置を確認してください。", "P/Q occupy the last two members, with GF 0x11d and coefficients 2^i. Check that this matches your layout."));
        }
        if self.level == RaidLevel::Raid10 {
            ui.label(tr!(
                "メンバー1・2、3・4…をミラーペアとして扱います。",
                "Members 1–2, 3–4, and so on are treated as mirror pairs."
            ));
        }
    }
    fn members_ui(&mut self, ui: &mut egui::Ui) {
        let mut remove = None;
        let mut shift = None;
        for (index, member) in self.members.iter_mut().enumerate() {
            ui.push_id(index, |ui| {
                ui.separator();
                ui.horizontal_wrapped(|ui| {
                    ui.strong(tr_format!("メンバー {}", "Member {}", index + 1));
                    ui.checkbox(
                        &mut member.missing,
                        tr!("この位置は欠損", "This position is missing"),
                    );
                    if ui
                        .add_enabled(index > 0, egui::Button::new(tr!("上へ", "Move up")))
                        .clicked()
                    {
                        shift = Some(index);
                    }
                    if ui.button(tr!("削除", "Remove")).clicked() {
                        remove = Some(index);
                    }
                });
                if !member.missing {
                    path_row(
                        ui,
                        tr!("メンバーイメージ", "Member image"),
                        &mut member.path,
                        Picker::Open,
                    );
                }
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tr!("開始バイト", "Start byte")).id;
                    text_input::singleline(
                        ui,
                        &mut member.offset,
                        ui.make_persistent_id("offset"),
                        label,
                        130.0,
                        "",
                    );
                    let label = ui.label(tr!("使用長", "Usable length")).id;
                    text_input::singleline(
                        ui,
                        &mut member.length,
                        ui.make_persistent_id("length"),
                        label,
                        160.0,
                        "",
                    );
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
                .add_enabled(
                    self.members.len() < 16,
                    egui::Button::new(tr!("メンバーを追加", "Add member")),
                )
                .clicked()
            {
                self.members.push(MemberInput {
                    offset: "0".into(),
                    ..Default::default()
                });
            }
            if ui
                .button(tr!("使用できる長さを揃える", "Align usable lengths"))
                .clicked()
            {
                self.error = self.fill_lengths().err().unwrap_or_default();
            }
        });
        ui.small(tr!("長さを揃える操作は最小メンバーの境界までを使います。表示した範囲以外は読みません。取得マップがある場合は宣言された範囲も検査します。", "Alignment uses the smallest member boundary. Only displayed ranges are read. Declared ranges in any acquisition maps are also checked."));
    }
    fn evidence_ui(&self, ui: &mut egui::Ui) -> Option<usize> {
        let mut selected = None;
        if let Some(result) = &self.search {
            ui.separator();
            ui.strong(tr_format!(
                "構造を照合できた候補 {}",
                "{} candidates with structural evidence",
                result.candidates.len()
            ));
            if result.truncated {
                ui.label(tr!(
                    "探索上限に達しました。すべての構成を検査した結果ではありません。",
                    "Search limit reached. Not every configuration was checked."
                ));
            }
            for (index, candidate) in result.candidates.iter().enumerate() {
                let config = &candidate.inspection.evidence.config;
                ui.horizontal_wrapped(|ui| {
                    if ui
                        .button(tr_format!(
                            "候補 {} を選択",
                            "Select candidate {}",
                            index + 1
                        ))
                        .clicked()
                    {
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
                            .unwrap_or_else(|| tr!("欠損", "Missing").into())
                    })
                    .collect::<Vec<_>>()
                    .join(" → ");
                ui.small(tr_format!("順序: {order}", "Order: {order}"));
            }
        }
        if let Some(report) = &self.inspection {
            ui.separator();
            ui.strong(tr!(
                "選択した構成のサンプル検査",
                "Sample check of the selected configuration"
            ));
            ui.label(tr_format!(
                "論理容量 {} / サンプル {} / 不一致・欠損 {}",
                "Logical capacity {} / {} samples / {} mismatched or missing",
                view::size_text(Some(report.evidence.logical_bytes)),
                view::size_text(Some(report.sampled_bytes)),
                view::size_text(Some(report.unavailable_sample_bytes))
            ));
            ui.label(tr!("サンプルやパリティの一致だけで、元の配列や削除前の内容は確定しません。スキャン結果も確認してください。", "Matching samples or parity alone does not establish the original array layout or content. Check the scan results as well."));
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
