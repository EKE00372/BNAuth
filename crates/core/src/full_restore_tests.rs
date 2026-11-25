use super::*;
use crate::{Group, Secret, TIME_SOURCE, TimeSyncRecord, parse_manual, prepare_import};

const T: i64 = 1_800_000_000_000;

fn source() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty();
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
    )
    .unwrap()
    .record();
    for index in 0..3 {
        let mut record = record.clone();
        record.id = Uuid::new_v4();
        record.sort_order = index;
        record.name = format!(" 原文標題 {index} ");
        record.secret = Secret::from_bytes([index as u8; 20]);
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
    vault.validate().unwrap();
    vault
}

#[test]
fn first_restore_preserves_every_group_record_field_and_original_order_without_mutation() {
    let mut source = source();
    let mut cross_group_copy = source.authenticators[0].clone();
    cross_group_copy.id = Uuid::new_v4();
    cross_group_copy.group_id = source.groups[2].id;
    cross_group_copy.sort_order = 0;
    cross_group_copy.description = "另一分組的獨立備註".into();
    source.authenticators.push(cross_group_copy);
    let original = source.clone();
    let preview = prepare_full_restore(None, &source).unwrap();
    assert_eq!(preview.base, None);
    assert_ne!(preview.candidate.vault_id, source.vault_id);
    assert_eq!(preview.candidate.revision, 0);
    assert_eq!(preview.candidate.groups, source.groups);
    assert_eq!(preview.candidate.authenticators, source.authenticators);
    assert_eq!(
        preview.candidate.service_time_sync,
        source.service_time_sync
    );
    assert_eq!(source, original);
}

#[test]
fn replacement_keeps_target_container_identity() {
    let source = source();
    let mut target = source.clone();
    target.vault_id = Uuid::new_v4();
    target.revision = 88;
    target.groups[1].name = "目標組名".into();
    target.groups[3].name = "目標空組".into();
    let original = target.clone();
    let preview = prepare_full_restore(Some(&target), &source).unwrap();
    assert_eq!(preview.base, Some((target.vault_id, 88)));
    assert_eq!(preview.candidate.vault_id, target.vault_id);
    assert_eq!(preview.candidate.revision, 88);
    assert_eq!(preview.candidate.groups, source.groups);
    assert_eq!(preview.candidate.authenticators, source.authenticators);
    assert_eq!(target, original);
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
        record.secret = Secret::from_bytes(seed);
        source.authenticators.insert(0, record);
    }
    source.validate().unwrap();
    let preview = prepare_full_restore(None, &source).unwrap();
    assert_eq!(preview.candidate.groups.len(), 20);
    assert_eq!(preview.candidate.authenticators.len(), 1_000);
    assert_eq!(preview.candidate.groups, source.groups);
    assert_eq!(preview.candidate.authenticators, source.authenticators);
}
