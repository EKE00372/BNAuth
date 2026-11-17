use super::*;
use crate::{
    Group, LegacyMetadata, RecoveryEvent, Secret, TIME_SOURCE, TimeSyncRecord, parse_manual,
    prepare_import,
};

const T: i64 = 1_800_000_000_000;

fn source() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty(T);
    let group = Uuid::new_v4();
    vault.groups.push(Group {
        id: group,
        name: "來源分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let record = prepare_import(
        &vault,
        "來源標題",
        " 備註\r\n原文 ",
        group,
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
    for index in (0..3).rev() {
        let mut record = record.clone();
        record.id = Uuid::new_v4();
        record.sort_order = index;
        record.name = format!(" 原文標題 {index} ");
        record.secret = if index == 0 {
            None
        } else {
            Some(Secret::from_bytes([index as u8; 20]))
        };
        record.serial = Some(format!("US1234567890{index:02}"));
        record.recovery_history.push(RecoveryEvent {
            at_ms: T,
            operation: "restore".into(),
            outcome: "saved".into(),
            confirmation: None,
        });
        record.legacy_metadata.push(LegacyMetadata {
            source: "source".into(),
            source_version: None,
            field: "original".into(),
            original_value: "完整歷史".into(),
            unit: None,
        });
        vault.authenticators.push(record);
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
    vault.export_group(group).unwrap()
}

fn group_records(snapshot: &VaultSnapshot, group: Uuid) -> Vec<Authenticator> {
    let mut records: Vec<_> = snapshot
        .authenticators
        .iter()
        .filter(|record| record.group_id == group)
        .cloned()
        .collect();
    records.sort_by_key(|record| record.sort_order);
    records
}

#[test]
fn fresh_import_preserves_group_records_ids_full_metadata_and_source_order() {
    let source = source();
    let original_source = source.clone();
    let target = VaultSnapshot::new_empty(T + 10);
    let original_target = target.clone();
    let preview = prepare_group_import(&target, &source, T + 20).unwrap();
    assert_eq!(preview.group_name, "來源分組");
    assert_eq!(preview.group_id, source.groups[0].id);
    assert_eq!(preview.added, 3);
    assert!(preview.created_group && preview.changed && preview.new_secret);
    assert_eq!(
        group_records(&preview.candidate, preview.group_id),
        source.authenticators
    );
    assert!(preview.candidate.service_time_sync.battlenet.is_none());
    assert_eq!(preview.candidate.vault_id, target.vault_id);
    assert_eq!(preview.candidate.revision, target.revision);
    assert_eq!(preview.candidate.provenance[0], source.provenance[0]);
    assert_eq!(
        preview
            .candidate
            .provenance
            .last()
            .unwrap()
            .source_time_sync,
        source.service_time_sync.battlenet
    );
    assert_eq!(source, original_source);
    assert_eq!(target, original_target);
    preview.candidate.validate().unwrap();
}

#[test]
fn same_name_merge_keeps_unrelated_order_then_the_source_block_and_repeat_is_a_noop() {
    let source = source();
    let mut target = VaultSnapshot::new_empty(T + 10);
    let mut group = source.groups[0].clone();
    group.id = Uuid::new_v4();
    group.sort_order = 1;
    target.groups.push(group.clone());
    let mut unrelated = source.authenticators[1].clone();
    unrelated.id = Uuid::new_v4();
    unrelated.name = "原有項目".into();
    unrelated.secret = Some(Secret::from_bytes([90; 20]));
    unrelated.group_id = group.id;
    unrelated.sort_order = 0;
    target.authenticators.push(unrelated.clone());
    for (index, order) in [(2, 1), (1, 2)] {
        let mut record = source.authenticators[index].clone();
        record.group_id = group.id;
        record.sort_order = order;
        target.authenticators.push(record);
    }
    target.validate().unwrap();
    let preview = prepare_group_import(&target, &source, T + 20).unwrap();
    assert!(!preview.created_group);
    assert!(preview.reordered);
    assert_eq!(preview.added, 1);
    assert_eq!(preview.existing, 2);
    assert!(!preview.new_secret, "only the seedless source row is new");
    let records = group_records(&preview.candidate, group.id);
    assert_eq!(records[0], unrelated);
    assert_eq!(
        records[1..]
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>(),
        source
            .authenticators
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>()
    );
    let repeated = prepare_group_import(&preview.candidate, &source, T + 30).unwrap();
    assert!(!repeated.changed && !repeated.new_secret);
    assert_eq!(repeated.candidate, preview.candidate);
}

#[test]
fn duplicate_is_moved_to_the_source_group_and_other_group_order_is_compacted() {
    let source = source();
    let mut target = VaultSnapshot::new_empty(T + 10);
    for (order, index) in [2, 1].into_iter().enumerate() {
        let mut record = source.authenticators[index].clone();
        record.group_id = target.groups[0].id;
        record.sort_order = order as u32;
        target.authenticators.push(record);
    }
    let mut one = source.clone();
    one.authenticators = vec![source.authenticators[2].clone()];
    one.authenticators[0].sort_order = 0;
    let preview = prepare_group_import(&target, &one, T + 20).unwrap();
    assert_eq!(preview.added, 0);
    assert_eq!(preview.moved, 1);
    assert!(!preview.new_secret);
    assert_eq!(
        group_records(&preview.candidate, target.groups[0].id)[0].id,
        source.authenticators[1].id
    );
    assert_eq!(
        group_records(&preview.candidate, target.groups[0].id)[0].sort_order,
        0
    );
    assert_eq!(
        group_records(&preview.candidate, preview.group_id)[0].id,
        source.authenticators[2].id
    );
}

#[test]
fn missing_material_is_supplemented_and_conflicting_user_fields_reject_the_entire_import() {
    let source = source();
    let mut target = VaultSnapshot::new_empty(T + 10);
    let mut record = source.authenticators[1].clone();
    record.group_id = target.groups[0].id;
    record.sort_order = 0;
    record.secret = None;
    record.serial = None;
    record.restore_code = None;
    record.restore_code_source = None;
    record.description.clear();
    record.recovery_history.clear();
    record.legacy_metadata.clear();
    target.authenticators.push(record);
    let preview = prepare_group_import(&target, &source, T + 20).unwrap();
    assert_eq!(preview.supplemented, 1);
    assert!(preview.new_secret);
    let restored = preview
        .candidate
        .authenticators
        .iter()
        .find(|record| record.id == source.authenticators[1].id)
        .unwrap();
    assert_eq!(restored.secret, source.authenticators[1].secret);
    assert_eq!(
        restored.restore_code_source,
        source.authenticators[1].restore_code_source
    );
    assert_eq!(restored.description, source.authenticators[1].description);
    for field in ["標題", "備註", "序號", "還原碼"] {
        let mut current = VaultSnapshot::new_empty(T + 10);
        let mut record = source.authenticators[1].clone();
        record.group_id = current.groups[0].id;
        record.sort_order = 0;
        match field {
            "標題" => record.name = "本機標題".into(),
            "備註" => record.description = "本機備註".into(),
            "序號" => record.serial = Some("US999999999999".into()),
            _ => record.restore_code = Some("DIFFERENT".into()),
        }
        current.authenticators.push(record);
        let before = current.clone();
        assert!(
            prepare_group_import(&current, &source, T + 20)
                .unwrap_err()
                .contains(field)
        );
        assert_eq!(current, before);
    }
}

#[test]
fn colliding_ids_keep_existing_data_and_allocate_new_ids_without_changing_seeds() {
    let source = source();
    let mut target = VaultSnapshot::new_empty(T + 10);
    for (order, id, byte) in [
        (0, source.groups[0].id, 70),
        (1, source.authenticators[1].id, 71),
    ] {
        let mut record = source.authenticators[1].clone();
        record.id = id;
        record.name = format!("本機項目 {order}");
        record.group_id = target.groups[0].id;
        record.sort_order = order;
        record.secret = Some(Secret::from_bytes([byte; 20]));
        target.authenticators.push(record);
    }
    let before = target.clone();
    let preview = prepare_group_import(&target, &source, T + 20).unwrap();
    assert_ne!(preview.group_id, source.groups[0].id);
    assert_eq!(preview.added, 3);
    for record in &before.authenticators {
        assert!(preview.candidate.authenticators.contains(record));
    }
    let records = group_records(&preview.candidate, preview.group_id);
    assert_ne!(records[1].id, source.authenticators[1].id);
    for (record, original) in records.iter().zip(&source.authenticators) {
        assert_eq!(record.secret, original.secret);
        assert_eq!(record.name, original.name);
    }
    assert!(
        !prepare_group_import(&preview.candidate, &source, T + 30)
            .unwrap()
            .changed
    );
    let mut seedless_collision = source.clone();
    seedless_collision.authenticators[0].id = target.groups[0].id;
    assert!(
        prepare_group_import(&target, &seedless_collision, T + 20)
            .unwrap_err()
            .contains("無法安全判重")
    );
    assert_eq!(target, before);
}

fn full_target(groups: usize, count: usize, source: &VaultSnapshot) -> VaultSnapshot {
    let mut target = VaultSnapshot::new_empty(T + 10);
    for index in 1..groups {
        target.groups.push(Group {
            id: Uuid::new_v4(),
            name: format!("目前分組 {index}"),
            sort_order: index as u32,
            is_ungrouped: false,
        });
    }
    for index in 0..count {
        let mut record = source.authenticators[1].clone();
        record.id = Uuid::new_v4();
        record.group_id = target.groups[index / 100].id;
        record.sort_order = (index % 100) as u32;
        let mut key = [0; 20];
        key[..8].copy_from_slice(&(index as u64 + 1000).to_be_bytes());
        record.secret = Some(Secret::from_bytes(key));
        target.authenticators.push(record);
    }
    target.validate().unwrap();
    target
}

#[test]
fn all_three_final_capacity_limits_are_checked_before_any_commit() {
    let source = source();
    let twenty_groups = full_target(20, 0, &source);
    assert!(
        prepare_group_import(&twenty_groups, &source, T + 20)
            .unwrap_err()
            .contains("20")
    );
    let thousand = full_target(10, 1000, &source);
    assert!(
        prepare_group_import(&thousand, &source, T + 20)
            .unwrap_err()
            .contains("1000")
    );
    let mut hundred = full_target(2, 199, &source);
    hundred.groups[1].name = source.groups[0].name.clone();
    assert!(
        prepare_group_import(&hundred, &source, T + 20)
            .unwrap_err()
            .contains("100")
    );
}

#[test]
fn ungrouped_maps_to_the_existing_system_group_and_empty_groups_still_import() {
    let mut source = source();
    let target = VaultSnapshot::new_empty(T + 10);
    source.groups[0].name = "未分組".into();
    source.groups[0].is_ungrouped = true;
    let preview = prepare_group_import(&target, &source, T + 20).unwrap();
    assert!(!preview.created_group);
    assert_eq!(preview.group_id, target.groups[0].id);
    assert_eq!(preview.candidate.groups.len(), 1);
    source.groups[0].name = "空分組".into();
    source.groups[0].is_ungrouped = false;
    source.authenticators.clear();
    let preview = prepare_group_import(&target, &source, T + 20).unwrap();
    assert!(preview.created_group && preview.changed && !preview.new_secret);
    assert_eq!(preview.candidate.groups[1].name, "空分組");
    assert!(
        !prepare_group_import(&preview.candidate, &source, T + 30)
            .unwrap()
            .changed
    );
}
