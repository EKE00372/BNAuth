use super::*;
use crate::runtime::GroupImportResponse;
use bnauth_core::{Group, Uuid, VaultSnapshot};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn source() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty();
    let group = Uuid::new_v4();
    vault.groups.push(Group {
        id: group,
        name: "匯入來源分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let material = bnauth_core::parse_manual(
        "3132333435363738393031323334353637383930",
        "US-1234-5678-9012",
        "AbCd123456",
    )
    .unwrap();
    let mut record = bnauth_core::prepare_import(&vault, "第一筆", "完整備註", group, material)
        .unwrap()
        .record();
    vault.authenticators.push(record.clone());
    record.id = Uuid::new_v4();
    record.name = "第二筆".into();
    record.secret = bnauth_core::Secret::from_bytes([2; 20]);
    record.serial = Some("US123456789013".into());
    record.sort_order = 1;
    vault.authenticators.push(record);
    vault.export_group(group).unwrap()
}

fn select_group_file(ui: &mut GroupUi, source: &VaultSnapshot) {
    assert!(matches!(ui.app.dialog, Some(Dialog::GroupImport(_))));
    let (sender, receiver) = std::sync::mpsc::channel();
    sender
        .send(Ok(Some(GroupImportResponse::Selected {
            path: "來源分組.bnauth".into(),
            source: Box::new(bnauth_core::ValidatedBackup::new(source.clone(), true).unwrap()),
        })))
        .unwrap();
    ui.app.runtime.group_import_rx = Some(receiver);
}

fn show_loaded(ui: &mut GroupUi, source: &VaultSnapshot) {
    ui.app.open_group_import(&ui.ctx);
    select_group_file(ui, source);
    wait_group_import_preview(ui);
}

fn wait_group_import_preview(ui: &mut GroupUi) {
    let deadline = Instant::now() + Duration::from_secs(5);
    while ui.app.runtime.group_import_rx.is_some() {
        ui.app.poll_runtime(&ui.ctx);
        assert!(
            Instant::now() < deadline,
            "group import preview did not finish"
        );
        std::thread::yield_now();
    }
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
}

fn preview_ui() -> (GroupUi, VaultSnapshot) {
    let mut ui = GroupUi::new();
    let source = source();
    let mut target = VaultSnapshot::new_empty();
    target.authenticators = source
        .authenticators
        .iter()
        .cloned()
        .map(|mut record| {
            record.id = Uuid::new_v4();
            record.group_id = target.groups[0].id;
            record
        })
        .collect::<Vec<_>>()
        .into();
    ui.use_memory_vault(target);
    show_loaded(&mut ui, &source);
    (ui, source)
}

#[test]
fn preview_cancel_is_read_only_and_confirmation_commits_then_reveals_the_imported_group() {
    let (mut ui, source) = preview_ui();
    let original = ui.app.runtime.app.current.clone();
    let existing = ui.app.model.entries[0].clone();
    let existing_code = ui.app.otp_cache.code(&existing.record, IMPORT_TEST_TIME);
    let calculations = ui.app.otp_cache.calculations;
    assert_eq!(ui.points("分組 匯入來源分組 共 2 個驗證器。").len(), 1);
    assert_eq!(ui.points("第一筆驗證器：第一筆").len(), 1);
    for (time, code) in [(59_000, "9428 7082"), (60_000, "3735 9152")] {
        ui.app.runtime.frame_time = time;
        ui.frame(Vec::new());
        assert_eq!(ui.points("目前驗證碼").len(), 1);
        assert_eq!(ui.points(code).len(), 1);
    }
    assert_eq!(ui.app.runtime.app.current, original);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.runtime.app.current, original);
    show_loaded(&mut ui, &source);
    let (sender, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    ui.app.runtime.sync_manual = true;
    let measured_at = Instant::now();
    let wall = bnauth_core::now_ms();
    let sample =
        bnauth_core::clock::validate_sample(wall + 150, wall, wall, measured_at, measured_at)
            .unwrap();
    let expected_clock = sample.record.clone();
    sender.send(Ok(sample)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert_eq!(ui.app.runtime.app.current, original);
    assert!(!ui.app.runtime.app.has_pending());
    ui.app.query = "保存後應清空".into();
    ui.click(ui.point("匯入"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(
        ui.app.runtime.app.current, original,
        "only an acknowledged save may update the vault"
    );
    wait_runtime(&mut ui);
    assert_eq!(ui.app.runtime.app.current.as_deref().unwrap().revision, 1);
    assert_eq!(ui.app.model.group_name(ui.app.group), Some("匯入來源分組"));
    let kept = ui.app.model.entry(existing.id).unwrap();
    assert!(std::ptr::eq(&*kept.record, &*existing.record));
    assert_eq!(
        ui.app.otp_cache.code(&kept.record, IMPORT_TEST_TIME),
        existing_code
    );
    assert_eq!(ui.app.otp_cache.calculations, calculations);
    assert!(ui.app.query.is_empty());
    let saved = ui.app.runtime.app.current.as_deref().unwrap();
    assert_eq!(
        saved.service_time_sync.battlenet.as_ref(),
        Some(&expected_clock)
    );
    let imported: Vec<_> = saved
        .authenticators
        .iter()
        .filter(|record| record.group_id == source.groups[0].id)
        .collect();
    assert_eq!(imported, source.authenticators.iter().collect::<Vec<_>>());
    assert_eq!(
        &saved.authenticators[..original.as_ref().unwrap().authenticators.len()],
        original.as_ref().unwrap().authenticators.as_slice()
    );
    assert!(ui.app.runtime.pending_import.is_none());
    assert_eq!(
        ui.app.runtime.sync_requests, 0,
        "the calibration saved with the group import skips automatic synchronization"
    );
}

#[test]
fn same_name_import_creates_the_next_available_group_and_preserves_existing_records() {
    let mut source = source();
    source.groups[0].name = "PTR".into();
    let mut original = VaultSnapshot::new_empty();
    let mut existing_group = source.groups[0].clone();
    existing_group.sort_order = 1;
    original.groups.push(existing_group);
    original.authenticators.clone_from(&source.authenticators);

    let mut ui = GroupUi::new();
    ui.use_memory_vault(original);
    for destination in ["PTR(2)", "PTR(3)"] {
        let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
        show_loaded(&mut ui, &source);
        assert_eq!(
            ui.points(&format!("將建立「{destination}」分組。")).len(),
            1
        );
        assert_eq!(ui.points("匯入").len(), 1);
        assert!(!ui.app.runtime.app.has_pending());
        ui.click(ui.point("匯入"));
        assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
        wait_runtime(&mut ui);

        let saved = ui.app.runtime.app.current.as_deref().unwrap();
        let imported_group = saved
            .groups
            .iter()
            .find(|group| group.name == destination)
            .unwrap();
        assert_eq!(saved.groups.len(), original.groups.len() + 1);
        assert_eq!(
            &saved.groups[..original.groups.len()],
            original.groups.as_slice()
        );
        assert_eq!(saved.revision, original.revision + 1);
        assert_eq!(ui.app.model.group_name(ui.app.group), Some(destination));
        let (previous, added) = saved.authenticators.split_at(original.authenticators.len());
        assert_eq!(previous, original.authenticators.as_slice());
        assert_eq!(added.len(), source.authenticators.len());
        for (index, (record, source_record)) in
            added.iter().zip(source.authenticators.iter()).enumerate()
        {
            assert_ne!(record.id, source_record.id);
            assert_eq!(record.group_id, imported_group.id);
            assert_eq!(record.sort_order, index as u32);
            assert_eq!(record.name, source_record.name);
            assert_eq!(record.secret, source_record.secret);
        }
    }
}

#[test]
fn cancelled_picker_keeps_preview_but_a_failed_load_clears_it_and_releases_the_close_guard() {
    let (mut ui, _) = preview_ui();
    let original = ui.app.runtime.app.current.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.group_import_rx = Some(rx);
    assert!(ui.app.runtime.close_blocked());
    ui.frame(Vec::new());
    ui.click(ui.point("×"));
    assert!(matches!(ui.app.dialog, Some(Dialog::GroupImport(_))));
    tx.send(Ok(None)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(matches!(&ui.app.dialog, Some(Dialog::GroupImport(form)) if form.loaded.is_some()));
    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.group_import_rx = Some(rx);
    let error = bnauth_core::ErrorCode::CorruptFile.notice().to_string();
    tx.send(Err(error.clone())).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    ui.frame(Vec::new());
    assert!(ui.points("匯入").is_empty());
    assert_eq!(ui.points(&error).len(), 1);
    assert!(!ui.app.runtime.close_blocked());
    assert_eq!(ui.app.runtime.app.current, original);
}

#[test]
fn empty_group_file_is_rejected_and_another_file_can_use_the_last_slot() {
    let mut source = source();
    let mut ui = GroupUi::new();
    let mut target = large_list_vault();
    target.authenticators.pop().unwrap();
    source.groups[0].name = target.groups[1].name.clone();
    source.authenticators.truncate(1);
    let mut empty = source.clone();
    empty.authenticators.clear();
    ui.use_memory_vault(target.clone());
    show_loaded(&mut ui, &empty);
    assert_eq!(ui.points("分組為空，無可匯入的驗證器").len(), 1);
    assert!(matches!(&ui.app.dialog, Some(Dialog::GroupImport(form)) if form.loaded.is_none()));
    assert!(ui.points("匯入").is_empty());
    assert!(!ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&target));

    select_group_file(&mut ui, &source);
    wait_group_import_preview(&mut ui);
    assert!(ui.points("分組為空，無可匯入的驗證器").is_empty());
    assert_eq!(ui.points("分組 Group 1 共 1 個驗證器。").len(), 1);
    ui.click(ui.point("匯入"));
    wait_runtime(&mut ui);
    let saved = ui.app.runtime.app.current.as_deref().unwrap();
    assert_eq!(saved.groups.len(), target.groups.len() + 1);
    assert_eq!(saved.authenticators.len(), bnauth_core::MAX_ENTRIES);
    assert_eq!(saved.revision, target.revision + 1);
    assert_eq!(ui.app.model.group_name(ui.app.group), Some("Group 1(2)"));
}

struct FailOnce {
    attempts: Arc<AtomicUsize>,
}
impl bnauth_app::Repository for FailOnce {
    fn commit(&mut self, candidate: &VaultSnapshot) -> Result<bnauth_app::CommitOutcome, String> {
        candidate.validate()?;
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err("人工磁碟保存失敗".into());
        }
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
fn failed_import_preserves_the_candidate_and_retry_or_discard_does_not_duplicate_data() {
    for retry in [false, true] {
        let (mut ui, _) = preview_ui();
        let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker_attempts = attempts.clone();
        let worker_vault = original.clone();
        ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
            Ok((
                Box::new(FailOnce {
                    attempts: worker_attempts.clone(),
                }),
                Some(worker_vault.clone()),
            ))
        }));
        crate::test_support::drain_memory_app(&mut ui.app.runtime.app);
        ui.click(ui.point("匯入"));
        wait_runtime(&mut ui);
        assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
        assert!(ui.app.runtime.app.has_pending());
        assert!(ui.app.runtime.pending_import.is_some());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        if retry {
            ui.app.runtime.app.retry().unwrap();
            wait_runtime(&mut ui);
            assert_eq!(
                ui.app
                    .runtime
                    .app
                    .current
                    .as_deref()
                    .unwrap()
                    .authenticators
                    .len(),
                original.authenticators.len() + 2
            );
            assert_eq!(ui.app.model.group_name(ui.app.group), Some("匯入來源分組"));
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
        } else {
            let token = ui.app.model.change_token();
            let record = ui.app.model.entries[0].record.clone();
            let code = ui.app.otp_cache.code(&record, IMPORT_TEST_TIME);
            let calculations = ui.app.otp_cache.calculations;
            ui.view = TestView::Saving;
            ui.frame(Vec::new());
            assert!(!ui.points(bnauth_core::WRITE_FAILED_NOTICE).is_empty());
            ui.click(ui.point("放棄"));
            assert!(ui.app.runtime.pending_import.is_none());
            ui.app.poll_runtime(&ui.ctx);
            assert!(!ui.app.runtime.app.has_pending() && !ui.app.runtime.app.saving);
            assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
            assert_eq!(ui.app.model.change_token(), token);
            assert_eq!(ui.app.otp_cache.code(&record, IMPORT_TEST_TIME), code);
            assert_eq!(ui.app.otp_cache.calculations, calculations);
            assert_eq!(ui.app.group, 0);
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
        }
        assert!(ui.app.runtime.pending_import.is_none());
    }
}
