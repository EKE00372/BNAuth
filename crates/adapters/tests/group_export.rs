use bnauth_adapters::{
    export_file::{self, Format},
    native,
};
use bnauth_core::{
    Group, LegacyMetadata, Provenance, RecoveryEvent, Secret, Uuid, VaultSnapshot, parse_manual,
    prepare_import,
};
use std::{fs, path::PathBuf};

fn fixture() -> (VaultSnapshot, Uuid) {
    let mut vault = VaultSnapshot::new_empty(1_800_000_000_000);
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
        1_800_000_000_000,
    )
    .unwrap()
    .candidate
    .authenticators
    .remove(0);
    let mut other = record.clone();
    other.id = Uuid::new_v4();
    other.name = "其他分組的私有資料".into();
    other.group_id = vault.groups[0].id;
    other.secret = Some(Secret::from_bytes([255; 20]));
    vault.authenticators.push(other);
    for index in (0..100).rev() {
        let mut record = record.clone();
        record.id = Uuid::new_v4();
        record.name = format!("完整名稱 {index:03}");
        record.sort_order = index;
        record.secret = if index == 0 {
            None
        } else {
            Some(Secret::from_bytes([index as u8; 20]))
        };
        record.recovery_history.push(RecoveryEvent {
            at_ms: 1_800_000_000_000,
            operation: "restore".into(),
            outcome: "saved".into(),
            confirmation: None,
        });
        record.legacy_metadata.push(LegacyMetadata {
            source: "test".into(),
            source_version: None,
            field: "original".into(),
            original_value: " 完整歷史 ".into(),
            unit: None,
        });
        vault.authenticators.push(record);
    }
    vault.revision = 7;
    vault.provenance.push(Provenance {
        source_vault_id: Uuid::new_v4(),
        source_revision: 2,
        scope: "full".into(),
        imported_at_ms: 1_800_000_000_000,
        source_time_sync: None,
    });
    vault.validate().unwrap();
    (vault, group)
}

#[test]
fn group_export_retains_all_one_hundred_records_fields_and_order_without_other_groups() {
    let (source, group) = fixture();
    let original = source.clone();
    let exported = source.export_group(group).unwrap();
    let bytes = native::encode_group(&exported).unwrap();
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
    assert!(decoded.authenticators[0].secret.is_none());
    assert_eq!(decoded.vault_id, source.vault_id);
    assert_eq!(decoded.revision, source.revision);
    assert_eq!(decoded.provenance, source.provenance);
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
    let empty = VaultSnapshot::new_empty(1_800_000_000_000);
    let ungrouped = empty.export_group(empty.groups[0].id).unwrap();
    assert!(
        native::decode_group(&native::encode_group(&ungrouped).unwrap())
            .unwrap()
            .authenticators
            .is_empty()
    );
    assert!(ungrouped.groups[0].is_ungrouped);
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

#[test]
fn group_decoder_keeps_native_json_strictness_and_accepts_editor_bom_and_crlf() {
    let (vault, group) = fixture();
    let exported = vault.export_group(group).unwrap();
    let bytes = native::encode_group(&exported).unwrap();
    let text = std::str::from_utf8(&bytes).unwrap();
    assert_eq!(
        native::decode_group(format!("\u{feff}{}", text.replace('\n', "\r\n")).as_bytes()).unwrap(),
        exported
    );
    for body in [
        text.replacen('{', "{\"scope\":\"group\",", 1),
        text.replacen('{', "{\"unknown\":true,", 1),
        format!("{text} false"),
    ] {
        assert!(native::decode_group(body.as_bytes()).is_err());
    }
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
    let name = export_file::suggested_filename(&exported.groups[0].name, Format::Group);
    assert_eq!(name, "遊戲帳號.bnauth");
    let path = directory.0.join(name);
    export_file::write(&path, &bytes, Format::Group, Some(&primary)).unwrap();
    assert_eq!(
        native::decode_group(&fs::read(&path).unwrap()).unwrap(),
        exported
    );
    let error = export_file::write(&primary, &bytes, Format::Group, Some(&primary)).unwrap_err();
    assert!(error.contains("不能覆蓋"));
    assert_eq!(fs::read(&primary).unwrap(), original.as_slice());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}
