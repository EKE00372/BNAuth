use super::*;
use crate::{
    Authenticator, Group, Secret, TIME_SOURCE, TimeSyncRecord, parse_manual, prepare_import,
};

const T: i64 = 1_800_000_000_000;

fn source() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty();
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
    )
    .unwrap()
    .record();
    for index in (0..3).rev() {
        let mut record = record.clone();
        record.id = Uuid::new_v4();
        record.sort_order = index;
        record.name = format!(" 原文標題 {index} ");
        record.secret = Secret::from_bytes([index as u8; 20]);
        record.serial = Some(format!("US1234567890{index:02}"));
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
    let target = VaultSnapshot::new_empty();
    let original_target = target.clone();
    let preview = prepare_group_import(&target, &source).unwrap();
    assert_eq!(preview.group_id, source.groups[0].id);
    assert_eq!(preview.candidate.groups.len(), target.groups.len() + 1);
    assert_eq!(preview.candidate.groups[1].name, source.groups[0].name);
    assert_eq!(
        preview
            .imported_records()
            .iter()
            .map(|record| record.name.as_str())
            .collect::<Vec<_>>(),
        [" 原文標題 0 ", " 原文標題 1 ", " 原文標題 2 "]
    );
    assert_eq!(
        group_records(&preview.candidate, preview.group_id),
        source.authenticators[..]
    );
    assert!(preview.candidate.service_time_sync.battlenet.is_none());
    assert_eq!(preview.candidate.vault_id, target.vault_id);
    assert_eq!(preview.candidate.revision, target.revision);
    assert_eq!(source, original_source);
    assert_eq!(target, original_target);
    preview.candidate.validate().unwrap();
}

#[test]
fn same_name_import_keeps_existing_group_and_reimport_appends_another_group() {
    let source = source();
    let mut target = VaultSnapshot::new_empty();
    let mut group = source.groups[0].clone();
    group.id = Uuid::new_v4();
    group.sort_order = 1;
    target.groups.push(group.clone());
    let mut unrelated = source.authenticators[1].clone();
    unrelated.id = Uuid::new_v4();
    unrelated.name = "原有項目".into();
    unrelated.secret = Secret::from_bytes([90; 20]);
    unrelated.group_id = group.id;
    unrelated.sort_order = 0;
    target.authenticators.push(unrelated);
    for (index, order) in [(2, 1), (1, 2)] {
        let mut record = source.authenticators[index].clone();
        record.group_id = group.id;
        record.sort_order = order;
        target.authenticators.push(record);
    }
    target.validate().unwrap();
    let preview = prepare_group_import(&target, &source).unwrap();
    assert_eq!(preview.candidate.groups[2].name, "來源分組(2)");
    assert_eq!(
        group_records(&preview.candidate, group.id),
        target.authenticators[..]
    );
    let imported = group_records(&preview.candidate, preview.group_id);
    assert_eq!(imported.len(), 3);
    for (index, (added, original)) in imported
        .iter()
        .zip(source.authenticators.iter())
        .enumerate()
    {
        assert_eq!(added.id == original.id, index == 0);
        let mut expected = original.clone();
        expected.id = added.id;
        expected.group_id = preview.group_id;
        assert_eq!(added, &expected);
    }
    let repeated = prepare_group_import(&preview.candidate, &source).unwrap();
    assert_eq!(repeated.candidate.groups[3].name, "來源分組(3)");
    assert_eq!(
        repeated.candidate.authenticators[..6],
        preview.candidate.authenticators[..]
    );
    let after_repeat = group_records(&repeated.candidate, repeated.group_id);
    assert_eq!(after_repeat.len(), 3);
    let mut all_ids: HashSet<_> = preview
        .candidate
        .authenticators
        .iter()
        .map(|record| record.id)
        .collect();
    for (added, original) in after_repeat.iter().zip(&imported) {
        assert!(all_ids.insert(added.id));
        let mut expected = original.clone();
        expected.id = added.id;
        expected.group_id = repeated.group_id;
        assert_eq!(added, &expected);
    }
}

#[test]
fn same_name_uses_the_first_available_numbered_group() {
    let mut source = source();
    source.groups[0].name = " PTR ".into();
    let empty = VaultSnapshot::new_empty();
    let mut target = prepare_group_import(&empty, &source).unwrap().candidate;
    let original_id = target.groups[1].id;
    target.groups[1].name = " ptr ".into();
    for name in [" ptr (2)", "PTR(3)", " PTR (4)"] {
        let sort_order = target.groups.len() as u32;
        target.groups.push(Group {
            id: Uuid::new_v4(),
            name: name.into(),
            sort_order,
            is_ungrouped: false,
        });
    }
    let separate = prepare_group_import(&target, &source).unwrap();
    assert_ne!(separate.group_id, original_id);
    assert_eq!(separate.candidate.groups.last().unwrap().name, " PTR (3)");
    assert_eq!(
        &separate.candidate.groups[..target.groups.len()],
        target.groups.as_slice()
    );
    assert_eq!(
        group_records(&separate.candidate, original_id),
        target.authenticators[..]
    );
    let imported = group_records(&separate.candidate, separate.group_id);
    for (actual, source_record) in imported.iter().zip(source.authenticators.iter()) {
        let mut expected = source_record.clone();
        assert_ne!(actual.id, expected.id);
        expected.id = actual.id;
        expected.group_id = separate.group_id;
        assert_eq!(*actual, expected);
    }
    assert_eq!(imported.len(), source.authenticators.len());
    separate.candidate.validate().unwrap();
}

#[test]
fn numbered_name_fits_the_byte_limit_without_splitting_unicode() {
    let mut source = source();
    source.groups[0].name = format!("{}a", "測".repeat(85));
    let empty = VaultSnapshot::new_empty();
    let target = prepare_group_import(&empty, &source).unwrap().candidate;
    let expected = format!("{}(2)", "測".repeat(84));
    let preview = prepare_group_import(&target, &source).unwrap();
    assert_eq!(preview.candidate.groups[2].name, expected);
}

#[test]
fn same_key_with_missing_or_different_metadata_keeps_each_complete_record() {
    let source = source();
    let mut target = VaultSnapshot::new_empty();
    let mut group = source.groups[0].clone();
    group.sort_order = 1;
    target.groups.push(group.clone());
    let mut record = source.authenticators[1].clone();
    record.sort_order = 0;
    record.serial = None;
    record.restore_code = None;
    record.description.clear();
    target.authenticators.push(record.clone());
    record.id = Uuid::new_v4();
    record.sort_order = 1;
    record.name = "本機標題".into();
    record.description = "本機備註".into();
    record.serial = Some("US999999999999".into());
    record.restore_code = Some("DIFFERENT".into());
    target.authenticators.push(record);
    let preview = prepare_group_import(&target, &source).unwrap();
    assert_eq!(preview.imported_records().len(), 3);
    assert_eq!(
        group_records(&preview.candidate, group.id),
        target.authenticators[..]
    );
    let records = group_records(&preview.candidate, preview.group_id);
    let imported = &records[1];
    let mut expected = source.authenticators[1].clone();
    assert_ne!(imported.id, expected.id);
    expected.id = imported.id;
    expected.group_id = preview.group_id;
    assert_eq!(imported, &expected);
}

#[test]
fn colliding_ids_keep_existing_data_and_allocate_new_ids_without_changing_seeds() {
    let source = source();
    let mut target = VaultSnapshot::new_empty();
    for (order, id, byte) in [
        (0, source.groups[0].id, 70),
        (1, source.authenticators[1].id, 71),
    ] {
        let mut record = source.authenticators[1].clone();
        record.id = id;
        record.name = format!("本機項目 {order}");
        record.group_id = target.groups[0].id;
        record.sort_order = order;
        record.secret = Secret::from_bytes([byte; 20]);
        target.authenticators.push(record);
    }
    let before = target.clone();
    let preview = prepare_group_import(&target, &source).unwrap();
    assert_ne!(preview.group_id, source.groups[0].id);
    assert_eq!(preview.imported_records().len(), 3);
    for record in before.authenticators.iter() {
        assert!(preview.candidate.authenticators.contains(record));
    }
    let records = group_records(&preview.candidate, preview.group_id);
    assert_ne!(records[1].id, source.authenticators[1].id);
    for (record, original) in records.iter().zip(source.authenticators.iter()) {
        assert_eq!(record.secret, original.secret);
        assert_eq!(record.name, original.name);
    }
    assert_eq!(target, before);
}

fn full_target(groups: usize, count: usize, source: &VaultSnapshot) -> VaultSnapshot {
    let mut target = VaultSnapshot::new_empty();
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
        record.secret = Secret::from_bytes(key);
        target.authenticators.push(record);
    }
    target.validate().unwrap();
    target
}

#[test]
fn capacity_checks_group_count_and_total_without_using_existing_group_capacity() {
    let source = source();
    let mut twenty_groups = full_target(20, 0, &source);
    assert!(
        prepare_group_import(&twenty_groups, &source)
            .unwrap_err()
            .contains("20")
    );
    twenty_groups.groups[1].name = source.groups[0].name.clone();
    assert!(
        check_group_import_capacity(&twenty_groups, &source)
            .unwrap_err()
            .contains("20")
    );
    let mut target = full_target(10, MAX_ENTRIES, &source);
    assert_eq!(
        prepare_group_import(&target, &source).unwrap_err(),
        "已達 1000 筆驗證器上限，無法匯入分組。"
    );
    let fitting_count = MAX_ENTRIES - source.authenticators.len();
    target.authenticators.truncate(fitting_count + 1);
    assert_eq!(
        prepare_group_import(&target, &source).unwrap_err(),
        "匯入後的驗證器數量將超過 1000 筆上限。"
    );
    target.authenticators.truncate(fitting_count);
    let preview = prepare_group_import(&target, &source).unwrap();
    assert_eq!(preview.candidate.authenticators.len(), MAX_ENTRIES);
    let mut hundred = full_target(2, 200, &source);
    hundred.groups[1].name = source.groups[0].name.clone();
    hundred.authenticators[100].secret = source.authenticators[0].secret.clone();
    let preview = prepare_group_import(&hundred, &source).unwrap();
    assert_eq!(
        preview.imported_records().len(),
        source.authenticators.len()
    );
    assert_eq!(
        preview.candidate.authenticators[..200],
        hundred.authenticators[..]
    );
}

#[test]
fn importing_a_default_group_name_creates_an_ordinary_group() {
    let mut source = source();
    let target = VaultSnapshot::new_empty();
    for (name, system_group) in [("未分組", true), ("預設", true), ("預設", false)] {
        source.groups[0].name = name.into();
        source.groups[0].is_ungrouped = system_group;
        let separate = prepare_group_import(&target, &source).unwrap();
        assert_eq!(separate.candidate.groups[1].name, "預設(2)");
        assert!(!separate.candidate.groups[1].is_ungrouped);
        assert_eq!(separate.candidate.groups[0], target.groups[0]);
    }
}
