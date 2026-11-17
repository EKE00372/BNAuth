use super::*;
use crate::runtime::GroupImportFile;
use bnauth_core::{Group, Uuid, VaultSnapshot};
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};

fn source() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty(IMPORT_TEST_TIME);
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
    let mut record = bnauth_core::prepare_import(
        &vault,
        "第一筆",
        "完整備註",
        group,
        material,
        IMPORT_TEST_TIME,
    )
    .unwrap()
    .candidate
    .authenticators
    .remove(0);
    record.secret = None;
    vault.authenticators.push(record.clone());
    record.id = Uuid::new_v4();
    record.name = "第二筆".into();
    record.sort_order = 1;
    vault.authenticators.push(record);
    vault.export_group(group).unwrap()
}

fn loaded(target: &VaultSnapshot, source: &VaultSnapshot) -> GroupImportFile {
    GroupImportFile {
        path: std::path::PathBuf::from("來源分組.bnauth"),
        group_name: source.groups[0].name.clone(),
        source_count: source.authenticators.len(),
        preview: bnauth_core::prepare_group_import(target, source, IMPORT_TEST_TIME + 1),
    }
}

fn preview_ui() -> (GroupUi, VaultSnapshot) {
    let mut ui = GroupUi::new();
    let target = VaultSnapshot::new_empty(IMPORT_TEST_TIME);
    ui.use_memory_vault(target.clone());
    let source = source();
    ui.app.open_group_import(&ui.ctx);
    if let Some(Dialog::GroupImport(form)) = &mut ui.app.dialog {
        form.loaded = Some(loaded(&target, &source));
    }
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    (ui, source)
}

#[test]
fn preview_cancel_is_read_only_and_confirmation_commits_then_reveals_the_imported_group() {
    let (mut ui, source) = preview_ui();
    let original = ui.app.runtime.app.current.clone();
    assert_eq!(ui.points("來源分組：匯入來源分組").len(), 1);
    assert_eq!(ui.points("來源共 2 個驗證器。").len(), 1);
    assert_eq!(ui.points("1. 第一筆 · 新增").len(), 1);
    assert_eq!(ui.app.runtime.app.current, original);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.runtime.app.current, original);
    ui.app.open_group_import(&ui.ctx);
    if let Some(Dialog::GroupImport(form)) = &mut ui.app.dialog {
        form.loaded = Some(loaded(original.as_ref().unwrap(), &source));
    }
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    ui.app.query = "保存後應清空".into();
    ui.click(ui.point("確認匯入並保存"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(
        ui.app.runtime.app.current, original,
        "only an acknowledged save may update the vault"
    );
    wait_runtime(&mut ui);
    assert_eq!(ui.app.runtime.app.current.as_ref().unwrap().revision, 1);
    assert_eq!(ui.app.model.group_name(ui.app.group), Some("匯入來源分組"));
    assert!(ui.app.query.is_empty());
    let imported: Vec<_> = ui
        .app
        .runtime
        .app
        .current
        .as_ref()
        .unwrap()
        .authenticators
        .iter()
        .collect();
    assert_eq!(imported, source.authenticators.iter().collect::<Vec<_>>());
    assert!(ui.app.runtime.pending_import_group.is_none());
    assert!(
        ui.app.runtime.sync_rx.is_none(),
        "seedless records do not trigger synchronization"
    );
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
    tx.send(Err("人工損壞檔案".into())).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    ui.frame(Vec::new());
    assert!(ui.points("確認匯入並保存").is_empty());
    assert_eq!(ui.points("人工損壞檔案").len(), 1);
    assert!(!ui.app.runtime.close_blocked());
    assert_eq!(ui.app.runtime.app.current, original);
}

#[test]
fn an_import_conflict_has_no_confirm_button_and_an_unchanged_import_has_zero_writes() {
    let (mut ui, source) = preview_ui();
    let target = ui.app.runtime.app.current.clone().unwrap();
    if let Some(Dialog::GroupImport(form)) = &mut ui.app.dialog {
        form.loaded.as_mut().unwrap().preview = Err("來源備註與既有資料衝突".into());
    }
    ui.frame(Vec::new());
    assert!(ui.points("確認匯入並保存").is_empty());
    assert_eq!(ui.points("來源備註與既有資料衝突").len(), 1);
    let preview =
        bnauth_core::prepare_group_import(&target, &source, IMPORT_TEST_TIME + 1).unwrap();
    ui.use_memory_vault(preview.candidate.clone());
    ui.app.open_group_import(&ui.ctx);
    if let Some(Dialog::GroupImport(form)) = &mut ui.app.dialog {
        form.loaded = Some(loaded(&preview.candidate, &source));
    }
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    let original = ui.app.runtime.app.current.clone();
    ui.click(ui.point("完成（未變更）"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.runtime.app.current, original);
    assert!(!ui.app.runtime.app.saving);
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
        let original = ui.app.runtime.app.current.clone().unwrap();
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
        ui.click(ui.point("確認匯入並保存"));
        wait_runtime(&mut ui);
        assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
        assert!(ui.app.runtime.app.has_pending());
        assert_eq!(attempts.load(Ordering::SeqCst), 1);
        if retry {
            ui.app.runtime.app.retry().unwrap();
            wait_runtime(&mut ui);
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
            assert_eq!(ui.app.model.group_name(ui.app.group), Some("匯入來源分組"));
            assert_eq!(attempts.load(Ordering::SeqCst), 2);
        } else {
            ui.app.runtime.app.discard();
            ui.app.poll_runtime(&ui.ctx);
            assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
            assert_eq!(ui.app.group, 0);
        }
        assert!(ui.app.runtime.pending_import_group.is_none());
    }
}

#[test]
fn late_reader_disconnect_is_reported_without_a_stale_candidate_or_stuck_shutdown() {
    let mut ui = GroupUi::new();
    ui.app.open_group_import(&ui.ctx);
    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.group_import_rx = Some(rx);
    ui.app.runtime.close_requested = true;
    drop(tx);
    ui.app.poll_runtime(&ui.ctx);
    assert!(ui.app.runtime.group_import_rx.is_none());
    assert!(!ui.app.runtime.close_requested);
    assert!(matches!(&ui.app.dialog, Some(Dialog::GroupImport(form)) if form.loaded.is_none()));
}
