use super::*;

use egui::text_edit::TextEditState;

const FIRST_SECRET: &str = "3132333435363738393031323334353637383930";
const SECOND_SECRET: &str = "3132333435363738393031323334353637383931";

#[derive(Clone, Copy, Debug)]
enum ImportField {
    Name,
    Description,
    Secret,
    Serial,
    Restore,
}

impl ImportField {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "名稱（必填）",
            Self::Description => "Email／備註（選填）",
            Self::Secret => "長密鑰（40／74位十六進位）",
            Self::Serial => "序號（選填；74位資料會自動帶入）",
            Self::Restore => "還原碼（選填）",
        }
    }

    fn value(self, ui: &GroupUi) -> &str {
        let form = ui.import();
        match self {
            Self::Name => &form.name,
            Self::Description => &form.description,
            Self::Secret => &form.secret,
            Self::Serial => &form.serial,
            Self::Restore => &form.restore,
        }
    }
}

fn open_fresh_import(ui: &mut GroupUi) {
    // Exercise the production import entry point, including its session boundary.
    ui.app.edit(&ui.ctx, None);
    for _ in 0..4 {
        ui.frame(Vec::new());
    }
    assert!(ui.import().name.is_empty());
    assert!(ui.import().secret.is_empty());
}

fn import_ui() -> GroupUi {
    let mut ui = GroupUi::new();
    ui.use_memory_vault(bnauth_core::VaultSnapshot::new_empty(IMPORT_TEST_TIME));
    open_fresh_import(&mut ui);
    ui
}

fn focus_import_field(ui: &mut GroupUi, field: ImportField) -> egui::Id {
    // The editor is the next row after its painted label. This uses pointer input
    // just as a user would; it does not install a synthetic TextEditState.
    let point = ui.point(field.label()) + Vec2::new(0.0, 24.0);
    ui.click(point);
    let id = ui
        .ctx
        .memory(|memory| memory.focused())
        .unwrap_or_else(|| panic!("{field:?} editor was not focused"));
    assert!(TextEditState::load(&ui.ctx, id).is_some());
    // Keep subsequent undo assertions about text edits rather than a click's
    // cursor-only change; each completed edit below leaves the cursor at End.
    ui.key(egui::Key::End);
    id
}

fn key_event(key: egui::Key, modifiers: egui::Modifiers) -> egui::Event {
    egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: true,
        repeat: false,
        modifiers,
    }
}

fn command(ui: &mut GroupUi, key: egui::Key) {
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    ui.frame(vec![key_event(key, modifiers)]);
    ui.frame(vec![egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed: false,
        repeat: false,
        modifiers,
    }]);
}

fn settle_undo(ui: &mut GroupUi) {
    // Advance egui's logical clock past its one-second stable undo interval.
    // No wall-clock sleeps or fabricated undo records are needed.
    ui.frame(Vec::new());
    ui.time += 1.1;
    ui.frame(Vec::new());
}

fn replace_field(ui: &mut GroupUi, field: ImportField, value: &str) -> egui::Id {
    let id = focus_import_field(ui, field);
    command(ui, egui::Key::A);
    ui.type_text(value);
    assert_eq!(field.value(ui), value, "wrong editor for {field:?}");
    settle_undo(ui);
    id
}

fn cursor_indices(ui: &GroupUi, id: egui::Id) -> (usize, usize) {
    let range = TextEditState::load(&ui.ctx, id)
        .expect("Expected stored editor state")
        .cursor
        .char_range()
        .expect("Expected a stored selection");
    (range.primary.index.0, range.secondary.index.0)
}

fn prepare_undoable_draft(ui: &mut GroupUi) -> (egui::Id, egui::Id) {
    let name = replace_field(ui, ImportField::Name, "Original draft name");
    replace_field(ui, ImportField::Name, "Changed draft name");
    let secret = replace_field(ui, ImportField::Secret, FIRST_SECRET);
    replace_field(ui, ImportField::Secret, SECOND_SECRET);
    (name, secret)
}

fn assert_draft_undo_still_works(ui: &mut GroupUi, ids: (egui::Id, egui::Id)) {
    assert_eq!(ui.import().name, "Changed draft name");
    assert_eq!(ui.import().secret, SECOND_SECRET);
    assert_eq!(focus_import_field(ui, ImportField::Name), ids.0);
    command(ui, egui::Key::Z);
    assert_eq!(ui.import().name, "Original draft name");
    assert_eq!(focus_import_field(ui, ImportField::Secret), ids.1);
    command(ui, egui::Key::Z);
    assert_eq!(ui.import().secret, FIRST_SECRET);
}

#[test]
fn fresh_import_after_actual_close_cannot_undo_canceled_name_or_secret() {
    let mut ui = import_ui();
    prepare_undoable_draft(&mut ui);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());

    open_fresh_import(&mut ui);
    for field in [ImportField::Name, ImportField::Secret] {
        focus_import_field(&mut ui, field);
        // Repeated undo/redo must also remain within the new empty session.
        for key in [egui::Key::Z, egui::Key::Z, egui::Key::Y] {
            command(&mut ui, key);
            assert!(
                field.value(&ui).is_empty(),
                "{field:?} restored canceled draft data after {key:?}"
            );
        }
    }
    assert!(
        ui.app
            .runtime
            .app
            .current
            .as_ref()
            .unwrap()
            .authenticators
            .is_empty()
    );
    assert!(!ui.app.runtime.app.saving);
}

#[test]
fn same_import_draft_keeps_name_and_secret_undo_after_preview_and_back() {
    let mut ui = import_ui();
    let ids = prepare_undoable_draft(&mut ui);
    ui.click(ui.point("預覽匯入"));
    assert!(ui.import().preview.is_some());
    ui.click(ui.point("返回修改"));
    assert!(ui.import().preview.is_none());
    assert_draft_undo_still_works(&mut ui, ids);
    assert!(!ui.app.runtime.app.saving);
    assert!(
        ui.app
            .runtime
            .app
            .current
            .as_ref()
            .unwrap()
            .authenticators
            .is_empty()
    );
}

#[test]
fn repeated_import_sessions_reuse_editor_ids_and_release_stored_states() {
    let mut ui = import_ui();
    let fields = [
        ImportField::Name,
        ImportField::Description,
        ImportField::Secret,
        ImportField::Serial,
        ImportField::Restore,
    ];
    let expected_ids = fields.map(|field| focus_import_field(&mut ui, field));

    for session in 0..20 {
        let ids = fields.map(|field| focus_import_field(&mut ui, field));
        assert_eq!(ids, expected_ids, "editor IDs grew with session {session}");
        assert_eq!(
            ui.ctx.data(|data| data.count::<TextEditState>()),
            fields.len(),
            "stored editor states grew with session {session}"
        );
        replace_field(&mut ui, ImportField::Name, &format!("Canceled {session}"));
        replace_field(&mut ui, ImportField::Secret, FIRST_SECRET);
        ui.click(ui.point("×"));
        assert!(ui.app.dialog.is_none());
        assert_eq!(
            ui.ctx.data(|data| data.count::<TextEditState>()),
            0,
            "closed session {session} retained editor text or undo state"
        );
        open_fresh_import(&mut ui);
    }
}

#[test]
fn formatted_serial_keeps_paste_and_edit_undo_through_preview_and_back() {
    let mut ui = import_ui();
    replace_field(&mut ui, ImportField::Name, "Serial undo draft");
    replace_field(&mut ui, ImportField::Secret, FIRST_SECRET);
    let id = focus_import_field(&mut ui, ImportField::Serial);
    ui.frame(vec![egui::Event::Paste("us140403118994".into())]);
    assert_eq!(ui.import().serial, "US-1404-0311-8994");

    command(&mut ui, egui::Key::Z);
    assert!(
        ui.import().serial.is_empty(),
        "one undo must remove the paste"
    );
    command(&mut ui, egui::Key::Y);
    assert_eq!(ui.import().serial, "US-1404-0311-8994");
    assert_eq!(cursor_indices(&ui, id), (17, 17));
    settle_undo(&mut ui);

    ui.key(egui::Key::Backspace);
    ui.type_text("5");
    settle_undo(&mut ui);
    assert_eq!(ui.import().serial, "US-1404-0311-8995");
    ui.click(ui.point("預覽匯入"));
    assert!(ui.import().preview.is_some());
    ui.click(ui.point("返回修改"));
    assert!(ui.import().preview.is_none());
    assert_eq!(focus_import_field(&mut ui, ImportField::Serial), id);
    command(&mut ui, egui::Key::Z);
    assert_eq!(ui.import().serial, "US-1404-0311-8994");
    command(&mut ui, egui::Key::Y);
    assert_eq!(ui.import().serial, "US-1404-0311-8995");
    assert_eq!(cursor_indices(&ui, id), (17, 17));
}
