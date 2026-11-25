use bnauth_adapters::{native, storage::Repository};
use bnauth_core::{Group, Uuid, VaultSnapshot, parse_manual, prepare_group_import, prepare_import};
use std::{fs, path::PathBuf};

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("BNAuth-group-import-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if let Ok(entries) = fs::read_dir(&self.0) {
            for entry in entries.flatten() {
                let _ = fs::remove_file(entry.path());
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn source() -> VaultSnapshot {
    let mut source = VaultSnapshot::new_empty();
    let group = Uuid::new_v4();
    source.groups.push(Group {
        id: group,
        name: " 保存後的原分組 ".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    source = prepare_import(
        &source,
        " 完整標題 ",
        " 備註\r\n中文 & <原文> ",
        group,
        parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AbCd123456",
        )
        .unwrap(),
    )
    .unwrap()
    .candidate(&source)
    .unwrap();
    let mut second = source.authenticators[0].clone();
    second.id = Uuid::new_v4();
    second.name = "另一個驗證器".into();
    second.description = "另一筆的備註".into();
    second.sort_order = 1;
    source.authenticators.push(second);
    source.export_group(group).unwrap()
}

#[test]
fn exported_group_file_import_save_and_reopen_preserve_duplicate_keys_and_source_bytes() {
    let directory = Directory::new();
    let source = source();
    let path = directory.0.join("來源 分組.bnauth");
    let encoded = native::encode_group(&source).unwrap();
    let mut legacy: serde_json::Value = serde_json::from_slice(&encoded).unwrap();
    legacy["provenance"] = serde_json::json!([{
        "source_vault_id": source.vault_id,
        "source_revision": 1,
        "scope": "group",
        "imported_at_ms": 1_800_000_000_000_i64,
        "source_time_sync": null,
    }]);
    for record in legacy["authenticators"].as_array_mut().unwrap() {
        record["created_at_ms"] = 1_800_000_000_000_i64.into();
        record["updated_at_ms"] = 1_800_000_000_010_i64.into();
    }
    let text = serde_json::to_string_pretty(&legacy).unwrap();
    let file_bytes = format!("\u{feff}{}", text.replace('\n', "\r\n")).into_bytes();
    fs::write(&path, &file_bytes).unwrap();
    let loaded = native::read_group(&path).unwrap();
    let primary = directory.0.join("BNAuth.bnauth");
    let (mut repo, _) = Repository::open(primary.clone()).unwrap();
    let mut target = VaultSnapshot::new_empty();
    let mut local = source.authenticators[0].clone();
    local.id = Uuid::new_v4();
    local.group_id = target.groups[0].id;
    local.name = "本機另外一組".into();
    target.authenticators.push(local.clone());
    let target = repo.commit(&target).unwrap().snapshot;
    let preview = prepare_group_import(&target, &loaded).unwrap();
    let imported = repo.commit(&preview.candidate).unwrap().snapshot;
    let persisted: serde_json::Value =
        serde_json::from_slice(&fs::read(&primary).unwrap()).unwrap();
    assert!(persisted.get("provenance").is_none());
    assert_eq!(imported.revision, target.revision + 1);
    assert_eq!(imported.vault_id, target.vault_id);
    assert_eq!(imported.groups[1].name, " 保存後的原分組 ");
    assert_eq!(imported.authenticators[0], local);
    assert_eq!(imported.authenticators[1..], source.authenticators[..]);
    assert_eq!(
        bnauth_core::totp(&imported.authenticators[1].secret, 59_000).unwrap(),
        "94287082"
    );
    drop(repo);
    let (mut repo, reopened) = Repository::open(primary.clone()).unwrap();
    assert_eq!(reopened.as_ref(), Some(&imported));
    let repeat = prepare_group_import(&imported, &loaded).unwrap();
    let saved = repo.commit(&repeat.candidate).unwrap().snapshot;
    assert_eq!(&saved.groups[..2], imported.groups.as_slice());
    assert_eq!(saved.groups[2].name, " 保存後的原分組 (2)");
    assert_eq!(saved.authenticators[..3], imported.authenticators[..]);
    assert_eq!(saved.authenticators.len(), 5);
    for (copy, original) in saved.authenticators[3..]
        .iter()
        .zip(source.authenticators.iter())
    {
        assert_ne!(copy.id, original.id);
        let mut expected = original.clone();
        expected.id = copy.id;
        expected.group_id = repeat.group_id;
        assert_eq!(copy, &expected);
    }
    drop(repo);
    let (_, reopened) = Repository::open(primary).unwrap();
    assert_eq!(reopened, Some(saved));
    assert_eq!(fs::read(&path).unwrap(), file_bytes);
}

#[test]
fn full_working_file_is_rejected_as_group_backup_without_touching_the_source() {
    let directory = Directory::new();
    let path = directory.0.join("invalid.bnauth");
    let full = native::encode(&VaultSnapshot::new_empty()).unwrap();
    fs::write(&path, &full).unwrap();
    assert!(native::read_group(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), full.as_slice());
}
