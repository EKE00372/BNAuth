use super::*;
use crate::{
    Group, LegacyMetadata, RecoveryEvent, Secret, TIME_SOURCE, TimeSyncRecord, parse_manual,
    prepare_import,
};

const T: i64 = 1_800_000_000_000;

fn source() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty(T);
    for (index, name) in [" 原分組 二 ", "原分組 一", "空分組"]
        .into_iter()
        .enumerate()
    {
        vault.groups.push(Group {
            id: Uuid::new_v4(),
            name: name.into(),
            sort_order: index as u32 + 1,
            is_ungrouped: false,
        });
    }
    let record = prepare_import(
        &vault,
        " 完整標題 ",
        " 備註\r\n中文 & <原文>\t ",
        vault.groups[1].id,
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
    for index in 0..3 {
        let mut record = record.clone();
        record.id = Uuid::new_v4();
        record.sort_order = index;
        record.name = format!(" 原文標題 {index} ");
        record.secret = if index == 0 {
            None
        } else {
            Some(Secret::from_bytes([index as u8; 20]))
        };
        record.recovery_history.push(RecoveryEvent {
            at_ms: T,
            operation: "restore".into(),
            outcome: "saved".into(),
            confirmation: Some("使用者原確認".into()),
        });
        record.legacy_metadata.push(LegacyMetadata {
            source: "source".into(),
            source_version: Some("0.2.0".into()),
            field: "original".into(),
            original_value: "完整歷史\r\n原文".into(),
            unit: None,
        });
        vault.authenticators.insert(0, record);
    }
    vault.revision = 13;
    vault.service_time_sync.battlenet = Some(TimeSyncRecord {
        source: TIME_SOURCE.into(),
        server_time_ms: T,
        local_wall_ms_at_sample: T - 500,
        offset_ms: 500,
        rtt_ms: 20,
    });
    vault.provenance.push(Provenance {
        source_vault_id: Uuid::new_v4(),
        source_revision: 1,
        scope: "group".into(),
        imported_at_ms: T - 1,
        source_time_sync: None,
    });
    vault.validate().unwrap();
    vault
}

#[test]
fn first_restore_preserves_every_group_record_field_and_original_order_without_mutation() {
    let source = source();
    let original = source.clone();
    let preview = prepare_full_restore(None, &source, T + 20).unwrap();
    assert_eq!(preview.base, None);
    assert!(preview.new_secret);
    assert_ne!(preview.candidate.vault_id, source.vault_id);
    assert_eq!(preview.candidate.revision, 0);
    assert_eq!(preview.candidate.created_at_ms, T + 20);
    assert_eq!(preview.candidate.updated_at_ms, T + 20);
    assert_eq!(preview.candidate.groups, source.groups);
    assert_eq!(preview.candidate.authenticators, source.authenticators);
    assert_eq!(
        preview.candidate.service_time_sync,
        source.service_time_sync
    );
    assert_eq!(preview.candidate.provenance[0], source.provenance[0]);
    assert_eq!(
        preview.candidate.provenance.len(),
        source.provenance.len() + 1
    );
    let provenance = preview.candidate.provenance.last().unwrap();
    assert_eq!(provenance.source_vault_id, source.vault_id);
    assert_eq!(provenance.source_revision, source.revision);
    assert_eq!(provenance.scope, "full");
    assert_eq!(provenance.imported_at_ms, T + 20);
    assert_eq!(
        provenance.source_time_sync,
        source.service_time_sync.battlenet
    );
    assert_eq!(source, original);
}

#[test]
fn replacement_keeps_target_container_identity_and_only_reports_actually_new_seeds() {
    let source = source();
    let mut target = source.clone();
    target.vault_id = Uuid::new_v4();
    target.created_at_ms = T - 100;
    target.revision = 88;
    target.groups[1].name = "目標組名".into();
    target.groups[3].name = "目標空組".into();
    let original = target.clone();
    let preview = prepare_full_restore(Some(&target), &source, T + 20).unwrap();
    assert_eq!(preview.base, Some((target.vault_id, 88)));
    assert!(!preview.new_secret);
    assert_eq!(preview.candidate.vault_id, target.vault_id);
    assert_eq!(preview.candidate.revision, 88);
    assert_eq!(preview.candidate.created_at_ms, target.created_at_ms);
    assert_eq!(preview.candidate.groups, source.groups);
    assert_eq!(preview.candidate.authenticators, source.authenticators);
    assert_eq!(target, original);
    target.authenticators[0].secret = None;
    assert!(
        prepare_full_restore(Some(&target), &source, T + 20)
            .unwrap()
            .new_secret
    );
    let mut seedless = source.clone();
    for record in &mut seedless.authenticators {
        record.secret = None;
    }
    assert!(
        !prepare_full_restore(None, &seedless, T + 20)
            .unwrap()
            .new_secret
    );
}

#[test]
fn maximum_capacity_restore_keeps_all_thousand_records_twenty_groups_and_empty_groups() {
    let mut source = source();
    for index in source.groups.len()..20 {
        source.groups.push(Group {
            id: Uuid::new_v4(),
            name: format!(" 原始分組 {index} "),
            sort_order: index as u32,
            is_ungrouped: false,
        });
    }
    let template = source.authenticators[0].clone();
    source.authenticators.clear();
    for index in 0..1_000 {
        let mut record = template.clone();
        record.id = Uuid::new_v4();
        record.group_id = source.groups[index / 100].id;
        record.sort_order = (index % 100) as u32;
        record.name = format!("原始標題 {index}");
        let mut seed = [0; 20];
        seed[..8].copy_from_slice(&(index as u64 + 1).to_be_bytes());
        record.secret = Some(Secret::from_bytes(seed));
        source.authenticators.insert(0, record);
    }
    source.validate().unwrap();
    let preview = prepare_full_restore(None, &source, T + 20).unwrap();
    assert_eq!(preview.candidate.groups.len(), 20);
    assert_eq!(preview.candidate.authenticators.len(), 1_000);
    assert_eq!(preview.candidate.groups, source.groups);
    assert_eq!(preview.candidate.authenticators, source.authenticators);
}

#[test]
fn invalid_partial_identity_colliding_and_history_overflow_sources_are_rejected_atomically() {
    let source = source();
    let group = source.export_group(source.groups[1].id).unwrap();
    assert!(prepare_full_restore(None, &group, T + 20).is_err());
    assert!(prepare_full_restore(None, &source, -1).is_err());
    let mut invalid = source.clone();
    invalid.authenticators[0].sort_order = 99;
    assert!(prepare_full_restore(None, &invalid, T + 20).is_err());
    let mut target = VaultSnapshot::new_empty(T + 10);
    target.vault_id = source.groups[1].id;
    let before = target.clone();
    assert!(
        prepare_full_restore(Some(&target), &source, T + 20)
            .unwrap_err()
            .contains("識別碼")
    );
    assert_eq!(target, before);
    target.vault_id = source.authenticators[0].id;
    assert!(prepare_full_restore(Some(&target), &source, T + 20).is_err());
    target.vault_id = Uuid::new_v4();
    target.revision = u64::MAX;
    assert!(
        prepare_full_restore(Some(&target), &source, T + 20)
            .unwrap_err()
            .contains("修訂號")
    );
    let mut full_history = source.clone();
    full_history.provenance = vec![source.provenance[0].clone(); 1_000];
    full_history.validate().unwrap();
    let before = full_history.clone();
    assert!(
        prepare_full_restore(None, &full_history, T + 20)
            .unwrap_err()
            .contains("1000")
    );
    assert_eq!(full_history, before);
}
