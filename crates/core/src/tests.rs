use super::*;

const T: i64 = 1_700_000_000_000;
const RFC_SEED: &str = "3132333435363738393031323334353637383930";

fn material() -> ManualMaterial {
    parse_manual(RFC_SEED, "", "").unwrap()
}
fn one_entry() -> VaultSnapshot {
    let vault = VaultSnapshot::new_empty(T);
    prepare_import(&vault, "旅行者", "", vault.groups[0].id, material(), T)
        .unwrap()
        .candidate
}
fn packed74() -> String {
    let suffix: String = b"US-1234-5678-9012"
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect();
    format!("{RFC_SEED}{suffix}")
}

#[test]
fn rfc6238_sha1_eight_digit_public_vectors() {
    // RFC6238 Appendix B, SHA1 column. Public sample key; no user credentials.
    // https://www.rfc-editor.org/rfc/rfc6238.html#appendix-B
    let secret = Secret::from_hex(RFC_SEED).unwrap();
    for (seconds, expected) in [
        (59_i64, "94287082"),
        (1_111_111_109, "07081804"),
        (1_111_111_111, "14050471"),
        (1_234_567_890, "89005924"),
        (2_000_000_000, "69279037"),
        (20_000_000_000, "65353130"),
    ] {
        assert_eq!(totp(&secret, seconds * 1000).unwrap(), expected);
    }
}

#[test]
fn totp_uses_utc_milliseconds_and_exact_thirty_second_boundaries() {
    let secret = material().secret;
    assert_eq!(totp(&secret, 30_000).unwrap(), "94287082");
    assert_eq!(
        totp(&secret, 30_000).unwrap(),
        totp(&secret, 59_999).unwrap()
    );
    assert_eq!(totp(&secret, 0).unwrap(), totp(&secret, 29_999).unwrap());
    assert_ne!(
        totp(&secret, 29_999).unwrap(),
        totp(&secret, 30_000).unwrap()
    );
    assert_ne!(
        totp(&secret, 59_999).unwrap(),
        totp(&secret, 60_000).unwrap()
    );
    assert!(totp(&secret, -1).is_err());
}

#[test]
fn secrets_are_strict_hex_and_debug_is_redacted() {
    let secret = Secret::from_hex(RFC_SEED).unwrap();
    assert_eq!(secret.hex(), RFC_SEED);
    assert_eq!(secret.bytes(), b"12345678901234567890");
    assert!(!format!("{secret:?}").contains(RFC_SEED));
    assert!(format!("{secret:?}").contains("REDACTED"));
    for bad in [
        "",
        "123",
        &"z".repeat(40),
        &"甲".repeat(40),
        &format!("{RFC_SEED}00"),
    ] {
        assert!(Secret::from_hex(bad).is_err());
    }
    let json = serde_json::to_string(&secret).unwrap();
    assert_eq!(json, format!("\"{RFC_SEED}\""));
    assert_eq!(serde_json::from_str::<Secret>(&json).unwrap(), secret);
    assert!(serde_json::from_str::<Secret>("[1,2,3]").is_err());
}

#[test]
fn manual40_and74_preserve_actual_supplied_fields() {
    let forty = parse_manual(
        &format!(" \n{RFC_SEED}\t"),
        "us-1234-5678-9012",
        "AbCd123456",
    )
    .unwrap();
    let seventy_four = parse_manual(&packed74().to_ascii_uppercase(), "", "").unwrap();
    assert_eq!(forty.secret, seventy_four.secret);
    assert_eq!(forty.serial.as_deref(), Some("US123456789012"));
    assert_eq!(seventy_four.serial, forty.serial);
    assert_eq!(display_serial("US123456789012"), "US-1234-5678-9012");
    assert_eq!(forty.restore_code.as_deref(), Some("AbCd123456"));
    assert!(seventy_four.restore_code.is_none());
    assert_eq!(forty.source, "manual-40");
    assert_eq!(seventy_four.source, "manual-74");
    assert!(parse_manual(&packed74(), "EU123456789012", "").is_err());
    assert!(parse_manual(&packed74(), "us123456789012", "").is_ok());
}

#[test]
fn packed_manual_serial_still_requires_valid_text_and_serial_format() {
    let pack = |serial: &[u8]| {
        format!(
            "{RFC_SEED}{}",
            serial
                .iter()
                .map(|byte| format!("{byte:02x}"))
                .collect::<String>()
        )
    };
    let material = parse_manual(&pack(b"us-1234-5678-9012"), "", "").unwrap();
    assert_eq!(material.serial.as_deref(), Some("US123456789012"));
    for invalid in [
        [0xff; 17].as_slice(),
        b"US-1234-5678-ABCD",
        b"US_1234-5678-9012",
        b"                 ",
    ] {
        assert!(parse_manual(&pack(invalid), "", "").is_err());
    }
}

#[test]
fn invalid_manual_formats_are_rejected_without_prefix_fallback() {
    for input in [
        "<xml/>".to_string(),
        format!("{RFC_SEED}extra"),
        format!("0x{RFC_SEED}"),
        format!("{RFC_SEED}{}", "00".repeat(17)),
        format!("{RFC_SEED}{}", "ff".repeat(17)),
        format!("{RFC_SEED}{}", "30".repeat(17)),
        "a".repeat(73),
        "a".repeat(75),
        "g".repeat(40),
    ] {
        assert!(parse_manual(&input, "", "").is_err());
    }
    for bad in [
        "U123456789012",
        "US-12345678-9012",
        "US12-3456789012",
        "12-1234-5678-9012",
        "US-1234-5678-901x",
        "美國123456789012",
    ] {
        assert!(normalize_serial(bad).is_err());
    }
    assert!(parse_manual(RFC_SEED, "", "code\nnew-line").is_err());
    assert!(parse_manual(RFC_SEED, "", &"a".repeat(65)).is_err());
    assert!(
        parse_manual(RFC_SEED, "", "   ")
            .unwrap()
            .restore_code
            .is_none()
    );
}

#[test]
fn duplicate_seed_keeps_name_group_id_and_existing_values() {
    let mut vault = one_entry();
    vault.revision = 7;
    let another = Uuid::new_v4();
    vault.groups.push(Group {
        id: another,
        name: "其他".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let original = vault.clone();
    let preview = prepare_import(&vault, "改名來源", "", another, material(), T + 99).unwrap();
    assert!(!preview.changed);
    assert!(!preview.new_secret);
    assert_eq!(preview.existing_id, Some(vault.authenticators[0].id));
    assert_eq!(preview.candidate, original);
    assert_eq!(vault, original);

    let supplement = parse_manual(&packed74(), "", "AbCd123456").unwrap();
    let preview =
        prepare_import(&vault, "改名來源", "補充備註", another, supplement, T + 100).unwrap();
    let entry = &preview.candidate.authenticators[0];
    assert!(preview.changed);
    assert!(!preview.new_secret);
    assert_eq!(entry.id, original.authenticators[0].id);
    assert_eq!(entry.group_id, original.groups[0].id);
    assert_eq!(entry.name, "旅行者");
    assert_eq!(entry.description, "補充備註");
    assert_eq!(entry.serial.as_deref(), Some("US123456789012"));
    assert_eq!(entry.restore_code.as_deref(), Some("AbCd123456"));
    assert_eq!(entry.restore_code_source.as_deref(), Some("manual"));
    assert_eq!(preview.candidate.revision, 7);
    assert_eq!(vault, original, "Preview must not mutate live data");
}

#[test]
fn conflicts_reject_and_same_serial_different_seed_is_a_separate_candidate() {
    let mut vault = one_entry();
    vault.authenticators[0].serial = Some("US123456789012".into());
    vault.authenticators[0].restore_code = Some("AbCd123456".into());
    vault.authenticators[0].restore_code_source = Some("manual".into());
    let original = vault.clone();
    let group = vault.groups[0].id;
    assert!(
        prepare_import(
            &vault,
            "x",
            "",
            group,
            parse_manual(RFC_SEED, "EU123456789012", "").unwrap(),
            T
        )
        .is_err()
    );
    assert!(
        prepare_import(
            &vault,
            "x",
            "",
            group,
            parse_manual(RFC_SEED, "", "different").unwrap(),
            T
        )
        .is_err()
    );
    assert_eq!(vault, original);
    let other = parse_manual(&"ab".repeat(20), "US123456789012", "").unwrap();
    let preview = prepare_import(&vault, "旅行者", "", group, other, T + 1).unwrap();
    assert!(preview.new_secret);
    assert!(preview.summary.contains("相同序號"));
    assert_eq!(preview.candidate.authenticators.len(), 2);
    assert_eq!(
        preview.candidate.authenticators[0],
        original.authenticators[0]
    );
    assert_ne!(
        preview.candidate.authenticators[0].id,
        preview.candidate.authenticators[1].id
    );
}

#[test]
fn snapshot_roundtrip_preserves_unicode_nulls_whitespace_and_redacts_seed() {
    let mut vault = one_entry();
    vault.authenticators[0].name = "旅行者 Привет 한글 日本語".into();
    vault.authenticators[0].description = "  \n註記\t保留  ".into();
    let json = serde_json::to_vec(&vault).unwrap();
    let decoded: VaultSnapshot = serde_json::from_slice(&json).unwrap();
    decoded.validate().unwrap();
    assert_eq!(decoded, vault);
    assert!(!format!("{vault:?}").contains(RFC_SEED));
    let mut no_seed = vault.clone();
    no_seed.authenticators[0].secret = None;
    no_seed.validate().unwrap();
    assert_eq!(
        serde_json::from_slice::<VaultSnapshot>(&serde_json::to_vec(&no_seed).unwrap()).unwrap(),
        no_seed
    );
}

#[test]
fn strict_dto_rejects_unknown_and_duplicate_fields() {
    let vault = one_entry();
    let mut json = serde_json::to_value(&vault).unwrap();
    json["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<VaultSnapshot>(json).is_err());
    let mut json = serde_json::to_value(&vault).unwrap();
    json["authenticators"][0]["unknown"] = serde_json::json!(true);
    assert!(serde_json::from_value::<VaultSnapshot>(json).is_err());
    let json = serde_json::to_string(&vault).unwrap();
    let duplicate = json.replacen(
        "\"schema_version\":1",
        "\"schema_version\":1,\"schema_version\":1",
        1,
    );
    assert!(serde_json::from_str::<VaultSnapshot>(&duplicate).is_err());
    let invalid_secret = json.replace(RFC_SEED, &"z".repeat(40));
    assert!(serde_json::from_str::<VaultSnapshot>(&invalid_secret).is_err());
}

#[test]
fn snapshot_rejects_relationship_corruption_and_unsupported_parameters() {
    let base = one_entry();
    let mut changed = base.clone();
    changed.groups.clear();
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.groups[0].is_ungrouped = false;
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.groups[0].sort_order = 1;
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.authenticators[0].sort_order = 1;
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.authenticators[0].group_id = Uuid::new_v4();
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.authenticators[0].id = changed.groups[0].id;
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.authenticators[0].digits = 6;
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.authenticators[0].algorithm = "SHA256".into();
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.authenticators[0].period_secs = 60;
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.authenticators[0].name = " \t".into();
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.scope = "group".into();
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed.schema_version = 2;
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    changed
        .authenticators
        .push(changed.authenticators[0].clone());
    assert!(changed.validate().is_err());
    let mut changed = base.clone();
    let mut duplicate = changed.authenticators[0].clone();
    duplicate.id = Uuid::new_v4();
    duplicate.sort_order = 1;
    changed.authenticators.push(duplicate);
    assert!(changed.validate().is_err());
}

#[test]
fn exact_text_byte_limits_and_above_limit_are_enforced() {
    let mut vault = one_entry();
    vault.authenticators[0].name = "a".repeat(MAX_NAME_BYTES);
    vault.authenticators[0].description = "b".repeat(MAX_DESCRIPTION_BYTES);
    vault.validate().unwrap();
    vault.authenticators[0].name.push('a');
    assert!(vault.validate().is_err());
    vault.authenticators[0].name.pop();
    vault.authenticators[0].description.push('b');
    assert!(vault.validate().is_err());
    vault.authenticators[0].description.pop();
    vault.groups.push(Group {
        id: Uuid::new_v4(),
        name: "g".repeat(MAX_GROUP_NAME_BYTES),
        sort_order: 1,
        is_ungrouped: false,
    });
    vault.validate().unwrap();
    vault.groups[1].name.push('g');
    assert!(vault.validate().is_err());
    let mut vault = one_entry();
    vault.authenticators[0].name = "甲".repeat(342);
    assert!(vault.validate().is_err());
}

#[test]
fn exact_group_entry_and_global_limits_are_enforced() {
    let mut vault = VaultSnapshot::new_empty(T);
    for index in 1..MAX_GROUPS {
        vault.groups.push(Group {
            id: Uuid::new_v4(),
            name: format!("分組{index}"),
            sort_order: index as u32,
            is_ungrouped: false,
        });
    }
    vault.validate().unwrap();
    vault.groups.push(Group {
        id: Uuid::new_v4(),
        name: "超出".into(),
        sort_order: 20,
        is_ungrouped: false,
    });
    assert!(vault.validate().is_err());
    vault.groups.pop();
    let prototype = one_entry().authenticators.remove(0);
    for group_index in 0..10 {
        for index in 0..MAX_GROUP_ENTRIES {
            let mut entry = prototype.clone();
            entry.id = Uuid::new_v4();
            entry.group_id = vault.groups[group_index].id;
            entry.sort_order = index as u32;
            let mut bytes = [0_u8; 20];
            bytes[..8].copy_from_slice(&(vault.authenticators.len() as u64).to_be_bytes());
            entry.secret = Some(Secret::from_bytes(bytes));
            vault.authenticators.push(entry);
        }
    }
    vault.validate().unwrap();
    assert!(prepare_import(&vault, "超出", "", vault.groups[10].id, material(), T).is_err());
    // Duplicate seeds must still be rejected across groups at the 1000-row limit.
    let last_secret = vault.authenticators.last().unwrap().secret.clone();
    vault.authenticators.last_mut().unwrap().secret = vault.authenticators[0].secret.clone();
    assert!(vault.validate().unwrap_err().contains("相同的長密鑰"));
    vault.authenticators.last_mut().unwrap().secret = last_secret;
    vault.validate().unwrap();
    let mut extra = prototype;
    extra.id = Uuid::new_v4();
    extra.group_id = vault.groups[10].id;
    extra.sort_order = 0;
    vault.authenticators.push(extra);
    assert!(vault.validate().is_err());
    vault.authenticators.pop();
    vault.authenticators.last_mut().unwrap().group_id = vault.groups[0].id;
    vault.authenticators.last_mut().unwrap().sort_order = 100;
    assert!(vault.validate().is_err());
}

#[test]
fn provenance_limit_matches_the_native_array_reader_limit() {
    let mut vault = one_entry();
    let source = Provenance {
        source_vault_id: Uuid::new_v4(),
        source_revision: 1,
        scope: "full".into(),
        imported_at_ms: T,
        source_time_sync: None,
    };
    vault.provenance = vec![source.clone(); 1000];
    vault.validate().unwrap();
    let json = serde_json::to_vec(&vault).unwrap();
    let decoded: VaultSnapshot = serde_json::from_slice(&json).unwrap();
    assert_eq!(decoded, vault);
    decoded.validate().unwrap();
    vault.provenance.push(source);
    assert!(vault.validate().is_err());
}

#[test]
fn group_name_uniqueness_matches_gui_case_and_whitespace_rules() {
    let mut vault = VaultSnapshot::new_empty(T);
    for name in [" Main ", "mAiN"] {
        vault.groups.push(Group {
            id: Uuid::new_v4(),
            name: name.into(),
            sort_order: vault.groups.len() as u32,
            is_ungrouped: false,
        });
    }
    assert!(vault.validate().is_err());
    vault.groups[2].name = "Other".into();
    vault.validate().unwrap();
    vault.groups[1].name = "ПРИВЕТ".into();
    vault.groups[2].name = "привет".into();
    assert!(vault.validate().is_err());
}
