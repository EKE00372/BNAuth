use super::*;
use crate::app::full_restore::RestoreFile;
use bnauth_core::{Group, Uuid, VaultSnapshot};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(super) fn source() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty();
    for (index, name) in [" 原備份分組 ", "來源空分組"].into_iter().enumerate() {
        vault.groups.push(Group {
            id: Uuid::new_v4(),
            name: name.into(),
            sort_order: index as u32 + 1,
            is_ungrouped: false,
        });
    }
    let mut record = bnauth_core::prepare_import(
        &vault,
        "原始第一筆",
        " 完整備註\r\n原文 ",
        vault.groups[1].id,
        bnauth_core::parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AbCd123456",
        )
        .unwrap(),
    )
    .unwrap()
    .record();
    vault.authenticators.push(record.clone());
    record.id = Uuid::new_v4();
    record.name = "原始第二筆".into();
    record.secret = bnauth_core::Secret::from_bytes([2; 20]);
    record.serial = Some("US123456789013".into());
    record.sort_order = 1;
    vault.authenticators.insert(0, record);
    vault.validate().unwrap();
    vault
}

pub(super) fn keep_restore_offline(ui: &mut GroupUi) {
    // Restore tests drive storage and supply any needed clock samples locally.
    ui.app.runtime.recovery.dispatch = Some(Box::new(|_, _, _| {
        Err("restore tests do not use live services")
    }));
}

pub(super) fn load_preview(ui: &mut GroupUi, source: &VaultSnapshot) {
    let preview =
        bnauth_core::prepare_full_restore(ui.app.runtime.app.current.as_deref(), source).unwrap();
    ui.app.open_full_restore(&ui.ctx);
    if let Some(Dialog::FullRestore(form)) = &mut ui.app.dialog {
        form.loaded = Some(RestoreFile {
            path: "完整來源.bnauth".into(),
            preview,
        });
    } else {
        panic!("Expected full restore preview");
    }
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
}

#[test]
fn full_backup_counts_all_saved_data_and_cancelling_never_changes_query_or_selection() {
    let mut ui = GroupUi::new();
    let original = ui.app.runtime.app.current.clone().unwrap();
    let selected = ui.app.model.entries[0].id;
    ui.app.selected.insert(selected);
    ui.app.group = ui.app.model.groups[2].id;
    let group = ui.app.group;
    ui.app.query = "不匹配任何驗證器".into();
    ui.app.open_full_backup(&ui.ctx);
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    assert_eq!(
        ui.points(&format!(
        "匯出所有資料，共 {} 個分組、{} 個驗證器。",
            original.groups.len(),
            original.authenticators.len()
        ))
        .len(),
        1
    );
    assert_eq!(ui.points("另存新檔").len(), 1);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
    assert!(ui.app.runtime.export_job.is_none());
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert_eq!(*ui.app.selected, BTreeSet::from([selected]));
    assert_eq!(ui.app.group, group);
    assert_eq!(ui.app.query, "不匹配任何驗證器");
}

#[test]
fn replacement_requires_explicit_confirmation_and_only_resets_ui_and_clock_after_save_ack() {
    let mut ui = GroupUi::new();
    keep_restore_offline(&mut ui);
    let original = ui.app.runtime.app.current.clone();
    let source = source();
    let selected = ui.app.model.entries[0].id;
    ui.app.selected.insert(selected);
    ui.app.group = ui.app.model.groups[2].id;
    ui.app.query = "保存後才清空".into();
    ui.app.runtime.clock.note_failure("舊校時錯誤".into());
    let (_tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(rx);
    load_preview(&mut ui, &source);
    assert_eq!(ui.points("備份包含 3 個分組、2 個驗證器。").len(), 1);
    assert_eq!(ui.points("2.  原備份分組  · 2 個").len(), 1);
    assert_eq!(ui.points("3. 來源空分組 · 0 個").len(), 1);
    ui.click(ui.point("還原"));
    assert!(
        ui.app.dialog.is_some(),
        "replacement must still await its checkbox"
    );
    assert!(!ui.app.runtime.app.saving);
    assert_eq!(ui.app.runtime.app.current, original);
    ui.click(ui.point("我已知曉目前資料將被全部取代。"));
    ui.click(ui.point("還原"));
    assert!(ui.app.dialog.is_none());
    assert!(ui.app.runtime.pending_full_restore);
    assert_eq!(ui.app.runtime.app.current, original);
    assert_eq!(ui.app.query, "保存後才清空");
    assert_eq!(ui.app.runtime.clock.last_error(), Some("舊校時錯誤"));
    wait_runtime(&mut ui);
    let restored = ui.app.runtime.app.current.as_deref().unwrap();
    assert_eq!(restored.groups, source.groups);
    assert_eq!(restored.authenticators, source.authenticators);
    assert_eq!(restored.revision, original.as_ref().unwrap().revision + 1);
    assert_eq!(restored.vault_id, original.as_ref().unwrap().vault_id);
    assert_eq!(ui.app.group, 0);
    assert!(ui.app.query.is_empty());
    assert!(ui.app.selected.is_empty());
    assert!(ui.app.runtime.clock.last_error().is_none());
    assert!(!ui.app.runtime.clock.is_synced());
    assert!(ui.app.runtime.sync_rx.is_none());
    assert!(!ui.app.runtime.pending_full_restore);
}

#[test]
fn first_restore_needs_no_replace_checkbox_and_creates_a_complete_database_after_ack() {
    let mut ui = GroupUi::new();
    // The in-memory repository has no disk state; the missing phase represents
    // the production repository opened with no fixed primary file present.
    ui.use_memory_vault(VaultSnapshot::new_empty());
    keep_restore_offline(&mut ui);
    ui.app.runtime.app.current = None;
    ui.app.runtime.app.phase = bnauth_app::Phase::Missing;
    let source = source();
    load_preview(&mut ui, &source);
    assert!(ui.points("我已知曉目前資料將被全部取代。").is_empty());
    assert_eq!(ui.points("將在 EXE 同目錄建立 BNAuth.bnauth。").len(), 1);
    ui.click(ui.point("還原"));
    assert!(ui.app.runtime.app.current.is_none());
    assert!(ui.app.runtime.app.saving);
    wait_runtime(&mut ui);
    let restored = ui.app.runtime.app.current.as_deref().unwrap();
    assert_eq!(restored.groups, source.groups);
    assert_eq!(restored.authenticators, source.authenticators);
    assert_ne!(restored.vault_id, source.vault_id);
    assert_eq!(restored.revision, 1);
    assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Ready);
    assert!(!ui.app.runtime.pending_full_restore);
}

#[test]
fn cancelled_or_failed_read_preserves_the_vault_and_releases_the_close_guard() {
    let mut ui = GroupUi::new();
    let original = ui.app.runtime.app.current.clone();
    load_preview(&mut ui, &source());
    let Some(Dialog::FullRestore(form)) = &mut ui.app.dialog else {
        panic!("Expected full restore preview");
    };
    let previous_error = "資料庫已變更，請重新選擇備份並預覽。";
    form.error = previous_error.into();
    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.full_restore_rx = Some(rx);
    assert!(ui.app.runtime.close_blocked());
    ui.frame(Vec::new());
    ui.click(ui.point("×"));
    assert!(matches!(ui.app.dialog, Some(Dialog::FullRestore(_))));
    tx.send(Ok(None)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(matches!(&ui.app.dialog, Some(Dialog::FullRestore(form))
        if form.loaded.is_some() && form.error == previous_error));
    assert!(!ui.app.runtime.close_blocked());

    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.full_restore_rx = Some(rx);
    let error = bnauth_core::ErrorCode::ReadFailed.notice().to_string();
    tx.send(Err(error.clone())).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(ui.app.runtime.full_restore_rx.is_none());
    assert!(!ui.app.runtime.close_blocked());
    assert!(
        matches!(&ui.app.dialog, Some(Dialog::FullRestore(form)) if form.loaded.is_none() && !form.confirmed)
    );
    ui.frame(Vec::new());
    assert_eq!(ui.points(&error).len(), 1);
    assert!(ui.points("還原").is_empty());
    assert_eq!(ui.app.runtime.app.current, original);
}

struct FailRestoreOnce {
    attempts: Arc<AtomicUsize>,
}

impl bnauth_app::Repository for FailRestoreOnce {
    fn commit(&mut self, _candidate: &VaultSnapshot) -> Result<bnauth_app::CommitOutcome, String> {
        panic!("A restore retry must retain its restore operation");
    }

    fn restore_from_preview(
        &mut self,
        candidate: &VaultSnapshot,
    ) -> Result<bnauth_app::CommitOutcome, String> {
        if self.attempts.fetch_add(1, Ordering::SeqCst) == 0 {
            return Err("人工還原保存失敗".into());
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
fn failed_restore_keeps_its_candidate_for_retry_and_discard_clears_pending_ui_intent() {
    for retry in [false, true] {
        let mut ui = GroupUi::new();
        keep_restore_offline(&mut ui);
        let mut original = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let mut backup = source();
        let calibration = bnauth_core::TimeSyncRecord {
            source: bnauth_core::TIME_SOURCE.into(),
            server_time_ms: IMPORT_TEST_TIME + 500,
            local_wall_ms_at_sample: IMPORT_TEST_TIME,
            offset_ms: 500,
            rtt_ms: 0,
        };
        original.service_time_sync.battlenet = Some(calibration.clone());
        backup.service_time_sync.battlenet = Some(calibration.clone());
        let attempts = Arc::new(AtomicUsize::new(0));
        let worker_attempts = attempts.clone();
        let worker_vault = original.clone();
        ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
            Ok((
                Box::new(FailRestoreOnce {
                    attempts: worker_attempts.clone(),
                }),
                Some(worker_vault.clone()),
            ))
        }));
        crate::test_support::drain_memory_app(&mut ui.app.runtime.app);
        ui.app.load_shared_clock();
        ui.app.query = "失敗時保留搜尋".into();
        load_preview(&mut ui, &backup);
        ui.click(ui.point("我已知曉目前資料將被全部取代。"));
        ui.click(ui.point("還原"));
        wait_runtime(&mut ui);
        assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
        assert!(ui.app.runtime.app.has_pending());
        assert!(ui.app.runtime.pending_full_restore);
        assert_eq!(ui.app.query, "失敗時保留搜尋");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        ui.view = TestView::Saving;
        ui.frame(Vec::new());
        assert!(!ui.points(bnauth_core::WRITE_FAILED_NOTICE).is_empty());
        if retry {
            let previous = bnauth_core::clock::ClockObservation::at(
                bnauth_core::now_ms() + 3_600_000,
                std::time::Instant::now(),
            );
            ui.app.runtime.clock = bnauth_core::clock::RuntimeClock::from_saved_observation(
                Some(&calibration),
                previous,
            )
            .unwrap();
            ui.app.poll_runtime(&ui.ctx);
            assert_eq!(ui.app.runtime.clock.last_success(), Some(&calibration));
            assert_eq!(ui.app.runtime.app.pending_time_sync(), Some(&calibration));
            ui.click(ui.point("重試"));
            wait_runtime(&mut ui);
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
            let restored = ui.app.runtime.app.current.as_deref().unwrap();
            assert_eq!(restored.authenticators.len(), 2);
            assert_eq!(
                restored.service_time_sync.battlenet.as_ref(),
                Some(&calibration)
            );
            assert!(ui.app.query.is_empty());
        } else {
            let token = ui.app.model.change_token();
            let record = ui.app.model.entries[0].record.clone();
            let code = ui.app.otp_cache.code(&record, IMPORT_TEST_TIME);
            let calculations = ui.app.otp_cache.calculations;
            ui.click(ui.point("放棄"));
            assert!(!ui.app.runtime.pending_full_restore);
            ui.app.poll_runtime(&ui.ctx);
            assert!(!ui.app.runtime.app.has_pending() && !ui.app.runtime.app.saving);
            assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
            assert_eq!(ui.app.model.change_token(), token);
            assert_eq!(ui.app.otp_cache.code(&record, IMPORT_TEST_TIME), code);
            assert_eq!(ui.app.otp_cache.calculations, calculations);
            assert_eq!(ui.app.query, "失敗時保留搜尋");
            assert_eq!(attempts.load(Ordering::SeqCst), 1);
        }
        assert!(!ui.app.runtime.pending_full_restore);
    }
}
