use bnauth_adapters::{
    export_file::{self, Format},
    native,
};
use bnauth_core::{Group, Secret, Uuid, VaultSnapshot, parse_manual, prepare_import};
use std::{fs, path::PathBuf};

fn fixture() -> (VaultSnapshot, Uuid) {
    let mut vault = VaultSnapshot::new_empty();
    let group = Uuid::new_v4();
    vault.groups.push(Group {
        id: group,
        name: "遊戲帳號".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let record = prepare_import(
        &vault,
        "原文標題",
        " 備註\r\n第二行 ",
        group,
        parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AbCd123456",
        )
        .unwrap(),
    )
    .unwrap()
    .record();
    let mut other = record.clone();
    other.id = Uuid::new_v4();
    other.name = "其他分組的私有資料".into();
    other.group_id = vault.groups[0].id;
    other.secret = Secret::from_bytes([255; 20]);
    vault.authenticators.push(other);
    for index in (0..100).rev() {
        let mut record = record.clone();
        record.id = Uuid::new_v4();
        record.name = format!("完整名稱 {index:03}");
        record.sort_order = index;
        record.secret = Secret::from_bytes([index as u8; 20]);
        vault.authenticators.push(record);
    }
    vault.revision = 7;
    vault.validate().unwrap();
    (vault, group)
}

#[test]
fn group_export_retains_all_one_hundred_records_fields_and_order_without_other_groups() {
    let (source, group) = fixture();
    let original = source.clone();
    let exported = source.export_group_from_validated(group).unwrap();
    let bytes = native::encode_backup_from_validated(&exported.file_view()).unwrap();
    let decoded = native::decode_group(&bytes).unwrap();
    assert_eq!(decoded, exported);
    assert_eq!(decoded.scope, "group");
    assert_eq!(decoded.groups.len(), 1);
    assert_eq!(decoded.groups[0].id, group);
    assert_eq!(decoded.groups[0].name, "遊戲帳號");
    assert_eq!(decoded.groups[0].sort_order, 0);
    assert!(!decoded.groups[0].is_ungrouped);
    assert_eq!(decoded.authenticators.len(), 100);
    for (order, record) in decoded.authenticators.iter().enumerate() {
        assert_eq!(record.sort_order, order as u32);
        assert_eq!(
            record,
            source
                .authenticators
                .iter()
                .find(|original| original.id == record.id)
                .unwrap()
        );
    }
    assert_eq!(decoded.vault_id, source.vault_id);
    assert_eq!(decoded.revision, source.revision);
    assert!(!String::from_utf8_lossy(&bytes).contains("其他分組的私有資料"));
    assert!(
        native::decode(&bytes).is_err(),
        "group exports cannot replace a full working file"
    );
    assert!(native::encode(&exported).is_err());
    assert_eq!(source, original);
}

#[test]
fn empty_and_ungrouped_exports_are_valid_and_invalid_group_relationships_are_rejected() {
    let empty = VaultSnapshot::new_empty();
    let ungrouped = empty.export_group(empty.groups[0].id).unwrap();
    assert!(
        native::decode_group(&native::encode_group(&ungrouped).unwrap())
            .unwrap()
            .authenticators
            .is_empty()
    );
    assert!(ungrouped.groups[0].is_ungrouped);
    let mut legacy = ungrouped.clone();
    legacy.groups[0].name = "未分組".into();
    assert_eq!(
        native::decode_group(&serde_json::to_vec(&legacy).unwrap()).unwrap(),
        ungrouped
    );
    assert!(empty.export_group(Uuid::new_v4()).is_err());
    assert!(native::decode_group(&native::encode(&empty).unwrap()).is_err());
    let (vault, group) = fixture();
    let mut exported = vault.export_group(group).unwrap();
    exported.authenticators[0].group_id = vault.groups[0].id;
    assert!(native::encode_group(&exported).is_err());
    let mut exported = vault.export_group(group).unwrap();
    exported.groups.push(vault.groups[0].clone());
    assert!(native::encode_group(&exported).is_err());
    let mut exported = vault.export_group(group).unwrap();
    exported.authenticators[1].sort_order = 0;
    assert!(native::encode_group(&exported).is_err());
}

struct FixtureDirectory(PathBuf);
impl Drop for FixtureDirectory {
    fn drop(&mut self) {
        if let Ok(files) = fs::read_dir(&self.0) {
            for file in files.flatten() {
                let _ = fs::remove_file(file.path());
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}

#[test]
fn group_file_uses_group_name_and_verifies_contents_while_protecting_the_working_file() {
    let (source, group) = fixture();
    let exported = source.export_group(group).unwrap();
    let bytes = native::encode_group(&exported).unwrap();
    let directory = FixtureDirectory(
        std::env::temp_dir().join(format!("BNAuth-group-export-{}", Uuid::new_v4())),
    );
    fs::create_dir(&directory.0).unwrap();
    let primary = directory.0.join("BNAuth.bnauth");
    let original = native::encode(&source).unwrap();
    fs::write(&primary, &original).unwrap();
    let name = export_file::suggested_backup_filename(&exported.groups[0].name, (2026, 1, 4));
    assert_eq!(name, "遊戲帳號-260104.bnauth");
    let path = directory.0.join(name);
    export_file::write(&path, &bytes, Format::Group, Some(&primary)).unwrap();
    assert_eq!(
        native::decode_group(&fs::read(&path).unwrap()).unwrap(),
        exported
    );
    let error = export_file::write(&primary, &bytes, Format::Group, Some(&primary)).unwrap_err();
    assert_eq!(error, "請勿覆蓋設定檔。");
    assert_eq!(fs::read(&primary).unwrap(), original.as_slice());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}
