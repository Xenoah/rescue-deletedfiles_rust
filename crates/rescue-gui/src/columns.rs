//! Column boundaries are pointer targets; the labelled headers provide the same
//! width operation to the keyboard. Avoid unnamed, invisible Tab stops.
use eframe::egui;

pub const DEFAULT: [f32; 5] = [250.0, 178.0, 178.0, 105.0, 90.0];
pub const MINIMUM: [f32; 5] = [140.0, 110.0, 110.0, 70.0, 65.0];

pub fn keyboard_width(ui: &egui::Ui, response: &egui::Response, width: &mut f32, index: usize) {
    let description = tr!(
        "Enterで並べ替え。左右の矢印キーで列幅を変更。",
        "Enter to sort. Left and right arrow keys change the column width."
    );
    response.clone().on_hover_text(description);
    ui.ctx().accesskit_node_builder(response.id, |node| {
        node.set_description(description);
    });
    if response.has_focus() {
        ui.memory_mut(|memory| {
            memory.set_focus_lock_filter(
                response.id,
                egui::EventFilter {
                    horizontal_arrows: true,
                    ..Default::default()
                },
            );
        });
        let change = ui.input_mut(|input| {
            let left = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowLeft);
            let right = input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowRight);
            (i32::from(right) - i32::from(left)) as f32 * 16.0
        });
        *width = (*width + change).clamp(MINIMUM[index], 1200.0);
    }
}

pub fn boundaries(ui: &egui::Ui, edges: &[f32; 5], top: f32, bottom: f32, widths: &mut [f32; 5]) {
    for (index, x) in edges.iter().copied().enumerate() {
        let p0 = egui::pos2(x, top);
        let p1 = egui::pos2(x, bottom);
        let rect = egui::Rect::from_min_max(p0, p1).expand(4.0);
        let response = ui.interact(
            rect,
            ui.id().with(("column-boundary", index)),
            egui::Sense::click_and_drag() - egui::Sense::FOCUSABLE,
        );
        if response.dragged() {
            widths[index] = (widths[index] + response.drag_delta().x).clamp(MINIMUM[index], 1200.0);
        }
        let stroke = if response.hovered() || response.dragged() {
            ui.ctx().set_cursor_icon(egui::CursorIcon::ResizeColumn);
            ui.visuals().widgets.active.bg_stroke
        } else {
            ui.visuals().widgets.noninteractive.bg_stroke
        };
        ui.painter().line_segment([p0, p1], stroke);
    }
}
