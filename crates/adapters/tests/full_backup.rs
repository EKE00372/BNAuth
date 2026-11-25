use bnauth_adapters::{
    export_file::{self, Format},
    native,
};
use bnauth_core::{
    Group, Secret, TIME_SOURCE, TimeSyncRecord, Uuid, VaultSnapshot, parse_manual, prepare_import,
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
    let mut source = VaultSnapshot::new_empty();
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
    )
    .unwrap()
    .record();
    for index in (0..1000_u64).rev() {
        let mut record = record.clone();
        record.id = Uuid::new_v4();
        record.group_id = source.groups[index as usize / 50].id;
        record.sort_order = (index % 50) as u32;
        record.name = format!(" 完整標題 {index:04} ");
        let mut key = [0_u8; 20];
        // Each group repeats the same set of keys; the full backup must retain all copies.
        key[..8].copy_from_slice(&(index % 50).to_be_bytes());
        record.secret = Secret::from_bytes(key);
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
    let filename = export_file::suggested_backup_filename("BNAuth", (2026, 11, 24));
    assert_eq!(filename, "BNAuth-261124.bnauth");
    assert_eq!(Format::Full.title(), "備份完整資料庫");
    assert_eq!(Format::Full.label(), "完整備份");
    let backup = directory.0.join(filename);
    let backup_contents = native::encode_backup_from_validated(&source.file_view()).unwrap();
    export_file::write(&backup, &backup_contents, Format::Full, Some(&primary)).unwrap();
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
fn full_reader_accepts_legacy_history_bom_and_crlf_without_modifying_source_bytes() {
    let directory = Directory::new();
    let empty = VaultSnapshot::new_empty();
    let source = prepare_import(
        &empty,
        "Legacy row",
        "Saved note",
        empty.groups[0].id,
        parse_manual(
            "3132333435363738393031323334353637383930",
            "US123456789012",
            "AB12CD34EF",
        )
        .unwrap(),
    )
    .unwrap()
    .candidate(&empty)
    .unwrap();
    let mut legacy = serde_json::to_value(&source).unwrap();
    legacy["created_at_ms"] = T.into();
    legacy["updated_at_ms"] = (T + 10).into();
    let row = &mut legacy["authenticators"][0];
    row["created_at_ms"] = T.into();
    row["updated_at_ms"] = (T + 5).into();
    row["source"] = "manual-40".into();
    row["restore_code_source"] = "manual".into();
    row["recovery_history"] = serde_json::json!([{
        "at_ms": T, "operation": "restore", "outcome": "saved", "confirmation": null
    }]);
    row["legacy_metadata"] = serde_json::json!([{
        "source": "WinAuth", "source_version": null, "field": "original",
        "original_value": "sample", "unit": null
    }]);
    let entry = serde_json::json!({
        "source_vault_id": source.vault_id,
        "source_revision": 1,
        "scope": "full",
        "imported_at_ms": T,
        "source_time_sync": null,
    });
    legacy["provenance"] = serde_json::Value::Array(vec![entry; 1_000]);
    let body = serde_json::to_string_pretty(&legacy).unwrap();
    let bytes = format!("\u{feff}{}", body.replace('\n', "\r\n")).into_bytes();
    let path = directory.0.join("編輯器另存.bnauth");
    fs::write(&path, &bytes).unwrap();
    let loaded = native::read_full(&path).unwrap();
    assert_eq!(loaded, source);
    let restored = bnauth_core::prepare_full_restore(None, &loaded).unwrap();
    let saved: serde_json::Value =
        serde_json::from_slice(&native::encode(&restored.candidate).unwrap()).unwrap();
    for key in ["created_at_ms", "updated_at_ms", "provenance"] {
        assert!(saved.get(key).is_none(), "{key}");
    }
    for key in [
        "created_at_ms",
        "updated_at_ms",
        "source",
        "restore_code_source",
        "recovery_history",
        "legacy_metadata",
    ] {
        assert!(saved["authenticators"][0].get(key).is_none(), "{key}");
    }
    assert_eq!(fs::read(&path).unwrap(), bytes);
}

#[test]
fn full_reader_rejects_partial_invalid_and_oversized_files_and_preserves_the_source() {
    let directory = Directory::new();
    let path = directory.0.join("來源.bnauth");
    let source = VaultSnapshot::new_empty();
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
    assert_eq!(
        native::read_full(&path).unwrap_err(),
        "E15：檔案過大\n超過 32 MiB 上限。"
    );
    assert_eq!(
        fs::metadata(&path).unwrap().len(),
        native::MAX_FILE as u64 + 1
    );
}

#[test]
fn full_export_protects_the_primary_and_rejects_incorrect_extension() {
    let directory = Directory::new();
    let source = VaultSnapshot::new_empty();
    let primary = directory.0.join("BNAuth.bnauth");
    let bytes = native::encode(&source).unwrap();
    fs::write(&primary, &bytes).unwrap();
    assert_eq!(
        export_file::write(&primary, &bytes, Format::Full, Some(&primary))
            .unwrap_err(),
        "請勿覆蓋設定檔。"
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
