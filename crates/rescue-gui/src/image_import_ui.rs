//! Convert local container files to a logical RAW source for the normal scan workflow.
use super::*;
use rescue_core::disk_image::{ImageInput, ImportedFile};

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Lvm,
    Vdi,
    Qcow,
    Qed,
    Parallels,
    CompressedRaw,
    Dmg,
    Optical,
    Android,
    Ddrescue,
    Ewf,
    Vhd,
    Vhdx,
    Vmdk,
    Aff,
    Raw,
}

impl Format {
    const ALL: [Self; 16] = [
        Self::Vdi,
        Self::Qcow,
        Self::CompressedRaw,
        Self::Dmg,
        Self::Optical,
        Self::Ewf,
        Self::Vhd,
        Self::Vhdx,
        Self::Vmdk,
        Self::Aff,
        Self::Raw,
        Self::Android,
        Self::Ddrescue,
        Self::Lvm,
        Self::Qed,
        Self::Parallels,
    ];

    fn from_path(path: &str) -> Option<Self> {
        if Path::new(path)
            .file_name()
            .is_some_and(|n| n.eq_ignore_ascii_case("DiskDescriptor.xml"))
        {
            return Some(Self::Parallels);
        }
        match Path::new(path)
            .extension()?
            .to_str()?
            .to_ascii_lowercase()
            .as_str()
        {
            "gz" | "bz2" | "xz" | "zst" | "zstd" | "lz4" => Some(Self::CompressedRaw),
            "bin" => Some(Self::Optical),
            "dmg" | "udif" => Some(Self::Dmg),
            "pv" | "lvm" => Some(Self::Lvm),
            "vdi" => Some(Self::Vdi),
            "qed" => Some(Self::Qed),
            "hds" => Some(Self::Parallels),
            "qcow2" | "qcow1" | "qcow" => Some(Self::Qcow),
            "simg" | "sparse" => Some(Self::Android),
            "e01" | "ex01" => Some(Self::Ewf),
            "vhd" => Some(Self::Vhd),
            "vhdx" => Some(Self::Vhdx),
            "vmdk" => Some(Self::Vmdk),
            "aff" => Some(Self::Aff),
            "raw" | "dd" | "img" => Some(Self::Raw),
            ext if (3..=9).contains(&ext.len()) && ext.bytes().all(|c| c.is_ascii_digit()) => {
                Some(Self::Raw)
            }
            _ => None,
        }
    }

    fn reader(self) -> &'static str {
        match self {
            Self::CompressedRaw => "compressed-raw",
            Self::Optical => "cd-raw",
            Self::Android => "android-sparse",
            Self::Ddrescue => "ddrescue",
            Self::Dmg => "dmg",
            Self::Lvm => "lvm2",
            Self::Vdi => "vdi",
            Self::Qed => "qed",
            Self::Parallels => "parallels",
            Self::Qcow => "qcow",
            Self::Ewf => "ewf-verified",
            Self::Vhd => "vhd",
            Self::Vhdx => "vhdx",
            Self::Vmdk => "vmdk",
            Self::Aff => "aff",
            Self::Raw => "raw",
        }
    }

    fn label(self) -> &'static str {
        match self {
            Self::CompressedRaw => "Compressed RAW (gzip, bzip2, XZ, Zstd, LZ4)",
            Self::Android => "Android sparse",
            Self::Ddrescue => "GNU ddrescue (RAW + map)",
            Self::Optical => "CD-ROM (Mode 1 / XA Form 1)",
            Self::Dmg => "DMG (UDIF)",
            Self::Lvm => "LVM2",
            Self::Vdi => "VDI",
            Self::Qed => "QED",
            Self::Parallels => "Parallels HDS / DiskDescriptor.xml",
            Self::Qcow => "QCOW / QCOW2",
            Self::Ewf => "E01 / Ex01 (libewf)",
            Self::Vhd => "VHD",
            Self::Vhdx => "VHDX",
            Self::Vmdk => "VMDK",
            Self::Aff => "AFF",
            Self::Raw => "RAW",
        }
    }

    fn split(self) -> bool {
        matches!(
            self,
            Self::Lvm
                | Self::Raw
                | Self::Ewf
                | Self::Vmdk
                | Self::Qed
                | Self::Parallels
                | Self::Qcow
                | Self::Vhd
                | Self::Vhdx
                | Self::Vdi
        )
    }
}

struct ImageChoice {
    origin: Option<String>,
    id: String,
    name: String,
    bytes: u64,
}

impl ImageChoice {
    fn label(&self, lvm: bool) -> String {
        if let Some(origin) = &self.origin {
            tr_format!(
                "{}（{} のスナップショット） · {}",
                "{} (snapshot of {}) · {}",
                self.name,
                origin,
                self.id
            )
        } else if lvm {
            format!("{} · {}", self.name, self.id)
        } else {
            format!("{} · {}", self.id, self.name)
        }
    }
}

struct SelectionLookup {
    cancel: Arc<AtomicBool>,
    result: mpsc::Receiver<rescue_core::Result<Vec<ImageChoice>>>,
}

impl Drop for SelectionLookup {
    fn drop(&mut self) {
        self.cancel.store(true, Ordering::Relaxed);
    }
}

#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    pub first: String,
    format: Option<Format>,
    segments: String,
    ddrescue_map: String,
    raw_sha256: String,
    raw_bytes: String,
    selected_id: String,
    choices: Option<Vec<ImageChoice>>,
    lookup: Option<SelectionLookup>,
    pub output: String,
    worker: Option<thread::JoinHandle<()>>,
    error: Option<Text>,
}

impl State {
    pub fn show(&mut self, source: &str) {
        if self.first != source {
            self.first = source.to_owned();
            self.format = Format::from_path(source);
            self.segments.clear();
            self.reset_selection();
            self.ddrescue_map.clear();
            self.raw_sha256.clear();
            self.raw_bytes.clear();
            self.output.clear();
        }
        self.error = None;
        self.open = true;
    }

    fn reset_selection(&mut self) {
        self.lookup = None;
        self.choices = None;
        self.selected_id.clear();
    }

    fn source_picker(&mut self, ui: &mut egui::Ui, format: Format) {
        let lvm = format == Format::Lvm;
        if let Some(lookup) = &self.lookup {
            match lookup.result.try_recv() {
                Ok(result) => {
                    self.lookup = None;
                    match result {
                        Ok(rows) => {
                            if !rows.iter().any(|row| row.id == self.selected_id) {
                                self.selected_id.clear();
                            }
                            self.choices = Some(rows);
                        }
                        Err(err) => {
                            self.error = Some(message!(
                                "一覧を取得できませんでした。{err}",
                                "Could not list entries. {err}"
                            ))
                        }
                    }
                }
                Err(mpsc::TryRecvError::Disconnected) => {
                    self.lookup = None;
                    self.error = Some(message!(
                        "読み取りが中断しました。",
                        "Listing was interrupted."
                    ));
                }
                Err(mpsc::TryRecvError::Empty) => {}
            }
        }
        if let Some(rows) = &self.choices {
            let active = if lvm {
                tr!("ボリュームを選択", "Choose a volume")
            } else {
                tr!("現在のディスク", "Active disk")
            };
            let label = ui
                .label(if lvm {
                    tr!("論理ボリューム", "Logical volume")
                } else {
                    tr!("スナップショット", "Snapshot")
                })
                .id;
            egui::ComboBox::from_id_salt("qcow2-snapshot")
                .selected_text(
                    rows.iter()
                        .find(|s| s.id == self.selected_id)
                        .map_or_else(|| active.to_owned(), |s| s.label(lvm)),
                )
                .show_ui(ui, |ui| {
                    if !lvm {
                        ui.selectable_value(&mut self.selected_id, String::new(), active);
                    }
                    for row in rows {
                        ui.selectable_value(
                            &mut self.selected_id,
                            row.id.clone(),
                            format!("{} ({} B)", row.label(lvm), row.bytes),
                        );
                    }
                })
                .response
                .labelled_by(label);
        } else {
            text_row(
                ui,
                if lvm {
                    tr!("論理ボリューム名またはUUID", "Logical volume name or UUID")
                } else {
                    tr!(
                        "内部スナップショットID（空欄で現在の状態）",
                        "Internal snapshot ID (leave empty for the active disk)"
                    )
                },
                &mut self.selected_id,
                "",
            );
            if ui
                .add_enabled(
                    self.lookup.is_none() && !self.first.is_empty(),
                    egui::Button::new(if lvm {
                        tr!("論理ボリューム一覧", "List volumes")
                    } else {
                        tr!("スナップショット一覧", "List snapshots")
                    }),
                )
                .clicked()
            {
                let request = self.request(format);
                let cancel = Arc::new(AtomicBool::new(false));
                let worker_cancel = Arc::clone(&cancel);
                let (send, result) = mpsc::channel();
                let context = ui.ctx().clone();
                self.error = None;
                self.lookup = Some(SelectionLookup { cancel, result });
                thread::spawn(move || {
                    let rows = if lvm {
                        rescue_core::disk_image::list_lvm_volumes(&request, &worker_cancel).map(
                            |entries| {
                                entries
                                    .into_iter()
                                    .map(|v| ImageChoice {
                                        origin: v.snapshot.map(|s| s.origin),
                                        id: v.id,
                                        name: format!("{}/{}", v.group, v.name),
                                        bytes: v.bytes,
                                    })
                                    .collect()
                            },
                        )
                    } else {
                        rescue_core::disk_image::list_qcow2_snapshots(
                            &request.source,
                            &worker_cancel,
                        )
                        .map(|entries| {
                            entries
                                .into_iter()
                                .map(|v| ImageChoice {
                                    origin: None,
                                    id: v.id,
                                    name: v.name,
                                    bytes: v.guest_bytes,
                                })
                                .collect()
                        })
                    };
                    let _ = send.send(rows);
                    context.request_repaint();
                });
            }
        }
    }

    fn request(&self, format: Format) -> ImageInput {
        let segments = if format.split() {
            self.segments
                .lines()
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect()
        } else {
            Vec::new()
        };
        ImageInput {
            source: PathBuf::from(&self.first),
            image_type: Some(format.reader().into()),
            segment: segments,
            sector_size: None,
        }
    }

    pub fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

impl App {
    pub(super) fn import_image_dialog(&mut self, ctx: &egui::Context) {
        let mut start = false;
        let mut close = false;
        let state = &mut self.image_import;
        let modal = responsive_modal(ctx, "import-saved-image", 620.0).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 620.0));
            ui.heading(tr!("イメージをRAWに変換", "Convert an image to RAW"));
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height()-177.0).max(120.0))
                .show_accessible(ui, tr!("変換設定","Conversion settings"), |ui| {
                let old = state.first.clone();
                path_row(ui, tr!("元のイメージ（分割時は先頭）", "Source image (first segment if split)"), &mut state.first, Picker::Open);
                if state.first != old {
                    state.format = Format::from_path(&state.first);
                    state.segments.clear();
                    state.reset_selection();
                    state.ddrescue_map.clear();
                    state.raw_sha256.clear();
                    state.raw_bytes.clear();
                }
                let before_format = state.format;
                let label = ui.label(tr!("イメージ形式", "Image format")).id;
                egui::ComboBox::from_id_salt("disk-image-format")
                    .selected_text(state.format.map(Format::label).unwrap_or(tr!("形式を選択","Choose a format")))
                    .show_ui(ui, |ui| {
                        for format in Format::ALL { ui.selectable_value(&mut state.format, Some(format), format.label()); }
                    }).response.labelled_by(label);
                if state.format != before_format {state.reset_selection();}
                if let Some(format) = state.format {
                    match format {
                        Format::Raw => {ui.small(tr!("連番は.000または.001から自動検出します。他の名前は追加セグメントを順番に指定してください。","Numbered RAW sets starting at .000 or .001 are discovered automatically. For other names, add the segments in order."));}
                        Format::Ddrescue => {
                            path_row(ui, tr!("GNU ddrescueのマップ", "GNU ddrescue mapfile"), &mut state.ddrescue_map, Picker::Open);
                            ui.small(tr!("元ディスクと同じ位置に取得したRAW用です。読み取り失敗と未試行の領域は復元対象から除きます。位置を変更した取得にはCLIを使用してください。", "For captures using original disk offsets. Failed and untried regions are excluded from recovery. Use the CLI for shifted captures."));
                        }
                        Format::Android => {ui.small(tr!("新しい保存先フォルダー名を指定してください。RAWと読取マップを一緒に保存し、省略されたブロックを未取得として扱います。","Enter a new directory name. The RAW and read map are saved together, with omitted blocks marked unavailable."));}
                        Format::CompressedRaw => {ui.small(tr!("圧縮ストリームを順に展開し、論理ディスクのSHA-256を計算します。","Expands compressed streams in order and calculates the logical disk SHA-256."));}
                        Format::Optical => {ui.small(tr!("各セクターのEDCとP/Qパリティを照合し、データ部分を取り出します。","Checks each sector’s EDC and P/Q parity, then extracts its 2048 data bytes."));}
                        Format::Lvm => {ui.small(tr!("グループに含まれるすべての物理ボリュームのイメージを指定してください。","Supply every physical-volume image in the group."));}
                        Format::Dmg => {ui.small(tr!("単独UDIFのブロック表と記録されたCRC-32を照合します。", "Checks the block maps and recorded CRC-32 of a standalone UDIF image."));}
                        Format::Ewf => { ui.small(tr!("libewfのewfinfo・ewfverify・ewfexportで取得エラーと論理SHA-256を確認します。", "Uses ewfinfo, ewfverify and ewfexport from libewf to check acquisition errors and the logical SHA-256.")); }
                        Format::Vmdk => {ui.small(tr!("差分VMDKでは直近の親からベースまで順に指定します。単独の疎・圧縮VMDKや、記述ファイルとフラット・分割エクステントも選べます。", "For a sparse snapshot, select parents nearest first through the base. You can also import a standalone sparse or compressed VMDK, or a descriptor with its flat or split extents."));}
                        Format::Parallels => {ui.small(tr!("DiskDescriptor.xmlと、記述されているすべてのイメージを選んでください。単体のスパースHDSには元の論理RAWのSHA-256が必要です。","Select DiskDescriptor.xml and every image it names. A lone sparse HDS requires the original logical RAW SHA-256."));}
                        Format::Qed => {ui.small(tr!("差分QEDは直近の親から順にQEDまたはRAWを指定してください。","For a QED overlay, select its QED or RAW backing files in order from nearest parent to base."));}
                        Format::Qcow => {ui.small(tr!("単独QCOW v1、QCOW2 v2/v3に対応します。差分QCOW2は直近の親から順にRAWまたはQCOW2を指定してください。","For a QCOW2 overlay, select its backing files in order from nearest parent to base (RAW or QCOW2). Standalone QCOW v1 and QCOW2 v2/v3 are also supported."));}
                        Format::Vhdx => {ui.small(tr!("差分VHDXでは、選択した子から近い順に親VHDXを指定します。", "For a differencing VHDX, list its parent files nearest first."));}
                        Format::Vhd => {ui.small(tr!("差分VHDでは、選択した子から近い順に親VHDを指定します。", "For a differencing VHD, select every parent VHD from nearest parent to base."));}
                        Format::Vdi => {ui.small(tr!("差分VDIでは、選択した子から固定・可変のベースまで、親VDIを近い順に指定します。", "For a differencing VDI, select its parents nearest first, ending with the fixed or dynamic base."));}
                        _ => {ui.small(tr!("The Sleuth Kitの対応ドライバーとimg_catを使用します。", "Uses img_cat and the appropriate driver from The Sleuth Kit."));}
                    }
                    if format == Format::Qcow {
                        state.source_picker(ui, format);
                    }
                    if format.split() {
                        let mut changed = false;
                        let caption = if format == Format::Parallels {tr!("記述ファイルにあるイメージ（1行1パス、順不同）","Images named by the descriptor (one path per line, any order)")} else if format == Format::Lvm {tr!("追加の物理ボリューム（1行1パス、順不同）","Additional physical volumes (one path per line, any order)")} else if matches!(format, Format::Vdi | Format::Qed | Format::Qcow | Format::Vhd | Format::Vhdx) {
                            tr!("親ディスク（直近の親から順に1行1パス）","Backing files (nearest parent first, one path per line)")
                        } else if format == Format::Vmdk {
                            tr!("差分の親（直近から順に）、または記述ファイルにある構成ファイル（1行1パス）", "Parent images nearest first, or extents named in the descriptor (one path per line)")
                        } else {
                            tr!("追加セグメント（順番どおり、1行1パス）", "Additional segments in order (one path per line)")
                        };
                        let label = ui.label(caption).id;
                        changed |= ui.add(egui::TextEdit::multiline(&mut state.segments).desired_rows(2).desired_width(f32::INFINITY)).labelled_by(label).changed();
                        if ui.button(tr!("セグメントを追加", "Append segment")).clicked()
                            && let Some(p) = rfd::FileDialog::new().pick_file() {
                            if !state.segments.is_empty() && !state.segments.ends_with('\n') {state.segments.push('\n');}
                            state.segments.push_str(&p.to_string_lossy());
                            changed = true;
                        }
                        if format == Format::Lvm {if changed {state.reset_selection();} state.source_picker(ui,format);}
                    }
                }
                if matches!(state.format,Some(Format::Lvm | Format::Raw | Format::Vdi | Format::Parallels | Format::Qed | Format::Qcow | Format::Vhd | Format::Vhdx | Format::Vmdk)) {
                    ui.collapsing(
                        tr!("元ディスクと照合", "Verify against the original disk"),
                        |ui| {
                            text_row(
                                ui,
                                tr!(
                                    "論理ディスクのSHA-256（任意）",
                                    "Logical disk SHA-256 (optional)"
                                ),
                                &mut state.raw_sha256,
                                "",
                            );
                            text_row(
                                ui,
                                tr!(
                                    "論理ディスクのバイト数（任意）",
                                    "Logical disk length in bytes (optional)"
                                ),
                                &mut state.raw_bytes,
                                "",
                            );
                        },
                    );
                }
                if matches!(state.format, Some(Format::Android | Format::Ddrescue)) {
                    path_row(ui, tr!("新しい保存先フォルダー", "New destination directory"), &mut state.output, Picker::File);
                } else {
                    path_row(ui, tr!("新しいRAWファイル", "New RAW file"), &mut state.output, Picker::Raw);
                }
            });
            if let Some(error) = &state.error { theme::error_banner(ui, &error.to_string()); }
            ui.horizontal_wrapped(|ui| {
                start = ui.add_enabled(state.format.is_some() && (state.format != Some(Format::Lvm) || !state.selected_id.trim().is_empty()) && (state.format != Some(Format::Ddrescue) || !state.ddrescue_map.trim().is_empty()) && !state.first.trim().is_empty() && !state.output.trim().is_empty(),
                    theme::primary(ui, tr!("変換してスキャン元に設定", "Convert and use as source"), 224.0)).clicked();
                close = ui.button(tr!("閉じる", "Close")).clicked();
            });
        });
        if modal.should_close() || close || start {
            state.lookup = None;
            state.open = false;
        }
        if start {
            self.start_image_import();
        }
    }

    fn start_image_import(&mut self) {
        let state = &mut self.image_import;
        let Some(format) = state.format else {
            return;
        };
        state.error = None;
        let request = state.request(format);
        let ddrescue_map = PathBuf::from(&state.ddrescue_map);
        let selected_id = state.selected_id.clone();
        let expected_hash = state.raw_sha256.trim().to_owned();
        let expected_size = state.raw_bytes.trim().to_owned();
        let output = PathBuf::from(&state.output);
        let (tx, rx) = mpsc::channel();
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancel);
        self.messages = Some(rx);
        self.stage = Stage::Importing;
        self.status_error = false;
        self.show_validation = false;
        self.status = message!(
            "イメージの論理データを読み取っています。",
            "Reading the logical disk image."
        );
        state.worker = Some(thread::spawn(move || {
            let result = (|| {
                for path in std::iter::once(&request.source).chain(&request.segment) {
                    let canonical = rescue_core::canonical_source_path(path)?;
                    if rescue_core::is_device(&canonical) || !canonical.is_file() {
                        return Err(rescue_core::Error::Request(
                            "choose a regular disk image file".into(),
                        ));
                    }
                }
                if format == Format::Ddrescue {
                    rescue_core::disk_image::import_ddrescue(
                        &request.source,
                        &ddrescue_map,
                        &output,
                        Default::default(),
                        &cancel,
                    )
                } else if format == Format::Android {
                    rescue_core::disk_image::import_android(&request.source, &output, &cancel)
                } else if matches!(
                    format,
                    Format::Lvm
                        | Format::Raw
                        | Format::Vdi
                        | Format::Qed
                        | Format::Parallels
                        | Format::Qcow
                        | Format::Vhd
                        | Format::Vhdx
                        | Format::Vmdk
                ) {
                    let bytes = if expected_size.is_empty() {
                        None
                    } else {
                        Some(expected_size.parse().map_err(|_| {
                            rescue_core::Error::Request(
                                tr!(
                                    "ディスクのバイト数は整数で指定してください。",
                                    "Enter the disk length as an unsigned integer."
                                )
                                .into(),
                            )
                        })?)
                    };
                    let expected = rescue_core::disk_image::RawExpectation {
                        bytes,
                        sha256: (!expected_hash.is_empty()).then_some(expected_hash.as_str()),
                    };
                    if format == Format::Lvm {
                        return rescue_core::disk_image::import_lvm_volume(
                            &request,
                            selected_id.trim(),
                            &output,
                            expected,
                            &cancel,
                        );
                    }
                    let selector = (format == Format::Qcow && !selected_id.is_empty()).then_some(
                        rescue_core::disk_image::Qcow2SnapshotSelector::Id(&selected_id),
                    );
                    rescue_core::disk_image::import_with_snapshot(
                        &request, &output, expected, selector, &cancel,
                    )
                } else {
                    rescue_core::disk_image::import(&request, &output, &cancel)
                }
            })();
            let _ = tx.send(Message::ImageImported(result));
        }));
    }

    pub(super) fn finish_image_import(&mut self, result: rescue_core::Result<ImportedFile>) {
        self.image_import.join();
        self.stage = Stage::Settings;
        match result {
            Ok(import) => {
                self.source = import.path.to_string_lossy().into_owned();
                self.source_mode = SourceMode::Image;
                self.recovered_volume = None;
                self.discovered_id = None;
                self.discovery = None;
                self.image_original = None;
                self.invalidate_changed_source();
                self.status_error = false;
                if let Some(proof) = import.ddrescue {
                    self.status = message!(
                        "ddrescueの読取マップ付きRAWを設定しました。取得 {} バイト、失敗 {} バイト、未試行・範囲外 {} バイト。",
                        "Selected ddrescue mapped RAW. {} copied, {} failed and {} untried or outside-map bytes.",
                        proof.finished_bytes,
                        proof.nontrimmed_bytes + proof.nonscraped_bytes + proof.bad_sector_bytes,
                        proof.untried_bytes + proof.outside_map_bytes
                    );
                    return;
                }
                if let Some(proof) = import.android_sparse {
                    self.status = message!(
                        "読取マップ付きRAWを設定しました。記録済み {} バイト、未取得 {} バイト。",
                        "Mapped RAW selected. {} recorded bytes, {} unavailable bytes.",
                        proof.recorded_bytes,
                        proof.unrecorded_bytes
                    );
                    return;
                }
                if let Some(proof) = import.raw {
                    let verification = if proof.expected_sha256_verified {
                        tr!("指定のSHA-256と一致", "original SHA-256 matched")
                    } else {
                        tr!("SHA-256を計算", "SHA-256 calculated")
                    };
                    self.status = message!(
                        "{} セグメントから {} バイトのRAWを設定しました。{} {}",
                        "Selected RAW from {} segments ({} bytes). {} {}",
                        proof.segments.len(),
                        import.bytes,
                        verification,
                        import.sha256
                    );
                    return;
                }
                self.status = message!(
                    "RAWをスキャン元に設定しました。{} バイト、SHA-256 {}",
                    "RAW selected for scanning. {} bytes, SHA-256 {}",
                    import.bytes,
                    import.sha256
                );
            }
            Err(rescue_core::Error::Cancelled) => {
                self.status = message!("変換を取り消しました。", "Conversion cancelled.");
            }
            Err(err) => {
                self.image_import.open = true;
                self.status_error = true;
                self.status = message!("変換できませんでした。{err}", "Conversion failed. {err}");
                self.image_import.error = Some(self.status.clone());
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{
        fs,
        time::{Duration, Instant},
    };

    #[test]
    fn image_import_keeps_the_input_on_error_and_selects_a_completed_raw() {
        let dir = tempfile::tempdir().unwrap();
        let source = dir.path().join("bad.vdi");
        fs::write(&source, b"incomplete VDI").unwrap();
        let output = dir.path().join("result.raw");
        let mut app = App::blank();
        app.source = source.to_string_lossy().into_owned();
        app.image_import.show(&app.source);
        app.image_import.output = output.to_string_lossy().into_owned();
        app.start_image_import();
        let deadline = Instant::now() + Duration::from_secs(10);
        while app.stage == Stage::Importing && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(10));
            app.receive();
        }
        assert!(app.stage == Stage::Settings && app.image_import.open && app.status_error);
        assert!(app.image_import.error.is_some());
        assert_eq!(Path::new(&app.source), source);
        assert!(!output.exists());
        assert_eq!(fs::read(&source).unwrap(), b"incomplete VDI");

        app.finish_image_import(Ok(ImportedFile {
            path: output.clone(),
            bytes: 512,
            sha256: "abcd".into(),
            ewf: None,
            android_sparse: None,
            ddrescue: None,
            raw: None,
        }));
        assert_eq!(Path::new(&app.source), output);
        assert!(app.source_mode == SourceMode::Image && !app.status_error);
        assert!(app.status.to_string().contains("abcd"));
    }
}
