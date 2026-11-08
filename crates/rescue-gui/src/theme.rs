//! Shared visual hierarchy. Status labels always retain text, not just color.
use eframe::egui::{self, Color32, RichText};

pub const INK: Color32 = Color32::from_rgb(27, 43, 65);
pub const MUTED: Color32 = Color32::from_rgb(80, 98, 119);
pub const ACCENT: Color32 = Color32::from_rgb(0, 104, 104);
pub const TINT: Color32 = Color32::from_rgb(223, 244, 240);
pub const WARNING: Color32 = Color32::from_rgb(145, 76, 0);
pub const BORDER: Color32 = Color32::from_rgb(209, 220, 231);

pub fn configure(ctx: &egui::Context) {
    ctx.set_visuals(egui::Visuals::light());
    ctx.style_mut(|s| {
        s.spacing.item_spacing = egui::vec2(8.0, 8.0);
        s.spacing.interact_size.y = 28.0;
        s.spacing.button_padding = egui::vec2(10.0, 5.0);
        s.visuals.panel_fill = Color32::from_rgb(241, 245, 249);
        s.visuals.window_fill = Color32::WHITE;
        s.visuals.extreme_bg_color = Color32::WHITE;
        s.visuals.faint_bg_color = Color32::from_rgb(235, 242, 247);
        s.visuals.override_text_color = Some(INK);
        s.visuals.weak_text_color = Some(MUTED);
        s.visuals.selection.bg_fill = TINT;
        s.visuals.selection.stroke = egui::Stroke::new(1.5_f32, ACCENT);
        s.visuals.widgets.inactive.bg_fill = Color32::from_rgb(231, 238, 245);
        s.visuals.widgets.inactive.weak_bg_fill = Color32::from_rgb(231, 238, 245);
        s.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, BORDER);
        s.visuals.widgets.hovered.bg_fill = Color32::from_rgb(211, 234, 233);
        s.visuals.widgets.hovered.weak_bg_fill = Color32::from_rgb(211, 234, 233);
        s.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.5_f32, ACCENT);
        s.visuals.widgets.active.bg_stroke = egui::Stroke::new(2.0_f32, ACCENT);
        s.visuals.hyperlink_color = ACCENT;
        s.visuals.warn_fg_color = WARNING;
        s.visuals.error_fg_color = Color32::from_rgb(173, 40, 50);
        s.text_styles
            .insert(egui::TextStyle::Heading, egui::FontId::proportional(23.0));
    });
}

pub fn card() -> egui::Frame {
    egui::Frame::new()
        .fill(Color32::WHITE)
        .stroke(egui::Stroke::new(1.0_f32, BORDER))
        .corner_radius(10)
        .inner_margin(16)
}

pub fn section(ui: &mut egui::Ui, number: &str, title: &str, hint: &str) {
    ui.horizontal(|ui| {
        egui::Frame::new()
            .fill(TINT)
            .corner_radius(6)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.label(RichText::new(number).strong().color(ACCENT));
            });
        ui.label(RichText::new(title).size(18.0).strong());
    });
    ui.label(RichText::new(hint).color(MUTED));
    ui.add_space(4.0);
}

pub fn primary(label: &str, width: f32) -> egui::Button<'_> {
    egui::Button::new(RichText::new(label).strong().color(Color32::WHITE))
        .fill(ACCENT)
        .min_size(egui::vec2(width, 38.0))
        .corner_radius(7)
}

pub fn step(ui: &mut egui::Ui, number: usize, label: &str, active: bool, complete: bool) {
    egui::Frame::new()
        .fill(if active {
            ACCENT
        } else if complete {
            TINT
        } else {
            Color32::TRANSPARENT
        })
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(10, 5))
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!("{number}  {label}"))
                    .strong()
                    .color(if active {
                        Color32::WHITE
                    } else if complete {
                        ACCENT
                    } else {
                        MUTED
                    }),
            );
        });
}

pub fn badge(ui: &mut egui::Ui, label: impl Into<String>, warning: bool) {
    egui::Frame::new()
        .fill(if warning {
            Color32::from_rgb(255, 242, 214)
        } else {
            TINT
        })
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(label)
                    .color(if warning { WARNING } else { ACCENT })
                    .strong(),
            );
        });
}
