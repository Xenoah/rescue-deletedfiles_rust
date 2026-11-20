//! Restore known file layouts or use position-independent chunk fingerprints.
use super::*;
use rescue_core::disk_image::{self, FingerprintedFile, ImportedFile};

#[derive(Clone, Copy, Default, PartialEq, Eq)]
enum Mode {
    #[default]
    Layout,
    FindChunks,
    WholeHash,
    CreateManifest,
}
pub(super) enum Outcome {
    File(ImportedFile),
    Manifest(FingerprintedFile),
    WholeHash(disk_image::HashMatch),
}
#[derive(Clone, Default)]
struct HashFields {
    digest: String,
    length: String,
    first: String,
    block_size: String,
    start: String,
    end: String,
    work: String,
    regular: bool,
    detect_spacing: bool,
    run_blocks: String,
    step: String,
    maximum_step: String,
}

fn number<T: std::str::FromStr>(s: &str, name: &str) -> rescue_core::Result<Option<T>> {
    if s.trim().is_empty() {
        Ok(None)
    } else {
        s.trim().parse().map(Some).map_err(|_| {
            rescue_core::Error::Request(format!(
                "{name}: {}",
                tr!("整数を入力してください", "enter an integer")
            ))
        })
    }
}

impl HashFields {
    fn show(&mut self, ui: &mut egui::Ui) {
        text_row(
            ui,
            tr!("原本のSHA-256", "Original SHA-256"),
            &mut self.digest,
            tr!("16進数64桁", "64 hexadecimal digits"),
        );
        ui.columns(2, |c| {
            text_row(
                &mut c[0],
                tr!("原本のバイト数", "Original length in bytes"),
                &mut self.length,
                "",
            );
            text_row(
                &mut c[1],
                tr!("既知の先頭位置（任意）", "Known first offset (optional)"),
                &mut self.first,
                "",
            );
        });
        ui.checkbox(
            &mut self.regular,
            tr!("等間隔に分かれた断片を検索", "Search regularly spaced runs"),
        );
        if self.regular {
            ui.checkbox(
                &mut self.detect_spacing,
                tr!(
                    "先頭位置から前後の間隔を探索",
                    "Detect spacing from the known first offset"
                ),
            );
            ui.columns(2, |c| {
                text_row(
                    &mut c[0],
                    tr!("各断片のブロック数", "Blocks per run"),
                    &mut self.run_blocks,
                    "1",
                );
                if self.detect_spacing {
                    text_row(
                        &mut c[1],
                        tr!("探索する最大間隔（ブロック）", "Maximum spacing in blocks"),
                        &mut self.maximum_step,
                        "8",
                    );
                } else {
                    text_row(
                        &mut c[1],
                        tr!(
                            "先頭間の間隔（ブロック、逆順は負数）",
                            "Start-to-start spacing (negative for reverse)"
                        ),
                        &mut self.step,
                        "2",
                    );
                }
            });
        }
        ui.collapsing(
            tr!("検索範囲と上限", "Search range and limits"),
            |ui| {
                ui.columns(2, |c| {
                    text_row(
                        &mut c[0],
                        tr!("開始バイト", "Start byte"),
                        &mut self.start,
                        "0",
                    );
                    text_row(
                        &mut c[1],
                        tr!("終了バイト（含まない）", "Exclusive end byte"),
                        &mut self.end,
                        tr!("ディスクの末尾", "End of source"),
                    );
                });
                ui.columns(2, |c| {
                    text_row(
                        &mut c[0],
                        tr!("ブロックのバイト数", "Block size in bytes"),
                        &mut self.block_size,
                        "512",
                    );
                    text_row(
                        &mut c[1],
                        tr!("検索の上限バイト数", "Search byte budget"),
                        &mut self.work,
                        "536870912",
                    );
                });
            },
        );
    }

    fn request(&self) -> rescue_core::Result<disk_image::HashSearch<'_>> {
        Ok(disk_image::HashSearch {
            sha256: self.digest.trim(),
            file_bytes: number(&self.length, tr!("原本のバイト数", "Original length"))?
                .unwrap_or(0),
            first_offset: number(&self.first, tr!("先頭位置", "First offset"))?,
            start: number(&self.start, tr!("開始バイト", "Start byte"))?.unwrap_or(0),
            end: number(&self.end, tr!("終了バイト", "End byte"))?,
            block_size: number(&self.block_size, tr!("ブロックのバイト数", "Block size"))?
                .unwrap_or(512),
            max_work: number(&self.work, tr!("検索の上限バイト数", "Search byte budget"))?
                .unwrap_or(disk_image::DEFAULT_HASH_WORK),
        })
    }
    fn recover(
        &self,
        source: &Path,
        output: &Path,
        cancel: &AtomicBool,
    ) -> rescue_core::Result<disk_image::HashMatch> {
        let request = self.request()?;
        if !self.regular {
            return disk_image::recover_by_hash(source, output, &request, cancel);
        }
        let spacing = if self.detect_spacing {
            disk_image::StrideSpacing::UpTo(
                number(&self.maximum_step, tr!("最大間隔", "Maximum spacing"))?.unwrap_or(8),
            )
        } else {
            disk_image::StrideSpacing::Fixed(
                number(&self.step, tr!("間隔", "Spacing"))?.unwrap_or(2),
            )
        };
        let stride = disk_image::StrideSearch {
            run_blocks: number(
                &self.run_blocks,
                tr!("各断片のブロック数", "Blocks per run"),
            )?
            .unwrap_or(1),
            spacing,
        };
        disk_image::recover_by_stride(source, output, &request, stride, cancel)
    }
}

#[derive(Default)]
pub(super) struct State {
    pub open: bool,
    mode: Mode,
    layout: String,
    manifest: String,
    original: String,
    chunk_size: Option<u32>,
    sector_aligned: bool,
    hash: HashFields,
    output: String,
    error: Option<Text>,
    worker: Option<thread::JoinHandle<()>>,
}
impl State {
    pub fn show(&mut self) {
        self.mode = Mode::Layout;
        self.error = None;
        self.open = true;
    }
    pub fn show_whole_hash(&mut self) {
        self.mode = Mode::WholeHash;
        self.error = None;
        self.open = true;
    }
    pub fn show_fingerprints(&mut self, has_source: bool) {
        self.mode = if has_source {
            Mode::FindChunks
        } else {
            Mode::CreateManifest
        };
        self.error = None;
        self.open = true;
    }
    pub fn join(&mut self) {
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
    pub fn working_title(&self) -> &'static str {
        match self.mode {
            Mode::WholeHash => tr!("SHA-256を照合中", "Searching for the original SHA-256"),
            Mode::Layout => tr!("配置表からファイルを復元中", "Recovering from the layout"),
            Mode::FindChunks => tr!(
                "一致する断片をディスク上で検索中",
                "Finding matching chunks on the disk"
            ),
            Mode::CreateManifest => {
                tr!("原本のハッシュを計算中", "Fingerprinting the original file")
            }
        }
    }
    pub fn working_output(&self) -> &str {
        if self.mode == Mode::CreateManifest {
            &self.manifest
        } else {
            &self.output
        }
    }
    fn ready(&self, source: &str) -> bool {
        match self.mode {
            Mode::WholeHash => {
                !source.trim().is_empty()
                    && !self.output.trim().is_empty()
                    && !self.hash.digest.trim().is_empty()
                    && !self.hash.length.trim().is_empty()
                    && (!self.hash.regular
                        || !self.hash.detect_spacing
                        || !self.hash.first.trim().is_empty())
            }
            Mode::CreateManifest => {
                !self.original.trim().is_empty() && !self.manifest.trim().is_empty()
            }
            Mode::FindChunks => {
                !source.trim().is_empty()
                    && !self.manifest.trim().is_empty()
                    && !self.output.trim().is_empty()
            }
            Mode::Layout => {
                !source.trim().is_empty()
                    && !self.layout.trim().is_empty()
                    && !self.output.trim().is_empty()
            }
        }
    }
    fn inputs(&mut self, ui: &mut egui::Ui, source: &str) {
        if matches!(self.mode, Mode::FindChunks | Mode::CreateManifest) {
            ui.horizontal_wrapped(|ui| {
                ui.selectable_value(
                    &mut self.mode,
                    Mode::FindChunks,
                    tr!("断片を探して復元", "Find and restore"),
                );
                ui.selectable_value(
                    &mut self.mode,
                    Mode::CreateManifest,
                    tr!("原本から記録を作成", "Create from original"),
                );
            });
        }
        match self.mode {
            Mode::WholeHash => {
                ui.label(source);
                ui.small(tr!("連続したファイルを検索します。先頭位置が分かる場合は、2つの領域に分かれたファイルも検索します。等間隔の断片は下で指定できます。", "Search for a contiguous file. A known first offset also enables a search for two runs. For regular fragmentation, select the spacing option below."));
                self.hash.show(ui);
            }
            Mode::Layout => {
                ui.label(source);
                ui.small(tr!("元ファイルのSHA-256と、ディスク先頭からのバイト位置を記したJSON配置表を選んでください。", "Choose a JSON layout containing the original SHA-256 and byte offsets from the start of this source."));
                path_row(
                    ui,
                    tr!("JSON配置表", "JSON layout"),
                    &mut self.layout,
                    Picker::Open,
                );
            }
            Mode::FindChunks => {
                if source.trim().is_empty() {
                    ui.label(tr!(
                        "設定画面で復元元のディスクまたはイメージを選択してください。",
                        "Select a disk or image in Settings to search for the file."
                    ));
                } else {
                    ui.label(source);
                }
                ui.small(tr!(
                    "削除前に記録した断片のハッシュでディスクを検索します。",
                    "Search the disk with chunk hashes recorded before the file was lost."
                ));
                path_row(
                    ui,
                    tr!("断片ハッシュのJSON", "Fingerprint manifest"),
                    &mut self.manifest,
                    Picker::Open,
                );
                ui.checkbox(
                    &mut self.sector_aligned,
                    tr!("512バイト境界のみを検索", "Search only 512-byte boundaries"),
                );
            }
            Mode::CreateManifest => {
                path_row(
                    ui,
                    tr!(
                        "削除前の原本または同一のコピー",
                        "Original file or identical copy"
                    ),
                    &mut self.original,
                    Picker::Open,
                );
                let size = self.chunk_size.get_or_insert(4096);
                ui.horizontal_wrapped(|ui| {
                    let label = ui.label(tr!("断片のバイト数", "Chunk size in bytes")).id;
                    egui::ComboBox::from_id_salt("fingerprint-chunk-size")
                        .selected_text(size.to_string())
                        .show_ui(ui, |ui| {
                            for n in [512, 1024, 4096, 65536, 1048576] {
                                ui.selectable_value(size, n, n.to_string());
                            }
                        })
                        .response
                        .labelled_by(label);
                });
                path_row(
                    ui,
                    tr!("新しい断片ハッシュのJSON", "New fingerprint manifest"),
                    &mut self.manifest,
                    Picker::File,
                );
            }
        }
        if self.mode != Mode::CreateManifest {
            path_row(
                ui,
                tr!("復元ファイルの保存先", "New output file"),
                &mut self.output,
                Picker::File,
            );
        }
    }
}
impl App {
    pub(super) fn extent_dialog(&mut self, ctx: &egui::Context) {
        let mut start = false;
        let mut close = false;
        let state = &mut self.extent_recovery;
        let modal = responsive_modal(ctx, "recover-known-layout", 620.0).show(ctx, |ui| {
            ui.set_width((ctx.content_rect().width() - 48.0).clamp(240.0, 620.0));
            ui.heading(if state.mode == Mode::Layout {
                tr!("配置表からファイルを復元", "Recover a file from a layout")
            } else if state.mode == Mode::WholeHash {
                tr!(
                    "原本のSHA-256でファイルを復元",
                    "Recover by whole-file SHA-256"
                )
            } else {
                tr!("断片ハッシュによる復元", "File fingerprints")
            });
            egui::ScrollArea::vertical()
                .max_height((ctx.content_rect().height() - 180.0).max(115.0))
                .show_accessible(ui, tr!("復元の設定", "Recovery settings"), |ui| {
                    state.inputs(ui, &self.source);
                    if let Some(error) = &state.error {
                        theme::error_banner(ui, &error.to_string());
                    }
                });
            ui.horizontal_wrapped(|ui| {
                let action = if state.mode == Mode::CreateManifest {
                    tr!("断片ハッシュを保存", "Save fingerprint manifest")
                } else {
                    tr!("ハッシュを照合して保存", "Verify hash and save")
                };
                start = ui
                    .add_enabled(state.ready(&self.source), theme::primary(ui, action, 210.0))
                    .clicked();
                close = ui.button(tr!("閉じる", "Close")).clicked();
            });
        });
        if modal.should_close() || close || start {
            state.open = false;
        }
        if start {
            self.start_extent_recovery();
        }
    }

    fn start_extent_recovery(&mut self) {
        self.cancel = Arc::new(AtomicBool::new(false));
        let cancel = Arc::clone(&self.cancel);
        let (tx, rx) = mpsc::channel();
        self.messages = Some(rx);
        self.stage = Stage::RestoringExtents;
        self.status_error = false;
        let state = &mut self.extent_recovery;
        state.error = None;
        self.status = match state.mode {
            Mode::WholeHash => message!(
                "原本のサイズとSHA-256に一致する領域を探しています。",
                "Searching for the original length and SHA-256."
            ),
            Mode::Layout => message!(
                "配置表の領域を読み取っています。",
                "Reading the ranges in the layout."
            ),
            Mode::FindChunks => message!(
                "一致する断片を探しています。",
                "Searching for matching chunks."
            ),
            Mode::CreateManifest => message!(
                "原本のハッシュを計算しています。",
                "Fingerprinting the original file."
            ),
        };
        let source = PathBuf::from(&self.source);
        let layout = PathBuf::from(&state.layout);
        let manifest = PathBuf::from(&state.manifest);
        let original = PathBuf::from(&state.original);
        let output = PathBuf::from(&state.output);
        let mode = state.mode;
        let hash = state.hash.clone();
        let alignment = if state.sector_aligned { 512 } else { 1 };
        let size = state.chunk_size.unwrap_or(4096);
        state.worker = Some(thread::spawn(move || {
            let result = match mode {
                Mode::WholeHash => hash
                    .recover(&source, &output, &cancel)
                    .map(Outcome::WholeHash),
                Mode::Layout => disk_image::recover_extents(&source, &layout, &output, &cancel)
                    .map(Outcome::File),
                Mode::FindChunks => disk_image::recover_fingerprinted(
                    &source, &manifest, &output, alignment, 0, None, &cancel,
                )
                .map(|result| Outcome::File(result.file)),
                Mode::CreateManifest => {
                    disk_image::fingerprint_file(&original, &manifest, size, &cancel)
                        .map(Outcome::Manifest)
                }
            };
            let _ = tx.send(Message::ExtentsRecovered(result));
        }));
    }

    pub(super) fn finish_extent_recovery(&mut self, result: rescue_core::Result<Outcome>) {
        self.extent_recovery.join();
        self.stage = Stage::Settings;
        self.status_error = false;
        match result {
            Ok(Outcome::WholeHash(result)) => {
                self.status = message!(
                    "{} に {} バイトを保存しました。使用した領域は {} 個です。SHA-256 {}",
                    "Saved {} ({} bytes, {} source runs). SHA-256 {}",
                    result.file.path.display(),
                    result.file.bytes,
                    result.extents.len(),
                    result.file.sha256
                );
            }
            Ok(Outcome::File(file)) => {
                self.status = message!(
                    "照合したファイルを {} に保存しました（{} バイト）。SHA-256 {}",
                    "Verified and saved {} ({} bytes). SHA-256 {}",
                    file.path.display(),
                    file.bytes,
                    file.sha256
                );
            }
            Ok(Outcome::Manifest(proof)) => {
                self.status = message!(
                    "{} に {} 断片の記録を保存しました。原本のSHA-256 {}",
                    "Saved {} with {} chunk fingerprints. Original SHA-256 {}",
                    proof.manifest.path.display(),
                    proof.chunks,
                    proof.sha256
                );
            }
            Err(rescue_core::Error::Cancelled) => {
                self.status = message!("処理を取り消しました。", "Operation cancelled.");
            }
            Err(error) => {
                self.status_error = true;
                self.status = message!("処理に失敗しました。{}", "Operation failed. {}", error);
                self.extent_recovery.error = Some(self.status.clone());
                self.extent_recovery.open = true;
            }
        }
    }
}
