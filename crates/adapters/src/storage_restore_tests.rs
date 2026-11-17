use super::*;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("BNAuth-restore-test-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
    fn primary(&self) -> PathBuf {
        self.0.join("BNAuth.bnauth")
    }
    fn copies(&self) -> Vec<PathBuf> {
        fs::read_dir(&self.0)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .filter(|path| {
                path.file_name()
                    .unwrap()
                    .to_string_lossy()
                    .starts_with("BNAuth-before-restore-")
            })
            .collect()
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if self.0.parent() == Some(std::env::temp_dir().as_path())
            && self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("BNAuth-restore-test-")
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}
const T: i64 = 1_800_000_000_000;
fn replacement(original: &VaultSnapshot) -> VaultSnapshot {
    let mut source = VaultSnapshot::new_empty(T);
    source.groups.push(bnauth_core::Group {
        id: Uuid::new_v4(),
        name: " 原分組名稱 ".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    source.groups.push(bnauth_core::Group {
        id: Uuid::new_v4(),
        name: "空組".into(),
        sort_order: 2,
        is_ungrouped: false,
    });
    source = bnauth_core::prepare_import(
        &source,
        " 原標題 ",
        " 完整備註\r\n第二行 ",
        source.groups[1].id,
        bnauth_core::parse_manual(
            "3132333435363738393031323334353637383930",
            "US123456789012",
            "AbCd123456",
        )
        .unwrap(),
        T,
    )
    .unwrap()
    .candidate;
    let mut missing = source.authenticators[0].clone();
    missing.id = Uuid::new_v4();
    missing.name = "缺密鑰紀錄".into();
    missing.secret = None;
    missing.sort_order = 1;
    source.authenticators.push(missing);
    source.authenticators[0]
        .legacy_metadata
        .push(bnauth_core::LegacyMetadata {
            source: "fixture".into(),
            source_version: None,
            field: "原文".into(),
            original_value: "歷史資料\t完整".into(),
            unit: None,
        });
    source.vault_id = original.vault_id;
    source.revision = original.revision;
    source.created_at_ms = original.created_at_ms;
    source
}

#[test]
fn replacement_keeps_verified_previous_bytes_and_complete_restored_data_after_reopen() {
    let dir = Directory::new();
    let (mut repo, _) = Repository::open(dir.primary()).unwrap();
    let original = repo.commit(&VaultSnapshot::new_empty(T)).unwrap().snapshot;
    let old_bytes = fs::read(dir.primary()).unwrap();
    let candidate = replacement(&original);
    let source = dir.0.join("完整備份.bnauth");
    let source_bytes = native::encode(&candidate).unwrap();
    fs::write(&source, &source_bytes).unwrap();
    let before_restore = native::json_decode_calls();
    let restored = repo.restore(&candidate).unwrap();
    // Only the installed primary is parsed. Source/copy identity, hashes and
    // exact copy readback already prove preservation of the validated old bytes.
    assert_eq!(native::json_decode_calls() - before_restore, 1);
    assert!(restored.warning.is_none());
    let copy = repo.restore_backup().unwrap().to_path_buf();
    assert_eq!(dir.copies().as_slice(), std::slice::from_ref(&copy));
    assert_eq!(fs::read(&copy).unwrap(), old_bytes);
    assert_eq!(native::decode(&fs::read(&copy).unwrap()).unwrap(), original);
    assert_eq!(restored.snapshot.revision, original.revision + 1);
    assert_eq!(restored.snapshot.vault_id, original.vault_id);
    assert_eq!(restored.snapshot.groups, candidate.groups);
    assert_eq!(restored.snapshot.authenticators, candidate.authenticators);
    assert_eq!(fs::read(source).unwrap(), source_bytes.as_slice());
    drop(repo);
    let (repo, loaded) = Repository::open(dir.primary()).unwrap();
    assert_eq!(loaded, Some(restored.snapshot));
    assert_eq!(fs::read(copy).unwrap(), old_bytes);
    drop(repo);
}

#[test]
fn first_restore_creates_primary_without_a_previous_copy() {
    let dir = Directory::new();
    let (mut repo, loaded) = Repository::open(dir.primary()).unwrap();
    assert!(loaded.is_none());
    let restored = repo
        .restore(&replacement(&VaultSnapshot::new_empty(T)))
        .unwrap()
        .snapshot;
    assert_eq!(restored.revision, 1);
    assert!(repo.restore_backup().is_none());
    assert!(dir.copies().is_empty());
    drop(repo);
    let (repo, loaded) = Repository::open(dir.primary()).unwrap();
    assert_eq!(loaded, Some(restored));
    drop(repo);
}

#[test]
fn failing_to_create_or_flush_the_copy_never_stages_or_replaces_the_primary() {
    for fault in [FaultPoint::BeforeRestoreCopy, FaultPoint::RestoreCopyFlush] {
        let dir = Directory::new();
        let (mut repo, _) = Repository::open(dir.primary()).unwrap();
        let original = repo.commit(&VaultSnapshot::new_empty(T)).unwrap().snapshot;
        let bytes = fs::read(dir.primary()).unwrap();
        repo.fault = Some(Fault::At(fault));
        assert!(repo.restore(&replacement(&original)).is_err());
        assert_eq!(repo.current.as_ref(), Some(&original));
        assert_eq!(fs::read(dir.primary()).unwrap(), bytes);
        assert!(dir.copies().is_empty());
        assert!(find_remnants(&dir.0).unwrap().is_empty());
        assert!(repo.restore_backup().is_none());
        assert!(!repo.cleanup_required() && !repo.is_blocked());
    }
}

#[test]
fn failed_restore_commit_retains_and_reuses_the_same_verified_copy_on_retry() {
    let dir = Directory::new();
    let (mut repo, _) = Repository::open(dir.primary()).unwrap();
    let original = repo.commit(&VaultSnapshot::new_empty(T)).unwrap().snapshot;
    let bytes = fs::read(dir.primary()).unwrap();
    let candidate = replacement(&original);
    repo.fault = Some(Fault::At(FaultPoint::BeforeReplace));
    let before_failure = native::json_decode_calls();
    assert!(repo.restore(&candidate).is_err());
    assert_eq!(native::json_decode_calls() - before_failure, 0);
    let copies = dir.copies();
    assert_eq!(copies.len(), 1);
    assert_eq!(fs::read(&copies[0]).unwrap(), bytes);
    assert_eq!(fs::read(dir.primary()).unwrap(), bytes);
    assert_eq!(repo.current.as_ref(), Some(&original));
    repo.fault = None;
    let before_retry = native::json_decode_calls();
    repo.restore(&candidate).unwrap();
    assert_eq!(native::json_decode_calls() - before_retry, 1);
    assert_eq!(dir.copies(), copies);
    assert_eq!(repo.restore_backup(), Some(copies[0].as_path()));
    assert_eq!(fs::read(&copies[0]).unwrap(), bytes);
}

#[test]
fn restore_sharing_violation_keeps_primary_validation_and_reuses_copy_on_retry() {
    use std::os::windows::fs::OpenOptionsExt;
    use windows_sys::Win32::Storage::FileSystem::FILE_SHARE_READ;
    let dir = Directory::new();
    let (mut repo, _) = Repository::open(dir.primary()).unwrap();
    let original = repo.commit(&VaultSnapshot::new_empty(T)).unwrap().snapshot;
    let bytes = fs::read(dir.primary()).unwrap();
    let candidate = replacement(&original);
    let blocker = OpenOptions::new()
        .read(true)
        .share_mode(FILE_SHARE_READ)
        .open(dir.primary())
        .unwrap();
    let before_failure = native::json_decode_calls();
    let error = repo.restore(&candidate).unwrap_err();
    // Keep both complete primary validations on recovery while avoiding any
    // parse of the already verified original or independent safety copy.
    assert_eq!(native::json_decode_calls() - before_failure, 2);
    assert!(error.contains("原有資料已確認保留"));
    let copies = dir.copies();
    assert_eq!(copies.len(), 1);
    assert_eq!(fs::read(&copies[0]).unwrap(), bytes);
    assert_eq!(fs::read(dir.primary()).unwrap(), bytes);
    assert_eq!(repo.current.as_ref(), Some(&original));
    assert!(!repo.cleanup_required() && !repo.is_blocked());
    assert!(find_remnants(&dir.0).unwrap().is_empty());
    drop(blocker);
    let before_retry = native::json_decode_calls();
    let restored = repo.restore(&candidate).unwrap();
    assert_eq!(native::json_decode_calls() - before_retry, 1);
    assert_eq!(restored.snapshot.groups, candidate.groups);
    assert_eq!(restored.snapshot.authenticators, candidate.authenticators);
    assert_eq!(dir.copies(), copies);
    assert_eq!(repo.restore_backup(), Some(copies[0].as_path()));
    assert_eq!(fs::read(&copies[0]).unwrap(), bytes);
}

#[test]
fn modified_previous_copy_or_invalid_candidate_stops_restore_without_touching_either_file() {
    let dir = Directory::new();
    let (mut repo, _) = Repository::open(dir.primary()).unwrap();
    let original = repo.commit(&VaultSnapshot::new_empty(T)).unwrap().snapshot;
    let bytes = fs::read(dir.primary()).unwrap();
    let mut candidate = replacement(&original);
    candidate.revision += 1;
    let before_invalid = native::json_decode_calls();
    assert!(repo.restore(&candidate).is_err());
    assert_eq!(native::json_decode_calls() - before_invalid, 0);
    assert!(dir.copies().is_empty());
    candidate.revision -= 1;
    repo.fault = Some(Fault::At(FaultPoint::BeforeWrite));
    assert!(repo.restore(&candidate).is_err());
    let copy = dir.copies().remove(0);
    fs::write(&copy, b"external modification").unwrap();
    repo.fault = None;
    let before_modified = native::json_decode_calls();
    assert!(repo.restore(&candidate).is_err());
    assert_eq!(native::json_decode_calls() - before_modified, 0);
    assert_eq!(fs::read(copy).unwrap(), b"external modification");
    assert_eq!(fs::read(dir.primary()).unwrap(), bytes);
    assert_eq!(repo.current.as_ref(), Some(&original));
}
