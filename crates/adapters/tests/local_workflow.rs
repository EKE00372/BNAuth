use bnauth_adapters::storage::Repository;
use bnauth_core::{VaultSnapshot, parse_manual, prepare_import, totp};
use std::{fs, path::PathBuf};

struct Fixture(PathBuf);
impl Drop for Fixture {
    fn drop(&mut self) {
        if self.0.parent() == Some(std::env::temp_dir().as_path())
            && self
                .0
                .file_name()
                .unwrap()
                .to_string_lossy()
                .starts_with("BNAuth-workflow-")
        {
            let _ = fs::remove_dir_all(&self.0);
        }
    }
}

#[test]
fn manual_import_reopen_copy_and_metadata_edit_preserve_otp_and_identity() {
    let dir =
        Fixture(std::env::temp_dir().join(format!("BNAuth-workflow-{}", uuid::Uuid::new_v4())));
    let original_dir = dir.0.join("來源 有愛");
    let copied_dir = dir.0.join("攜帶 副本");
    fs::create_dir_all(&original_dir).unwrap();
    fs::create_dir(&copied_dir).unwrap();
    let path = original_dir.join("BNAuth.bnauth");
    let (mut repo, _) = Repository::open(path.clone()).unwrap();
    let empty = repo
        .commit(&VaultSnapshot::new_empty(1_700_000_000_000))
        .unwrap()
        .snapshot;
    // RFC 6238's public SHA-1 test key, never an account credential.
    let key = "3132333435363738393031323334353637383930";
    let first = prepare_import(
        &empty,
        "繁體 / Кириллица / Latin",
        "備註",
        empty.groups[0].id,
        parse_manual(key, "", "").unwrap(),
        1_700_000_000_001,
    )
    .unwrap();
    let imported = repo.commit(&first.candidate).unwrap().snapshot;
    assert_eq!(
        totp(imported.authenticators[0].secret.as_ref().unwrap(), 59_000).unwrap(),
        "94287082"
    );
    let serial_hex: String = b"US-1234-5678-9012"
        .iter()
        .map(|b| format!("{b:02X}"))
        .collect();
    let duplicate = prepare_import(
        &imported,
        "不應取代名稱",
        "不應取代備註",
        empty.groups[0].id,
        parse_manual(&format!("{key}{serial_hex}"), "", "AbCd123456").unwrap(),
        1_700_000_000_002,
    )
    .unwrap();
    assert!(!duplicate.new_secret);
    assert!(duplicate.changed);
    let supplemented = repo.commit(&duplicate.candidate).unwrap().snapshot;
    assert_eq!(supplemented.authenticators.len(), 1);
    assert_eq!(
        supplemented.authenticators[0].id,
        imported.authenticators[0].id
    );
    assert_eq!(
        supplemented.authenticators[0].name,
        imported.authenticators[0].name
    );
    assert_eq!(
        supplemented.authenticators[0].serial.as_deref(),
        Some("US123456789012")
    );
    drop(repo);
    let (repo, reopened) = Repository::open(path.clone()).unwrap();
    assert_eq!(reopened.as_ref(), Some(&supplemented));
    drop(repo);
    fs::copy(path, copied_dir.join("BNAuth.bnauth")).unwrap();
    let (mut portable, copied) = Repository::open(copied_dir.join("BNAuth.bnauth")).unwrap();
    let mut changed = copied.unwrap();
    assert_eq!(changed, supplemented);
    changed.authenticators[0].name = "改名後仍為同一個驗證器".into();
    changed.authenticators[0].description.clear();
    let edited = portable.commit(&changed).unwrap().snapshot;
    assert_eq!(edited.authenticators[0].id, imported.authenticators[0].id);
    assert_eq!(
        edited.authenticators[0].restore_code.as_deref(),
        Some("AbCd123456")
    );
    assert_eq!(
        totp(edited.authenticators[0].secret.as_ref().unwrap(), 59_000).unwrap(),
        "94287082"
    );
}
