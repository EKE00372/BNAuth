use super::*;

#[derive(Clone, Copy, Debug)]
enum Target {
    Reorder,
    Group,
}

fn drag_ui() -> (GroupUi, Arc<AtomicUsize>) {
    drag_ui_with_full_group(false)
}

fn drag_ui_with_full_group(fill_other: bool) -> (GroupUi, Arc<AtomicUsize>) {
    let mut ui = GroupUi::new();
    let mut vault = bnauth_core::VaultSnapshot::new_empty();
    for (sort_order, name) in [(1, "Drag Target"), (2, "Other")] {
        vault.groups.push(bnauth_core::Group {
            id: uuid::Uuid::new_v4(),
            name: name.into(),
            sort_order,
            is_ungrouped: false,
        });
    }
    for (group, name, secret) in [
        (0, "Drag First", "1".repeat(40)),
        (2, "Other record", "3".repeat(40)),
        (0, "Drag Second", "2".repeat(40)),
    ] {
        vault = bnauth_core::prepare_import(
            &vault,
            name,
            "Complete note 🧪",
            vault.groups[group].id,
            bnauth_core::parse_manual(&secret, "", "").unwrap(),
        )
        .unwrap()
        .candidate(&vault)
        .unwrap();
    }
    if fill_other {
        for order in 1..crate::model::MAX_GROUP_ENTRIES {
            let mut record = vault.authenticators[1].clone();
            record.id = uuid::Uuid::new_v4();
            record.sort_order = order as u32;
            vault.authenticators.push(record);
        }
    }
    vault.validate().unwrap();
    ui.use_memory_vault(vault.clone());
    let commits = Arc::new(AtomicUsize::new(0));
    let worker_commits = commits.clone();
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(CountingRepository(worker_commits.clone())),
            Some(vault.clone()),
        ))
    }));
    drain_memory_app(&mut ui.app.runtime.app);
    ui.app.dialog = None;
    for _ in 0..6 {
        frame(&mut ui, Vec::new());
    }
    assert_eq!(commits.load(Ordering::Relaxed), 0);
    (ui, commits)
}

fn pointer_button(
    pos: Pos2,
    button: egui::PointerButton,
    pressed: bool,
    modifiers: egui::Modifiers,
) -> egui::Event {
    egui::Event::PointerButton {
        pos,
        button,
        pressed,
        modifiers,
    }
}

fn begin_drag(ui: &mut GroupUi, button: egui::PointerButton, modifiers: egui::Modifiers) {
    let source = ui.point("Drag First");
    frame(ui, vec![egui::Event::PointerMoved(source)]);
    frame(ui, vec![pointer_button(source, button, true, modifiers)]);
    frame(
        ui,
        vec![egui::Event::PointerMoved(source + Vec2::new(20.0, 0.0))],
    );
}

fn over_target(ui: &mut GroupUi, target: Target) -> Pos2 {
    let point = match target {
        Target::Reorder => ui.point("Drag Second") + Vec2::new(0.0, 25.0),
        Target::Group => ui.point("Drag Target"),
    };
    for _ in 0..2 {
        frame(ui, vec![egui::Event::PointerMoved(point)]);
    }
    point
}

fn assert_saved_once(
    ui: &mut GroupUi,
    commits: &AtomicUsize,
    before: &bnauth_core::VaultSnapshot,
    target: Target,
) {
    // Let the real runtime receive the worker result and apply the saved model.
    wait_runtime(ui);
    for _ in 0..4 {
        frame(ui, Vec::new());
    }
    let saved = ui.app.runtime.app.current.as_deref().unwrap();
    let mut expected = before.clone();
    expected.revision += 1;
    expected.authenticators[2].sort_order = 0;
    let source_names: &[&str] = match target {
        Target::Reorder => {
            expected.authenticators[0].sort_order = 1;
            &["Drag Second", "Drag First"]
        }
        Target::Group => {
            expected.authenticators[0].group_id = before.groups[1].id;
            &["Drag Second"]
        }
    };
    assert_eq!(saved, &expected, "{target:?}: wrong persisted snapshot");
    assert!(std::ptr::eq(&*saved.groups, &*before.groups));
    assert!(std::ptr::eq(
        &*saved.authenticators[1],
        &*before.authenticators[1],
    ));
    let reopened = crate::model::VaultModel::from_snapshot(saved);
    for model in [&ui.app.model, &reopened] {
        let names: Vec<_> = model
            .filtered(model.groups[0].id, "", false)
            .iter()
            .map(|id| model.entry(*id).unwrap().record.name.as_str())
            .collect();
        assert_eq!(names, source_names);
    }
    assert_eq!(commits.load(Ordering::Relaxed), 1, "{target:?}");
    assert!(!ui.app.runtime.app.has_pending());
    assert!(!ui.app.runtime.app.saving);
    assert!(ui.app.dragging.is_none());
    assert!(ui.app.drag_origin.is_none());
}

#[test]
fn secondary_drag_never_reorders_moves_or_saves() {
    for target in [Target::Reorder, Target::Group] {
        let (mut ui, commits) = drag_ui();
        let before = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let token = ui.app.model.change_token();
        begin_drag(
            &mut ui,
            egui::PointerButton::Secondary,
            egui::Modifiers::NONE,
        );
        let point = over_target(&mut ui, target);
        assert!(ui.app.dragging.is_none(), "{target:?}");
        frame(
            &mut ui,
            vec![pointer_button(
                point,
                egui::PointerButton::Secondary,
                false,
                egui::Modifiers::NONE,
            )],
        );
        assert!(ui.app.dragging.is_none());
        assert!(ui.app.drag_origin.is_none());
        assert_no_writes(&mut ui, &commits, &before, token);
    }
}

#[test]
fn primary_held_on_blank_does_not_authorize_secondary_drag_and_next_primary_drag_saves_once() {
    for target in [Target::Reorder, Target::Group] {
        let (mut ui, commits) = drag_ui();
        let before = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let token = ui.app.model.change_token();
        // Start on the unused footer gap, outside row, scrollbar and resize
        // hit regions. The primary press therefore owns no data row.
        let footer = ui.point("總共 3 個，本組 2 個");
        let blank = Pos2::new(250.0, footer.y);
        frame(&mut ui, vec![egui::Event::PointerMoved(blank)]);
        frame(
            &mut ui,
            vec![pointer_button(
                blank,
                egui::PointerButton::Primary,
                true,
                egui::Modifiers::NONE,
            )],
        );
        assert!(ui.app.dragging.is_none());
        begin_drag(
            &mut ui,
            egui::PointerButton::Secondary,
            egui::Modifiers::NONE,
        );
        let point = over_target(&mut ui, target);
        assert!(
            ui.ctx
                .input(|input| input.pointer.button_down(egui::PointerButton::Primary))
        );
        assert!(ui.app.dragging.is_none(), "{target:?}: wrong press origin");
        for button in [egui::PointerButton::Secondary, egui::PointerButton::Primary] {
            frame(
                &mut ui,
                vec![pointer_button(point, button, false, egui::Modifiers::NONE)],
            );
            assert!(ui.app.dragging.is_none());
            assert!(ui.app.drag_origin.is_none());
        }
        assert_no_writes(&mut ui, &commits, &before, token);

        // A fresh primary press on the row must establish a new source after
        // the rejected gesture, without retaining or blocking its old state.
        begin_drag(&mut ui, egui::PointerButton::Primary, egui::Modifiers::NONE);
        let point = over_target(&mut ui, target);
        frame(
            &mut ui,
            vec![pointer_button(
                point,
                egui::PointerButton::Primary,
                false,
                egui::Modifiers::NONE,
            )],
        );
        assert_saved_once(&mut ui, &commits, &before, target);
    }
}

#[test]
fn secondary_release_during_primary_drag_waits_for_primary_and_saves_once() {
    for target in [Target::Reorder, Target::Group] {
        let (mut ui, commits) = drag_ui();
        let before = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let token = ui.app.model.change_token();
        let first = ui.app.model.entries[0].id;
        begin_drag(&mut ui, egui::PointerButton::Primary, egui::Modifiers::NONE);
        // Tap the secondary button over a row, then continue the left drag.
        // A tab's deliberate right-click context menu is a separate interaction.
        let point = over_target(&mut ui, Target::Reorder);
        assert_eq!(ui.app.dragged_entry().map(|entry| entry.id), Some(first));
        for pressed in [true, false] {
            frame(
                &mut ui,
                vec![pointer_button(
                    point,
                    egui::PointerButton::Secondary,
                    pressed,
                    egui::Modifiers::NONE,
                )],
            );
        }
        assert!(
            ui.ctx
                .input(|input| input.pointer.button_down(egui::PointerButton::Primary))
        );
        assert_eq!(
            ui.app.dragged_entry().map(|entry| entry.id),
            Some(first),
            "{target:?}: premature drop"
        );
        assert_no_writes(&mut ui, &commits, &before, token);
        let point = over_target(&mut ui, target);
        frame(
            &mut ui,
            vec![pointer_button(
                point,
                egui::PointerButton::Primary,
                false,
                egui::Modifiers::NONE,
            )],
        );
        assert_saved_once(&mut ui, &commits, &before, target);
    }
}

#[test]
fn primary_drag_reorder_and_group_drop_save_once() {
    for target in [Target::Reorder, Target::Group] {
        let (mut ui, commits) = drag_ui_with_full_group(matches!(target, Target::Group));
        let before = ui.app.runtime.app.current.as_deref().unwrap().clone();
        if matches!(target, Target::Group) {
            let token = ui.app.model.change_token();
            begin_drag(&mut ui, egui::PointerButton::Primary, egui::Modifiers::NONE);
            let full = ui.point("Other");
            let cursor = frame(&mut ui, vec![egui::Event::PointerMoved(full)]);
            assert_eq!(cursor, egui::CursorIcon::NoDrop);
            assert!(ui.app.drop.is_none());
            frame(
                &mut ui,
                vec![pointer_button(
                    full,
                    egui::PointerButton::Primary,
                    false,
                    egui::Modifiers::NONE,
                )],
            );
            assert_no_writes(&mut ui, &commits, &before, token);
        }
        begin_drag(&mut ui, egui::PointerButton::Primary, egui::Modifiers::NONE);
        let point = over_target(&mut ui, target);
        frame(
            &mut ui,
            vec![pointer_button(
                point,
                egui::PointerButton::Primary,
                false,
                egui::Modifiers::NONE,
            )],
        );
        assert_saved_once(&mut ui, &commits, &before, target);
    }
}

#[test]
fn ctrl_click_toggles_selection_and_ctrl_drag_never_saves() {
    let (mut ui, commits) = drag_ui();
    let before = ui.app.runtime.app.current.as_deref().unwrap().clone();
    let token = ui.app.model.change_token();
    let rows = ui.app.model.filtered(ui.app.group, "", false);
    let [first, second] = rows[..] else {
        panic!("expected both drag rows in the source group");
    };
    let ctrl = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    frame(&mut ui, vec![egui::Event::ModifiersChanged(ctrl)]);
    // Toggle first on, second on, then first off and back on with real Ctrl-clicks.
    for (label, selected) in [
        ("Drag First", BTreeSet::from([first])),
        ("Drag Second", BTreeSet::from([first, second])),
        ("Drag First", BTreeSet::from([second])),
        ("Drag First", BTreeSet::from([first, second])),
    ] {
        let point = ui.point(label);
        for pressed in [true, false] {
            frame(
                &mut ui,
                vec![
                    egui::Event::PointerMoved(point),
                    pointer_button(point, egui::PointerButton::Primary, pressed, ctrl),
                ],
            );
        }
        assert_eq!(*ui.app.selected, selected);
    }
    begin_drag(&mut ui, egui::PointerButton::Primary, ctrl);
    let point = over_target(&mut ui, Target::Reorder);
    assert!(ui.app.dragging.is_none());
    frame(
        &mut ui,
        vec![pointer_button(
            point,
            egui::PointerButton::Primary,
            false,
            ctrl,
        )],
    );
    frame(
        &mut ui,
        vec![egui::Event::ModifiersChanged(egui::Modifiers::NONE)],
    );
    assert_eq!(*ui.app.selected, BTreeSet::from([first, second]));
    assert!(ui.app.drag_origin.is_none());
    assert_no_writes(&mut ui, &commits, &before, token);
}

#[test]
fn escape_cancels_primary_drag_and_release_never_saves() {
    for target in [Target::Reorder, Target::Group] {
        let (mut ui, commits) = drag_ui();
        let before = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let token = ui.app.model.change_token();
        begin_drag(&mut ui, egui::PointerButton::Primary, egui::Modifiers::NONE);
        let point = over_target(&mut ui, target);
        assert!(ui.app.dragging.is_some());
        edit_key(&mut ui, egui::Key::Escape, false);
        assert!(ui.app.dragging.is_none());
        assert!(ui.app.drag_origin.is_none());
        assert!(ui.app.drop.is_none());
        frame(
            &mut ui,
            vec![pointer_button(
                point,
                egui::PointerButton::Primary,
                false,
                egui::Modifiers::NONE,
            )],
        );
        assert_no_writes(&mut ui, &commits, &before, token);
    }
}
