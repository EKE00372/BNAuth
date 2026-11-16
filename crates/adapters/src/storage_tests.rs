use super::*;
use std::process::{Command, Stdio};

struct TestDirectory(PathBuf);
impl TestDirectory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("BNAuth-storage-test-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn vault(&self) -> PathBuf {
        self.0.join("BNAuth.bnauth")
    }
}
impl Drop for TestDirectory {
    fn drop(&mut self) {
        // Only the unique test directory created by this fixture is removed.
        if self.0.parent() == Some(std::env::temp_dir().as_path())
            && self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("BNAuth-storage-test-")
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
fn empty() -> VaultSnapshot {
    VaultSnapshot::new_empty(now_ms())
}

fn add_test_group(snapshot: &mut VaultSnapshot, name: &str) {
    snapshot.groups.push(bnauth_core::Group {
        id: Uuid::new_v4(),
        name: name.into(),
        sort_order: snapshot.groups.len() as u32,
        is_ungrouped: false,
    });
}

fn assert_primary_is_read_only(path: &Path) {
    assert!(fs::read(path).is_ok(), "external reads remain available");
    assert!(
        OpenOptions::new().write(true).open(path).is_err(),
        "external write handle must be refused"
    );
    assert!(
        fs::remove_file(path).is_err(),
        "external deletion must be refused"
    );
    assert!(
        fs::rename(path, path.with_extension("moved")).is_err(),
        "external rename must be refused"
    );
}

#[test]
fn primary_guard_allows_reads_blocks_changes_and_releases_on_repository_drop() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let first = repo.commit(&empty()).unwrap().snapshot;
    assert_primary_is_read_only(&dir.vault());
    let mut changed = first;
    add_test_group(&mut changed, "保存變更");
    let second = repo.commit(&changed).unwrap().snapshot;
    assert_eq!(second.revision, 2);
    assert_primary_is_read_only(&dir.vault());
    let bytes = fs::read(dir.vault()).unwrap();
    drop(repo);
    fs::write(dir.vault(), &bytes).unwrap();
    let moved = dir.0.join("closed.moved");
    fs::rename(dir.vault(), &moved).unwrap();
    fs::rename(&moved, dir.vault()).unwrap();
    let writer = OpenOptions::new().write(true).open(dir.vault()).unwrap();
    assert!(
        Repository::open(dir.vault()).is_err(),
        "an existing writer prevents acquiring a protected primary"
    );
    drop(writer);
    let (repo, loaded) = Repository::open(dir.vault()).unwrap();
    assert_eq!(loaded.as_ref(), Some(&second));
    assert_primary_is_read_only(&dir.vault());
    drop(repo);
    fs::remove_file(dir.vault()).unwrap();
}

#[test]
fn create_commit_reopen_preserves_snapshot_and_leaves_only_main_file() {
    let dir = TestDirectory::new();
    let (mut repo, loaded) = Repository::open(dir.vault()).unwrap();
    assert!(loaded.is_none());
    let first = repo.commit(&empty()).unwrap();
    assert_eq!(first.snapshot.revision, 1);
    assert!(first.warning.is_none());
    let mut changed = first.snapshot.clone();
    add_test_group(&mut changed, "保存變更");
    let second = repo.commit(&changed).unwrap();
    assert_eq!(second.snapshot.revision, 2);
    assert_eq!(second.snapshot.vault_id, first.snapshot.vault_id);
    drop(repo);
    let (repo, reloaded) = Repository::open(dir.vault()).unwrap();
    assert_eq!(reloaded.unwrap(), second.snapshot);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    drop(repo);
}

#[test]
fn metadata_commit_preserves_complete_records_and_primary_guard_after_rollback_cleanup() {
    use bnauth_core::{LegacyMetadata, Provenance, RecoveryEvent, TimeSyncRecord};

    const TIME: i64 = 1_800_000_000_000;
    let mut candidate = VaultSnapshot::new_empty(TIME);
    candidate = bnauth_core::prepare_import(
        &candidate,
        "  人工驗證器  ",
        "原始備註\n保留原文",
        candidate.groups[0].id,
        bnauth_core::parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AB12CD34EF",
        )
        .unwrap(),
        TIME,
    )
    .unwrap()
    .candidate;
    let record = &mut candidate.authenticators[0];
    record.recovery_history.push(RecoveryEvent {
        at_ms: TIME,
        operation: "fixture".into(),
        outcome: "retained".into(),
        confirmation: Some("人工紀錄".into()),
    });
    record.legacy_metadata.push(LegacyMetadata {
        source: "fixture".into(),
        source_version: Some("1".into()),
        field: "original_text".into(),
        original_value: "  原始內容\n  ".into(),
        unit: Some("text".into()),
    });
    let sync = TimeSyncRecord {
        source: bnauth_core::TIME_SOURCE.into(),
        server_time_ms: TIME + 42,
        local_wall_ms_at_sample: TIME,
        offset_ms: 42,
        rtt_ms: 100,
    };
    candidate.service_time_sync.battlenet = Some(sync.clone());
    candidate.provenance.push(Provenance {
        source_vault_id: Uuid::new_v4(),
        source_revision: 3,
        scope: "full".into(),
        imported_at_ms: TIME,
        source_time_sync: Some(sync),
    });
    candidate.validate().unwrap();

    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let first = repo.commit(&candidate).unwrap().snapshot;
    assert_primary_is_read_only(&dir.vault());
    let mut changed = first.clone();
    changed.authenticators[0].description = "只有備註變更".into();
    let second = repo.commit(&changed).unwrap();
    assert!(second.warning.is_none());
    let mut expected = changed;
    expected.revision += 1;
    expected.updated_at_ms = second.snapshot.updated_at_ms;
    assert_eq!(second.snapshot, expected);
    assert_eq!(repo.current.as_ref(), Some(&expected));
    assert!(!repo.is_blocked());
    assert!(!repo.cleanup_required());
    assert!(find_remnants(&dir.0).unwrap().is_empty());
    assert_primary_is_read_only(&dir.vault());
    drop(repo);

    let (repo, reopened) = Repository::open(dir.vault()).unwrap();
    assert_eq!(reopened.as_ref(), Some(&expected));
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    assert_primary_is_read_only(&dir.vault());
    drop(repo);
}

#[test]
fn external_text_reformat_requires_closed_repository_and_can_then_be_loaded() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    let compact = serde_json::to_vec(&original).unwrap();
    assert!(fs::write(dir.vault(), &compact).is_err());
    drop(repo);
    fs::write(dir.vault(), &compact).unwrap();
    assert_eq!(fs::read(dir.vault()).unwrap(), compact);
    let (_, loaded) = Repository::open(dir.vault()).unwrap();
    assert_eq!(loaded.as_ref(), Some(&original));
}

#[test]
fn stale_revision_invalid_candidate_and_external_changes_do_not_overwrite() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let first = repo.commit(&empty()).unwrap().snapshot;
    let bytes = fs::read(dir.vault()).unwrap();
    let mut bad = first.clone();
    bad.revision = 0;
    assert!(repo.commit(&bad).is_err());
    bad = first.clone();
    bad.groups.clear();
    assert!(repo.commit(&bad).is_err());
    assert_eq!(fs::read(dir.vault()).unwrap(), bytes);
    assert_eq!(repo.current.as_ref(), Some(&first));
    assert!(!repo.cleanup_required());
    assert!(!repo.is_blocked());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    assert_primary_is_read_only(&dir.vault());
    let mut external = first.clone();
    add_test_group(&mut external, "外部修改");
    external.revision += 1;
    let external_bytes = native::encode(&external).unwrap();
    assert!(fs::write(dir.vault(), &*external_bytes).is_err());
    assert_eq!(fs::read(dir.vault()).unwrap(), bytes);
    drop(repo);
    fs::write(dir.vault(), &*external_bytes).unwrap();
    let (mut repo, loaded) = Repository::open(dir.vault()).unwrap();
    assert_eq!(loaded.as_ref(), Some(&external));
    assert!(
        repo.commit(&first)
            .unwrap_err()
            .contains("與上次讀取的資料檔不符")
    );
    assert_eq!(fs::read(dir.vault()).unwrap(), *external_bytes);
}

#[test]
fn first_create_never_overwrites_a_file_created_after_open() {
    for external in [
        native::encode(&empty()).unwrap().to_vec(),
        b"external file is not valid JSON".to_vec(),
    ] {
        let dir = TestDirectory::new();
        let (mut repo, _) = Repository::open(dir.vault()).unwrap();
        fs::write(dir.vault(), &external).unwrap();
        assert!(
            repo.commit(&empty())
                .unwrap_err()
                .contains("已由其他程式建立")
        );
        assert_eq!(fs::read(dir.vault()).unwrap(), external);
        assert!(repo.current.is_none());
        assert!(!repo.cleanup_required());
        assert!(find_remnants(&dir.0).unwrap().is_empty());
    }
}

#[test]
fn stamp_reader_detects_changed_bytes_and_replacement_with_identical_bytes() {
    let dir = TestDirectory::new();
    let path = dir.vault();
    assert!(read_stamp_locked(&path).unwrap().is_none());
    // Fingerprinting an unexpected file must not depend on it being valid JSON.
    fs::write(&path, b"external first version").unwrap();
    let first = read_stamp_locked(&path).unwrap().unwrap();
    assert_eq!(read_stamp_locked(&path).unwrap(), Some(first));
    fs::write(&path, b"external second version").unwrap();
    let changed = read_stamp_locked(&path).unwrap().unwrap();
    assert_eq!(changed.identity, first.identity);
    assert_ne!(changed.hash, first.hash);

    // Keep the original file alive so the OS cannot reuse its identity.
    fs::rename(&path, dir.0.join("original.bin")).unwrap();
    fs::write(&path, b"external second version").unwrap();
    let replaced = read_stamp_locked(&path).unwrap().unwrap();
    assert_eq!(replaced.hash, changed.hash);
    assert_ne!(replaced.identity, changed.identity);
    assert_ne!(replaced, changed);
}

#[test]
fn sharing_violation_keeps_main_file_and_can_retry_after_handle_closes() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let mut changed = repo.commit(&empty()).unwrap().snapshot;
    let original = changed.clone();
    let old = fs::read(dir.vault()).unwrap();
    let blocker = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(dir.vault())
        .unwrap();
    add_test_group(&mut changed, "保存變更");
    let before_failure = native::json_decode_calls();
    let error = repo.commit(&changed).unwrap_err();
    // Keep the complete result readback and reacquire validation. Fingerprint-only
    // recovery checks must not allocate and decode another full snapshot.
    assert_eq!(native::json_decode_calls() - before_failure, 2);
    assert!(error.contains("原有資料已確認保留"));
    assert_eq!(fs::read(dir.vault()).unwrap(), old);
    assert_eq!(repo.current.as_ref(), Some(&original));
    assert!(!repo.is_blocked());
    assert!(!repo.cleanup_required());
    assert!(find_remnants(&dir.0).unwrap().is_empty());
    drop(blocker);
    assert_primary_is_read_only(&dir.vault());
    let before_retry = native::json_decode_calls();
    let saved = repo.commit(&changed).unwrap().snapshot;
    assert_eq!(native::json_decode_calls() - before_retry, 1);
    let mut expected = changed;
    expected.revision += 1;
    expected.updated_at_ms = saved.updated_at_ms;
    assert_eq!(saved, expected);
    assert_eq!(repo.current.as_ref(), Some(&expected));
    assert_primary_is_read_only(&dir.vault());
    assert!(find_remnants(&dir.0).unwrap().is_empty());
}

#[test]
fn injected_write_flush_stage_replace_and_readback_failures_preserve_old_commit() {
    for fault in [
        Fault::At(FaultPoint::BeforeWrite),
        Fault::PartialWrite,
        Fault::At(FaultPoint::BeforeFlush),
        Fault::At(FaultPoint::AfterStage),
        Fault::At(FaultPoint::BeforeReplace),
        Fault::Replace1176,
        Fault::Replace1177,
        Fault::At(FaultPoint::ReadBack),
    ] {
        let dir = TestDirectory::new();
        let (mut repo, _) = Repository::open(dir.vault()).unwrap();
        let first = repo.commit(&empty()).unwrap().snapshot;
        let old = fs::read(dir.vault()).unwrap();
        let mut changed = first.clone();
        add_test_group(&mut changed, "保存變更");
        repo.fault = Some(fault);
        assert!(repo.commit(&changed).is_err(), "{fault:?}");
        assert_eq!(fs::read(dir.vault()).unwrap(), old, "{fault:?}");
        assert_eq!(repo.current.as_ref(), Some(&first));
        assert_primary_is_read_only(&dir.vault());
        assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1, "{fault:?}");
        repo.fault = None;
        let saved = repo.commit(&changed).unwrap();
        assert_eq!(saved.snapshot.revision, 2);
    }
}

#[test]
fn failed_first_save_can_retry_without_leaving_a_false_commit() {
    for fault in [
        Fault::PartialWrite,
        Fault::At(FaultPoint::BeforeFlush),
        Fault::At(FaultPoint::BeforeReplace),
        Fault::At(FaultPoint::ReadBack),
    ] {
        let dir = TestDirectory::new();
        let (mut repo, _) = Repository::open(dir.vault()).unwrap();
        let candidate = empty();
        repo.fault = Some(fault);
        assert!(repo.commit(&candidate).is_err());
        assert!(!dir.vault().exists());
        assert!(repo.current.is_none());
        repo.fault = None;
        assert_eq!(repo.commit(&candidate).unwrap().snapshot.revision, 1);
    }
}

#[test]
fn failed_save_reports_retained_cleanup_until_retry_without_committing() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    let original_bytes = fs::read(dir.vault()).unwrap();
    let mut candidate = original.clone();
    add_test_group(&mut candidate, "保存變更");
    repo.fault = Some(Fault::PartialWriteAndCleanup);
    assert!(
        repo.commit(&candidate)
            .unwrap_err()
            .contains("暫存檔尚未移除")
    );
    assert!(repo.cleanup_required());
    assert!(!repo.is_blocked());
    assert_eq!(repo.current.as_ref(), Some(&original));
    assert_eq!(fs::read(dir.vault()).unwrap(), original_bytes);
    assert_primary_is_read_only(&dir.vault());
    assert_eq!(find_remnants(&dir.0).unwrap().len(), 1);
    assert!(
        repo.commit(&candidate)
            .unwrap_err()
            .contains("重試移除暫存檔")
    );
    repo.fault = None;
    repo.retry_cleanup().unwrap();
    assert!(!repo.cleanup_required());
    assert!(!repo.is_blocked());
    assert_eq!(repo.current.as_ref(), Some(&original));
    assert_eq!(fs::read(dir.vault()).unwrap(), original_bytes);
    assert!(find_remnants(&dir.0).unwrap().is_empty());
}

#[test]
fn cleanup_failure_reports_committed_state_and_retry_only_removes_residue() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let mut next = repo.commit(&empty()).unwrap().snapshot;
    add_test_group(&mut next, "保存變更");
    repo.fault = Some(Fault::At(FaultPoint::Cleanup));
    let outcome = repo.commit(&next).unwrap();
    assert!(outcome.warning.unwrap().contains("已保存"));
    assert_eq!(outcome.snapshot.revision, 2);
    assert_eq!(native::read(&dir.vault()).unwrap().0, outcome.snapshot);
    assert!(!repo.cleanup_pending.is_empty());
    assert!(
        repo.commit(&outcome.snapshot)
            .unwrap_err()
            .contains("重試移除暫存檔")
    );
    assert_eq!(repo.current.as_ref().unwrap().revision, 2);
    assert_primary_is_read_only(&dir.vault());
    repo.fault = None;
    repo.retry_cleanup().unwrap();
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    assert_eq!(repo.current.as_ref().unwrap().revision, 2);
    assert_primary_is_read_only(&dir.vault());
}

#[test]
fn external_write_in_replace_window_preserves_its_rollback_and_blocks_further_writes() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    let mut candidate = original.clone();
    add_test_group(&mut candidate, "保存變更");
    repo.fault = Some(Fault::ExternalChangeBeforeReplace);
    let error = repo.commit(&candidate).unwrap_err();
    assert!(error.contains("外部修改"));
    assert!(repo.blocked);
    assert_eq!(repo.current.as_ref(), Some(&original));
    let leftovers = find_remnants(&dir.0).unwrap();
    assert_eq!(leftovers.len(), 1);
    assert_eq!(leftovers[0].extension().unwrap(), "rollback");
    let protected_external = native::read(&leftovers[0]).unwrap().0;
    assert_eq!(protected_external.groups.len(), 2);
    assert_eq!(protected_external.groups[1].name, "外部修改");
    assert_eq!(protected_external.revision, original.revision + 1);
    assert_eq!(
        native::read(&dir.vault()).unwrap().0.groups,
        candidate.groups
    );
    repo.fault = None;
    assert!(repo.commit(&candidate).is_err());
    assert_eq!(native::read(&leftovers[0]).unwrap().0, protected_external);
}

#[test]
fn same_path_case_and_parent_alias_are_single_writer_even_on_same_thread() {
    let dir = TestDirectory::new();
    let (repo, _) = Repository::open(dir.vault()).unwrap();
    assert!(Repository::open(dir.vault()).is_err());
    let alias = dir
        .0
        .join("..")
        .join(dir.0.file_name().unwrap())
        .join("BNAuth.bnauth");
    assert!(Repository::open(alias).is_err());
    let alias = PathBuf::from(dir.0.to_string_lossy().to_uppercase()).join("BNAuth.bnauth");
    assert!(Repository::open(alias).is_err());
    let other = dir.vault();
    assert!(
        std::thread::spawn(move || Repository::open(other).is_err())
            .join()
            .unwrap()
    );
    drop(repo);
    assert!(Repository::open(dir.vault()).is_ok());
}

#[test]
fn hardlinked_main_and_corrupt_or_newer_main_are_not_replaced() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    repo.commit(&empty()).unwrap();
    drop(repo);
    fs::hard_link(dir.vault(), dir.0.join("copy.bnauth")).unwrap();
    assert!(Repository::open(dir.vault()).is_err());
    fs::remove_file(dir.0.join("copy.bnauth")).unwrap();
    let mut future = serde_json::to_value(native::read(&dir.vault()).unwrap().0).unwrap();
    future["schema_version"] = serde_json::json!(2);
    let bytes = serde_json::to_vec_pretty(&future).unwrap();
    fs::write(dir.vault(), &bytes).unwrap();
    assert!(Repository::open(dir.vault()).is_err());
    assert_eq!(fs::read(dir.vault()).unwrap(), bytes);
}

#[test]
fn unexpected_recovery_files_are_not_auto_promoted_or_deleted() {
    let dir = TestDirectory::new();
    let residue = dir.0.join(".BNAuth.bnauth.interrupted.tmp");
    let bytes = native::encode(&empty()).unwrap();
    fs::write(&residue, &*bytes).unwrap();
    assert!(Repository::open(dir.vault()).is_err());
    assert!(!dir.vault().exists());
    assert_eq!(fs::read(residue).unwrap(), *bytes);
}

#[test]
fn repository_mutex_child() {
    let Some(path) = std::env::var_os("BNAUTH_STORAGE_LOCK_CHILD") else {
        return;
    };
    assert!(Repository::open(PathBuf::from(path)).is_err());
}

#[test]
fn cross_process_global_mutex_rejects_second_writer() {
    let dir = TestDirectory::new();
    let (repo, _) = Repository::open(dir.vault()).unwrap();
    let status = Command::new(std::env::current_exe().unwrap())
        .arg("--exact")
        .arg("storage::tests::repository_mutex_child")
        .env("BNAUTH_STORAGE_LOCK_CHILD", dir.vault())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .unwrap();
    assert!(status.success());
    drop(repo);
}

#[test]
fn cleanup_retry_keeps_an_externally_modified_residue_and_blocks() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    repo.fault = Some(Fault::At(FaultPoint::Cleanup));
    let committed = repo.commit(&original).unwrap().snapshot;
    let residue = repo.cleanup_pending[0].path.clone();
    fs::write(&residue, b"external replacement during pending cleanup").unwrap();
    repo.fault = None;
    let error = repo.retry_cleanup().unwrap_err();
    assert!(error.contains("外部修改"));
    assert!(repo.blocked);
    assert_eq!(
        fs::read(&residue).unwrap(),
        b"external replacement during pending cleanup"
    );
    assert_eq!(repo.current.as_ref(), Some(&committed));
    assert_eq!(native::read(&dir.vault()).unwrap().0, committed);
    assert!(repo.commit(&committed).is_err());
    assert!(repo.retry_cleanup().is_err());
    assert_eq!(
        fs::read(residue).unwrap(),
        b"external replacement during pending cleanup"
    );
}

#[test]
fn cleanup_retry_rejects_a_different_file_even_with_identical_bytes() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    repo.fault = Some(Fault::At(FaultPoint::Cleanup));
    repo.commit(&original).unwrap();
    let residue = repo.cleanup_pending[0].path.clone();
    let bytes = fs::read(&residue).unwrap();
    // Keep the original inode alive so its file ID cannot be immediately reused.
    fs::rename(&residue, dir.0.join("externally-moved-original.bin")).unwrap();
    fs::write(&residue, &bytes).unwrap();
    repo.fault = None;
    assert!(repo.retry_cleanup().unwrap_err().contains("外部修改"));
    assert!(repo.blocked);
    assert_eq!(fs::read(residue).unwrap(), bytes);
}

#[test]
fn temp_creation_error_keeps_primary_and_allows_retry_without_cleanup() {
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::Other,
    ] {
        for existing in [false, true] {
            let dir = TestDirectory::new();
            let (mut repo, _) = Repository::open(dir.vault()).unwrap();
            let candidate = if existing {
                repo.commit(&empty()).unwrap().snapshot
            } else {
                empty()
            };
            let original = existing.then(|| fs::read(dir.vault()).unwrap());
            let acknowledged = repo.current.clone();

            repo.fault = Some(Fault::TempCreate(kind));
            let error = repo.commit(&candidate).unwrap_err();
            assert!(error.contains("無法建立保存所需的暫存檔"));
            assert!(error.contains("模擬暫存檔建立失敗"));
            assert!(!error.contains("已保留"));
            assert!(!error.contains("不是本次保存建立"));
            assert!(!repo.is_blocked());
            assert!(!repo.cleanup_required());
            assert!(find_remnants(&dir.0).unwrap().is_empty());
            assert_eq!(repo.current, acknowledged);
            if let Some(bytes) = original {
                assert_eq!(fs::read(dir.vault()).unwrap(), bytes);
                assert_primary_is_read_only(&dir.vault());
            } else {
                assert!(!dir.vault().exists());
            }

            repo.fault = None;
            let saved = repo.commit(&candidate).unwrap().snapshot;
            assert_eq!(saved.revision, candidate.revision + 1);
            assert_eq!(native::read(&dir.vault()).unwrap().0, saved);
            assert!(find_remnants(&dir.0).unwrap().is_empty());
        }
    }
}

#[test]
fn create_new_failure_does_not_delete_the_file_it_did_not_create() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    repo.fault = Some(Fault::TempAlreadyExists);
    let error = repo.commit(&empty()).unwrap_err();
    assert!(error.contains("已保留，不會刪除"));
    assert!(!error.contains("無法建立保存所需的暫存檔"));
    let remnants = find_remnants(&dir.0).unwrap();
    assert_eq!(remnants.len(), 1);
    assert_eq!(
        fs::read(&remnants[0]).unwrap(),
        b"external pre-existing temp"
    );
    assert!(repo.current.is_none());
    assert!(repo.blocked);
    assert!(!dir.vault().exists());
    assert!(repo.cleanup_pending.is_empty());
    assert!(repo.retry_cleanup().is_err());
    assert_eq!(
        fs::read(&remnants[0]).unwrap(),
        b"external pre-existing temp"
    );
}

#[test]
fn rollback_race_keeps_external_main_bytes_and_old_backup() {
    for existing in [false, true] {
        let dir = TestDirectory::new();
        let (mut repo, _) = Repository::open(dir.vault()).unwrap();
        let candidate = if existing {
            repo.commit(&empty()).unwrap().snapshot
        } else {
            empty()
        };
        let old = existing.then(|| fs::read(dir.vault()).unwrap());
        repo.fault = Some(Fault::ExternalChangeBeforeRecovery);
        assert!(repo.commit(&candidate).is_err());
        assert!(repo.blocked);
        let external_path = if existing {
            find_remnants(&dir.0)
                .unwrap()
                .into_iter()
                .find(|p| p.extension().is_some_and(|s| s == "recovery"))
                .unwrap()
        } else {
            dir.vault()
        };
        assert_eq!(
            fs::read(&external_path).unwrap(),
            b"external recovery content"
        );
        if let Some(old) = old {
            let rollback = find_remnants(&dir.0)
                .unwrap()
                .into_iter()
                .find(|p| p.extension().is_some_and(|s| s == "rollback"))
                .unwrap();
            assert_eq!(fs::read(rollback).unwrap(), old);
            assert_eq!(repo.current.as_ref(), Some(&candidate));
        } else {
            assert!(repo.current.is_none());
        }
        repo.fault = None;
        assert!(repo.retry_cleanup().is_err());
        assert!(repo.commit(&candidate).is_err());
        assert_eq!(
            fs::read(external_path).unwrap(),
            b"external recovery content"
        );
    }
}

#[test]
fn first_create_install_race_never_overwrites_the_external_main() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    repo.fault = Some(Fault::ExternalFirstCreate);
    assert!(repo.commit(&empty()).is_err());
    assert_eq!(
        fs::read(dir.vault()).unwrap(),
        b"external first-create content"
    );
    assert!(repo.current.is_none());
    assert!(repo.blocked);
    assert!(!find_remnants(&dir.0).unwrap().is_empty());
}

#[test]
fn post_commit_cleanup_race_reports_committed_with_preserved_external_residue() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    repo.fault = Some(Fault::ExternalChangeBeforeCleanup);
    let outcome = repo.commit(&original).unwrap();
    assert_eq!(outcome.snapshot.revision, 2);
    assert!(outcome.warning.unwrap().contains("外部修改"));
    assert_eq!(native::read(&dir.vault()).unwrap().0, outcome.snapshot);
    assert_eq!(repo.current.as_ref(), Some(&outcome.snapshot));
    assert!(repo.blocked);
    let residue = &repo.cleanup_pending[0].path;
    assert_eq!(fs::read(residue).unwrap(), b"external cleanup content");
}

#[test]
fn verified_delete_handle_prevents_write_and_rename_until_disposition() {
    let dir = TestDirectory::new();
    let path = dir.0.join("guarded.tmp");
    fs::write(&path, &*native::encode(&empty()).unwrap()).unwrap();
    let mut file = platform::open_guarded(&path, true).unwrap().unwrap();
    inspect_open(&mut file).unwrap();
    assert!(fs::write(&path, b"must not replace verified content").is_err());
    assert!(fs::rename(&path, dir.0.join("renamed.tmp")).is_err());
    platform::delete_open(&file).unwrap();
    drop(file);
    assert!(!path.exists());
    assert!(!dir.0.join("renamed.tmp").exists());
}

#[test]
fn primary_guard_swap_window_still_detects_byte_only_external_changes() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    let compact = serde_json::to_vec(&original).unwrap();
    repo.fault = Some(Fault::ExternalReformatBeforeReplace);
    assert!(repo.commit(&original).unwrap_err().contains("外部修改"));
    assert!(repo.is_blocked());
    assert_eq!(repo.current.as_ref(), Some(&original));
    let rollback = find_remnants(&dir.0)
        .unwrap()
        .into_iter()
        .find(|path| path.extension().is_some_and(|ext| ext == "rollback"))
        .unwrap();
    assert_eq!(fs::read(rollback).unwrap(), compact);
    assert_primary_is_read_only(&dir.vault());
}

#[test]
fn primary_guard_reacquire_failure_preserves_unacknowledged_files_and_blocks() {
    for existing in [false, true] {
        let dir = TestDirectory::new();
        let (mut repo, _) = Repository::open(dir.vault()).unwrap();
        let candidate = if existing {
            repo.commit(&empty()).unwrap().snapshot
        } else {
            empty()
        };
        let previous = repo.current.clone();
        repo.fault = Some(Fault::At(FaultPoint::ReacquirePrimary));
        assert!(
            repo.commit(&candidate)
                .unwrap_err()
                .contains("恢復檔案保護")
        );
        assert!(repo.is_blocked());
        assert!(repo.primary_guard.is_none());
        assert_eq!(repo.current, previous);
        let unacknowledged = fs::read(dir.vault()).unwrap();
        assert_eq!(
            native::read(&dir.vault()).unwrap().0.revision,
            candidate.revision + 1
        );
        if existing {
            assert!(
                find_remnants(&dir.0)
                    .unwrap()
                    .iter()
                    .any(|path| path.extension().is_some_and(|ext| ext == "rollback"))
            );
        }
        repo.fault = None;
        assert!(repo.commit(&candidate).is_err());
        assert!(repo.retry_cleanup().is_err());
        assert_eq!(fs::read(dir.vault()).unwrap(), unacknowledged);
    }
}

#[test]
fn primary_guard_reacquire_after_rollback_failure_retains_recovery_and_blocks() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    let old = fs::read(dir.vault()).unwrap();
    repo.fault = Some(Fault::ReadBackAndReacquire);
    assert!(repo.commit(&original).unwrap_err().contains("恢復檔案保護"));
    assert!(repo.is_blocked());
    assert!(repo.primary_guard.is_none());
    assert_eq!(repo.current.as_ref(), Some(&original));
    assert_eq!(fs::read(dir.vault()).unwrap(), old);
    assert!(
        find_remnants(&dir.0)
            .unwrap()
            .iter()
            .any(|path| path.extension().is_some_and(|ext| ext == "recovery"))
    );
    repo.fault = None;
    assert!(repo.commit(&original).is_err());
    assert!(repo.retry_cleanup().is_err());
}
