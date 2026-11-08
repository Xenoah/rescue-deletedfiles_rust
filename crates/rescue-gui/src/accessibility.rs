//! Exercise the actual egui tree and keyboard/IME event path. Native UIA and
//! real input-method integration are checked separately by the Windows harness.
use super::*;
use egui::accesskit::{Node, NodeId, TreeUpdate};

fn frame(ctx: &egui::Context, app: &mut App, mut events: Vec<egui::Event>) -> TreeUpdate {
    app.ime
        .normalize(ctx.memory(|memory| memory.focused()), &mut events);
    ctx.run(
        egui::RawInput {
            screen_rect: Some(egui::Rect::from_min_size(
                egui::Pos2::ZERO,
                ctx.data(|data| data.get_temp::<egui::Vec2>(egui::Id::new("test-viewport")))
                    .unwrap_or(egui::vec2(1260.0, 820.0)),
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
    configure_style(&ctx);
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

#[test]
fn report_follows_recovery_in_tab_order_including_a_narrow_window() {
    for size in [egui::vec2(1260.0, 820.0), egui::vec2(720.0, 520.0)] {
        let (ctx, mut app) = app();
        ctx.data_mut(|data| data.insert_temp(egui::Id::new("test-viewport"), size));
        app.report = "Saved session: 資料/session.sqlite".into();
        for _ in 0..3 {
            frame(&ctx, &mut app, vec![]);
        }
        ctx.memory_mut(|m| m.request_focus(egui::Id::new("result-search")));
        frame(&ctx, &mut app, vec![]);
        let mut stops = vec![];
        for _ in 0..40 {
            let tree = key(&ctx, &mut app, egui::Key::Tab, egui::Modifiers::NONE);
            let label = name(&tree, tree.focus);
            assert!(
                !label.trim().is_empty(),
                "unnamed stop at {size:?}: {:?}",
                node(&tree, tree.focus)
            );
            stops.push(label.clone());
            if label == "Recover selected files" {
                let tree = key(&ctx, &mut app, egui::Key::Tab, egui::Modifiers::NONE);
                assert_eq!(name(&tree, tree.focus), "Saved locations and reports");
                break;
            }
        }
        assert_eq!(
            stops.last().map(String::as_str),
            Some("Recover selected files"),
            "{size:?}: {stops:?}"
        );
    }
}

#[test]
fn scroll_bars_support_keyboard_and_accessible_range_actions_without_changing_selection() {
    let (ctx, mut app) = app();
    ctx.data_mut(|d| d.insert_temp(egui::Id::new("test-viewport"), egui::vec2(720.0, 520.0)));
    for _ in 0..3 {
        frame(&ctx, &mut app, vec![]);
    }
    let tree = frame(&ctx, &mut app, vec![]);
    let id = tree
        .nodes
        .iter()
        .find(|(_, n)| n.label() == Some("Files: horizontal scroll"))
        .unwrap()
        .0;
    let request = |action, data| {
        egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
            action,
            target: id,
            data,
        })
    };
    frame(
        &ctx,
        &mut app,
        vec![request(egui::accesskit::Action::Focus, None)],
    );
    frame(&ctx, &mut app, vec![]);
    let before = app.selected.clone();
    let tree = key(&ctx, &mut app, egui::Key::ArrowRight, egui::Modifiers::NONE);
    assert_eq!(node(&tree, id).numeric_value(), Some(32.0));
    let tree = frame(
        &ctx,
        &mut app,
        vec![request(
            egui::accesskit::Action::SetValue,
            Some(egui::accesskit::ActionData::NumericValue(80.0)),
        )],
    );
    assert_eq!(node(&tree, id).numeric_value(), Some(80.0));
    let tree = key(&ctx, &mut app, egui::Key::End, egui::Modifiers::NONE);
    assert_eq!(
        node(&tree, id).numeric_value(),
        node(&tree, id).max_numeric_value()
    );
    assert_eq!(app.selected, before);
}

#[test]
fn icon_checkboxes_expose_names_and_update_preview_target() {
    let (ctx, mut app) = app();
    app.view = ViewStyle::Icons;
    let tree = frame(&ctx, &mut app, vec![]);
    let id = tree
        .nodes
        .iter()
        .find(|(_, n)| n.label() == Some("Select 日本語.txt for recovery"))
        .unwrap()
        .0;
    frame(
        &ctx,
        &mut app,
        vec![egui::Event::AccessKitActionRequest(
            egui::accesskit::ActionRequest {
                action: egui::accesskit::Action::Focus,
                target: id,
                data: None,
            },
        )],
    );
    key(&ctx, &mut app, egui::Key::Space, egui::Modifiers::NONE);
    assert!(app.selected.contains("日本語.txt"));
    assert_eq!(app.focused.as_deref(), Some("日本語.txt"));
}

#[test]
fn pointer_resizes_the_column_boundary_instead_of_the_used_label_width() {
    let (ctx, mut app) = app();
    let tree = frame(&ctx, &mut app, vec![]);
    let next = tree
        .nodes
        .iter()
        .find(|(_, n)| n.label() == Some("Date created"))
        .unwrap()
        .1
        .bounds()
        .unwrap();
    let start = egui::pos2(next.x0 as f32 - 4.0, ((next.y0 + next.y1) * 0.5) as f32);
    let finish = start + egui::vec2(48.0, 0.0);
    let selection = app.selected.clone();
    frame(&ctx, &mut app, vec![egui::Event::PointerMoved(start)]);
    frame(
        &ctx,
        &mut app,
        vec![egui::Event::PointerButton {
            pos: start,
            button: egui::PointerButton::Primary,
            pressed: true,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    frame(&ctx, &mut app, vec![egui::Event::PointerMoved(finish)]);
    frame(
        &ctx,
        &mut app,
        vec![egui::Event::PointerButton {
            pos: finish,
            button: egui::PointerButton::Primary,
            pressed: false,
            modifiers: egui::Modifiers::NONE,
        }],
    );
    assert_eq!(app.column_widths[0], columns::DEFAULT[0] + 48.0);
    assert_eq!(app.selected, selection);
}

#[test]
fn settings_have_named_keyboard_targets_and_reach_start_scan() {
    for size in [egui::vec2(1260.0, 820.0), egui::vec2(720.0, 520.0)] {
        let (ctx, mut app) = app();
        ctx.data_mut(|d| d.insert_temp(egui::Id::new("test-viewport"), size));
        app.stage = Stage::Settings;
        app.source_mode = SourceMode::Image;
        app.source = "owned/source.raw".into();
        app.destination = "owned/output".into();
        app.session = None;
        app.report = Text::default();
        for _ in 0..3 {
            frame(&ctx, &mut app, vec![]);
        }
        let mut stops = vec![];
        for _ in 0..40 {
            let tree = key(&ctx, &mut app, egui::Key::Tab, egui::Modifiers::NONE);
            let label = name(&tree, tree.focus);
            assert!(
                !label.trim().is_empty(),
                "{size:?}: {:?}",
                node(&tree, tree.focus)
            );
            stops.push(label.clone());
            if label == "Start scan" {
                break;
            }
        }
        assert_eq!(
            stops.last().map(String::as_str),
            Some("Start scan"),
            "{stops:?}"
        );
    }
}

#[test]
fn recovery_completion_moves_keyboard_focus_to_the_saved_report() {
    let (ctx, mut app) = app();
    let (sender, receiver) = mpsc::channel();
    app.messages = Some(receiver);
    app.stage = Stage::Recovering;
    sender
        .send(Message::Finished {
            results: vec![],
            status: "Saved: 1".into(),
            detail: "Saved report: owned/report.json".into(),
        })
        .unwrap();
    app.receive();
    let tree = frame(&ctx, &mut app, vec![]);
    assert_eq!(name(&tree, tree.focus), "Saved locations and reports");
    key(&ctx, &mut app, egui::Key::Space, egui::Modifiers::NONE);
    assert!(app.show_report);
}

#[test]
fn windows_ime_cancel_without_empty_preedit_keeps_committed_text_and_next_conversion() {
    let (ctx, mut app) = app();
    ctx.memory_mut(|memory| memory.request_focus(egui::Id::new("result-search")));
    frame(&ctx, &mut app, vec![]);
    // egui-winit maps Windows Preedit("", None) to Disabled. There can be
    // another Disabled after Commit; neither may erase committed text.
    for event in [
        egui::ImeEvent::Enabled,
        egui::ImeEvent::Preedit("日本語".into()),
        egui::ImeEvent::Disabled,
        egui::ImeEvent::Commit("日本語".into()),
        egui::ImeEvent::Disabled,
        egui::ImeEvent::Enabled,
        egui::ImeEvent::Preedit("とりけし".into()),
        egui::ImeEvent::Disabled,
    ] {
        frame(&ctx, &mut app, vec![egui::Event::Ime(event)]);
    }
    assert_eq!(app.filter, "日本語");
    for event in [
        egui::ImeEvent::Enabled,
        egui::ImeEvent::Preedit("げんぶん".into()),
        egui::ImeEvent::Preedit("原文".into()),
        egui::ImeEvent::Disabled,
        egui::ImeEvent::Commit("原文".into()),
        egui::ImeEvent::Disabled,
    ] {
        frame(&ctx, &mut app, vec![egui::Event::Ime(event)]);
    }
    assert_eq!(app.filter, "日本語原文");
    assert_eq!(app.selected.len(), 1);
}

#[test]
fn windows_ime_delayed_native_cancellation_batch_preserves_committed_text() {
    let (ctx, mut app) = app();
    let owner = egui::Id::new("result-search");
    ctx.memory_mut(|memory| memory.request_focus(owner));
    frame(&ctx, &mut app, vec![]);
    frame(&ctx, &mut app, vec![egui::Event::Text("既存".into())]);
    frame(
        &ctx,
        &mut app,
        vec![
            egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Ime(egui::ImeEvent::Preedit("とりけし".into())),
        ],
    );
    let escape = |pressed| egui::Event::Key {
        key: egui::Key::Escape,
        physical_key: Some(egui::Key::Escape),
        pressed,
        repeat: false,
        modifiers: egui::Modifiers::NONE,
    };
    // Actual Windows trace 42499949821: after a native prediction wait,
    // cancellation and both queued Escape releases arrive in one input batch.
    frame(
        &ctx,
        &mut app,
        vec![
            egui::Event::Ime(egui::ImeEvent::Disabled),
            egui::Event::Ime(egui::ImeEvent::Disabled),
            egui::Event::Ime(egui::ImeEvent::Commit(String::new())),
            egui::Event::Ime(egui::ImeEvent::Disabled),
            egui::Event::Ime(egui::ImeEvent::Disabled),
            escape(false),
            escape(true),
            escape(false),
        ],
    );
    assert_eq!(app.filter, "既存");
    assert_eq!(ctx.memory(|memory| memory.focused()), Some(owner));
    frame(
        &ctx,
        &mut app,
        vec![
            egui::Event::Ime(egui::ImeEvent::Enabled),
            egui::Event::Ime(egui::ImeEvent::Preedit("原文".into())),
            egui::Event::Ime(egui::ImeEvent::Disabled),
            egui::Event::Ime(egui::ImeEvent::Commit("原文".into())),
            egui::Event::Ime(egui::ImeEvent::Disabled),
        ],
    );
    assert_eq!(app.filter, "既存原文");
    key(&ctx, &mut app, egui::Key::Escape, egui::Modifiers::NONE);
    assert_eq!(ctx.memory(|memory| memory.focused()), None);
    assert_eq!(app.filter, "既存原文");
}

#[test]
fn windows_ime_escape_cancellation_keeps_the_owner_until_preedit_is_removed() {
    for ordering in 0..3 {
        let (ctx, mut app) = app();
        let owner = egui::Id::new("result-search");
        let escape = |pressed| egui::Event::Key {
            key: egui::Key::Escape,
            physical_key: None,
            pressed,
            repeat: false,
            modifiers: egui::Modifiers::NONE,
        };
        ctx.memory_mut(|memory| memory.request_focus(owner));
        frame(&ctx, &mut app, vec![]);
        frame(&ctx, &mut app, vec![egui::Event::Text("既存".into())]);
        for event in [
            egui::ImeEvent::Enabled,
            egui::ImeEvent::Preedit("とりけし".into()),
        ] {
            frame(&ctx, &mut app, vec![egui::Event::Ime(event)]);
        }
        assert_eq!(app.filter, "既存とりけし");
        let disabled = egui::Event::Ime(egui::ImeEvent::Disabled);
        match ordering {
            0 => {
                frame(&ctx, &mut app, vec![escape(true), disabled]);
            }
            1 => {
                frame(&ctx, &mut app, vec![disabled, escape(true)]);
            }
            _ => {
                frame(&ctx, &mut app, vec![escape(true)]);
                assert_eq!(ctx.memory(|memory| memory.focused()), Some(owner));
                frame(&ctx, &mut app, vec![disabled]);
            }
        }
        frame(&ctx, &mut app, vec![escape(false)]);
        assert_eq!(app.filter, "既存", "cancel ordering {ordering}");
        assert_eq!(ctx.memory(|memory| memory.focused()), Some(owner));
        assert_eq!(app.selected.len(), 1);
        for event in [
            egui::ImeEvent::Enabled,
            egui::ImeEvent::Preedit("原文".into()),
            egui::ImeEvent::Disabled,
            egui::ImeEvent::Commit("原文".into()),
            egui::ImeEvent::Disabled,
        ] {
            frame(&ctx, &mut app, vec![egui::Event::Ime(event)]);
        }
        assert_eq!(app.filter, "既存原文");
        key(&ctx, &mut app, egui::Key::Escape, egui::Modifiers::NONE);
        assert_eq!(ctx.memory(|memory| memory.focused()), None);
        assert_eq!(app.filter, "既存原文");
    }
}
