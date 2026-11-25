use super::*;
use crate::Group;

const SERIAL: &str = "us-1234-5678-9012";
const CODE: &str = "aBcD2E3f4g";

fn new_target(vault: &VaultSnapshot) -> RecoveryTarget<'static> {
    RecoveryTarget {
        name: "測試驗證器",
        description: "owner@example.test",
        group: vault.ungrouped_id().unwrap(),
    }
}

#[test]
fn new_recovery_appends_an_independent_same_key_record_in_each_target_group() {
    let mut vault = VaultSnapshot::new_empty();
    let group = Uuid::new_v4();
    vault.groups.push(Group {
        id: group,
        name: "另一分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let seed = Secret::from_bytes([4; 20]);
    let other_material = RecoveryMaterial::new(SERIAL, "OtherGroup").unwrap();
    let first = prepare_recovery(&vault, new_target(&vault), &other_material, &seed)
        .unwrap()
        .candidate(&vault)
        .unwrap();
    let target = RecoveryTarget {
        name: "另一分組的驗證器",
        description: "",
        group,
    };
    let material = RecoveryMaterial::new(SERIAL, CODE).unwrap();
    let added = prepare_recovery(&first, target, &material, &seed).unwrap();
    let added = added.candidate(&first).unwrap();
    assert_eq!(added.authenticators[0], first.authenticators[0]);
    let record = &added.authenticators[1];
    assert_ne!(record.id, first.authenticators[0].id);
    assert_eq!(record.group_id, group);
    assert_eq!(record.secret, seed);
    assert_eq!(record.restore_code.as_deref(), Some(CODE));
    let repeated = prepare_recovery(&added, target, &material, &seed).unwrap();
    let repeated = repeated.candidate(&added).unwrap();
    assert_eq!(
        &repeated.authenticators[..2],
        added.authenticators.as_slice()
    );
    let another = &repeated.authenticators[2];
    assert_ne!(another.id, record.id);
    assert_eq!(another.group_id, group);
    assert_eq!(another.sort_order, 1);
    assert_eq!(another.secret, seed);
    assert_eq!(another.restore_code.as_deref(), Some(CODE));
}
