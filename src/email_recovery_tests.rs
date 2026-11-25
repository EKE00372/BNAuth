use super::*;
use crate::app::email_recovery::Reply;
use bnauth_core::{Secret, VaultSnapshot};
use std::{
    cell::{Cell, RefCell},
    rc::Rc,
    sync::{
        Arc,
        atomic::{AtomicBool, AtomicUsize, Ordering},
        mpsc::{self, Sender},
    },
};

const EMAIL: &str = "fixture@example.test";
const SERIAL: &str = "US-1234-5678-9012";
const RESTORE: &str = "ABCD123456";
const NAME: &str = "人工恢復驗證器";
const SEED: [u8; 20] = *b"12345678901234567890"; // RFC 6238 example.

fn frame(ui: &mut GroupUi, closing: bool, events: Vec<egui::Event>) -> Vec<egui::ViewportCommand> {
    ui.time += 0.04;
    let mut input = egui::RawInput {
        screen_rect: Some(Rect::from_min_size(Pos2::ZERO, ui.size)),
        time: Some(ui.time),
        events,
        ..Default::default()
    };
    if closing {
        input
            .viewports
            .get_mut(&egui::ViewportId::ROOT)
            .unwrap()
            .events
            .push(egui::ViewportEvent::Close);
    }
    let ctx = ui.ctx.clone();
    let mut app_frame = eframe::Frame::_new_kittest();
    let mut output = ctx.run_ui(input, |root| {
        eframe::App::logic(&mut ui.app, root.ctx(), &mut app_frame);
        eframe::App::ui(&mut ui.app, root, &mut app_frame);
    });
    output.textures_delta.clear();
    ui.shapes = output.shapes;
    output
        .viewport_output
        .remove(&egui::ViewportId::ROOT)
        .unwrap()
        .commands
}

fn draw(ui: &mut GroupUi) {
    frame(ui, false, Vec::new());
}

fn click(ui: &mut GroupUi, label: &str) -> Vec<egui::ViewportCommand> {
    let point = ui.point(label);
    click_at(ui, point)
}

fn click_at(ui: &mut GroupUi, point: Pos2) -> Vec<egui::ViewportCommand> {
    let mut commands = Vec::new();
    for pressed in [true, false] {
        commands.extend(frame(
            ui,
            false,
            vec![
                egui::Event::PointerMoved(point),
                egui::Event::PointerButton {
                    pos: point,
                    button: egui::PointerButton::Primary,
                    pressed,
                    modifiers: egui::Modifiers::NONE,
                },
            ],
        ));
    }
    commands.extend(frame(ui, false, Vec::new()));
    commands
}

fn until(ui: &mut GroupUi, done: impl Fn(&GroupUi) -> bool) -> Vec<egui::ViewportCommand> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut commands = Vec::new();
    loop {
        commands.extend(frame(ui, false, Vec::new()));
        if done(ui) {
            return commands;
        }
        assert!(Instant::now() < deadline, "recovery test did not finish");
        std::thread::sleep(Duration::from_millis(1));
    }
}

fn ready(snapshot: VaultSnapshot) -> GroupUi {
    let mut ui = GroupUi::new();
    ui.use_memory_vault(snapshot);
    ui.app.dialog = None;
    for _ in 0..3 {
        draw(&mut ui);
    }
    ui
}

fn mock_reply(ui: &mut GroupUi) -> (Sender<Reply>, Rc<Cell<usize>>) {
    let (sender, receiver) = mpsc::channel();
    let receiver = RefCell::new(Some(receiver));
    let requests = Rc::new(Cell::new(0));
    let count = requests.clone();
    ui.app.runtime.recovery.dispatch = Some(Box::new(move |email, material, deadline| {
        assert_eq!(email, EMAIL);
        assert_eq!(material.serial(), "US123456789012");
        assert_eq!(material.restore_code(), RESTORE);
        let remaining = deadline.saturating_duration_since(Instant::now());
        assert!(!remaining.is_zero() && remaining <= Duration::from_secs(30));
        count.set(count.get() + 1);
        receiver.borrow_mut().take().ok_or("人工回覆已取用")
    }));
    (sender, requests)
}

fn open_add_menu(ui: &mut GroupUi) {
    click(ui, "＋ 新增");
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.points("從密鑰恢復").len(), 1);
    assert_eq!(ui.points("從 Email 恢復").len(), 1);
}

fn open_form(ui: &mut GroupUi) {
    open_add_menu(ui);
    click(ui, "從 Email 恢復");
    assert!(matches!(ui.app.dialog, Some(Dialog::EmailRecovery(_))));
    draw(ui);
}

fn fill_recovery_material(ui: &mut GroupUi) {
    for (editor, value) in [
        (DialogEditor::RecoveryEmail, EMAIL),
        (DialogEditor::RecoverySerial, "us123456789012"),
        (DialogEditor::RecoveryCode, "aBcD123456"),
    ] {
        ui.ctx
            .memory_mut(|memory| memory.request_focus(editor.id()));
        frame(ui, false, vec![egui::Event::Text(value.to_owned())]);
    }
}

fn fill_recovery_name(ui: &mut GroupUi) {
    ui.ctx
        .memory_mut(|memory| memory.request_focus(DialogEditor::RecoveryName.id()));
    frame(ui, false, vec![egui::Event::Text(NAME.into())]);
}

fn fill_manual_material(ui: &mut GroupUi) {
    for (editor, value) in [
        (DialogEditor::ImportName, "從密鑰新增".to_owned()),
        (DialogEditor::ImportSecret, Secret::from_bytes(SEED).hex()),
    ] {
        ui.ctx
            .memory_mut(|memory| memory.request_focus(editor.id()));
        frame(ui, false, vec![egui::Event::Text(value)]);
    }
}

fn submit(ui: &mut GroupUi) {
    open_form(ui);
    fill_recovery_material(ui);
    fill_recovery_name(ui);
    click(ui, "開始恢復");
    assert!(ui.app.runtime.recovery.in_progress());
}

fn send_seed(sender: &Sender<Reply>) {
    sender.send(Ok(Secret::from_bytes(SEED))).unwrap();
}

fn current(ui: &GroupUi) -> &VaultSnapshot {
    ui.app.runtime.app.current.as_deref().unwrap()
}

#[test]
fn manual_recovery_close_button_cancels_the_preview() {
    let original = VaultSnapshot::new_empty();
    let mut ui = ready(original.clone());
    open_add_menu(&mut ui);
    click(&mut ui, "從密鑰恢復");
    assert!(matches!(ui.app.dialog, Some(Dialog::Import(_))));
    assert_eq!(ui.points("匯入驗證器").len(), 1);
    fill_manual_material(&mut ui);
    click(&mut ui, "預覽匯入");
    assert!(!ui.points("確認").is_empty());
    click(&mut ui, "×");
    assert!(ui.app.dialog.is_none());
    assert!(!ui.app.runtime.app.has_pending());
    assert_eq!(current(&ui), &original);
}

#[test]
fn email_recovery_saves_a_valid_secret_before_showing_success() {
    let replace = |ui: &mut GroupUi, editor: DialogEditor, value: &str| {
        ui.ctx
            .memory_mut(|memory| memory.request_focus(editor.id()));
        let mut events = vec![
            egui::Event::Key {
                key: egui::Key::A,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::COMMAND,
            },
            egui::Event::Key {
                key: egui::Key::Backspace,
                physical_key: None,
                pressed: true,
                repeat: false,
                modifiers: egui::Modifiers::NONE,
            },
        ];
        if !value.is_empty() {
            events.push(egui::Event::Paste(format!("\u{000b}{value}\u{001f}")));
        }
        frame(ui, false, events);
    };
    let confirmed_name = "預覽後修正的名稱";
    let confirmed_note = "預覽後修改的備註";
    let mut original = VaultSnapshot::new_empty();
    let target_group = uuid::Uuid::new_v4();
    original.groups.push(bnauth_core::Group {
        id: target_group,
        name: "恢復目標".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let mut ui = ready(original.clone());
    let (sender, requests) = mock_reply(&mut ui);
    open_form(&mut ui);
    assert_eq!(
        ui.points("憑帳號 Email、序列號和還原碼，取回綁定於該帳號的驗證器。")
            .len(),
        1
    );
    for hint in ["example@mail.com", "US-1234-5678-9012", "A2C4E6G8J0"] {
        assert_eq!(ui.points(hint).len(), 1);
    }
    let required = [
        DialogEditor::RecoveryEmail,
        DialogEditor::RecoverySerial,
        DialogEditor::RecoveryCode,
        DialogEditor::RecoveryName,
    ];
    assert!(required.iter().all(|&id| !ui.has_invalid_border(id)));
    click(&mut ui, "開始恢復");
    assert!(required.iter().all(|&id| ui.has_invalid_border(id)));
    assert_eq!(requests.get(), 0);
    fill_recovery_material(&mut ui);
    draw(&mut ui);
    assert_eq!(ui.points("us123456789012").len(), 1);
    assert_eq!(ui.points("aBcD123456").len(), 1);
    assert_eq!(ui.points("名稱 *").len(), 1);
    click(&mut ui, "開始恢復");
    assert_eq!(ui.points(SERIAL).len(), 1);
    assert_eq!(ui.points(RESTORE).len(), 1);
    assert!(ui.has_invalid_border(DialogEditor::RecoveryName));
    assert!(ui.points("驗證器名稱不可為空白。").is_empty());
    assert!(!ui.app.runtime.recovery.in_progress());
    assert_eq!(requests.get(), 0);

    fill_recovery_name(&mut ui);
    assert!(!ui.has_invalid_border(DialogEditor::RecoveryName));
    for code in ["", "ABCD12345", "ABCD1234567"] {
        replace(&mut ui, DialogEditor::RecoveryCode, code);
        assert!(!ui.has_invalid_border(DialogEditor::RecoveryCode));
        click(&mut ui, "開始恢復");
        assert!(ui.has_invalid_border(DialogEditor::RecoveryCode));
        assert_eq!(requests.get(), 0);
    }
    replace(&mut ui, DialogEditor::RecoveryCode, "aB-cD12.34");
    assert!(!ui.has_invalid_border(DialogEditor::RecoveryCode));
    click(&mut ui, "開始恢復");
    assert!(
        ui.points("還原碼格式不正確。請使用大寫英文字母或數字。")
            .is_empty()
    );
    assert!(ui.has_invalid_border(DialogEditor::RecoveryCode));
    assert_eq!(ui.points("aB-cD12.34").len(), 1);
    assert!(!ui.app.runtime.recovery.in_progress());
    assert_eq!(requests.get(), 0);

    replace(&mut ui, DialogEditor::RecoveryCode, "abcd123456");
    assert!(!ui.has_invalid_border(DialogEditor::RecoveryCode));
    click(&mut ui, "開始恢復");
    let message = "已保存恢復的驗證器";
    assert!(ui.points(message).is_empty());
    assert!(ui.points("確認").is_empty());
    assert_eq!(current(&ui), &original);

    send_seed(&sender);
    until(&mut ui, |ui| !ui.points("確認").is_empty());
    assert_eq!(ui.points("從 Email 恢復").len(), 1);
    assert_eq!(ui.points("Email *").len(), 1);
    let serial = ui.point("序號 *");
    let restore = ui.point("還原碼 *");
    assert!(serial.x < restore.x && (serial.y - restore.y).abs() < 2.0);
    assert_eq!(ui.points(SERIAL).len(), 1);
    assert_eq!(ui.points(RESTORE).len(), 1);
    assert!(ui.points("開始恢復").is_empty());
    let destination = ui.point("恢復至");
    let cancel = ui.point("取消");
    let confirm = ui.point("確認");
    assert!(destination.x < cancel.x && cancel.x < confirm.x && confirm.x - cancel.x < 64.0);
    assert!((destination.y - confirm.y).abs() < 2.0 && (cancel.y - confirm.y).abs() < 2.0);
    let code = bnauth_core::totp(&Secret::from_bytes(SEED), ui.app.runtime.frame_time).unwrap();
    assert_eq!(
        ui.points(&format!("{} {}", &code[..4], &code[4..])).len(),
        1
    );
    assert!(!ui.app.runtime.app.has_pending());
    assert_eq!(current(&ui), &original);
    assert!(ui.points(message).is_empty());

    replace(&mut ui, DialogEditor::RecoveryName, "");
    click(&mut ui, "確認");
    assert!(ui.has_invalid_border(DialogEditor::RecoveryName));
    assert!(ui.points("驗證器名稱不可為空白。").is_empty());
    assert!(ui.points("重試").is_empty());
    assert!(!ui.app.runtime.app.has_pending());
    replace(&mut ui, DialogEditor::RecoveryName, confirmed_name);
    assert!(!ui.has_invalid_border(DialogEditor::RecoveryName));
    assert_eq!(ui.points(EMAIL).len(), 2);
    replace(&mut ui, DialogEditor::RecoveryDescription, confirmed_note);
    assert_eq!(ui.points(confirmed_note).len(), 1);
    click(&mut ui, "將 Email 存為備註");
    assert_eq!(ui.points(EMAIL).len(), 2);
    click(&mut ui, "將 Email 存為備註");
    assert_eq!(ui.points(EMAIL).len(), 1);
    replace(&mut ui, DialogEditor::RecoveryDescription, confirmed_note);
    assert_eq!(requests.get(), 1);

    let dropdown = ui
        .points("預設")
        .into_iter()
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .expect("recovery destination dropdown");
    click_at(&mut ui, dropdown);
    let option = ui
        .points("恢復目標")
        .into_iter()
        .max_by(|a, b| a.y.total_cmp(&b.y))
        .expect("recovery destination group");
    click_at(&mut ui, option);
    assert_eq!(current(&ui), &original);
    assert_eq!(requests.get(), 1);
    click(&mut ui, "確認");
    until(&mut ui, |ui| {
        !ui.app.runtime.recovery.in_progress() && !ui.points(message).is_empty()
    });
    let saved = current(&ui);
    assert_eq!(saved.revision, original.revision + 1);
    let [record] = saved.authenticators.as_slice() else {
        panic!("expected one saved authenticator");
    };
    assert_eq!(record.secret, Secret::from_bytes(SEED));
    assert_eq!(record.group_id, target_group);
    assert_eq!(record.name, confirmed_name);
    assert_eq!(record.description, confirmed_note);
    assert_eq!(requests.get(), 1);
    assert_eq!(ui.points(message).len(), 1);
}

#[test]
fn a_pending_manual_clock_sample_is_saved_with_email_recovery_after_blocked_close() {
    let original = VaultSnapshot::new_empty();
    let mut ui = ready(original.clone());
    let (sender, requests) = mock_reply(&mut ui);
    let (sync_sender, sync_reply) = mpsc::channel();
    ui.app.runtime.sync_rx = Some(sync_reply);
    ui.app.runtime.sync_manual = true;
    submit(&mut ui);
    let instant = Instant::now();
    let wall = bnauth_core::now_ms();
    let sample =
        bnauth_core::clock::validate_sample(wall + 150, wall, wall, instant, instant).unwrap();
    let expected = sample.record.clone();
    sync_sender.send(Ok(sample)).unwrap();
    draw(&mut ui);
    assert!(ui.app.runtime.sync_rx.is_none());
    assert_eq!(
        ui.app.runtime.pending_clock_record.as_ref(),
        Some(&expected)
    );
    assert_eq!(current(&ui), &original);

    send_seed(&sender);
    until(&mut ui, |ui| !ui.points("確認").is_empty());
    let commands = frame(&mut ui, true, Vec::new());
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!ui.app.runtime.close_requested);
    assert!(!ui.app.runtime.app.has_pending());
    assert!(matches!(ui.app.dialog, Some(Dialog::EmailRecovery(_))));
    assert_eq!(current(&ui), &original);

    let mut commands = click(&mut ui, "確認");
    commands.extend(until(&mut ui, |ui| {
        !ui.app.runtime.recovery.in_progress() && !ui.app.runtime.app.saving
    }));
    let saved = current(&ui);
    assert_eq!(saved.revision, original.revision + 1);
    assert_eq!(saved.authenticators.len(), 1);
    assert_eq!(saved.service_time_sync.battlenet.as_ref(), Some(&expected));
    assert_eq!(ui.app.model.entries.len(), 1);
    assert_eq!(ui.app.runtime.sync_requests, 0);
    assert!(ui.app.runtime.pending_clock_record.is_none());
    assert_eq!(requests.get(), 1);
    assert!(!commands.contains(&egui::ViewportCommand::Close));
}

#[cfg(windows)]
#[test]
fn email_recovery_saves_reopens_and_keeps_confirmed_duplicate_records() {
    use std::{fs, path::PathBuf};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0.join("BNAuth.bnauth"));
            let _ = fs::remove_dir(&self.0);
        }
    }
    struct Disk(bnauth_adapters::storage::Repository);
    impl bnauth_app::Repository for Disk {
        fn commit(
            &mut self,
            candidate: &VaultSnapshot,
        ) -> Result<bnauth_app::CommitOutcome, String> {
            self.0
                .commit(candidate)
                .map(|outcome| bnauth_app::CommitOutcome {
                    snapshot: outcome.snapshot,
                    warning: outcome.warning,
                })
        }
        fn retry_cleanup(&mut self) -> Result<(), String> {
            self.0.retry_cleanup()
        }
    }

    let dir = Fixture(
        std::env::temp_dir().join(format!("BNAuth-email-recovery-{}", uuid::Uuid::new_v4())),
    );
    fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("BNAuth.bnauth");
    let mut ui = ready(VaultSnapshot::new_empty());
    let repo_path = path.clone();
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        let (mut repo, snapshot) = bnauth_adapters::storage::Repository::open(repo_path.clone())?;
        let snapshot = match snapshot {
            Some(snapshot) => snapshot,
            None => repo.commit(&VaultSnapshot::new_empty())?.snapshot,
        };
        Ok((Box::new(Disk(repo)), Some(snapshot)))
    }));
    until(&mut ui, |ui| {
        ui.app.runtime.app.phase == bnauth_app::Phase::Ready
    });
    let original = current(&ui).clone();

    let (sender, requests) = mock_reply(&mut ui);
    submit(&mut ui);
    assert_eq!(requests.get(), 1);
    assert_eq!(current(&ui), &original);
    let bytes = fs::read(&path).unwrap();
    send_seed(&sender);
    until(&mut ui, |ui| !ui.points("確認").is_empty());
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    click(&mut ui, "確認");
    until(&mut ui, |ui| current(ui).revision == original.revision + 1);
    let saved = current(&ui).clone();
    assert_eq!(saved.revision, original.revision + 1);
    let [record] = saved.authenticators.as_slice() else {
        panic!("expected one saved authenticator");
    };
    assert_eq!(record.name, NAME);
    assert_eq!(record.description, EMAIL);
    assert_eq!(record.serial.as_deref(), Some("US123456789012"));
    assert_eq!(record.restore_code.as_deref(), Some(RESTORE));
    assert!(
        ui.app.runtime.sync_rx.is_none(),
        "test recovery must remain offline"
    );
    assert_eq!(ui.app.runtime.sync_requests, 1);

    let (sender, requests) = mock_reply(&mut ui);
    submit(&mut ui);
    send_seed(&sender);
    until(&mut ui, |ui| !ui.points("確認").is_empty());
    assert_eq!(current(&ui), &saved);
    click(&mut ui, "確認");
    until(&mut ui, |ui| current(ui).revision == saved.revision + 1);
    assert_eq!(requests.get(), 1);
    let duplicated = current(&ui).clone();
    assert_eq!(duplicated.revision, saved.revision + 1);
    assert_eq!(duplicated.authenticators[0], saved.authenticators[0]);
    assert_ne!(
        duplicated.authenticators[0].id,
        duplicated.authenticators[1].id
    );
    assert_eq!(ui.app.runtime.sync_requests, 2);
    drop(ui);

    let (_repo, reopened) = bnauth_adapters::storage::Repository::open(path).unwrap();
    let reopened = reopened.unwrap();
    assert_eq!(reopened, duplicated);
    let secret = &reopened.authenticators[0].secret;
    assert_eq!(bnauth_core::totp(secret, 59_000).unwrap(), "94287082");
}

#[test]
fn email_recovery_lists_matching_names_in_all_groups_and_cancel_does_not_save() {
    let mut empty = VaultSnapshot::new_empty();
    let group = uuid::Uuid::new_v4();
    empty.groups.push(bnauth_core::Group {
        id: group,
        name: "既有分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let seed = Secret::from_bytes(SEED).hex();
    let cross_group_copy = bnauth_core::prepare_import(
        &empty,
        "其他分組的驗證器",
        "保留其他分組的紀錄",
        empty.groups[0].id,
        bnauth_core::parse_manual(&seed, SERIAL, "OtherGroup").unwrap(),
    )
    .unwrap()
    .candidate(&empty)
    .unwrap();
    let original = bnauth_core::prepare_import(
        &cross_group_copy,
        "既有名稱",
        "既有備註",
        group,
        bnauth_core::parse_manual(&seed, "", "").unwrap(),
    )
    .unwrap()
    .candidate(&cross_group_copy)
    .unwrap();
    let mut ui = ready(original.clone());
    ui.app.group = ui.app.model.groups[1].id;
    let (sender, requests) = mock_reply(&mut ui);
    submit(&mut ui);
    send_seed(&sender);
    until(&mut ui, |ui| !ui.points("確認").is_empty());
    assert!(ui.app.runtime.recovery.in_progress());
    assert!(!ui.app.runtime.app.saving);
    assert_eq!(current(&ui), &original);
    assert_eq!(ui.points("此驗證器密鑰與下列相同：").len(), 1);
    for hint in [
        "「預設」的「其他分組的驗證器」",
        "「既有分組」的「既有名稱」",
    ] {
        assert_eq!(ui.points(hint).len(), 1);
    }
    click(&mut ui, "取消");
    assert!(!ui.app.runtime.recovery.in_progress());
    assert!(!ui.app.runtime.app.has_pending());
    assert_eq!(current(&ui), &original);
    assert_eq!(requests.get(), 1);
}

#[test]
fn email_recovery_close_button_releases_the_last_slot_for_manual_recovery() {
    let group_limit = bnauth_core::MAX_GROUP_ENTRIES;
    let mut original = large_list_vault();
    original.groups.truncate(1);
    original.authenticators.truncate(group_limit - 1);
    original.authenticators[0].secret = Secret::from_bytes(SEED);
    original.validate().unwrap();
    let target = original.groups[0].id;
    for received in [false, true] {
        let mut ui = ready(original.clone());
        let (sender, requests) = mock_reply(&mut ui);
        submit(&mut ui);
        if received {
            send_seed(&sender);
            until(&mut ui, |ui| !ui.points("確認").is_empty());
        }
        click(&mut ui, "×");
        assert!(ui.app.dialog.is_none());
        assert!(!ui.app.runtime.recovery.in_progress());
        assert!(!ui.app.runtime.app.has_pending());
        assert_eq!(current(&ui), &original);
        assert!(sender.send(Err("取消後到達的回覆".into())).is_err());

        open_add_menu(&mut ui);
        click(&mut ui, "從密鑰恢復");
        fill_manual_material(&mut ui);
        click(&mut ui, "預覽匯入");
        click(&mut ui, "確認");
        until(&mut ui, |ui| current(ui).revision == original.revision + 1);
        let saved = current(&ui);
        assert_eq!(saved.authenticators.len(), group_limit);
        assert_eq!(
            &saved.authenticators[..group_limit - 1],
            original.authenticators.as_slice()
        );
        let entry = &saved.authenticators[group_limit - 1];
        assert_ne!(entry.id, original.authenticators[0].id);
        assert_eq!(entry.group_id, target);
        assert_eq!(entry.secret, Secret::from_bytes(SEED));
        assert_eq!(entry.sort_order, group_limit as u32 - 1);
        assert_eq!(requests.get(), 1);
    }
}

#[test]
fn email_recovery_at_capacity_stops_before_sending_a_request() {
    for full_vault in [false, true] {
        let mut original = large_list_vault();
        let group_limit = bnauth_core::MAX_GROUP_ENTRIES;
        if full_vault {
            // Keep the selected group below its own limit to exercise the vault limit.
            let overflow_group = uuid::Uuid::new_v4();
            let sort_order = original.groups.len() as u32;
            original.groups.push(bnauth_core::Group {
                id: overflow_group,
                name: "其他分組".into(),
                sort_order,
                is_ungrouped: false,
            });
            let moved = &mut original.authenticators[group_limit - 1];
            moved.group_id = overflow_group;
            moved.sort_order = 0;
        } else {
            original.authenticators.truncate(group_limit);
            original.groups.truncate(1);
        }
        let mut ui = ready(original.clone());
        let (_sender, requests) = mock_reply(&mut ui);
        if full_vault {
            click(&mut ui, "＋ 新增");
            assert!(ui.app.dialog.is_none());
            assert!(ui.points("從密鑰恢復").is_empty());
            assert!(ui.points("從 Email 恢復").is_empty());
        } else {
            open_form(&mut ui);
            fill_recovery_material(&mut ui);
            fill_recovery_name(&mut ui);
            click(&mut ui, "開始恢復");
            assert!(!ui.points("分組數量已達上限。").is_empty());
        }
        assert_eq!(requests.get(), 0);
        assert!(!ui.app.runtime.recovery.in_progress());
        assert!(!ui.app.runtime.app.has_pending());
        assert_eq!(current(&ui), &original);
    }
}

#[test]
fn email_recovery_requires_explicit_confirmation_for_a_different_seed() {
    let empty = VaultSnapshot::new_empty();
    let original = bnauth_core::prepare_import(
        &empty,
        "既有同序號驗證器",
        "保留原資料",
        empty.groups[0].id,
        bnauth_core::parse_manual("1111111111111111111111111111111111111111", SERIAL, RESTORE)
            .unwrap(),
    )
    .unwrap()
    .candidate(&empty)
    .unwrap();
    let mut ui = ready(original.clone());
    let (sender, requests) = mock_reply(&mut ui);
    submit(&mut ui);
    send_seed(&sender);
    draw(&mut ui);
    assert!(ui.app.runtime.recovery.in_progress());
    assert!(!ui.app.runtime.app.saving);
    assert_eq!(current(&ui), &original);
    assert!(ui.points("此驗證器密鑰與下列相同：").is_empty());
    click(&mut ui, "確認");
    until(&mut ui, |ui| current(ui).revision == original.revision + 1);
    let saved = current(&ui);
    assert_eq!(requests.get(), 1);
    assert_eq!(saved.authenticators.len(), 2);
    assert_eq!(saved.authenticators[0], original.authenticators[0]);
    assert_ne!(saved.authenticators[0].id, saved.authenticators[1].id);
    assert_eq!(saved.authenticators[1].secret, Secret::from_bytes(SEED));
}

struct FailAndWaitForRetry {
    attempts: Arc<AtomicUsize>,
    release: Arc<AtomicBool>,
    first_record: Option<bnauth_core::Authenticator>,
}

impl bnauth_app::Repository for FailAndWaitForRetry {
    fn commit(&mut self, candidate: &VaultSnapshot) -> Result<bnauth_app::CommitOutcome, String> {
        let [record] = candidate.authenticators.as_slice() else {
            panic!("expected one confirmed authenticator");
        };
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            self.first_record = Some(record.clone());
            return Err("人工磁碟保存失敗".into());
        }
        assert_eq!(self.first_record.as_ref(), Some(record));
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.release.load(Ordering::Acquire) {
            if Instant::now() >= deadline {
                return Err("人工保存閘門逾時".into());
            }
            std::thread::sleep(Duration::from_millis(1));
        }
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

#[test]
fn email_recovery_blocks_close_while_waiting_and_closes_after_save_retry() {
    let original = VaultSnapshot::new_empty();
    let mut ui = ready(original.clone());
    let attempts = Arc::new(AtomicUsize::new(0));
    let release = Arc::new(AtomicBool::new(false));
    let (worker_vault, worker_attempts, worker_release) =
        (original.clone(), attempts.clone(), release.clone());
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(FailAndWaitForRetry {
                attempts: worker_attempts.clone(),
                release: worker_release.clone(),
                first_record: None,
            }),
            Some(worker_vault.clone()),
        ))
    }));
    until(&mut ui, |ui| {
        ui.app.runtime.app.phase == bnauth_app::Phase::Ready
    });
    let (sender, requests) = mock_reply(&mut ui);
    submit(&mut ui);
    for _ in 0..2 {
        let commands = frame(&mut ui, true, Vec::new());
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(!commands.contains(&egui::ViewportCommand::Close));
        assert!(!ui.app.runtime.close_requested);
        assert!(matches!(ui.app.dialog, Some(Dialog::EmailRecovery(_))));
    }
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    send_seed(&sender);
    until(&mut ui, |ui| !ui.points("確認").is_empty());
    assert_eq!(attempts.load(Ordering::SeqCst), 0);
    assert_eq!(current(&ui), &original);
    click(&mut ui, "確認");
    until(&mut ui, |ui| ui.app.runtime.app.error.is_some());
    assert!(!ui.app.runtime.close_requested && ui.app.runtime.close_blocked());
    assert!(ui.app.runtime.app.has_pending());
    assert_eq!(current(&ui), &original);
    assert_eq!(attempts.load(Ordering::SeqCst), 1);
    assert_eq!(requests.get(), 1);

    let retry = ui.point("重試");
    let discard = ui.point("放棄");
    assert!((retry.y - discard.y).abs() < 2.0);
    assert!(retry.x > ui.size.x / 2.0 && retry.x < discard.x && discard.x - retry.x < 64.0);
    click(&mut ui, "重試");
    assert!(ui.app.runtime.app.saving);
    assert_eq!(current(&ui), &original);
    let commands = frame(&mut ui, true, Vec::new());
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!commands.contains(&egui::ViewportCommand::Close));
    assert!(ui.app.runtime.close_requested && ui.app.runtime.close_blocked());
    release.store(true, Ordering::Release);
    let commands = until(&mut ui, |ui| !ui.app.runtime.close_blocked());
    assert!(commands.contains(&egui::ViewportCommand::Close));
    assert_eq!(attempts.load(Ordering::SeqCst), 2);
    assert_eq!(requests.get(), 1);
    assert_eq!(current(&ui).revision, original.revision + 1);
    assert_eq!(current(&ui).authenticators.len(), 1);
}

#[test]
fn email_recovery_reply_failure_keeps_form_open_until_explicit_dialog_close() {
    let message = "E12：HTTP 錯誤，服務回傳 HTTP 404。";
    let original = VaultSnapshot::new_empty();
    let mut ui = ready(original.clone());
    let (sender, requests) = mock_reply(&mut ui);
    submit(&mut ui);
    let commands = frame(&mut ui, true, Vec::new());
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!ui.app.runtime.close_requested);

    sender.send(Err(message.into())).unwrap();
    draw(&mut ui);
    assert_eq!(ui.points(message).len(), 1);
    assert_eq!(ui.points(EMAIL).len(), 2);
    assert_eq!(ui.points(NAME).len(), 1);
    assert_eq!(ui.points("開始恢復").len(), 1);
    assert!(!ui.app.runtime.app.has_pending());

    let commands = frame(&mut ui, true, Vec::new());
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(!ui.app.runtime.close_requested);
    let commands = click(&mut ui, "×");
    assert!(!commands.contains(&egui::ViewportCommand::Close));
    assert!(ui.app.dialog.is_none());
    assert!(!ui.app.runtime.close_requested);
    let commands = frame(&mut ui, true, Vec::new());
    assert!(!commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(ui.app.runtime.close_requested);
    assert!(!ui.app.runtime.close_blocked());
    assert_eq!(requests.get(), 1);
    assert_eq!(current(&ui), &original);
}

#[test]
fn recovery_save_retry_cleans_residue_before_writing_and_discard_drops_the_result() {
    struct RecoverySaveWithResidue {
        attempts: Arc<AtomicUsize>,
        cleanups: Arc<AtomicUsize>,
        occupied: Arc<AtomicBool>,
        cleanup_required: bool,
    }

    impl bnauth_app::Repository for RecoverySaveWithResidue {
        fn state(&self) -> bnauth_app::RepositoryState {
            bnauth_app::RepositoryState {
                cleanup_required: self.cleanup_required,
                ..Default::default()
            }
        }

        fn commit(
            &mut self,
            candidate: &VaultSnapshot,
        ) -> Result<bnauth_app::CommitOutcome, String> {
            assert!(!self.cleanup_required);
            if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
                self.cleanup_required = true;
                return Err("人工保存失敗，暫存檔被占用".into());
            }
            candidate.validate()?;
            let mut snapshot = candidate.clone();
            snapshot.revision += 1;
            Ok(bnauth_app::CommitOutcome {
                snapshot,
                warning: None,
            })
        }

        fn retry_cleanup(&mut self) -> Result<(), String> {
            self.cleanups.fetch_add(1, Ordering::SeqCst);
            if self.occupied.load(Ordering::Acquire) {
                return Err("暫存檔仍被占用".into());
            }
            self.cleanup_required = false;
            Ok(())
        }

        fn defer_cleanup(&mut self) {
            self.cleanup_required = false;
        }
    }

    for (email, discard) in [(true, false), (false, false), (true, true)] {
        let original = VaultSnapshot::new_empty();
        let attempts = Arc::new(AtomicUsize::new(0));
        let cleanups = Arc::new(AtomicUsize::new(0));
        let occupied = Arc::new(AtomicBool::new(true));
        let worker_cleanups = cleanups.clone();
        let (worker_vault, worker_attempts, worker_occupied) =
            (original.clone(), attempts.clone(), occupied.clone());
        let mut ui = ready(original.clone());
        ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
            Ok((
                Box::new(RecoverySaveWithResidue {
                    attempts: worker_attempts.clone(),
                    cleanups: worker_cleanups.clone(),
                    occupied: worker_occupied.clone(),
                    cleanup_required: false,
                }),
                Some(worker_vault.clone()),
            ))
        }));
        until(&mut ui, |ui| {
            ui.app.runtime.app.phase == bnauth_app::Phase::Ready
        });
        let sync_sender = (!discard).then(|| {
            let (sender, reply) = mpsc::channel();
            ui.app.runtime.sync_rx = Some(reply);
            ui.app.runtime.sync_manual = true;
            sender
        });
        let (sender, requests) = mock_reply(&mut ui);
        if email {
            submit(&mut ui);
            send_seed(&sender);
        } else {
            open_add_menu(&mut ui);
            click(&mut ui, "從密鑰恢復");
            fill_manual_material(&mut ui);
            click(&mut ui, "預覽匯入");
        }
        until(&mut ui, |ui| !ui.points("確認").is_empty());
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
        assert!(!ui.app.runtime.app.has_pending());
        click(&mut ui, "確認");
        until(&mut ui, |ui| {
            ui.app.runtime.app.repository_state.cleanup_required
        });
        assert!(ui.app.runtime.app.has_pending());
        let commands = frame(&mut ui, true, Vec::new());
        assert!(commands.contains(&egui::ViewportCommand::CancelClose));
        assert!(ui.app.runtime.close_requested);

        assert!(ui.points("清理暫存檔").is_empty());
        click(&mut ui, "重試");
        until(&mut ui, |ui| {
            ui.app.runtime.app.error.as_deref() == Some("暫存檔仍被占用")
        });
        assert!(ui.app.runtime.app.has_pending());
        assert!(ui.app.runtime.close_blocked());
        assert_eq!(current(&ui), &original);
        assert_eq!(attempts.load(Ordering::SeqCst), 1);

        if discard {
            let commands = click(&mut ui, "放棄");
            assert!(commands.contains(&egui::ViewportCommand::Close));
            assert!(!ui.app.runtime.app.has_pending());
            assert!(!ui.app.runtime.recovery.in_progress());
            assert!(!ui.app.runtime.app.repository_state.needs_attention());
            assert!(ui.app.runtime.app.error.is_none());
            assert_eq!(current(&ui), &original);
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
            assert_eq!(cleanups.load(Ordering::SeqCst), 1);
        } else {
            let instant = Instant::now();
            let wall = bnauth_core::now_ms();
            let sample =
                bnauth_core::clock::validate_sample(wall + 150, wall, wall, instant, instant)
                    .unwrap();
            let saved_clock = sample.record.clone();
            sync_sender.unwrap().send(Ok(sample)).unwrap();
            draw(&mut ui);
            assert_eq!(
                ui.app.runtime.pending_clock_record.as_ref(),
                Some(&saved_clock)
            );
            occupied.store(false, Ordering::Release);
            let mut commands = click(&mut ui, "重試");
            commands.extend(until(&mut ui, |ui| !ui.app.runtime.close_blocked()));
            assert!(commands.contains(&egui::ViewportCommand::Close));
            assert_eq!(current(&ui).revision, original.revision + 1);
            let [record] = current(&ui).authenticators.as_slice() else {
                panic!("expected one saved authenticator");
            };
            assert_eq!(record.secret, Secret::from_bytes(SEED));
            assert_eq!(current(&ui).service_time_sync.battlenet, Some(saved_clock));
            assert_eq!(ui.app.runtime.sync_requests, 0);
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
            assert_eq!(cleanups.load(Ordering::SeqCst), 2);
        }
        assert_eq!(requests.get(), usize::from(email));
    }
}

#[test]
fn email_recovery_save_without_acknowledgement_abandons_only_memory_and_keeps_both_files() {
    use bnauth_adapters::native;
    use std::{fs, path::PathBuf};

    const ERROR_SUMMARY: &str = "保存期間原資料檔被外部修改，或無法確認原資料備份是否完整；已保留所有檔案，沒有刪除原資料備份。";

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            let _ = fs::remove_file(self.0.join("BNAuth.bnauth"));
            let _ = fs::remove_file(self.0.join("BNAuth.rollback"));
            let _ = fs::remove_dir(&self.0);
        }
    }

    struct SaveWithoutAcknowledgement {
        folder: PathBuf,
        blocked: bool,
    }
    impl bnauth_app::Repository for SaveWithoutAcknowledgement {
        fn state(&self) -> bnauth_app::RepositoryState {
            bnauth_app::RepositoryState {
                blocked: self.blocked,
                ..Default::default()
            }
        }

        fn commit(
            &mut self,
            candidate: &VaultSnapshot,
        ) -> Result<bnauth_app::CommitOutcome, String> {
            assert!(!self.blocked);
            let main = self.folder.join("BNAuth.bnauth");
            fs::copy(&main, self.folder.join("BNAuth.rollback")).unwrap();
            let mut installed = candidate.clone();
            installed.revision += 1;
            fs::write(&main, &*native::encode(&installed).unwrap()).unwrap();
            self.blocked = true;
            Err(bnauth_core::ErrorCode::WriteFailed
                .with_detail(ERROR_SUMMARY)
                .to_string())
        }

        fn retry_cleanup(&mut self) -> Result<(), String> {
            panic!("blocked save must not initiate cleanup")
        }
    }

    let original = VaultSnapshot::new_empty();
    let original_bytes = native::encode(&original).unwrap();
    let fixture = Fixture(std::env::temp_dir().join(format!(
        "BNAuth-unconfirmed-recovery-{}",
        uuid::Uuid::new_v4()
    )));
    fs::create_dir(&fixture.0).unwrap();
    let main = fixture.0.join("BNAuth.bnauth");
    let rollback = fixture.0.join("BNAuth.rollback");
    fs::write(&main, &*original_bytes).unwrap();
    let folder = fixture.0.clone();
    let worker_vault = original.clone();
    let mut ui = ready(original.clone());
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(SaveWithoutAcknowledgement {
                folder: folder.clone(),
                blocked: false,
            }),
            Some(worker_vault.clone()),
        ))
    }));
    until(&mut ui, |ui| {
        ui.app.runtime.app.phase == bnauth_app::Phase::Ready
    });
    let (sender, requests) = mock_reply(&mut ui);
    submit(&mut ui);
    send_seed(&sender);
    until(&mut ui, |ui| !ui.points("確認").is_empty());
    assert_eq!(fs::read(&main).unwrap(), *original_bytes);
    assert!(!rollback.exists());
    assert!(!ui.app.runtime.app.has_pending());
    click(&mut ui, "確認");
    until(&mut ui, |ui| ui.app.runtime.app.repository_state.blocked);
    let installed_bytes = fs::read(&main).unwrap();
    let installed = native::decode(&installed_bytes).unwrap();
    assert_eq!(installed.revision, original.revision + 1);
    assert_eq!(installed.authenticators[0].secret, Secret::from_bytes(SEED));
    assert_eq!(current(&ui), &original);
    assert_eq!(fs::read(&rollback).unwrap(), *original_bytes);

    ui.size = Vec2::new(
        crate::runtime::MIN_WINDOW_WIDTH as f32,
        crate::runtime::MIN_WINDOW_HEIGHT as f32,
    );
    let commands = frame(&mut ui, true, Vec::new());
    assert!(commands.contains(&egui::ViewportCommand::CancelClose));
    assert!(ui.app.runtime.app.has_pending());
    assert!(!ui.points(bnauth_core::WRITE_FAILED_NOTICE).is_empty());
    assert!(ui.points(ERROR_SUMMARY).is_empty());
    click(&mut ui, "重試");
    assert!(ui.app.runtime.app.has_pending());
    assert!(!ui.app.runtime.app.saving);
    let model_token = ui.app.model.change_token();
    let commands = click(&mut ui, "放棄");
    assert!(commands.contains(&egui::ViewportCommand::Close));
    assert!(!ui.app.runtime.app.has_pending());
    assert!(!ui.app.runtime.recovery.in_progress());
    assert!(ui.app.runtime.app.repository_state.blocked);
    assert_eq!(ui.app.model.change_token(), model_token);
    assert_eq!(requests.get(), 1);
    drop(ui);

    assert_eq!(fs::read(&main).unwrap(), installed_bytes);
    assert_eq!(fs::read(&rollback).unwrap(), *original_bytes);
}
