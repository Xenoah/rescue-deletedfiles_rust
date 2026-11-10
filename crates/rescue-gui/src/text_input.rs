use eframe::egui;

/// TextEdit exposes text selection but egui 0.33 does not implement SetValue.
/// Provide the same single-line edit to UIA/AccessKit clients as to the keyboard.
pub fn singleline(
    ui: &mut egui::Ui,
    value: &mut String,
    id: egui::Id,
    label: egui::Id,
    width: f32,
    hint: &str,
) -> egui::Response {
    use egui::accesskit::{Action, ActionData};
    const LIMIT: usize = 32_768;
    let replacement = ui.input(|input| {
        input
            .accesskit_action_requests(id, Action::SetValue)
            .filter_map(|request| match &request.data {
                Some(ActionData::Value(text))
                    if !text.contains(['\0', '\r', '\n']) && text.chars().count() <= LIMIT =>
                {
                    Some(text.to_string())
                }
                _ => None,
            })
            .last()
    });
    let mut changed = false;
    if ui.is_enabled()
        && let Some(text) = replacement
        && *value != text
    {
        *value = text;
        changed = true;
    }
    let mut response = ui
        .add_sized(
            [width, 36.0],
            egui::TextEdit::singleline(value)
                .id(id)
                .hint_text(hint)
                .char_limit(LIMIT),
        )
        .labelled_by(label);
    if response.enabled() {
        ui.ctx()
            .accesskit_node_builder(response.id, |node| node.add_action(Action::SetValue));
    }
    if changed {
        response.mark_changed();
    }
    response
}
