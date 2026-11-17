use bnauth_adapters::{
    export_file::{self, Format},
    native,
};
use bnauth_core::{
    Group, LegacyMetadata, Provenance, RecoveryEvent, Secret, TIME_SOURCE, TimeSyncRecord, Uuid,
    VaultSnapshot, parse_manual, prepare_import,
};
use std::{fs, path::PathBuf};

const T: i64 = 1_800_000_000_000;

struct Directory(PathBuf);
impl Directory {
    fn new() -> Self {
        let path = std::env::temp_dir().join(format!("BNAuth-full-backup-{}", Uuid::new_v4()));
        fs::create_dir(&path).unwrap();
        Self(path)
    }
}
impl Drop for Directory {
    fn drop(&mut self) {
        if let Ok(files) = fs::read_dir(&self.0) {
            for file in files.flatten() {
                let _ = fs::remove_file(file.path());
            }
        }
        let _ = fs::remove_dir(&self.0);
    }
}

fn full_fixture() -> VaultSnapshot {
    let mut source = VaultSnapshot::new_empty(T);
    for order in 1..20 {
        source.groups.push(Group {
            id: Uuid::new_v4(),
            name: format!(" 完整分組 {:02} ", 20 - order),
            sort_order: order,
            is_ungrouped: false,
        });
    }
    let record = prepare_import(
        &source,
        " 完整標題 ",
        " 備註\r\n中文 & <原文>\t ",
        source.groups[0].id,
        parse_manual(
            "3132333435363738393031323334353637383930",
            "US-1234-5678-9012",
            "AbCd123456",
        )
        .unwrap(),
        T,
    )
    .unwrap()
    .candidate
    .authenticators
    .remove(0);
    for index in (0..1000_u64).rev() {
        let mut record = record.clone();
        record.id = Uuid::new_v4();
        record.group_id = source.groups[index as usize / 50].id;
        record.sort_order = (index % 50) as u32;
        record.name = format!(" 完整標題 {index:04} ");
        let mut key = [0_u8; 20];
        key[..8].copy_from_slice(&index.to_be_bytes());
        record.secret = (index % 10 != 0).then(|| Secret::from_bytes(key));
        record.recovery_history.push(RecoveryEvent {
            at_ms: T,
            operation: "restore".into(),
            outcome: "saved".into(),
            confirmation: Some("來源確認原文".into()),
        });
        record.legacy_metadata.push(LegacyMetadata {
            source: "fixture".into(),
            source_version: Some("1".into()),
            field: "original".into(),
            original_value: " 完整歷史\r\n原值 ".into(),
            unit: None,
        });
        source.authenticators.push(record);
    }
    source.revision = 42;
    source.service_time_sync.battlenet = Some(TimeSyncRecord {
        source: TIME_SOURCE.into(),
        server_time_ms: T,
        local_wall_ms_at_sample: T - 500,
        offset_ms: 500,
        rtt_ms: 20,
    });
    source.provenance.push(Provenance {
        source_vault_id: Uuid::new_v4(),
        source_revision: 13,
        scope: "full".into(),
        imported_at_ms: T - 1,
        source_time_sync: source.service_time_sync.battlenet.clone(),
    });
    source.validate().unwrap();
    source
}

#[test]
fn full_backup_write_and_read_preserve_all_twenty_groups_thousand_records_and_order() {
    let directory = Directory::new();
    let source = full_fixture();
    let original = source.clone();
    let encoded = native::encode(&source).unwrap();
    let primary = directory.0.join("BNAuth.bnauth");
    fs::write(&primary, &encoded).unwrap();
    let filename = export_file::suggested_filename("BNAuth-full", Format::Full);
    assert_eq!(filename, "BNAuth-full.bnauth");
    assert_eq!(Format::Full.title(), "備份完整資料庫");
    assert_eq!(Format::Full.label(), "完整備份");
    let backup = directory.0.join(filename);
    export_file::write(&backup, &encoded, Format::Full, Some(&primary)).unwrap();
    let backup_bytes = fs::read(&backup).unwrap();
    let decoded = native::read_full(&backup).unwrap();
    assert_eq!(decoded, source);
    assert_eq!(decoded.groups.len(), 20);
    assert_eq!(decoded.authenticators.len(), 1000);
    assert_eq!(fs::read(&backup).unwrap(), backup_bytes);
    assert_eq!(fs::read(&primary).unwrap(), encoded.as_slice());
    assert_eq!(source, original);
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 2);
}

#[test]
fn full_reader_accepts_bom_crlf_and_empty_database_without_modifying_source_bytes() {
    let directory = Directory::new();
    let source = VaultSnapshot::new_empty(T);
    let encoded = native::encode(&source).unwrap();
    let body = std::str::from_utf8(&encoded).unwrap();
    let bytes = format!("\u{feff}{}", body.replace('\n', "\r\n")).into_bytes();
    let path = directory.0.join("編輯器另存.bnauth");
    fs::write(&path, &bytes).unwrap();
    assert_eq!(native::read_full(&path).unwrap(), source);
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn full_reader_rejects_partial_invalid_and_oversized_files_and_preserves_the_source() {
    let directory = Directory::new();
    let path = directory.0.join("來源.bnauth");
    let source = VaultSnapshot::new_empty(T);
    let group = source.export_group(source.groups[0].id).unwrap();
    let bytes = native::encode_group(&group).unwrap();
    fs::write(&path, &bytes).unwrap();
    assert!(native::read_full(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), bytes.as_slice());
    fs::write(&path, b"{invalid JSON").unwrap();
    assert!(native::read_full(&path).is_err());
    assert_eq!(fs::read(&path).unwrap(), b"{invalid JSON");
    fs::File::create(&path)
        .unwrap()
        .set_len(native::MAX_FILE as u64 + 1)
        .unwrap();
    assert!(native::read_full(&path).unwrap_err().contains("32 MiB"));
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        native::MAX_FILE as u64 + 1
    );
}

#[test]
fn full_export_protects_the_primary_and_rejects_incorrect_extension() {
    let directory = Directory::new();
    let source = VaultSnapshot::new_empty(T);
    let primary = directory.0.join("BNAuth.bnauth");
    let bytes = native::encode(&source).unwrap();
    fs::write(&primary, &bytes).unwrap();
    assert!(
        export_file::write(&primary, &bytes, Format::Full, Some(&primary))
            .unwrap_err()
            .contains("不能覆蓋")
    );
    assert!(
        export_file::write(
            &directory.0.join("backup.xml"),
            &bytes,
            Format::Full,
            Some(&primary)
        )
        .unwrap_err()
        .contains(".bnauth")
    );
    assert_eq!(fs::read(&primary).unwrap(), bytes.as_slice());
    assert_eq!(fs::read_dir(&directory.0).unwrap().count(), 1);
}
