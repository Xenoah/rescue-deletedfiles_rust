//! Convert local container files to a logical RAW source for the normal scan workflow.
use super::*;
use rescue_core::disk_image::{ImageInput, ImportedFile};

#[derive(Clone, Copy, PartialEq)]
enum Format {
    Vdi,
    Qcow2,
    CompressedRaw,
    Dmg,
    Ewf,
    Vhd,
    Vhdx,
    Vmdk,
    Aff,
    Raw,
}

impl Format {
    const ALL: [Self; 10] = [
        Self::Vdi,
        Self::Qcow2,
        Self::CompressedRaw,
        Self::Dmg,
        Self::Ewf,
        Self::Vhd,
        Self::Vhdx,
        Self::Vmdk,
        Self::Aff,
        Self::Raw,
    ];

    fn from_path(path: &str) -> Option<Self> {
        match Path::new(path)
            .extension()?
            .to_str()?
            .to_ascii_lowercase()
            .as_str()
        {
            "gz" | "bz2" | "xz" | "zst" | "zstd" | "lz4" => Some(Self::CompressedRaw),
            "dmg" | "udif" => Some(Self::Dmg),
            "vdi" => Some(Self::Vdi),
            "qcow2" | "qcow" => Some(Self::Qcow2),
            "e01" | "ex01" => Some(Self::Ewf),
            "vhd" => Some(Self::Vhd),
            "vhdx" => Some(Self::Vhdx),
            "vmdk" => Some(Self::Vmdk),
            "aff" => Some(Self::Aff),
            "raw" | "dd" | "img" | "001" => Some(Self::Raw),
            _ => None,
        }
    }

    fn reader(self) -> &'static str {
        match self {
            Self::CompressedRaw => "compressed-raw",
            Self::Dmg => "dmg",
            Self::Vdi => "vdi",
            Self::Qcow2 => "qcow2",
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
            Self::Dmg => "DMG (UDIF)",
            Self::Vdi => "VDI",
            Self::Qcow2 => "QCOW2",
            Self::Ewf => "E01 / Ex01 (libewf)",
            Self::Vhd => "VHD",
            Self::Vhdx => "VHDX",
            Self::Vmdk => "VMDK",
            Self::Aff => "AFF",
            Self::Raw => "RAW",
        }
    }

    fn split(self) -> bool {
        matches!(self, Self::Raw | Self::Ewf)
    }
}

#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    pub first: String,
    format: Option<Format>,
    segments: String,
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
            self.output.clear();
        }
        self.error = None;
        self.open = true;
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
                }
                let label = ui.label(tr!("イメージ形式", "Image format")).id;
                egui::ComboBox::from_id_salt("disk-image-format")
                    .selected_text(state.format.map(Format::label).unwrap_or(tr!("形式を選択","Choose a format")))
                    .show_ui(ui, |ui| {
                        for format in Format::ALL { ui.selectable_value(&mut state.format, Some(format), format.label()); }
                    }).response.labelled_by(label);
                if let Some(format) = state.format {
                    match format {
                        Format::CompressedRaw => {ui.small(tr!("圧縮ストリームを順に展開し、論理ディスクのSHA-256を計算します。","Expands compressed streams in order and calculates the logical disk SHA-256."));}
                        Format::Dmg => {ui.small(tr!("単独UDIFのブロック表と記録されたCRC-32を照合します。", "Checks the block maps and recorded CRC-32 of a standalone UDIF image."));}
                        Format::Ewf => { ui.small(tr!("libewfのewfinfo・ewfverify・ewfexportで取得エラーと論理SHA-256を確認します。", "Uses ewfinfo, ewfverify and ewfexport from libewf to check acquisition errors and the logical SHA-256.")); }
                        Format::Vdi | Format::Qcow2 => {ui.small(tr!("親ディスクを参照しない単独イメージに対応します。", "Supports standalone images without a backing disk."));}
                        _ => {ui.small(tr!("The Sleuth Kitの対応ドライバーとimg_catを使用します。", "Uses img_cat and the appropriate driver from The Sleuth Kit."));}
                    }
                    if format.split() {
                        let label = ui.label(tr!("追加セグメント（順番どおり、1行1パス）", "Additional segments in order (one path per line)")).id;
                        ui.add(egui::TextEdit::multiline(&mut state.segments).desired_rows(2).desired_width(f32::INFINITY)).labelled_by(label);
                        if ui.button(tr!("セグメントを追加", "Append segment")).clicked()
                            && let Some(p) = rfd::FileDialog::new().pick_file() {
                            if !state.segments.is_empty() && !state.segments.ends_with('\n') {state.segments.push('\n');}
                            state.segments.push_str(&p.to_string_lossy());
                        }
                    }
                }
                path_row(ui, tr!("新しいRAWファイル", "New RAW file"), &mut state.output, Picker::Raw);
            });
            if let Some(error) = &state.error { theme::error_banner(ui, &error.to_string()); }
            ui.horizontal_wrapped(|ui| {
                start = ui.add_enabled(state.format.is_some() && !state.first.trim().is_empty() && !state.output.trim().is_empty(),
                    theme::primary(ui, tr!("変換してスキャン元に設定", "Convert and use as source"), 224.0)).clicked();
                close = ui.button(tr!("閉じる", "Close")).clicked();
            });
        });
        if modal.should_close() || close || start {
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
        let segments = if format.split() {
            state
                .segments
                .lines()
                .filter(|p| !p.is_empty())
                .map(PathBuf::from)
                .collect()
        } else {
            Vec::new()
        };
        let request = ImageInput {
            source: PathBuf::from(&state.first),
            image_type: Some(format.reader().into()),
            segment: segments,
            sector_size: None,
        };
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
                rescue_core::disk_image::import(&request, &output, &cancel)
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
        }));
        assert_eq!(Path::new(&app.source), output);
        assert!(app.source_mode == SourceMode::Image && !app.status_error);
        assert!(app.status.to_string().contains("abcd"));
    }
}
