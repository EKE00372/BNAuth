use super::*;
use std::process::{Command, Stdio};

impl Repository {
    pub(super) fn assert_acknowledged(&self, expected: &VaultSnapshot) {
        let current = self.current.expect("an existing primary is acknowledged");
        assert_eq!(current, AcknowledgedPrimary::new(expected, current.stamp));
        let bytes = native::encode(expected).unwrap();
        assert_eq!(current.stamp.hash[..], Sha256::digest(&*bytes)[..]);
    }
}

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
    VaultSnapshot::new_empty()
}

fn add_test_group(snapshot: &mut VaultSnapshot, name: &str) {
    let sort_order = snapshot.groups.len() as u32;
    snapshot.groups.push(bnauth_core::Group {
        id: Uuid::new_v4(),
        name: name.into(),
        sort_order,
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
fn metadata_commit_preserves_complete_records_and_primary_guard_after_rollback_cleanup() {
    use bnauth_core::TimeSyncRecord;

    const TIME: i64 = 1_800_000_000_000;
    let mut candidate = VaultSnapshot::new_empty();
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
    )
    .unwrap()
    .candidate(&candidate)
    .unwrap();
    let sync = TimeSyncRecord {
        source: bnauth_core::TIME_SOURCE.into(),
        server_time_ms: TIME + 42,
        local_wall_ms_at_sample: TIME,
        offset_ms: 42,
        rtt_ms: 100,
    };
    candidate.service_time_sync.battlenet = Some(sync);
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
    assert_eq!(second.snapshot, expected);
    repo.assert_acknowledged(&expected);
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
    let mut legacy = original;
    legacy.groups[0].name = "未分組".into();
    add_test_group(&mut legacy, "預設");
    add_test_group(&mut legacy, "預設(2)");
    let compact = serde_json::to_vec(&legacy).unwrap();
    assert!(fs::write(dir.vault(), &compact).is_err());
    drop(repo);
    fs::write(dir.vault(), &compact).unwrap();
    let (mut repo, loaded) = Repository::open(dir.vault()).unwrap();
    let mut expected = legacy;
    expected.groups[0].name = "預設".into();
    expected.groups[1].name = "預設(3)".into();
    assert_eq!(loaded.as_ref(), Some(&expected));
    assert_eq!(fs::read(dir.vault()).unwrap(), compact);

    add_test_group(&mut expected, "改名後新增分組");
    let saved = repo.commit_edit(&expected).unwrap();
    expected.revision += 1;
    assert_eq!(saved.snapshot, expected);
    assert_eq!(native::read_full(&dir.vault()).unwrap(), expected);
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
    repo.assert_acknowledged(&first);
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
    // Readback verified the original's identity and all its bytes, retaining its guard.
    assert_eq!(native::json_decode_calls() - before_failure, 0);
    assert_eq!(error, ErrorCode::FileInUse.notice().to_string());
    assert_eq!(fs::read(dir.vault()).unwrap(), old);
    repo.assert_acknowledged(&original);
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
    assert_eq!(saved, expected);
    repo.assert_acknowledged(&expected);
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
        let error = repo.commit(&changed).unwrap_err();
        assert_eq!(error, ErrorCode::WriteFailed.notice().to_string());
        assert_eq!(fs::read(dir.vault()).unwrap(), old, "{fault:?}");
        repo.assert_acknowledged(&first);
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
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;

    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    let original_bytes = fs::read(dir.vault()).unwrap();
    let mut candidate = original.clone();
    add_test_group(&mut candidate, "保存變更");
    repo.fault = Some(Fault::PartialWriteAndCleanup);
    assert_eq!(
        repo.commit(&candidate).unwrap_err(),
        ErrorCode::WriteFailed.notice().to_string()
    );
    assert!(repo.cleanup_required());
    assert!(!repo.is_blocked());
    repo.assert_acknowledged(&original);
    assert_eq!(fs::read(dir.vault()).unwrap(), original_bytes);
    assert_primary_is_read_only(&dir.vault());
    assert_eq!(find_remnants(&dir.0).unwrap().len(), 1);
    assert!(repo.commit(&candidate).unwrap_err().contains("清理暫存檔"));
    let residue = dir.0.join(repo.cleanup_pending[0].path.file_name().unwrap());
    let cleanup_notice = ErrorCode::FileInUse.manual_delete(residue.display()).to_string();
    assert_eq!(repo.retry_cleanup().unwrap_err(), cleanup_notice);
    repo.fault = None;
    let blocker = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(&residue)
        .unwrap();
    assert_eq!(repo.retry_cleanup().unwrap_err(), cleanup_notice);
    assert!(repo.cleanup_required());
    assert!(!repo.is_blocked());
    drop(blocker);
    repo.retry_cleanup().unwrap();
    assert!(!repo.cleanup_required());
    assert!(!repo.is_blocked());
    repo.assert_acknowledged(&original);
    assert_eq!(fs::read(dir.vault()).unwrap(), original_bytes);
    assert!(find_remnants(&dir.0).unwrap().is_empty());

    repo.fault = Some(Fault::PartialWriteAndCleanup);
    assert!(repo.commit(&candidate).is_err());
    let residue = repo.cleanup_pending[0].path.clone();
    repo.defer_cleanup();
    assert!(!repo.cleanup_required());
    assert!(residue.exists());
    assert_eq!(fs::read(dir.vault()).unwrap(), original_bytes);
    repo.fault = None;
    let saved = repo.commit(&candidate).unwrap().snapshot;
    assert!(residue.exists());
    drop(repo);

    let (_repo, loaded) = Repository::open(dir.vault()).unwrap();
    assert_eq!(loaded, Some(saved));
    assert!(!residue.exists());
}

#[test]
fn startup_restores_a_retained_rollback_when_the_primary_is_missing() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    let mut next = original.clone();
    add_test_group(&mut next, "保存變更");
    repo.fault = Some(Fault::At(FaultPoint::Cleanup));
    let outcome = repo.commit(&next).unwrap();
    assert!(outcome.warning.is_none());
    assert_eq!(outcome.snapshot.revision, 2);
    assert_eq!(native::read(&dir.vault()).unwrap().0, outcome.snapshot);
    assert!(!repo.cleanup_required());
    assert_eq!(find_remnants(&dir.0).unwrap().len(), 1);
    assert_primary_is_read_only(&dir.vault());
    repo.fault = None;
    let saved = repo.commit(&outcome.snapshot).unwrap().snapshot;
    assert_eq!(saved.revision, 3);
    drop(repo);

    let held_primary = dir.0.join("held-primary.bnauth");
    fs::rename(dir.vault(), &held_primary).unwrap();
    let (mut repo, loaded) = Repository::open(dir.vault()).unwrap();
    assert_eq!(loaded.as_ref(), Some(&original));
    assert_eq!(native::read(&dir.vault()).unwrap().0, original);
    assert_eq!(native::read(&held_primary).unwrap().0, saved);
    assert!(find_remnants(&dir.0).unwrap().is_empty());
    assert_primary_is_read_only(&dir.vault());
    let mut edited = original;
    add_test_group(&mut edited, "復原後可保存");
    let updated = repo.commit(&edited).unwrap().snapshot;
    assert_eq!(native::read(&dir.vault()).unwrap().0, updated);
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
    repo.assert_acknowledged(&original);
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
fn native_files_with_null_or_missing_secrets_are_rejected_and_preserved() {
    let dir = TestDirectory::new();
    let primary = dir.vault();
    let group_file = dir.0.join("原生分組.bnauth");
    let vault = empty();
    let group = vault.groups[0].id;
    let vault = bnauth_core::prepare_import(
        &vault,
        "人工驗證器",
        "",
        group,
        bnauth_core::parse_manual("3132333435363738393031323334353637383930", "", "").unwrap(),
    )
    .unwrap()
    .candidate(&vault)
    .unwrap();
    let exported = vault.export_group(group).unwrap();
    for null_secret in [true, false] {
        for (path, snapshot, read) in [
            (&primary, &vault, native::read_full as fn(&Path) -> _),
            (&group_file, &exported, native::read_group),
        ] {
            let mut json = serde_json::to_value(snapshot).unwrap();
            let record = json["authenticators"][0].as_object_mut().unwrap();
            if null_secret {
                *record.get_mut("secret").unwrap() = serde_json::Value::Null;
            } else {
                record.remove("secret").unwrap();
            }
            let bytes = serde_json::to_vec_pretty(&json).unwrap();
            fs::write(path, &bytes).unwrap();
            assert_eq!(read(path).unwrap_err(), ErrorCode::CorruptFile.notice().to_string());
            if path == &primary {
                let error = Repository::open(primary.clone()).err().unwrap();
                assert_eq!(error, ErrorCode::CorruptFile.notice().to_string());
            }
            assert_eq!(fs::read(path).unwrap(), bytes);
        }
    }
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
}

#[test]
fn startup_cleans_recovery_files_with_a_readable_primary_or_restores_the_latest_valid_file() {
    use std::os::windows::fs::OpenOptionsExt;

    let dir = TestDirectory::new();
    let residue = dir.0.join(format!(".BNAuth.bnauth.{}.tmp", Uuid::new_v4()));
    let bytes = native::encode(&empty()).unwrap();
    fs::write(&residue, &*bytes).unwrap();
    let (repo, loaded) = Repository::open(dir.vault()).unwrap();
    assert!(loaded.is_none());
    assert!(!residue.exists());
    assert!(!dir.vault().exists());
    drop(repo);

    let backup = residue.with_extension("BAK");
    let other = dir.0.join(".BNAuth.bnauth.notes.tmp");
    let rollback = residue.with_extension("rollback");
    let recovery = residue.with_extension("recovery");
    let legacy_marker = residue.with_extension("cleanup");
    fs::write(&backup, &*bytes).unwrap();
    fs::write(&other, b"notes").unwrap();
    fs::write(&residue, b"partial save").unwrap();
    fs::write(&rollback, &*bytes).unwrap();
    fs::write(&recovery, b"partial recovery").unwrap();
    fs::write(&legacy_marker, b"legacy cleanup marker").unwrap();
    let primary = empty();
    fs::write(dir.vault(), &*native::encode(&primary).unwrap()).unwrap();
    let (repo, loaded) = Repository::open(dir.vault()).unwrap();
    assert_eq!(loaded.as_ref(), Some(&primary));
    assert!(!residue.exists());
    assert!(!rollback.exists());
    assert!(!recovery.exists());
    assert!(!legacy_marker.exists());
    assert_eq!(fs::read(backup).unwrap(), *bytes);
    assert_eq!(fs::read(other).unwrap(), b"notes");
    drop(repo);

    fs::write(dir.vault(), b"damaged primary").unwrap();
    fs::write(&rollback, &*bytes).unwrap();
    fs::write(&recovery, &*bytes).unwrap();
    let error = Repository::open(dir.vault()).err().unwrap();
    assert_eq!(error, ErrorCode::CorruptFile.notice().to_string());
    assert_eq!(fs::read(dir.vault()).unwrap(), b"damaged primary");
    assert_eq!(fs::read(&rollback).unwrap(), *bytes);
    assert_eq!(fs::read(&recovery).unwrap(), *bytes);

    fs::rename(dir.vault(), dir.0.join("damaged-primary.bnauth")).unwrap();
    fs::rename(&recovery, dir.0.join("held-recovery.bnauth")).unwrap();
    let occupied_rollback = OpenOptions::new()
        .read(true)
        .share_mode(0)
        .open(&rollback)
        .unwrap();
    let error = Repository::open(dir.vault()).err().unwrap();
    assert_eq!(error, ErrorCode::FileInUse.notice().to_string());
    assert!(!dir.vault().exists());
    drop(occupied_rollback);
    assert_eq!(fs::read(&rollback).unwrap(), *bytes);

    fs::write(&rollback, b"damaged rollback").unwrap();
    fs::write(&recovery, b"damaged recovery").unwrap();
    let error = Repository::open(dir.vault()).err().unwrap();
    assert_eq!(error, ErrorCode::CorruptFile.notice().to_string());
    assert!(!dir.vault().exists());
    assert_eq!(fs::read(&rollback).unwrap(), b"damaged rollback");
    assert_eq!(fs::read(&recovery).unwrap(), b"damaged recovery");

    let mut previous = primary;
    previous.revision = 4;
    let mut latest = previous.clone();
    latest.revision = 5;
    add_test_group(&mut latest, "較新的復原資料");
    fs::write(&rollback, &*native::encode(&previous).unwrap()).unwrap();
    fs::write(&recovery, &*native::encode(&latest).unwrap()).unwrap();
    let (_repo, loaded) = Repository::open(dir.vault()).unwrap();
    assert_eq!(loaded.as_ref(), Some(&latest));
    assert_eq!(native::read(&dir.vault()).unwrap().0, latest);
    assert!(!rollback.exists());
    assert!(!recovery.exists());
}

#[test]
fn repository_mutex_child() {
    let Some(path) = std::env::var_os("BNAUTH_STORAGE_LOCK_CHILD") else {
        return;
    };
    assert_eq!(
        Repository::open(PathBuf::from(path)).err(),
        Some(ErrorCode::FileInUse.notice().to_string())
    );
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
    repo.fault = Some(Fault::PartialWriteAndCleanup);
    assert!(repo.commit(&original).is_err());
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
    repo.assert_acknowledged(&original);
    assert_eq!(native::read(&dir.vault()).unwrap().0, original);
    assert!(repo.commit(&original).is_err());
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
    repo.fault = Some(Fault::PartialWriteAndCleanup);
    assert!(repo.commit(&original).is_err());
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
            let acknowledged = repo.current;

            repo.fault = Some(Fault::TempCreate(kind));
            let error = repo.commit(&candidate).unwrap_err();
            let expected = if kind == std::io::ErrorKind::PermissionDenied {
                ErrorCode::PermissionDenied
            } else {
                ErrorCode::WriteFailed
            };
            assert_eq!(error, expected.notice().to_string());
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
    assert!(error.contains("已保留在原位置"));
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
            repo.assert_acknowledged(&candidate);
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
fn verified_rollback_prevents_external_writes_through_post_commit_cleanup() {
    let dir = TestDirectory::new();
    let (mut repo, _) = Repository::open(dir.vault()).unwrap();
    let original = repo.commit(&empty()).unwrap().snapshot;
    repo.fault = Some(Fault::ExternalChangeBeforeCleanup);
    let outcome = repo.commit(&original).unwrap();
    assert_eq!(outcome.snapshot.revision, 2);
    assert!(outcome.warning.is_none());
    assert_eq!(native::read(&dir.vault()).unwrap().0, outcome.snapshot);
    repo.assert_acknowledged(&outcome.snapshot);
    assert!(!repo.blocked);
    assert!(!repo.cleanup_required());
    assert!(find_remnants(&dir.0).unwrap().is_empty());
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
    repo.assert_acknowledged(&original);
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
        let previous = repo.current;
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
    repo.assert_acknowledged(&original);
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
