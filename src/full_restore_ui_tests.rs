use super::*;
use crate::app::full_restore::RestoreFile;
use bnauth_core::{Group, Uuid, VaultSnapshot};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

pub(super) fn source() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty(IMPORT_TEST_TIME);
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
        IMPORT_TEST_TIME,
    )
    .unwrap()
    .candidate
    .authenticators
    .remove(0);
    record.secret = None;
    vault.authenticators.push(record.clone());
    record.id = Uuid::new_v4();
    record.name = "原始第二筆".into();
    record.sort_order = 1;
    vault.authenticators.insert(0, record);
    vault.validate().unwrap();
    vault
}

pub(super) fn load_preview(ui: &mut GroupUi, source: &VaultSnapshot) {
    let preview = bnauth_core::prepare_full_restore(
        ui.app.runtime.app.current.as_ref(),
        source,
        IMPORT_TEST_TIME + 1,
    )
    .unwrap();
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
            "將備份全部 {} 個分組、{} 個驗證器。",
            original.groups.len(),
            original.authenticators.len()
        ))
        .len(),
        1
    );
    assert_eq!(ui.points("另存完整備份…").len(), 1);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert_eq!(ui.app.selected, BTreeSet::from([selected]));
    assert_eq!(ui.app.query, "不匹配任何驗證器");
    assert_eq!(ui.app.group, group);
    assert!(!ui.app.runtime.app.saving);
    assert!(ui.app.runtime.export_rx.is_none());
}

#[test]
fn replacement_requires_explicit_confirmation_and_only_resets_ui_and_clock_after_save_ack() {
    let mut ui = GroupUi::new();
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
    ui.click(ui.point("確認還原並保存"));
    assert!(
        ui.app.dialog.is_some(),
        "replacement must still await its checkbox"
    );
    assert!(!ui.app.runtime.app.saving);
    assert_eq!(ui.app.runtime.app.current, original);
    ui.click(ui.point("我確認以備份取代目前全部資料"));
    ui.click(ui.point("確認還原並保存"));
    assert!(ui.app.dialog.is_none());
    assert!(ui.app.runtime.pending_full_restore);
    assert_eq!(ui.app.runtime.app.current, original);
    assert_eq!(ui.app.query, "保存後才清空");
    assert_eq!(ui.app.runtime.clock.last_error(), Some("舊校時錯誤"));
    wait_runtime(&mut ui);
    let restored = ui.app.runtime.app.current.as_ref().unwrap();
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
    ui.use_memory_vault(VaultSnapshot::new_empty(IMPORT_TEST_TIME));
    ui.app.runtime.app.current = None;
    ui.app.runtime.app.phase = bnauth_app::Phase::Missing;
    let source = source();
    load_preview(&mut ui, &source);
    assert!(ui.points("我確認以備份取代目前全部資料").is_empty());
    assert_eq!(ui.points("將在 EXE 同目錄建立 BNAuth.bnauth。").len(), 1);
    ui.click(ui.point("確認還原並保存"));
    assert!(ui.app.runtime.app.current.is_none());
    assert!(ui.app.runtime.app.saving);
    wait_runtime(&mut ui);
    let restored = ui.app.runtime.app.current.as_ref().unwrap();
    assert_eq!(restored.groups, source.groups);
    assert_eq!(restored.authenticators, source.authenticators);
    assert_ne!(restored.vault_id, source.vault_id);
    assert_eq!(restored.revision, 1);
    assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Ready);
    assert!(!ui.app.runtime.pending_full_restore);
}

#[test]
fn cancelled_or_failed_reader_preserves_the_vault_and_disconnect_releases_shutdown_guard() {
    let mut ui = GroupUi::new();
    let original = ui.app.runtime.app.current.clone();
    load_preview(&mut ui, &source());
    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.full_restore_rx = Some(rx);
    assert!(ui.app.runtime.close_blocked());
    ui.frame(Vec::new());
    ui.click(ui.point("×"));
    assert!(matches!(ui.app.dialog, Some(Dialog::FullRestore(_))));
    tx.send(Ok(None)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(matches!(&ui.app.dialog, Some(Dialog::FullRestore(form)) if form.loaded.is_some()));
    assert!(!ui.app.runtime.close_blocked());
    for disconnected in [false, true] {
        let (tx, rx) = std::sync::mpsc::channel();
        ui.app.runtime.full_restore_rx = Some(rx);
        ui.app.runtime.close_requested = true;
        if !disconnected {
            tx.send(Err("人工備份讀取失敗".into())).unwrap();
        }
        drop(tx);
        ui.app.poll_runtime(&ui.ctx);
        assert!(ui.app.runtime.full_restore_rx.is_none());
        assert!(!ui.app.runtime.close_requested);
        assert!(
            matches!(&ui.app.dialog, Some(Dialog::FullRestore(form)) if form.loaded.is_none() && !form.confirmed)
        );
        ui.frame(Vec::new());
        assert!(ui.points("確認還原並保存").is_empty());
        assert_eq!(ui.app.runtime.app.current, original);
    }
}

struct FailRestoreOnce {
    attempts: Arc<AtomicUsize>,
}

impl bnauth_app::Repository for FailRestoreOnce {
    fn commit(&mut self, _candidate: &VaultSnapshot) -> Result<bnauth_app::CommitOutcome, String> {
        panic!("A restore retry must retain its restore operation");
    }

    fn restore(&mut self, candidate: &VaultSnapshot) -> Result<bnauth_app::CommitOutcome, String> {
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
        let original = ui.app.runtime.app.current.clone().unwrap();
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
        ui.app.query = "失敗時保留搜尋".into();
        load_preview(&mut ui, &source());
        ui.click(ui.point("我確認以備份取代目前全部資料"));
        ui.click(ui.point("確認還原並保存"));
        wait_runtime(&mut ui);
        assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
        assert!(ui.app.runtime.app.has_pending());
        assert!(ui.app.runtime.pending_full_restore);
        assert_eq!(ui.app.query, "失敗時保留搜尋");
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        if retry {
            ui.app.runtime.app.retry().unwrap();
            wait_runtime(&mut ui);
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
            assert_eq!(
                ui.app
                    .runtime
                    .app
                    .current
                    .as_ref()
                    .unwrap()
                    .authenticators
                    .len(),
                2
            );
            assert!(ui.app.query.is_empty());
        } else {
            ui.app.runtime.app.discard();
            ui.app.poll_runtime(&ui.ctx);
            assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
            assert_eq!(ui.app.query, "失敗時保留搜尋");
        }
        assert!(!ui.app.runtime.pending_full_restore);
    }
}
