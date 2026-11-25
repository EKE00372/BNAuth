//! Integration of persisted domain identities with the transient u64 GUI projection.
use super::*;
use bnauth_core::{TIME_SOURCE, TimeSyncRecord, parse_manual, prepare_import};

const T: i64 = 1_700_000_000_000;

fn fixture() -> VaultSnapshot {
    let mut vault = VaultSnapshot::new_empty();
    for name in ["A", "B"] {
        let sort_order = vault.groups.len() as u32;
        vault.groups.push(bnauth_core::Group {
            id: Uuid::new_v4(),
            name: name.into(),
            sort_order,
            is_ungrouped: false,
        });
    }
    // Deliberately interleave groups in the persisted array. sort_order is authoritative.
    for (name, group_index, key) in [
        ("A first", 1, "02"),
        ("B first", 2, "01"),
        ("A second", 1, "03"),
    ] {
        let material = parse_manual(&key.repeat(20), "", "").unwrap();
        vault = prepare_import(
            &vault,
            name,
            "  備註\nПривет\t한글  ",
            vault.groups[group_index].id,
            material,
        )
        .unwrap()
        .candidate(&vault)
        .unwrap();
    }
    vault.revision = 42;
    let sample = TimeSyncRecord {
        source: TIME_SOURCE.into(),
        server_time_ms: T,
        local_wall_ms_at_sample: T - 5,
        offset_ms: 5,
        rtt_ms: 20,
    };
    vault.service_time_sync.battlenet = Some(sample);
    let entry = &mut vault.authenticators[0];
    entry.serial = Some("US123456789012".into());
    entry.restore_code = Some("AbCd123456".into());
    vault.validate().unwrap();
    vault
}

#[test]
fn bridge_local_edits_only_change_requested_display_membership_and_order_fields() {
    let original = fixture();
    let mut view = VaultModel::from_snapshot(&original);
    let a = view
        .groups
        .iter()
        .find(|group| group.name == "A")
        .unwrap()
        .id;
    let b = view
        .groups
        .iter()
        .find(|group| group.name == "B")
        .unwrap()
        .id;
    let moving = view
        .entries
        .iter()
        .find(|entry| entry.record.name == "A first")
        .unwrap()
        .id;
    let b_first = view
        .entries
        .iter()
        .find(|entry| entry.record.name == "B first")
        .unwrap()
        .id;
    let persistent = view.entry(moving).unwrap().record.id;
    view.update_entry(moving, "改名 Привет", "\n新說明\t ")
        .unwrap();
    view.move_entries(&[moving], b).unwrap();
    view.reorder_entry(moving, Some(b_first)).unwrap();
    view.rename_group(a, "遊戲").unwrap();
    view.move_group(b, -1).unwrap();
    let saved = view.snapshot();
    assert_eq!(saved.vault_id, original.vault_id);
    assert_eq!(
        saved.revision, original.revision,
        "Repository owns revision advancement"
    );
    assert_eq!(saved.service_time_sync, original.service_time_sync);
    assert_eq!(saved.authenticators.len(), original.authenticators.len());
    for after in saved.authenticators.iter() {
        let mut expected = original
            .authenticators
            .iter()
            .find(|entry| entry.id == after.id)
            .unwrap()
            .clone();
        // Only these UI-editable properties may change.
        expected.name = after.name.clone();
        expected.description = after.description.clone();
        expected.group_id = after.group_id;
        expected.sort_order = after.sort_order;
        assert_eq!(&expected, after);
    }
    let moved = saved
        .authenticators
        .iter()
        .find(|entry| entry.id == persistent)
        .unwrap();
    assert_eq!(moved.name, "改名 Привет");
    assert_eq!(moved.description, "\n新說明\t ");
    assert_eq!(moved.sort_order, 0);
    assert_eq!(moved.group_id, view.persistent_group(b).unwrap());
    let mut reopened = VaultModel::from_snapshot(&saved);
    assert_eq!(reopened.snapshot(), saved);
}

#[test]
fn applying_committed_snapshots_keeps_transient_keys_for_the_same_uuids() {
    let original = fixture();
    let mut view = VaultModel::from_snapshot(&original);
    let groups: std::collections::HashMap<_, _> = view
        .groups
        .iter()
        .map(|group| (group.persistent_id, group.id))
        .collect();
    let entries: std::collections::HashMap<_, _> = view
        .entries
        .iter()
        .map(|entry| (entry.record.id, entry.id))
        .collect();
    let mut acknowledged = original.clone();
    acknowledged.revision += 1;
    acknowledged.groups.swap(1, 2);
    for (index, group) in acknowledged.groups.iter_mut().enumerate() {
        group.sort_order = index as u32;
    }
    acknowledged.authenticators.reverse();
    acknowledged.authenticators[0].name = "已保存改名".into();
    acknowledged.validate().unwrap();
    view.apply_snapshot(&acknowledged);
    for group in &view.groups {
        assert_eq!(group.id, groups[&group.persistent_id]);
    }
    for entry in &view.entries {
        assert_eq!(entry.id, entries[&entry.record.id]);
    }
    assert_eq!(view.snapshot(), acknowledged);
}

#[test]
fn moving_all_entries_out_then_deleting_the_empty_group_preserves_every_record_on_reopen() {
    let original = fixture();
    let mut view = VaultModel::from_snapshot(&original);
    let a = view
        .groups
        .iter()
        .find(|group| group.name == "A")
        .unwrap()
        .id;
    let a_uuid = view.persistent_group(a).unwrap();
    let b_first = view
        .entries
        .iter()
        .find(|entry| entry.record.name == "B first")
        .unwrap()
        .id;
    assert_eq!(view.move_entries(&[b_first], a).unwrap(), 1);
    assert!(view.delete_group(a).is_err());
    let selected = view.filtered(a, "", false);
    assert_eq!(view.move_entries(&selected, UNGROUPED_ID).unwrap(), 3);
    view.delete_group(a).unwrap();

    let saved = view.snapshot();
    saved.validate().unwrap();
    assert_eq!(saved.authenticators.len(), original.authenticators.len());
    assert!(!saved.groups.iter().any(|group| group.id == a_uuid));
    let mut expected = original.clone();
    expected.groups.retain(|group| group.id != a_uuid);
    for (index, group) in expected.groups.iter_mut().enumerate() {
        group.sort_order = index as u32;
    }
    let display_order = ["A first", "A second", "B first"];
    for entry in expected.authenticators.iter_mut() {
        entry.group_id = original.groups[0].id;
        entry.sort_order = display_order
            .iter()
            .position(|name| *name == entry.name)
            .unwrap() as u32;
    }
    assert_eq!(saved, expected);
    let mut reopened = VaultModel::from_snapshot(&saved);
    assert_eq!(reopened.snapshot(), saved);
}
