use super::*;

#[path = "drag_ui_tests.rs"]
mod drag_ui_tests;

use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

const WINDOW_SIZE: Vec2 = Vec2::new(400.0, 892.0);
const INPUT_CHAR_LIMIT: usize = 256;

struct CountingRepository(Arc<AtomicUsize>);

impl bnauth_app::Repository for CountingRepository {
    fn commit(
        &mut self,
        candidate: &bnauth_core::VaultSnapshot,
    ) -> Result<bnauth_app::CommitOutcome, String> {
        self.0.fetch_add(1, Ordering::Relaxed);
        candidate.validate()?;
        let mut snapshot = candidate.clone();
        snapshot.revision += 1;
        Ok(bnauth_app::CommitOutcome {
            snapshot,
            warning: None,
        })
    }

    fn retry_cleanup(&mut self) -> Result<(), String> {
        Ok(())
    }
}

fn ready_ui(name: &str, description: &str) -> (GroupUi, Arc<AtomicUsize>) {
    let mut ui = GroupUi::new();
    let mut vault = bnauth_core::VaultSnapshot::new_empty(IMPORT_TEST_TIME);
    let material =
        bnauth_core::parse_manual("1111111111111111111111111111111111111111", "", "").unwrap();
    vault = bnauth_core::prepare_import(
        &vault,
        name,
        description,
        vault.ungrouped_id().unwrap(),
        material,
        IMPORT_TEST_TIME,
    )
    .unwrap()
    .candidate;
    ui.use_memory_vault(vault.clone());
    let commits = Arc::new(AtomicUsize::new(0));
    let repository_commits = commits.clone();
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(CountingRepository(repository_commits.clone())),
            Some(vault.clone()),
        ))
    }));
    drain_memory_app(&mut ui.app.runtime.app);
    ui.app.dialog = None;
    for _ in 0..4 {
        frame(&mut ui, Vec::new());
    }
    (ui, commits)
}

// Exercise the full production App::logic -> App::ui route, including search,
// dialogs and save detection. All persistence is through CountingRepository.
fn frame(ui: &mut GroupUi, events: Vec<egui::Event>) {
    ui.time += 0.04;
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, WINDOW_SIZE)),
        time: Some(ui.time),
        events,
        ..Default::default()
    };
    let viewport = input.viewports.get_mut(&egui::ViewportId::ROOT).unwrap();
    viewport.native_pixels_per_point = Some(1.0);
    viewport.maximized = Some(false);
    viewport.minimized = Some(false);
    viewport.occluded = Some(false);
    viewport.outer_rect = Some(Rect::from_min_size(Pos2::new(100.0, 60.0), WINDOW_SIZE));
    viewport.inner_rect = viewport.outer_rect;
    let ctx = ui.ctx.clone();
    let mut app_frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(input, |root| {
        eframe::App::logic(&mut ui.app, root.ctx(), &mut app_frame);
        eframe::App::ui(&mut ui.app, root, &mut app_frame);
    });
    output.textures_delta.clear();
    ui.shapes = output.shapes;
}

fn click(ui: &mut GroupUi, point: Pos2) {
    for pressed in [true, false] {
        frame(
            ui,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        );
    }
    frame(ui, Vec::new());
}

fn click_label(ui: &mut GroupUi, label: &str) {
    let point = ui.point(label);
    click(ui, point);
}

fn command(ui: &mut GroupUi, key: egui::Key) {
    let modifiers = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    for pressed in [true, false] {
        frame(
            ui,
            vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers,
            }],
        );
    }
}

fn settle_undo(ui: &mut GroupUi) {
    frame(ui, Vec::new());
    ui.time += 1.1;
    frame(ui, Vec::new());
}

fn open_edit(ui: &mut GroupUi) {
    let id = ui.app.model.entries[0].id;
    ui.app.edit(&ui.ctx, Some(id));
    for _ in 0..4 {
        frame(ui, Vec::new());
    }
}

fn assert_no_writes(
    ui: &mut GroupUi,
    commits: &AtomicUsize,
    original: &bnauth_core::VaultSnapshot,
    token: u64,
) {
    // Give the real save detector and repository worker a chance to run before
    // counting, so an accidental asynchronous commit cannot escape the test.
    frame(ui, Vec::new());
    drain_memory_app(&mut ui.app.runtime.app);
    assert_eq!(commits.load(Ordering::Relaxed), 0);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(original));
    assert_eq!(ui.app.model.snapshot(), *original);
    assert_eq!(ui.app.model.change_token(), token);
    assert!(!ui.app.runtime.app.saving);
    assert!(!ui.app.runtime.app.has_pending());
}

fn megabyte_json() -> String {
    // No line breaks: egui's single-line filtering must not be the reason this
    // approximately 1 MiB clipboard payload becomes small. Multi-byte scalars,
    // combining marks and emoji are deliberately interleaved across the limit.
    let row = r#"{"event":"繁體中文🧪👨‍👩‍👧‍👦 é","message":"clipboard log entry"}"#;
    let payload = row.repeat((1024_usize * 1024).div_ceil(row.len()));
    assert!(payload.len() >= 1024 * 1024);
    payload
}

fn assert_layout_is_bounded(ui: &GroupUi, expected: &str, char_limit: usize) -> bool {
    fn visit(shape: &egui::Shape, expected: &str, char_limit: usize, found: &mut bool) {
        match shape {
            egui::Shape::Text(text) => {
                let source = text.galley.text();
                assert!(
                    source.chars().count() <= char_limit,
                    "a text galley laid out an over-limit clipboard payload"
                );
                *found |= source == expected;
            }
            egui::Shape::Vec(shapes) => {
                for shape in shapes {
                    visit(shape, expected, char_limit, found);
                }
            }
            _ => {}
        }
    }
    let mut found = false;
    for shape in &ui.shapes {
        visit(&shape.shape, expected, char_limit, &mut found);
    }
    found
}

#[test]
fn megabyte_unicode_search_paste_is_bounded_before_layout_and_never_writes() {
    let (mut ui, commits) = ready_ui("原始名稱", "原始備註");
    let original = ui.app.runtime.app.current.clone().unwrap();
    let token = ui.app.model.change_token();
    click_label(&mut ui, "全選");
    assert_eq!(ui.app.selected.len(), 1);
    click_label(&mut ui, "搜尋所有分頁的名稱");
    let payload = megabyte_json();
    let expected: String = payload.chars().take(INPUT_CHAR_LIMIT).collect();
    frame(&mut ui, vec![egui::Event::Paste(payload)]);
    assert_eq!(ui.app.query, expected);
    assert!(ui.app.selected.is_empty());
    // A newly focused search may not paint its text in the paste frame. Every
    // galley emitted by that frame must already be bounded; its next ordinary
    // frame must paint the exact bounded query without any synthetic state.
    assert_layout_is_bounded(&ui, &expected, INPUT_CHAR_LIMIT);
    frame(&mut ui, Vec::new());
    assert!(
        assert_layout_is_bounded(&ui, &expected, INPUT_CHAR_LIMIT),
        "the search did not lay out the bounded query on its next frame"
    );
    assert_no_writes(&mut ui, &commits, &original, token);

    click_label(&mut ui, "×");
    assert!(ui.app.query.is_empty());
    click_label(&mut ui, "全選");
    assert_eq!(ui.app.selected.len(), 1);
    click_label(&mut ui, "取消");
    assert!(ui.app.selected.is_empty());
    assert_no_writes(&mut ui, &commits, &original, token);
}

#[derive(Clone, Copy, Debug)]
enum EditField {
    Name,
    Description,
}

impl EditField {
    fn label(self) -> &'static str {
        match self {
            Self::Name => "名稱（必填）",
            Self::Description => "Email／備註（選填）",
        }
    }

    fn limit(self) -> usize {
        INPUT_CHAR_LIMIT
    }

    fn value(self, ui: &GroupUi) -> &str {
        match ui.app.dialog.as_ref() {
            Some(Dialog::Edit {
                name, description, ..
            }) => match self {
                Self::Name => name,
                Self::Description => description,
            },
            _ => panic!("Expected the edit draft for {self:?}"),
        }
    }
}

fn assert_edit_paste_bound_undo_and_close(field: EditField) {
    let (mut ui, commits) = ready_ui("原始名稱🧪", "原始備註 é");
    let original = ui.app.runtime.app.current.clone().unwrap();
    let token = ui.app.model.change_token();
    open_edit(&mut ui);
    let point = ui.point(field.label()) + Vec2::new(0.0, 24.0);
    click(&mut ui, point);
    assert!(ui.ctx.memory(|memory| memory.focused()).is_some());
    let before = field.value(&ui).to_owned();
    settle_undo(&mut ui);
    command(&mut ui, egui::Key::A);
    let payload = megabyte_json();
    let expected: String = payload.chars().take(field.limit()).collect();
    frame(&mut ui, vec![egui::Event::Paste(payload)]);
    assert_eq!(field.value(&ui), expected, "{field:?}");
    assert!(
        assert_layout_is_bounded(&ui, &expected, field.limit()),
        "the bounded edit value was not laid out in the paste frame"
    );

    settle_undo(&mut ui);
    command(&mut ui, egui::Key::Z);
    assert_eq!(field.value(&ui), before, "{field:?} undo");
    command(&mut ui, egui::Key::Y);
    assert_eq!(field.value(&ui), expected, "{field:?} redo");

    assert_eq!(expected.chars().count(), INPUT_CHAR_LIMIT);
    assert_no_writes(&mut ui, &commits, &original, token);
    click_label(&mut ui, "×");
    assert!(ui.app.dialog.is_none());
    assert_no_writes(&mut ui, &commits, &original, token);
}

#[test]
fn megabyte_unicode_name_paste_is_bounded_and_keeps_draft_undo_redo() {
    assert_edit_paste_bound_undo_and_close(EditField::Name);
}

#[test]
fn megabyte_unicode_note_paste_is_bounded_and_keeps_draft_undo_redo() {
    assert_edit_paste_bound_undo_and_close(EditField::Description);
}

fn assert_import_paste_bound_undo_and_close(field: EditField) {
    let (mut ui, commits) = ready_ui("原始名稱", "原始備註");
    let original = ui.app.runtime.app.current.clone().unwrap();
    let token = ui.app.model.change_token();
    ui.app.edit(&ui.ctx, None);
    for _ in 0..4 {
        frame(&mut ui, Vec::new());
    }
    let value = |ui: &GroupUi| match field {
        EditField::Name => ui.import().name.clone(),
        EditField::Description => ui.import().description.clone(),
    };
    let point = ui.point(field.label()) + Vec2::new(0.0, 24.0);
    click(&mut ui, point);
    settle_undo(&mut ui);
    let payload = megabyte_json();
    let expected: String = payload.chars().take(INPUT_CHAR_LIMIT).collect();
    frame(&mut ui, vec![egui::Event::Paste(payload)]);
    assert_eq!(value(&ui), expected, "import {field:?}");
    assert!(assert_layout_is_bounded(&ui, &expected, INPUT_CHAR_LIMIT));
    settle_undo(&mut ui);
    command(&mut ui, egui::Key::Z);
    assert!(value(&ui).is_empty(), "import {field:?} undo");
    command(&mut ui, egui::Key::Y);
    assert_eq!(value(&ui), expected, "import {field:?} redo");
    click_label(&mut ui, "×");
    assert!(ui.app.dialog.is_none());
    assert_no_writes(&mut ui, &commits, &original, token);
}

#[test]
fn megabyte_unicode_import_name_paste_is_bounded_and_keeps_draft_undo_redo() {
    assert_import_paste_bound_undo_and_close(EditField::Name);
}

#[test]
fn megabyte_unicode_import_note_paste_is_bounded_and_keeps_draft_undo_redo() {
    assert_import_paste_bound_undo_and_close(EditField::Description);
}

fn text_at_byte_limit(block: &str, limit: usize) -> String {
    let mut text = block.repeat(limit / block.len());
    text.push_str(&"x".repeat(limit - text.len()));
    assert_eq!(text.len(), limit);
    text
}

#[test]
fn byte_limit_existing_names_and_notes_survive_open_and_unchanged_apply_without_writes() {
    for (name, note) in [
        (
            "A".repeat(bnauth_core::MAX_NAME_BYTES),
            "N".repeat(bnauth_core::MAX_DESCRIPTION_BYTES),
        ),
        (
            text_at_byte_limit("中文🧪é", bnauth_core::MAX_NAME_BYTES),
            text_at_byte_limit("備註🧪 é 👨‍👩‍👧‍👦 ", bnauth_core::MAX_DESCRIPTION_BYTES),
        ),
    ] {
        let (mut ui, commits) = ready_ui(&name, &note);
        let original = ui.app.runtime.app.current.clone().unwrap();
        let token = ui.app.model.change_token();
        open_edit(&mut ui);
        assert_eq!(EditField::Name.value(&ui), name);
        assert_eq!(EditField::Description.value(&ui), note);
        for expected in [&name, &note] {
            assert!(
                assert_layout_is_bounded(&ui, expected, bnauth_core::MAX_DESCRIPTION_BYTES),
                "opening an existing long field must lay out its complete stored value"
            );
        }
        for field in [EditField::Name, EditField::Description] {
            let before = field.value(&ui).to_owned();
            let point = ui.point(field.label()) + Vec2::new(0.0, 24.0);
            click(&mut ui, point);
            settle_undo(&mut ui);
            command(&mut ui, egui::Key::A);
            frame(&mut ui, vec![egui::Event::Paste("暫時修改🧪".into())]);
            assert_eq!(field.value(&ui), "暫時修改🧪");
            settle_undo(&mut ui);
            command(&mut ui, egui::Key::Z);
            assert_eq!(field.value(&ui), before, "{field:?} long-value undo");
            command(&mut ui, egui::Key::Y);
            assert_eq!(field.value(&ui), "暫時修改🧪", "{field:?} redo");
            command(&mut ui, egui::Key::Z);
            assert_eq!(field.value(&ui), before, "{field:?} second undo");
        }
        click_label(&mut ui, "套用");
        assert!(ui.app.dialog.is_none());
        assert_no_writes(&mut ui, &commits, &original, token);
    }
}

#[test]
fn search_256_scalar_boundary_preserves_unicode_and_lowercase_expansion() {
    for name in [
        "A".repeat(INPUT_CHAR_LIMIT),
        "中文🧪é".chars().cycle().take(INPUT_CHAR_LIMIT).collect(),
        "İ".repeat(INPUT_CHAR_LIMIT / 2),
    ] {
        let (mut ui, commits) = ready_ui(&name, "原始備註");
        let original = ui.app.runtime.app.current.clone().unwrap();
        let token = ui.app.model.change_token();
        click_label(&mut ui, "搜尋所有分頁的名稱");
        // U+0130 lowercases to two scalars (i + combining dot). The 256-scalar
        // search boundary applies to the entered query after that expansion.
        let query = name.to_lowercase();
        assert_eq!(query.chars().count(), INPUT_CHAR_LIMIT);
        frame(&mut ui, vec![egui::Event::Paste(query.clone())]);
        assert_eq!(ui.app.query, query);
        assert_eq!(ui.app.visible_ids(), vec![ui.app.model.entries[0].id]);
        assert_no_writes(&mut ui, &commits, &original, token);
    }
}

fn edit_key(ui: &mut GroupUi, key: egui::Key, shift: bool) {
    for pressed in [true, false] {
        frame(
            ui,
            vec![egui::Event::Key {
                key,
                physical_key: Some(key),
                pressed,
                repeat: false,
                modifiers: egui::Modifiers {
                    shift,
                    ..Default::default()
                },
            }],
        );
    }
}

fn focus_edit_field(ui: &mut GroupUi, field: EditField) -> egui::Id {
    let point = ui.point(field.label()) + Vec2::new(0.0, 24.0);
    click(ui, point);
    let id = ui.ctx.memory(|memory| memory.focused()).unwrap();
    assert!(egui::text_edit::TextEditState::load(&ui.ctx, id).is_some());
    edit_key(ui, egui::Key::End, false);
    settle_undo(ui);
    id
}

fn edit_selection(ui: &GroupUi, id: egui::Id) -> egui::text::CCursorRange {
    egui::text_edit::TextEditState::load(&ui.ctx, id)
        .unwrap()
        .cursor
        .char_range()
        .unwrap()
}

fn edit_undo_points(
    ui: &GroupUi,
    id: egui::Id,
    text: &str,
) -> Vec<(egui::text::CCursorRange, String)> {
    // Inspect a clone of egui's real undo history without changing the editor
    // or manufacturing undo records. Rejected edits must add no hidden state.
    let state = egui::text_edit::TextEditState::load(&ui.ctx, id).unwrap();
    let mut undoer = state.undoer();
    let mut current = (state.cursor.char_range().unwrap(), text.to_owned());
    let mut points = Vec::new();
    while let Some(previous) = undoer.undo(&current).cloned() {
        points.push(previous.clone());
        current = previous;
    }
    points
}

fn edit_input(paste: bool, text: &str) -> egui::Event {
    if paste {
        egui::Event::Paste(text.into())
    } else {
        egui::Event::Text(text.into())
    }
}

fn legacy_field_values() -> [String; 2] {
    let values = ["a".repeat(300), "中🧪é".repeat(100)];
    for value in &values {
        assert_eq!(value.chars().count(), 300);
        assert!(value.len() <= bnauth_core::MAX_NAME_BYTES);
    }
    values
}

#[test]
fn legacy_partial_replacement_is_atomic_preserves_selection_and_unchanged_apply_never_writes() {
    for field in [EditField::Name, EditField::Description] {
        for paste in [false, true] {
            for before in legacy_field_values() {
                let (mut ui, commits) = ready_ui(&before, &before);
                let original = ui.app.runtime.app.current.clone().unwrap();
                let token = ui.app.model.change_token();
                open_edit(&mut ui);
                let id = focus_edit_field(&mut ui, field);
                edit_key(&mut ui, egui::Key::ArrowLeft, true);
                settle_undo(&mut ui);
                let selection = edit_selection(&ui, id);
                assert_eq!(selection.primary.index.0, 299);
                assert_eq!(selection.secondary.index.0, 300);
                let undo_points = edit_undo_points(&ui, id, &before);

                frame(&mut ui, vec![edit_input(paste, "X")]);
                assert_eq!(
                    field.value(&ui),
                    before,
                    "{field:?}, paste={paste}: rejecting replacement must not delete its selection"
                );
                assert_eq!(edit_selection(&ui, id), selection);
                settle_undo(&mut ui);
                assert_eq!(
                    edit_undo_points(&ui, id, &before),
                    undo_points,
                    "{field:?}, paste={paste}: rejection added an undo point"
                );
                for key in [egui::Key::Z, egui::Key::Y] {
                    command(&mut ui, key);
                    assert_eq!(
                        field.value(&ui),
                        before,
                        "{field:?}, paste={paste}, {key:?}"
                    );
                }
                assert_no_writes(&mut ui, &commits, &original, token);
                click_label(&mut ui, "套用");
                assert!(ui.app.dialog.is_none());
                assert_no_writes(&mut ui, &commits, &original, token);
            }
        }
    }
}

#[test]
fn legacy_long_fields_allow_delete_then_valid_correction_undo_and_exactly_one_apply_write() {
    for field in [EditField::Name, EditField::Description] {
        for paste in [false, true] {
            for before in legacy_field_values() {
                let (mut ui, commits) = ready_ui(&before, &before);
                let original = ui.app.runtime.app.current.clone().unwrap();
                let token = ui.app.model.change_token();
                open_edit(&mut ui);
                let id = focus_edit_field(&mut ui, field);

                // Deletion is still available while the stored value exceeds
                // the input cap, and undo restores the complete legacy value.
                edit_key(&mut ui, egui::Key::Backspace, false);
                let deleted: String = before.chars().take(299).collect();
                assert_eq!(field.value(&ui), deleted, "{field:?} delete above cap");
                settle_undo(&mut ui);
                command(&mut ui, egui::Key::Z);
                assert_eq!(field.value(&ui), before, "{field:?} deletion undo");

                edit_key(&mut ui, egui::Key::End, false);
                for _ in 0..45 {
                    edit_key(&mut ui, egui::Key::ArrowLeft, true);
                }
                settle_undo(&mut ui);
                assert_eq!(edit_selection(&ui, id).primary.index.0, 255);
                assert_eq!(edit_selection(&ui, id).secondary.index.0, 300);
                edit_key(&mut ui, egui::Key::Backspace, false);
                let reduced: String = before.chars().take(255).collect();
                assert_eq!(field.value(&ui), reduced, "{field:?} reduction below cap");
                settle_undo(&mut ui);

                frame(&mut ui, vec![edit_input(paste, "X")]);
                let corrected = format!("{reduced}X");
                assert_eq!(field.value(&ui), corrected, "{field:?}, paste={paste}");
                assert_eq!(corrected.chars().count(), INPUT_CHAR_LIMIT);
                settle_undo(&mut ui);
                frame(&mut ui, vec![edit_input(paste, "Y")]);
                assert_eq!(field.value(&ui), corrected, "{field:?} input cap changed");
                settle_undo(&mut ui);

                command(&mut ui, egui::Key::Z);
                assert_eq!(field.value(&ui), reduced, "{field:?} correction undo");
                command(&mut ui, egui::Key::Z);
                assert_eq!(field.value(&ui), before, "{field:?} full legacy undo");
                command(&mut ui, egui::Key::Y);
                assert_eq!(field.value(&ui), reduced, "{field:?} reduction redo");
                command(&mut ui, egui::Key::Y);
                assert_eq!(field.value(&ui), corrected, "{field:?} correction redo");
                assert_no_writes(&mut ui, &commits, &original, token);

                click_label(&mut ui, "套用");
                assert!(ui.app.dialog.is_none());
                for _ in 0..4 {
                    frame(&mut ui, Vec::new());
                    drain_memory_app(&mut ui.app.runtime.app);
                }
                let saved = ui.app.runtime.app.current.as_ref().unwrap();
                let mut expected = original.clone();
                expected.revision += 1;
                expected.authenticators[0].updated_at_ms = saved.authenticators[0].updated_at_ms;
                match field {
                    EditField::Name => expected.authenticators[0].name = corrected,
                    EditField::Description => expected.authenticators[0].description = corrected,
                }
                assert_eq!(
                    saved, &expected,
                    "{field:?}, paste={paste}: saved wrong data"
                );
                assert_eq!(ui.app.model.snapshot(), expected);
                assert_eq!(commits.load(Ordering::Relaxed), 1);
                assert!(!ui.app.runtime.app.saving);
                assert!(!ui.app.runtime.app.has_pending());
            }
        }
    }
}

#[test]
fn legacy_same_frame_selection_and_replacement_follow_event_order_and_keep_undo() {
    let key_event = |key, pressed, modifiers| egui::Event::Key {
        key,
        physical_key: Some(key),
        pressed,
        repeat: false,
        modifiers,
    };
    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    let select_all = egui::Modifiers {
        ctrl: true,
        command: true,
        ..Default::default()
    };
    for field in [EditField::Name, EditField::Description] {
        for paste in [false, true] {
            let before = "中🧪é".repeat(100);
            let (mut ui, commits) = ready_ui(&before, &before);
            let original = ui.app.runtime.app.current.clone().unwrap();
            let token = ui.app.model.change_token();
            open_edit(&mut ui);
            let id = focus_edit_field(&mut ui, field);
            assert_eq!(edit_selection(&ui, id).primary.index.0, 300);
            assert_eq!(edit_selection(&ui, id).secondary.index.0, 300);

            // OS input may queue selection movement and replacement before the
            // next paint. The replacement must use the new selection, and its
            // rejection must preserve that selection rather than delete it.
            frame(
                &mut ui,
                vec![
                    key_event(egui::Key::ArrowLeft, true, shift),
                    edit_input(paste, "X"),
                ],
            );
            assert_eq!(field.value(&ui), before, "{field:?}, paste={paste}");
            assert_eq!(edit_selection(&ui, id).primary.index.0, 299);
            assert_eq!(edit_selection(&ui, id).secondary.index.0, 300);
            frame(&mut ui, vec![key_event(egui::Key::ArrowLeft, false, shift)]);
            assert_no_writes(&mut ui, &commits, &original, token);

            let replacement = "有效修改🧪";
            frame(
                &mut ui,
                vec![
                    key_event(egui::Key::A, true, select_all),
                    edit_input(paste, replacement),
                ],
            );
            assert_eq!(field.value(&ui), replacement, "{field:?}, paste={paste}");
            frame(&mut ui, vec![key_event(egui::Key::A, false, select_all)]);
            settle_undo(&mut ui);
            assert_no_writes(&mut ui, &commits, &original, token);
            command(&mut ui, egui::Key::Z);
            assert_eq!(field.value(&ui), before, "{field:?} same-frame edit undo");
            click_label(&mut ui, "套用");
            assert!(ui.app.dialog.is_none());
            assert_no_writes(&mut ui, &commits, &original, token);
        }
    }
}

#[test]
fn legacy_partial_replacement_rejects_257_then_accepts_256_and_saves_exactly_once() {
    for field in [EditField::Name, EditField::Description] {
        for paste in [false, true] {
            let before = "中🧪é".repeat(100);
            let (mut ui, commits) = ready_ui(&before, &before);
            let original = ui.app.runtime.app.current.clone().unwrap();
            let token = ui.app.model.change_token();
            open_edit(&mut ui);
            let id = focus_edit_field(&mut ui, field);
            for _ in 0..100 {
                edit_key(&mut ui, egui::Key::ArrowLeft, true);
            }
            settle_undo(&mut ui);
            let selection = edit_selection(&ui, id);
            assert_eq!(selection.primary.index.0, 200);
            assert_eq!(selection.secondary.index.0, 300);
            let undo_points = edit_undo_points(&ui, id, &before);

            // This selection leaves room for 56 scalars. Reject all 57 of the
            // first replacement, even though part of that input could fit.
            let replacement = "替".repeat(56);
            frame(
                &mut ui,
                vec![edit_input(paste, &format!("{replacement}換"))],
            );
            assert_eq!(
                field.value(&ui),
                before,
                "{field:?}, paste={paste}: a 257-scalar result must reject the entire replacement"
            );
            assert_eq!(edit_selection(&ui, id), selection);
            settle_undo(&mut ui);
            assert_eq!(edit_undo_points(&ui, id, &before), undo_points);
            assert_no_writes(&mut ui, &commits, &original, token);

            // Replace the still-selected suffix directly, without a separate
            // deletion. This exact-boundary replacement is one undoable edit.
            frame(&mut ui, vec![edit_input(paste, &replacement)]);
            let corrected = format!(
                "{}{replacement}",
                before.chars().take(200).collect::<String>()
            );
            assert_eq!(corrected.chars().count(), INPUT_CHAR_LIMIT);
            assert_eq!(field.value(&ui), corrected, "{field:?}, paste={paste}");
            assert_eq!(edit_selection(&ui, id).primary.index.0, INPUT_CHAR_LIMIT);
            assert_eq!(edit_selection(&ui, id).secondary.index.0, INPUT_CHAR_LIMIT);
            settle_undo(&mut ui);
            command(&mut ui, egui::Key::Z);
            assert_eq!(
                field.value(&ui),
                before,
                "{field:?} partial replacement undo"
            );
            command(&mut ui, egui::Key::Y);
            assert_eq!(
                field.value(&ui),
                corrected,
                "{field:?} partial replacement redo"
            );
            assert_no_writes(&mut ui, &commits, &original, token);

            click_label(&mut ui, "套用");
            assert!(ui.app.dialog.is_none());
            for _ in 0..4 {
                frame(&mut ui, Vec::new());
                drain_memory_app(&mut ui.app.runtime.app);
            }
            let saved = ui.app.runtime.app.current.as_ref().unwrap();
            let mut expected = original.clone();
            expected.revision += 1;
            expected.authenticators[0].updated_at_ms = saved.authenticators[0].updated_at_ms;
            match field {
                EditField::Name => expected.authenticators[0].name = corrected,
                EditField::Description => expected.authenticators[0].description = corrected,
            }
            assert_eq!(
                saved, &expected,
                "{field:?}, paste={paste}: saved wrong replacement"
            );
            assert_eq!(ui.app.model.snapshot(), expected);
            assert_eq!(commits.load(Ordering::Relaxed), 1);
            assert!(!ui.app.runtime.app.saving);
            assert!(!ui.app.runtime.app.has_pending());
        }
    }
}

#[test]
fn legacy_ime_empty_commit_then_undo_and_paste_keeps_event_order_and_ordinary_limit() {
    for field in [EditField::Name, EditField::Description] {
        let before = "中🧪é".repeat(100);
        let (mut ui, commits) = ready_ui(&before, &before);
        let original = ui.app.runtime.app.current.clone().unwrap();
        let token = ui.app.model.change_token();
        open_edit(&mut ui);
        focus_edit_field(&mut ui, field);
        command(&mut ui, egui::Key::A);
        frame(
            &mut ui,
            vec![egui::Event::Ime(egui::ImeEvent::Preedit {
                text: "組字abc".into(),
                active_range_chars: None,
            })],
        );
        assert_eq!(field.value(&ui), "組字abc", "{field:?} selected preedit");
        settle_undo(&mut ui);
        assert_no_writes(&mut ui, &commits, &original, token);

        // Native Windows composition can remain active across idle frames.
        // Commit clears it, Undo restores the settled short preedit, and the
        // following paste therefore uses the ordinary 256-scalar input cap.
        let modifiers = egui::Modifiers {
            ctrl: true,
            command: true,
            ..Default::default()
        };
        frame(
            &mut ui,
            vec![
                egui::Event::Ime(egui::ImeEvent::Commit(String::new())),
                egui::Event::Key {
                    key: egui::Key::Z,
                    physical_key: Some(egui::Key::Z),
                    pressed: true,
                    repeat: false,
                    modifiers,
                },
                egui::Event::Paste("X".repeat(INPUT_CHAR_LIMIT + 1)),
            ],
        );
        assert_eq!(
            field.value(&ui),
            "X".repeat(INPUT_CHAR_LIMIT),
            "{field:?}: empty native IME commit must precede Undo and paste"
        );
        frame(
            &mut ui,
            vec![egui::Event::Key {
                key: egui::Key::Z,
                physical_key: Some(egui::Key::Z),
                pressed: false,
                repeat: false,
                modifiers,
            }],
        );
        assert_no_writes(&mut ui, &commits, &original, token);
        click_label(&mut ui, "×");
        assert!(ui.app.dialog.is_none());
        assert_no_writes(&mut ui, &commits, &original, token);
    }
}
