//! Optional comparison of the current recovery's recorded SHA-256 values.
use super::*;
use rescue_core::{KnownHashComparison, KnownHashOutcome};

#[derive(Default)]
pub(super) struct State {
    open: bool,
    manifest: String,
    output: String,
    report: serde_json::Value,
    receiver: Option<mpsc::Receiver<Result<KnownHashComparison, String>>>,
    comparison: Option<KnownHashComparison>,
    error: String,
    saved: bool,
}
impl State {
    pub(super) fn start(&mut self, report: serde_json::Value, folder: &Path) {
        *self = Self {
            open: true,
            report,
            output: folder
                .join("rescue-known-hashes.json")
                .display()
                .to_string(),
            ..Default::default()
        };
    }
    pub(super) fn show(&mut self, ctx: &egui::Context, session: Option<&Session>) {
        if !self.open {
            return;
        }
        if let Some(receiver) = &self.receiver
            && let Ok(result) = receiver.try_recv()
        {
            match result {
                Ok(comparison) => self.comparison = Some(comparison),
                Err(error) => self.error = error,
            }
            self.receiver = None;
        }
        let mut close = false;
        let response=responsive_modal(ctx,"known-hash-comparison",800.0).show(ctx,|ui| {
            ui.set_width((ctx.content_rect().width()-48.0).clamp(240.0,800.0));
            ui.heading(tr!("既知のSHA-256と照合", "Compare known SHA-256 hashes"));
            egui::ScrollArea::vertical().max_height((ctx.content_rect().height()-155.0).max(100.0))
                .show_accessible(ui,tr!("ハッシュ照合", "Hash comparison"),|ui| {
                    ui.label(tr!("復元時に計算したSHA-256を、手元の既知一覧と比較します。", "Compare SHA-256 computed during recovery with your known list."));
                    ui.small(tr!("保存後のファイルは読み直しません。一致しても元のパスや一覧自体の正しさは証明しません。", "Saved files are not re-read. A match does not establish an original path or authenticate the list."));
                    ui.add_enabled_ui(self.receiver.is_none(),|ui| {
                        let before=self.manifest.clone();
                        path_row(ui,tr!("SHA256SUMSファイル（UTF-8）", "SHA256SUMS file (UTF-8)"),&mut self.manifest,Picker::Open);
                        if before!=self.manifest {self.comparison=None;self.error.clear();self.saved=false;}
                        if ui.add_enabled(!self.manifest.trim().is_empty(),egui::Button::new(tr!("照合する", "Compare"))).clicked() {
                            self.error.clear();self.saved=false;self.comparison=None;
                            let path=PathBuf::from(&self.manifest);let report=self.report.clone();let (tx,rx)=mpsc::channel();self.receiver=Some(rx);
                            thread::spawn(move || {let result=rescue_core::read_known_hashes(&path).and_then(|bytes| rescue_core::compare_known_hashes(&report,&bytes)).map_err(|e|format!("{}: {e}",e.code()));let _=tx.send(result);});
                        }
                    });
                    if self.receiver.is_some() { ui.horizontal(|ui| {ui.spinner();ui.label(tr!("照合中…", "Comparing…"));}); }
                    if !self.error.is_empty() {ui.colored_label(theme::palette(ui).warning,&self.error);}
                    if let Some(comparison)=&self.comparison {
                        ui.separator();
                        let counts=&comparison.counts;
                        ui.horizontal_wrapped(|ui| {
                            ui.colored_label(theme::palette(ui).accent,tr_format!("一致 {} 件", "Matched: {}",counts.matched));
                            ui.label(tr_format!("一覧にない {} 件", "Not listed: {}",counts.not_listed));
                            ui.colored_label(theme::palette(ui).warning,tr_format!("照合できない {} 件", "Not comparable: {}",counts.not_comparable));
                        });
                        ui.small(tr!("一覧にない結果を破損とは判定しません。複数の既知名が一致しても、名前の変更や復元の根拠の書き換えは行いません。", "Not listed does not mean corrupt. Multiple matching labels do not rename files or change recovery evidence."));
                        ui.collapsing(tr!("ファイル別の結果", "Per-file results"),|ui| {
                            for row in comparison.results.iter().take(200) {
                                ui.horizontal_wrapped(|ui| {
                                    let (label,color)=match row.outcome {KnownHashOutcome::Matched=>(tr!("一致", "Matched"),theme::palette(ui).accent),KnownHashOutcome::NotListed=>(tr!("一覧にない", "Not listed"),theme::palette(ui).muted),KnownHashOutcome::NotComparable=>(tr!("照合できない", "Not comparable"),theme::palette(ui).warning)};
                                    ui.colored_label(color,label);ui.add(egui::Label::new(&row.displayed_name).wrap());
                                });
                                if row.outcome==KnownHashOutcome::Matched && let Some(hash)=&row.recorded_sha256 && let Some(labels)=comparison.known_contents.get(hash) {
                                    for label in labels.iter().take(3) {ui.small(label);}
                                    if labels.len()>3 {ui.small(tr_format!("ほか {} 件（全件は保存JSON）", "{} more (all in saved JSON)",labels.len()-3));}
                                }
                            }
                            if comparison.results.len()>200 {ui.small(tr!("先頭200件を表示。保存JSONには全件を含めます。", "Showing the first 200; saved JSON includes every result."));}
                        });
                        ui.collapsing(tr!("照合の記録", "Comparison record"),|ui| {
                            ui.small(tr_format!("既知一覧: {} 行 / {} 種類の内容", "Known list: {} entries / {} distinct contents",comparison.manifest_entries,comparison.known_contents.len()));
                            ui.label(format!("SHA-256: {}",comparison.manifest_sha256));
                        });
                        let previous_output=self.output.clone();
                        text_row(ui,tr!("照合結果の保存先（JSON）", "Comparison output (JSON)"),&mut self.output,"");
                        if previous_output!=self.output {self.saved=false;}
                        if ui.button(tr!("照合結果を保存", "Save comparison")).clicked() {
                            let result=session.ok_or_else(||rescue_core::Error::Request("no recovery session".into())).and_then(|s|s.guard_output(Path::new(&self.output))).and_then(|()|rescue_core::save_known_hash_comparison(comparison,Path::new(&self.output)));
                            match result {Ok(())=>{self.saved=true;self.error.clear();},Err(e)=>{self.saved=false;self.error=e.to_string();}}
                        }
                        if self.saved {ui.colored_label(theme::palette(ui).accent,tr!("照合結果を保存しました。元の復元記録と警告も含みます。", "Comparison saved, including the original recovery record and warnings."));}
                    }
                });
            ui.separator();close=ui.button(tr!("閉じる", "Close")).clicked();
        });
        if close || response.should_close() {
            self.open = false;
        }
        if self.receiver.is_some() {
            ctx.request_repaint_after(std::time::Duration::from_millis(50));
        }
    }
}
