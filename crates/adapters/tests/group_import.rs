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
    let mut source = VaultSnapshot::new_empty(1_800_000_000_000);
    let group = Uuid::new_v4();
    source.groups.push(Group {
        id: group,
        name: "保存後的原分組".into(),
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
        1_800_000_000_000,
    )
    .unwrap()
    .candidate;
    let mut missing = source.authenticators[0].clone();
    missing.id = Uuid::new_v4();
    missing.name = "尚無密鑰".into();
    missing.secret = None;
    missing.sort_order = 1;
    source.authenticators.push(missing);
    source.export_group(group).unwrap()
}

#[test]
fn exported_group_file_import_save_and_reopen_preserve_complete_data_and_source_bytes() {
    let directory = Directory::new();
    let source = source();
    let path = directory.0.join("來源 分組.bnauth");
    let encoded = native::encode_group(&source).unwrap();
    let text = std::str::from_utf8(&encoded).unwrap();
    let file_bytes = format!("\u{feff}{}", text.replace('\n', "\r\n")).into_bytes();
    fs::write(&path, &file_bytes).unwrap();
    let loaded = native::read_group(&path).unwrap();
    let primary = directory.0.join("BNAuth.bnauth");
    let (mut repo, _) = Repository::open(primary.clone()).unwrap();
    let target = repo
        .commit(&VaultSnapshot::new_empty(1_800_000_000_010))
        .unwrap()
        .snapshot;
    let preview = prepare_group_import(&target, &loaded, 1_800_000_000_020).unwrap();
    let imported = repo.commit(&preview.candidate).unwrap().snapshot;
    assert_eq!(imported.revision, target.revision + 1);
    assert_eq!(imported.vault_id, target.vault_id);
    assert_eq!(imported.groups[1].name, "保存後的原分組");
    assert_eq!(imported.authenticators, source.authenticators);
    assert_eq!(
        bnauth_core::totp(imported.authenticators[0].secret.as_ref().unwrap(), 59_000).unwrap(),
        "94287082"
    );
    drop(repo);
    let (repo, reopened) = Repository::open(primary).unwrap();
    assert_eq!(reopened.as_ref(), Some(&imported));
    drop(repo);
    assert_eq!(fs::read(&path).unwrap(), file_bytes);
    assert!(
        !prepare_group_import(&imported, &loaded, 1_800_000_000_030)
            .unwrap()
            .changed
    );
}

#[test]
fn invalid_oversized_and_full_working_files_are_rejected_without_touching_the_source() {
    let directory = Directory::new();
    let path = directory.0.join("invalid.bnauth");
    let full = native::encode(&VaultSnapshot::new_empty(1_800_000_000_000)).unwrap();
    fs::write(&path, &full).unwrap();
    assert!(native::read_group(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), full.as_slice());
    fs::write(&path, b"{invalid JSON").unwrap();
    assert!(native::read_group(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{invalid JSON");
    fs::write(&path, vec![b' '; native::MAX_FILE + 1]).unwrap();
    assert!(native::read_group(&path).unwrap_err().contains("32 MiB"));
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        native::MAX_FILE as u64 + 1
    );
}
