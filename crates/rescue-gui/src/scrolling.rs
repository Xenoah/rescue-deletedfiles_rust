//! egui's pointer scroll targets need names, visible keyboard focus and key
//! handling to be usable as Tab stops on small windows and large result sets.
use eframe::egui;
use egui::accesskit::{Action, ActionData, Role};
use egui::scroll_area::ScrollAreaOutput;

pub trait AccessibleScrollArea {
    fn show_accessible<R>(
        self,
        ui: &mut egui::Ui,
        name: &str,
        content: impl FnOnce(&mut egui::Ui) -> R,
    ) -> ScrollAreaOutput<R>;
}
impl AccessibleScrollArea for egui::ScrollArea {
    fn show_accessible<R>(
        self,
        ui: &mut egui::Ui,
        name: &str,
        content: impl FnOnce(&mut egui::Ui) -> R,
    ) -> ScrollAreaOutput<R> {
        let mut output = self.show(ui, content);
        finish(ui, &mut output, name);
        output
    }
}

pub fn finish<R>(ui: &egui::Ui, output: &mut ScrollAreaOutput<R>, name: &str) {
    let max = (output.content_size - output.inner_rect.size()).max(egui::Vec2::ZERO);
    let mut offset = output.state.offset;
    let targets = [
        (output.id.with("area"), None),
        (output.id.with(0usize), Some(0)),
        (output.id.with(1usize), Some(1)),
    ];
    for (id, axis) in targets {
        let Some(response) = ui.ctx().read_response(id) else {
            continue;
        };
        if !response.sense.is_focusable() {
            continue;
        }
        let label = match axis {
            Some(0) => tr_format!("{name}：横スクロール", "{name}: horizontal scroll"),
            Some(_) => tr_format!("{name}：縦スクロール", "{name}: vertical scroll"),
            None => tr_format!("{name}：スクロール領域", "{name}: scroll area"),
        };
        if response.has_focus() {
            ui.memory_mut(|memory| {
                memory.set_focus_lock_filter(
                    id,
                    egui::EventFilter {
                        horizontal_arrows: true,
                        vertical_arrows: true,
                        ..Default::default()
                    },
                )
            });
            let take = |key| ui.input_mut(|input| input.consume_key(egui::Modifiers::NONE, key));
            if take(egui::Key::ArrowLeft) && axis != Some(1) {
                offset.x -= 32.0;
            }
            if take(egui::Key::ArrowRight) && axis != Some(1) {
                offset.x += 32.0;
            }
            if take(egui::Key::ArrowUp) && axis != Some(0) {
                offset.y -= 32.0;
            }
            if take(egui::Key::ArrowDown) && axis != Some(0) {
                offset.y += 32.0;
            }
            let primary = axis.unwrap_or(if max.y > 0.0 { 1 } else { 0 });
            if take(egui::Key::PageUp) {
                offset[primary] -= output.inner_rect.size()[primary] * 0.9;
            }
            if take(egui::Key::PageDown) {
                offset[primary] += output.inner_rect.size()[primary] * 0.9;
            }
            if take(egui::Key::Home) {
                offset[primary] = 0.0;
            }
            if take(egui::Key::End) {
                offset[primary] = max[primary];
            }
            ui.painter().rect_stroke(
                response.rect.shrink(1.0),
                0.0,
                egui::Stroke::new(1.5_f32, ui.visuals().strong_text_color()),
                egui::StrokeKind::Inside,
            );
        }
        if response.enabled()
            && let Some(axis) = axis
        {
            ui.input(|input| {
                offset[axis] += 32.0
                    * (input.num_accesskit_action_requests(id, Action::Increment) as f32
                        - input.num_accesskit_action_requests(id, Action::Decrement) as f32);
                for request in input.accesskit_action_requests(id, Action::SetValue) {
                    if let Some(ActionData::NumericValue(value)) = request.data
                        && value.is_finite()
                    {
                        offset[axis] = value.clamp(0.0, f64::from(max[axis])) as f32;
                    }
                }
            });
        }
        offset = offset.max(egui::Vec2::ZERO).min(max);
        ui.ctx().accesskit_node_builder(id, |node| {
            node.set_label(label);
            node.set_description(tr!(
                "矢印キー、Page Up/Down、Home/Endで移動します。",
                "Use arrow keys, Page Up/Down or Home/End to scroll."
            ));
            node.set_role(if axis.is_some() {
                Role::ScrollBar
            } else {
                Role::ScrollView
            });
            if let Some(axis) = axis {
                node.set_orientation(if axis == 0 {
                    egui::accesskit::Orientation::Horizontal
                } else {
                    egui::accesskit::Orientation::Vertical
                });
                node.set_numeric_value(f64::from(offset[axis]));
                node.set_min_numeric_value(0.0);
                node.set_max_numeric_value(f64::from(max[axis]));
                node.set_numeric_value_step(32.0);
                if response.enabled() {
                    node.add_action(Action::Increment);
                    node.add_action(Action::Decrement);
                    node.add_action(Action::SetValue);
                }
            }
        });
    }
    if offset != output.state.offset {
        output.state.offset = offset;
        output.state.store(ui.ctx(), output.id);
        ui.ctx().request_repaint();
    }
}
