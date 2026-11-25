use super::*;

use egui::text_edit::TextEditState;

const SECRET: &str = "3132333435363738393031323334353637383930";
const SERIAL_LABEL: &str = "序號";
const RESTORE_LABEL: &str = "還原碼";
const DISPLAY_SERIAL: &str = "US-1404-0311-8994";

fn import_ui() -> GroupUi {
    let mut ui = GroupUi::new();
    ui.use_memory_vault(bnauth_core::VaultSnapshot::new_empty());
    ui.app.edit(&ui.ctx, None);
    for _ in 0..4 {
        ui.frame(Vec::new());
    }
    ui
}

fn focus(ui: &mut GroupUi, label: &str) -> egui::Id {
    let point = ui.point(label) + Vec2::new(0.0, 24.0);
    ui.click(point);
    let id = ui.ctx.memory(|memory| memory.focused()).unwrap();
    assert!(TextEditState::load(&ui.ctx, id).is_some());
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
    ui.frame(Vec::new());
    ui.time += 1.1;
    ui.frame(Vec::new());
}

fn cursor_indices(ui: &GroupUi, id: egui::Id) -> (usize, usize) {
    let range = TextEditState::load(&ui.ctx, id)
        .unwrap()
        .cursor
        .char_range()
        .unwrap();
    (range.primary.index.0, range.secondary.index.0)
}

#[test]
fn serial_paste_accepts_compact_dashed_and_mixed_input_with_cursor_and_undo() {
    for (input, accepted) in [
        ("us140403118994", "us140403118994"),
        ("\u{3000}us140403118994\u{3000}", "us140403118994"),
        ("us-1404-0311-8994", "us-1404-0311-8994"),
        ("\u{3000}us中1404 0311🧪8994é", "us140403118994"),
    ] {
        let mut ui = import_ui();
        let id = focus(&mut ui, SERIAL_LABEL);
        ui.frame(vec![egui::Event::Paste(input.into())]);
        assert_eq!(ui.import().serial, accepted, "input {input:?}");
        let end = (accepted.len(), accepted.len());
        assert_eq!(cursor_indices(&ui, id), end);
        settle_undo(&mut ui);
        command(&mut ui, egui::Key::Z);
        assert!(ui.import().serial.is_empty(), "one undo removes the paste");
        command(&mut ui, egui::Key::Y);
        assert_eq!(ui.import().serial, accepted);
        assert_eq!(cursor_indices(&ui, id), end);

        ui.key(egui::Key::Backspace);
        ui.type_text("5");
        assert_eq!(
            ui.import().serial,
            format!("{}5", &accepted[..accepted.len() - 1])
        );
        assert_eq!(cursor_indices(&ui, id), end);
        ui.click(ui.point("預覽匯入"));
        assert_eq!(ui.import().serial, "US-1404-0311-8995");
    }

    let mut ui = import_ui();
    focus(&mut ui, "密鑰 *");
    ui.type_text(SECRET);
    focus(&mut ui, SERIAL_LABEL);
    ui.frame(vec![egui::Event::Paste("us/1404-0311-8994".into())]);
    assert_eq!(ui.import().serial, "us/1404-0311-8994");
    ui.click(ui.point("預覽匯入"));
    assert_eq!(ui.import().serial, "us/1404-0311-8994");
    assert!(ui.import().preview.is_none());
    assert!(ui.has_invalid_border(DialogEditor::ImportSerial));
    assert!(ui.points("序號的分隔符位置無效。").is_empty());
}

#[test]
fn filtered_serial_paste_maps_both_selection_endpoints_and_ignores_invalid_replacement() {
    for (input, reverse) in [
        ("us140403118994", false),
        ("us140403118994", true),
        ("中u s1404　0311🧪8994", false),
        ("中u s1404　0311🧪8994", true),
    ] {
        let mut ui = import_ui();
        let id = focus(&mut ui, SERIAL_LABEL);
        let mut events = vec![egui::Event::Paste(input.into())];
        let shift = egui::Modifiers {
            shift: true,
            ..Default::default()
        };
        if reverse {
            for _ in 0..2 {
                events.push(key_event(egui::Key::ArrowLeft, egui::Modifiers::NONE));
            }
            for _ in 0..8 {
                events.push(key_event(egui::Key::ArrowLeft, shift));
            }
        } else {
            events.push(key_event(egui::Key::Home, egui::Modifiers::NONE));
            for _ in 0..4 {
                events.push(key_event(egui::Key::ArrowRight, egui::Modifiers::NONE));
            }
            for _ in 0..8 {
                events.push(key_event(egui::Key::ArrowRight, shift));
            }
        }
        ui.frame(events);
        assert_eq!(ui.import().serial, "us140403118994");
        let expected = if reverse { (4, 12) } else { (12, 4) };
        assert_eq!(cursor_indices(&ui, id), expected);
        ui.frame(Vec::new());
        assert_eq!(cursor_indices(&ui, id), expected);

        for event in [
            egui::Event::Text("中 é🧪".into()),
            egui::Event::Paste(" ／．－\u{3000}中文".into()),
        ] {
            ui.frame(vec![event]);
            assert_eq!(ui.import().serial, "us140403118994");
            assert_eq!(cursor_indices(&ui, id), expected);
        }
    }
}

#[test]
fn restore_preserves_ascii_input_and_formats_only_valid_submissions() {
    let mut ui = import_ui();
    let id = focus(&mut ui, RESTORE_LABEL);
    ui.frame(vec![egui::Event::Paste("中 ab-12 cd.34/ef🧪é".into())]);
    let original = "ab-12cd.34/ef";
    assert_eq!(ui.import().restore, original);
    assert_eq!(cursor_indices(&ui, id), (13, 13));
    settle_undo(&mut ui);
    command(&mut ui, egui::Key::Z);
    assert!(ui.import().restore.is_empty());
    command(&mut ui, egui::Key::Y);
    assert_eq!(ui.import().restore, original);
    assert_eq!(cursor_indices(&ui, id), (13, 13));
    settle_undo(&mut ui);

    let shift = egui::Modifiers {
        shift: true,
        ..Default::default()
    };
    ui.frame(
        (0..5)
            .map(|_| key_event(egui::Key::ArrowLeft, shift))
            .collect(),
    );
    assert_eq!(cursor_indices(&ui, id), (8, 13));
    for event in [
        egui::Event::Text("中é！ ".into()),
        egui::Event::Paste(" ／．－🧪".into()),
    ] {
        ui.frame(vec![event]);
        assert_eq!(ui.import().restore, original);
        assert_eq!(cursor_indices(&ui, id), (8, 13));
    }
    ui.type_text("q中!");
    assert_eq!(ui.import().restore, "ab-12cd.q!");
    assert_eq!(cursor_indices(&ui, id), (10, 10));
    settle_undo(&mut ui);
    command(&mut ui, egui::Key::Z);
    assert_eq!(ui.import().restore, original);
    command(&mut ui, egui::Key::Y);
    assert_eq!(ui.import().restore, "ab-12cd.q!");
    assert_eq!(cursor_indices(&ui, id), (10, 10));

    focus(&mut ui, "密鑰 *");
    ui.type_text(SECRET);
    assert!(!ui.has_invalid_border(DialogEditor::ImportRestore));
    assert!(!ui.has_invalid_border(DialogEditor::ImportName));
    ui.click(ui.point("預覽匯入"));
    assert!(ui.import().preview.is_none());
    assert_eq!(ui.import().restore, "ab-12cd.q!");
    assert!(ui.has_invalid_border(DialogEditor::ImportRestore));
    assert!(ui.has_invalid_border(DialogEditor::ImportName));
    assert!(ui.points("驗證器名稱不可為空白。").is_empty());
    assert!(
        ui.points("還原碼格式不正確。請輸入 10 位大寫英文字母或數字。")
            .is_empty()
    );
    focus(&mut ui, "名稱 *");
    ui.type_text("測試驗證器");
    assert!(!ui.has_invalid_border(DialogEditor::ImportName));
    assert!(ui.has_invalid_border(DialogEditor::ImportRestore));
    for wrong_length in ["ab12cd345", "ab12cd34567"] {
        focus(&mut ui, RESTORE_LABEL);
        command(&mut ui, egui::Key::A);
        ui.type_text(wrong_length);
        assert!(!ui.has_invalid_border(DialogEditor::ImportRestore));
        ui.click(ui.point("預覽匯入"));
        assert!(ui.has_invalid_border(DialogEditor::ImportRestore));
        assert!(ui.import().preview.is_none());
    }
    focus(&mut ui, RESTORE_LABEL);
    command(&mut ui, egui::Key::A);
    ui.type_text("ab12cd3456");
    assert_eq!(ui.import().restore, "ab12cd3456");
    ui.click(ui.point("預覽匯入"));
    assert_eq!(ui.import().restore, "AB12CD3456");
    assert_eq!(
        ui.import()
            .preview
            .as_ref()
            .unwrap()
            .record(&ui.import().name, &ui.import().description)
            .restore_code
            .as_deref(),
        Some("AB12CD3456")
    );
}

#[test]
fn rejected_material_characters_do_not_consume_the_existing_input_caps() {
    for (label, limit) in [(SERIAL_LABEL, 64), (RESTORE_LABEL, 128)] {
        let mut ui = import_ui();
        let id = focus(&mut ui, label);
        let value = |ui: &GroupUi| {
            if label == SERIAL_LABEL {
                ui.import().serial.clone()
            } else {
                ui.import().restore.clone()
            }
        };
        let expected = "a-1!".repeat(limit / 4);
        let payload = format!("{}{}z", "中！ ".repeat(128), expected);
        ui.frame(vec![egui::Event::Paste(payload)]);
        assert_eq!(value(&ui), expected, "{label}");
        assert_eq!(cursor_indices(&ui, id), (limit, limit));
        ui.type_text("9");
        assert_eq!(value(&ui), expected);
        assert_eq!(cursor_indices(&ui, id), (limit, limit));
        settle_undo(&mut ui);
        command(&mut ui, egui::Key::Z);
        assert!(value(&ui).is_empty());
        command(&mut ui, egui::Key::Y);
        assert_eq!(value(&ui), expected);
    }
}

#[test]
fn ime_and_accessibility_input_preserve_rejected_selections_and_filter_commits() {
    for (label, before, commit, expected) in [
        (
            SERIAL_LABEL,
            DISPLAY_SERIAL,
            "eu-9999-8888-7777中!",
            "eu-9999-8888-7777!",
        ),
        (RESTORE_LABEL, "AB12CD34EF", "x y-z中09", "xy-z09"),
    ] {
        let mut ui = import_ui();
        let id = focus(&mut ui, label);
        let value = |ui: &GroupUi| {
            if label == SERIAL_LABEL {
                ui.import().serial.clone()
            } else {
                ui.import().restore.clone()
            }
        };
        ui.frame(vec![egui::Event::Paste(before.into())]);
        command(&mut ui, egui::Key::A);
        let selection = cursor_indices(&ui, id);
        assert_ne!(selection.0, selection.1);

        for event in [
            egui::ImeEvent::Preedit {
                text: "ni".into(),
                active_range_chars: Some(0..2),
            },
            egui::ImeEvent::Commit("中文！ ".into()),
        ] {
            ui.frame(vec![egui::Event::Ime(event)]);
            assert_eq!(value(&ui), before, "{label}");
            assert_eq!(cursor_indices(&ui, id), selection);
        }
        for action in [
            egui::accesskit::Action::SetValue,
            egui::accesskit::Action::ReplaceSelectedText,
        ] {
            ui.frame(vec![egui::Event::AccessKitActionRequest(
                egui::accesskit::ActionRequest {
                    action,
                    target_tree: egui::accesskit::TreeId::ROOT,
                    target_node: id.accesskit_id(),
                    data: Some(egui::accesskit::ActionData::Value("中文！ ".into())),
                },
            )]);
            assert_eq!(value(&ui), before, "{label} {action:?}");
            assert_eq!(cursor_indices(&ui, id), selection);
        }
        ui.frame(vec![egui::Event::Ime(egui::ImeEvent::Commit(
            commit.into(),
        ))]);
        assert_eq!(value(&ui), expected, "{label}");
        let end = expected.chars().count();
        assert_eq!(cursor_indices(&ui, id), (end, end));
    }
}

#[test]
fn existing_restore_json_keeps_case_spaces_punctuation_and_unicode_after_metadata_render_and_apply()
{
    let restore = "Ab cD-12.中文";
    let vault = bnauth_core::VaultSnapshot::new_empty();
    let vault = bnauth_core::prepare_import(
        &vault,
        "既有驗證器",
        "既有備註",
        vault.ungrouped_id().unwrap(),
        bnauth_core::parse_manual(SECRET, "US140403118994", restore).unwrap(),
    )
    .unwrap()
    .candidate(&vault)
    .unwrap();
    let json = bnauth_adapters::native::encode(&vault).unwrap();
    let loaded = bnauth_adapters::native::decode(&json).unwrap();
    assert_eq!(
        loaded.authenticators[0].restore_code.as_deref(),
        Some(restore)
    );
    let mut ui = GroupUi::new();
    ui.use_memory_vault(loaded.clone());
    let token = ui.app.model.change_token();
    let id = ui.app.model.entries[0].id;
    ui.open(Dialog::details(&ui.app.model, &ui.app.model.entries[0]));
    assert_eq!(ui.points(restore).len(), 1);
    ui.click(ui.point("×"));
    ui.app.edit(&ui.ctx, Some(id));
    for _ in 0..4 {
        ui.frame(Vec::new());
    }
    ui.click(ui.point("套用"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.model.change_token(), token);
    assert_eq!(ui.app.model.snapshot(), loaded);
    assert_eq!(
        bnauth_adapters::native::encode(&ui.app.model.snapshot()).unwrap(),
        json
    );
    assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&loaded));
    assert!(!ui.app.runtime.app.saving);
    assert!(!ui.app.runtime.app.has_pending());
}

#[test]
fn valid_74_character_secret_autofills_the_serial_and_requires_a_name() {
    let mut ui = import_ui();
    assert_eq!(
        ui.points("接受 40 位密鑰或 74 位 WinAuth 未加密 secretdata，74位資料會自動帶入序號。")
            .len(),
        1
    );
    assert!(ui.points("* 為必填").is_empty());
    assert_eq!(ui.points("名稱 *").len(), 1);
    let serial_label = ui.point(SERIAL_LABEL);
    let restore_label = ui.point(RESTORE_LABEL);
    assert!((serial_label.y - restore_label.y).abs() < 1.0);
    assert!(serial_label.x < restore_label.x);
    let serial_hint = ui.point("US-1234-5678-9012");
    let restore_hint = ui.point("A2C4E6G8J0");
    assert!((serial_hint.y - restore_hint.y).abs() < 1.0);
    assert!(serial_hint.x < restore_hint.x);
    assert!(ui.import().serial.is_empty() && ui.import().restore.is_empty());
    focus(&mut ui, "密鑰 *");
    let suffix: String = b"US-1404-0311-8994"
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    let packed = format!("{SECRET}{suffix}");
    assert_eq!(packed.len(), 74);
    let invalid = format!("{}!", &SECRET[..39]);
    ui.frame(vec![egui::Event::Paste(invalid.clone())]);
    assert_eq!(ui.import().secret, invalid);
    ui.click(ui.point("預覽匯入"));
    assert!(ui.import().preview.is_none());
    assert!(ui.has_invalid_border(DialogEditor::ImportSecret));
    assert!(ui.points("密鑰格式不正確。").is_empty());
    focus(&mut ui, "密鑰 *");
    command(&mut ui, egui::Key::A);
    ui.frame(vec![egui::Event::Paste(packed)]);
    assert_eq!(ui.import().serial, DISPLAY_SERIAL);
    focus(&mut ui, SERIAL_LABEL);
    command(&mut ui, egui::Key::A);
    ui.key(egui::Key::Backspace);
    assert_eq!(ui.import().serial, DISPLAY_SERIAL);
    ui.click(ui.point("預覽匯入"));
    assert!(ui.import().name.is_empty());
    assert!(ui.import().preview.is_none());
    assert!(ui.has_invalid_border(DialogEditor::ImportName));
    assert!(!ui.has_invalid_border(DialogEditor::ImportSecret));
    assert!(ui.points("驗證器名稱不可為空白。").is_empty());

    let name = "自訂驗證器名稱";
    focus(&mut ui, "名稱 *");
    ui.type_text(name);
    ui.click(ui.point("預覽匯入"));
    let preview = ui
        .import()
        .preview
        .as_ref()
        .expect("valid 74-character material");
    let entry = preview.record(&ui.import().name, &ui.import().description);
    assert_eq!(entry.serial.as_deref(), Some("US140403118994"));
    assert_eq!(entry.name, name);
    assert_eq!(entry.secret.hex(), SECRET);
    assert!(!ui.app.runtime.app.saving);
}
