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
    let mut source = VaultSnapshot::new_empty();
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
    )
    .unwrap()
    .candidate(&source)
    .unwrap();
    let mut copy = source.authenticators[0].clone();
    copy.id = Uuid::new_v4();
    copy.name = "另一組的同密鑰紀錄".into();
    copy.group_id = source.groups[0].id;
    source.authenticators.push(copy);
    source.vault_id = original.vault_id;
    source.revision = original.revision;
    source
}

#[test]
fn replacement_keeps_complete_restored_data_after_reopen_without_making_a_copy() {
    let dir = Directory::new();
    let mut original = replacement(&VaultSnapshot::new_empty());
    original.revision = 1;
    let mut legacy: serde_json::Value =
        serde_json::from_slice(&native::encode(&original).unwrap()).unwrap();
    for record in legacy["authenticators"].as_array_mut().unwrap() {
        assert!(record.get("created_at_ms").is_none() && record.get("updated_at_ms").is_none());
        record["created_at_ms"] = T.into();
        record["updated_at_ms"] = (T + 1).into();
    }
    let old_bytes = serde_json::to_vec(&legacy).unwrap();
    fs::write(dir.primary(), &old_bytes).unwrap();
    let (mut repo, loaded) = Repository::open(dir.primary()).unwrap();
    assert_eq!(loaded.as_ref(), Some(&original));
    assert_eq!(fs::read(dir.primary()).unwrap(), old_bytes);
    let candidate = replacement(&original);
    let source = dir.0.join("完整備份.bnauth");
    let source_bytes = native::encode(&candidate).unwrap();
    fs::write(&source, &source_bytes).unwrap();
    let before_restore = native::json_decode_calls();
    let restored = repo.restore_from_preview(&candidate).unwrap();
    // Only the installed primary is parsed.
    assert_eq!(native::json_decode_calls() - before_restore, 1);
    assert!(restored.warning.is_none());
    assert_eq!(restored.snapshot.revision, original.revision + 1);
    assert_eq!(restored.snapshot.vault_id, original.vault_id);
    assert_eq!(restored.snapshot.groups, candidate.groups);
    assert_eq!(restored.snapshot.authenticators, candidate.authenticators);
    assert_eq!(fs::read(source).unwrap(), source_bytes.as_slice());
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 2);
    drop(repo);
    let (repo, loaded) = Repository::open(dir.primary()).unwrap();
    assert_eq!(loaded, Some(restored.snapshot));
    drop(repo);
}

#[test]
fn first_restore_creates_primary_without_a_previous_copy() {
    let dir = Directory::new();
    let (mut repo, loaded) = Repository::open(dir.primary()).unwrap();
    assert!(loaded.is_none());
    let restored = repo
        .restore_from_preview(&replacement(&VaultSnapshot::new_empty()))
        .unwrap()
        .snapshot;
    assert_eq!(restored.revision, 1);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    drop(repo);
    let (repo, loaded) = Repository::open(dir.primary()).unwrap();
    assert_eq!(loaded, Some(restored));
    drop(repo);
}

#[test]
fn failed_restore_keeps_the_original_primary_until_retry_succeeds() {
    let dir = Directory::new();
    let (mut repo, _) = Repository::open(dir.primary()).unwrap();
    let original = repo.commit(&VaultSnapshot::new_empty()).unwrap().snapshot;
    let bytes = fs::read(dir.primary()).unwrap();
    let candidate = replacement(&original);
    candidate.validate().unwrap();
    repo.fault = Some(Fault::At(FaultPoint::BeforeReplace));
    let before_failure = native::json_decode_calls();
    assert!(repo.restore_from_preview(&candidate).is_err());
    assert_eq!(native::json_decode_calls() - before_failure, 0);
    assert_eq!(fs::read(dir.primary()).unwrap(), bytes);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    repo.assert_acknowledged(&original);
    repo.fault = None;
    let before_retry = native::json_decode_calls();
    let restored = repo.restore_from_preview(&candidate).unwrap();
    assert_eq!(native::json_decode_calls() - before_retry, 1);
    assert_eq!(restored.snapshot.groups, candidate.groups);
    assert_eq!(restored.snapshot.authenticators, candidate.authenticators);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}
