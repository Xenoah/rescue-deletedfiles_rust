//! Exercise the actual egui tree and keyboard/IME event path. Native UIA and
//! real input-method integration are checked separately by the Windows harness.
use super::*;
use egui::accesskit::{Node, NodeId, TreeUpdate};

fn frame(ctx: &egui::Context, app: &mut App, events: Vec<egui::Event>) -> TreeUpdate {
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                egui::vec2(1260.0, 820.0),
            )),
            events,
            ..Default::default()
        },
        |ctx| app.render(ctx),
    )
    .platform_output
    .accesskit_update
    .unwrap()
}
fn key(
    ctx: &egui::Context,
    app: &mut App,
    key: egui::Key,
    modifiers: egui::Modifiers,
) -> TreeUpdate {
    for pressed in [true, false] {
        frame(
            ctx,
            app,
            vec![egui::Event::Key {
                key,
                physical_key: None,
                pressed,
                repeat: false,
                modifiers,
            }],
        );
    }
    frame(ctx, app, vec![])
}
fn node(tree: &TreeUpdate, id: NodeId) -> &Node {
    &tree
        .nodes
        .iter()
        .find(|(key, _)| *key == id)
        .expect("focused node must be exposed")
        .1
}
fn name(tree: &TreeUpdate, id: NodeId) -> String {
    let n = node(tree, id);
    if let Some(label) = n.label() {
        return label.into();
    }
    if !n.labelled_by().is_empty() {
        return n
            .labelled_by()
            .iter()
            .map(|id| name(tree, *id))
            .collect::<Vec<_>>()
            .join(" ");
    }
    if let Some(value) = n.value() {
        return value.into();
    }
    n.children()
        .iter()
        .map(|id| name(tree, *id))
        .collect::<Vec<_>>()
        .join(" ")
}
fn app() -> (egui::Context, App) {
    let ctx = egui::Context::default();
    ctx.enable_accesskit();
    let mut app = App::blank();
    app.language = Language::English;
    app.stage = Stage::Selecting;
    let mut session = super::tests::session(FileSystem::Ntfs);
    for id in ["first.pdf", "日本語.txt"] {
        session.candidates.push(Candidate {
            id: id.into(),
            record: 0,
            fragment_offset: None,
            sequence: 1,
            stream_name: None,
            name: id.into(),
            original_path: format!("資料/{id}"),
            path_verified: true,
            alternate_names: vec![],
            size: Some(61),
            supported: true,
            warnings: vec![],
            record_sha256: String::new(),
            extent_evidence: Default::default(),
            allocation_status: Default::default(),
            created: None,
            modified: None,
        });
    }
    app.session = Some(session);
    app.selected.insert("first.pdf".into());
    for _ in 0..3 {
        frame(&ctx, &mut app, vec![]);
    }
    (ctx, app)
}
#[test]
fn tab_has_named_stops_and_reaches_recovery_and_checkbox_sets_preview_target() {
    let (ctx, mut app) = app();
    ctx.memory_mut(|m| m.request_focus(egui::Id::new("result-search")));
    let tree = frame(&ctx, &mut app, vec![]);
    assert_eq!(name(&tree, tree.focus), "Search results");
    let mut stops = vec![];
    for _ in 0..30 {
        let tree = key(&ctx, &mut app, egui::Key::Tab, egui::Modifiers::NONE);
        let label = name(&tree, tree.focus);
        assert!(
            !label.trim().is_empty(),
            "unnamed Tab stop {:?}: {:?}",
            tree.focus,
            node(&tree, tree.focus)
        );
        if label == "Select 日本語.txt for recovery" {
            key(&ctx, &mut app, egui::Key::Space, egui::Modifiers::NONE);
            assert!(app.selected.contains("日本語.txt"));
            assert_eq!(app.focused.as_deref(), Some("日本語.txt"));
        }
        stops.push(label.clone());
        if label == "Recover selected files" {
            break;
        }
    }
    assert_eq!(
        stops.last().map(String::as_str),
        Some("Recover selected files"),
        "{stops:?}"
    );
    assert!(
        stops.len() <= 14,
        "unexpected invisible resizer stops: {stops:?}"
    );
    let tree = key(&ctx, &mut app, egui::Key::Tab, egui::Modifiers::SHIFT);
    assert_eq!(name(&tree, tree.focus), "Back to settings");
}
#[test]
fn column_header_resizes_with_arrows_without_sorting_or_moving_selection() {
    let (ctx, mut app) = app();
    let tree = frame(&ctx, &mut app, vec![]);
    let (id, _) = tree
        .nodes
        .iter()
        .find(|(_, n)| n.label().is_some_and(|n| n.starts_with("Name")))
        .unwrap();
    frame(
        &ctx,
        &mut app,
        vec![egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Focus,
                target: *id,
                data: None,
            },
        )],
    );
    frame(&ctx, &mut app, vec![]);
    let before = app.selected.clone();
    key(&ctx, &mut app, egui::Key::ArrowRight, egui::Modifiers::NONE);
    assert_eq!(app.column_widths[0], columns::DEFAULT[0] + 16.0);
    assert_eq!(app.selected, before);
    assert_eq!(app.sort, SortColumn::Name);
    assert!(!app.descending);
    key(&ctx, &mut app, egui::Key::ArrowLeft, egui::Modifiers::NONE);
    assert_eq!(app.column_widths[0], columns::DEFAULT[0]);
}
#[test]
fn japanese_ime_composition_commits_once_and_cancel_keeps_previous_query() {
    let (ctx, mut app) = app();
    ctx.memory_mut(|m| m.request_focus(egui::Id::new("result-search")));
    frame(&ctx, &mut app, vec![]);
    for event in [
        egui::ImeEvent::Enabled,
        egui::ImeEvent::Preedit("にほんご".into()),
        egui::ImeEvent::Preedit("日本語".into()),
        egui::ImeEvent::Commit("日本語".into()),
        egui::ImeEvent::Disabled,
    ] {
        frame(&ctx, &mut app, vec![egui::Event::Ime(event)]);
    }
    assert_eq!(app.filter, "日本語");
    assert_eq!(app.rows.len(), 1);
    // A cancelled preedit is removed without erasing already committed text.
    for event in [
        egui::ImeEvent::Enabled,
        egui::ImeEvent::Preedit("けす".into()),
        egui::ImeEvent::Preedit(String::new()),
        egui::ImeEvent::Disabled,
    ] {
        frame(&ctx, &mut app, vec![egui::Event::Ime(event)]);
    }
    assert_eq!(app.filter, "日本語");
    assert_eq!(app.selected.len(), 1);
}

#[test]
fn accessible_edit_preserves_unicode_and_cannot_edit_a_busy_form() {
    let (ctx, mut app) = app();
    let tree = frame(&ctx, &mut app, vec![]);
    let id = tree
        .nodes
        .iter()
        .find(|(id, n)| {
            n.role() == egui::accesskit::Role::TextInput && name(&tree, *id) == "Search results"
        })
        .unwrap()
        .0;
    let set_value = |value: &str| {
        egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action: egui::accesskit::Action::SetValue,
            target: id,
            data: Some(egui::accesskit::ActionData::Value(value.into())),
        })
    };
    assert!(node(&tree, id).supports_action(egui::accesskit::Action::SetValue));
    frame(&ctx, &mut app, vec![set_value("日本語")]);
    assert_eq!(app.filter, "日本語");
    assert_eq!(app.rows.len(), 1);
    frame(&ctx, &mut app, vec![set_value("first\nsecond")]);
    assert_eq!(app.filter, "日本語");
    app.stage = Stage::Recovering;
    frame(&ctx, &mut app, vec![set_value("should not change")]);
    assert_eq!(app.filter, "日本語");
}
