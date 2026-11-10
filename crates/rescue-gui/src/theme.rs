//! Theme-aware colors and readable controls shared by every screen.
use eframe::egui::{self, Color32, RichText};

#[derive(Clone, Copy)]
pub struct Palette {
    pub canvas: Color32,
    pub surface: Color32,
    pub ink: Color32,
    pub muted: Color32,
    pub accent: Color32,
    pub primary: Color32,
    pub tint: Color32,
    pub border: Color32,
    pub warning: Color32,
    pub warning_fill: Color32,
    pub error: Color32,
    pub error_fill: Color32,
}

fn colors(dark: bool) -> Palette {
    if dark {
        Palette {
            canvas: Color32::from_rgb(15, 23, 34),
            surface: Color32::from_rgb(25, 36, 50),
            ink: Color32::from_rgb(239, 245, 252),
            muted: Color32::from_rgb(185, 201, 219),
            accent: Color32::from_rgb(110, 224, 204),
            primary: Color32::from_rgb(0, 105, 94),
            tint: Color32::from_rgb(28, 65, 64),
            border: Color32::from_rgb(100, 119, 142),
            warning: Color32::from_rgb(255, 207, 122),
            warning_fill: Color32::from_rgb(64, 47, 24),
            error: Color32::from_rgb(255, 153, 165),
            error_fill: Color32::from_rgb(60, 28, 39),
        }
    } else {
        Palette {
            canvas: Color32::from_rgb(237, 242, 247),
            surface: Color32::WHITE,
            ink: Color32::from_rgb(23, 43, 64),
            muted: Color32::from_rgb(70, 89, 111),
            accent: Color32::from_rgb(0, 98, 88),
            primary: Color32::from_rgb(0, 98, 88),
            tint: Color32::from_rgb(222, 244, 238),
            border: Color32::from_rgb(132, 151, 173),
            warning: Color32::from_rgb(135, 69, 0),
            warning_fill: Color32::from_rgb(255, 242, 214),
            error: Color32::from_rgb(180, 35, 54),
            error_fill: Color32::from_rgb(255, 240, 242),
        }
    }
}

pub fn palette(ui: &egui::Ui) -> Palette {
    colors(ui.visuals().dark_mode)
}

pub fn configure(ctx: &egui::Context) {
    for mode in [egui::Theme::Light, egui::Theme::Dark] {
        let c = colors(mode == egui::Theme::Dark);
        ctx.style_mut_of(mode, |s| {
            s.spacing.item_spacing = egui::vec2(10.0, 10.0);
            s.spacing.interact_size.y = 36.0;
            s.spacing.button_padding = egui::vec2(12.0, 8.0);
            s.visuals = mode.default_visuals();
            s.visuals.panel_fill = c.canvas;
            s.visuals.window_fill = c.surface;
            s.visuals.extreme_bg_color = if mode == egui::Theme::Dark {
                Color32::from_rgb(10, 18, 29)
            } else {
                Color32::from_rgb(226, 234, 243)
            };
            s.visuals.faint_bg_color = c.tint.gamma_multiply(0.45);
            s.visuals.override_text_color = Some(c.ink);
            s.visuals.weak_text_color = Some(c.muted);
            s.visuals.selection.bg_fill = c.tint;
            s.visuals.selection.stroke = egui::Stroke::new(1.5_f32, c.accent);
            s.visuals.widgets.noninteractive.bg_stroke = egui::Stroke::new(1.0_f32, c.border);
            s.visuals.widgets.noninteractive.fg_stroke = egui::Stroke::new(1.0_f32, c.ink);
            s.visuals.widgets.inactive.bg_fill = c.surface;
            s.visuals.widgets.inactive.weak_bg_fill = c.surface;
            s.visuals.widgets.inactive.bg_stroke = egui::Stroke::new(1.0_f32, c.border);
            s.visuals.widgets.inactive.fg_stroke = egui::Stroke::new(1.0_f32, c.ink);
            s.visuals.widgets.hovered.bg_fill = c.tint;
            s.visuals.widgets.hovered.weak_bg_fill = c.tint;
            s.visuals.widgets.hovered.bg_stroke = egui::Stroke::new(1.5_f32, c.accent);
            s.visuals.widgets.hovered.fg_stroke = egui::Stroke::new(1.5_f32, c.ink);
            s.visuals.widgets.active.bg_fill = c.tint;
            s.visuals.widgets.active.weak_bg_fill = c.tint;
            s.visuals.widgets.active.bg_stroke = egui::Stroke::new(2.0_f32, c.accent);
            s.visuals.widgets.active.fg_stroke = egui::Stroke::new(1.5_f32, c.ink);
            s.visuals.hyperlink_color = c.accent;
            s.visuals.warn_fg_color = c.warning;
            s.visuals.error_fg_color = c.error;
            for (style, size) in [
                (egui::TextStyle::Heading, 26.0),
                (egui::TextStyle::Body, 16.0),
                (egui::TextStyle::Button, 16.0),
                (egui::TextStyle::Small, 14.0),
            ] {
                s.text_styles
                    .insert(style, egui::FontId::proportional(size));
            }
            s.text_styles
                .insert(egui::TextStyle::Monospace, egui::FontId::monospace(15.0));
        });
    }
}

pub fn card(ui: &egui::Ui) -> egui::Frame {
    let c = palette(ui);
    egui::Frame::new()
        .fill(c.surface)
        .stroke(egui::Stroke::new(1.0_f32, c.border))
        .corner_radius(10)
        .inner_margin(18)
}

/// Reserve the same frame in both states, keeping text-edit IDs and focus stable.
pub fn field(ui: &mut egui::Ui, error: Option<&str>, add: impl FnOnce(&mut egui::Ui)) {
    let c = palette(ui);
    let response = egui::Frame::new()
        .stroke(egui::Stroke::new(
            2.0_f32,
            if error.is_some() {
                c.error
            } else {
                Color32::TRANSPARENT
            },
        ))
        .corner_radius(6)
        .inner_margin(5)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            add(ui);
            if let Some(error) = error {
                ui.colored_label(c.error, tr_format!("要確認: {error}", "Check: {error}"));
            }
        })
        .response;
    let target = egui::Id::new("reveal-first-invalid-field");
    if error.is_some()
        && ui
            .ctx()
            .data(|d| d.get_temp::<bool>(target).unwrap_or(false))
    {
        response.scroll_to_me(Some(egui::Align::Center));
        ui.ctx().data_mut(|d| d.insert_temp(target, false));
    }
}

pub fn reveal_errors(ctx: &egui::Context) {
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("reveal-first-invalid-field"), true));
}

pub fn error_banner(ui: &mut egui::Ui, message: &str) {
    let c = palette(ui);
    egui::Frame::new()
        .fill(c.error_fill)
        .stroke(egui::Stroke::new(2.0_f32, c.error))
        .corner_radius(6)
        .inner_margin(10)
        .show(ui, |ui| {
            ui.set_width(ui.available_width());
            ui.label(RichText::new(message).color(c.error).strong());
        });
}

pub fn section(ui: &mut egui::Ui, number: &str, title: &str, hint: &str) {
    let c = palette(ui);
    ui.horizontal_wrapped(|ui| {
        egui::Frame::new()
            .fill(c.tint)
            .corner_radius(6)
            .inner_margin(6)
            .show(ui, |ui| {
                ui.label(RichText::new(number).strong().color(c.accent));
            });
        ui.label(RichText::new(title).size(20.0).strong());
    });
    ui.label(RichText::new(hint).color(c.muted));
    ui.add_space(4.0);
}

pub fn primary<'a>(ui: &egui::Ui, label: &'a str, width: f32) -> egui::Button<'a> {
    egui::Button::new(RichText::new(label).strong().color(Color32::WHITE))
        .fill(palette(ui).primary)
        .min_size(egui::vec2(width, 44.0))
        .corner_radius(7)
}

pub fn step(ui: &mut egui::Ui, number: usize, label: &str, active: bool, complete: bool) {
    let c = palette(ui);
    egui::Frame::new()
        .fill(if active {
            c.primary
        } else if complete {
            c.tint
        } else {
            c.surface
        })
        .stroke(egui::Stroke::new(
            1.0_f32,
            if active { c.primary } else { c.border },
        ))
        .corner_radius(6)
        .inner_margin(egui::Margin::symmetric(12, 7))
        .show(ui, |ui| {
            ui.label(
                RichText::new(format!("{number}  {label}"))
                    .strong()
                    .color(if active {
                        Color32::WHITE
                    } else if complete {
                        c.accent
                    } else {
                        c.muted
                    }),
            );
        });
}

pub fn badge(ui: &mut egui::Ui, label: impl Into<String>, warning: bool) {
    let c = palette(ui);
    egui::Frame::new()
        .fill(if warning { c.warning_fill } else { c.tint })
        .corner_radius(5)
        .inner_margin(egui::Margin::symmetric(8, 4))
        .show(ui, |ui| {
            ui.label(
                RichText::new(label)
                    .color(if warning { c.warning } else { c.accent })
                    .strong(),
            );
        });
}

#[cfg(test)]
mod tests {
    use super::*;
    fn luminance(color: Color32) -> f64 {
        let channels = [color.r(), color.g(), color.b()].map(|channel| {
            let c = f64::from(channel) / 255.0;
            if c <= 0.04045 {
                c / 12.92
            } else {
                ((c + 0.055) / 1.055).powf(2.4)
            }
        });
        channels[0] * 0.2126 + channels[1] * 0.7152 + channels[2] * 0.0722
    }
    #[test]
    fn both_palettes_keep_text_errors_and_actions_readable() {
        for dark in [false, true] {
            let c = colors(dark);
            for (fg, bg) in [
                (c.ink, c.surface),
                (c.ink, c.canvas),
                (c.muted, c.surface),
                (c.muted, c.canvas),
                (c.accent, c.tint),
                (c.error, c.error_fill),
                (c.warning, c.warning_fill),
                (Color32::WHITE, c.primary),
            ] {
                let a = luminance(fg);
                let b = luminance(bg);
                let contrast = (a.max(b) + 0.05) / (a.min(b) + 0.05);
                assert!(contrast >= 4.5, "dark={dark}, {fg:?} on {bg:?}: {contrast}");
            }
        }
    }
}
