use crate::{
    Authenticator, MAX_DESCRIPTION_BYTES, MAX_ENTRIES, MAX_GROUP_ENTRIES, MAX_NAME_BYTES, Secret,
    Uuid, VaultSnapshot, bounded, valid_name,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualMaterial {
    pub secret: Secret,
    pub serial: Option<String>,
    pub restore_code: Option<String>,
    pub source: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImportPreview {
    pub candidate: VaultSnapshot,
    pub existing_id: Option<Uuid>,
    pub changed: bool,
    pub new_secret: bool,
    pub summary: String,
}

/// Accept compact AA123456789012 or the exact AA-1234-5678-9012 shape.
pub fn normalize_serial(text: &str) -> Result<Option<String>, String> {
    let text = text.trim();
    if text.is_empty() {
        return Ok(None);
    }
    if !text.is_ascii() {
        return Err("序號格式應為AA-1234-5678-9012。".into());
    }
    let compact = if text.len() == 17 {
        let bytes = text.as_bytes();
        if bytes[2] != b'-' || bytes[7] != b'-' || bytes[12] != b'-' {
            return Err("序號的分隔符位置無效。".into());
        }
        format!(
            "{}{}{}{}",
            &text[..2],
            &text[3..7],
            &text[8..12],
            &text[13..17]
        )
    } else if text.len() == 14 {
        text.to_owned()
    } else {
        return Err("序號格式應為AA-1234-5678-9012。".into());
    };
    if !compact.as_bytes()[..2].iter().all(u8::is_ascii_alphabetic)
        || !compact.as_bytes()[2..].iter().all(u8::is_ascii_digit)
    {
        return Err("序號必須為兩個英文字母及十二位數字。".into());
    }
    Ok(Some(compact.to_ascii_uppercase()))
}

pub fn display_serial(serial: &str) -> String {
    match normalize_serial(serial) {
        Ok(Some(serial)) => format!(
            "{}-{}-{}-{}",
            &serial[..2],
            &serial[2..6],
            &serial[6..10],
            &serial[10..14]
        ),
        _ => serial.to_owned(),
    }
}

pub(crate) fn validate_restore(text: &str) -> Result<(), String> {
    // This is supplied recovery data, not a value derived or verified against the seed.
    // Preserve case and contents; the online recovery adapter owns provider validation.
    valid_name(text, 64, "還原碼")?;
    if text.chars().any(char::is_control) {
        return Err("還原碼不可包含控制字元。".into());
    }
    Ok(())
}

pub fn parse_manual(input: &str, serial: &str, restore: &str) -> Result<ManualMaterial, String> {
    let input = input.trim();
    if !matches!(input.len(), 40 | 74) || !input.as_bytes().iter().all(u8::is_ascii_hexdigit) {
        return Err(
            "只接受40位純密鑰或74位未加密secretdata；不接受XML、加密字串或其他格式。".into(),
        );
    }
    let secret = Secret::from_hex(&input[..40])?;
    let embedded = if input.len() == 74 {
        let mut bytes = Vec::with_capacity(17);
        for pair in input.as_bytes()[40..].as_chunks::<2>().0 {
            let pair = std::str::from_utf8(pair).map_err(|_| "序號編碼無效。")?;
            bytes.push(u8::from_str_radix(pair, 16).map_err(|_| "序號編碼無效。")?);
        }
        let value = std::str::from_utf8(&bytes).map_err(|_| "74位資料的序號不是有效ASCII文字。")?;
        let normalized = normalize_serial(value)?;
        if normalized.is_none() {
            return Err("74位資料缺少有效序號。".into());
        }
        normalized
    } else {
        None
    };
    let entered = normalize_serial(serial)?;
    if let (Some(a), Some(b)) = (&embedded, &entered)
        && a != b
    {
        return Err("74位資料內的序號與手填序號不同，請先核對。".into());
    }
    let restore_code = if restore.trim().is_empty() {
        None
    } else {
        validate_restore(restore)?;
        Some(restore.to_owned())
    };
    Ok(ManualMaterial {
        secret,
        serial: embedded.or(entered),
        restore_code,
        source: if input.len() == 74 {
            "manual-74"
        } else {
            "manual-40"
        }
        .into(),
    })
}

/// Produce a candidate only. The caller must show the preview and commit it through Repository.
/// Revision is intentionally unchanged here; the successful repository transaction advances it.
pub fn prepare_import(
    vault: &VaultSnapshot,
    name: &str,
    description: &str,
    group: Uuid,
    material: ManualMaterial,
    timestamp_ms: i64,
) -> Result<ImportPreview, String> {
    vault.validate()?;
    valid_name(name, MAX_NAME_BYTES, "驗證器名稱")?;
    bounded(description, MAX_DESCRIPTION_BYTES, "備註")?;
    if !vault.groups.iter().any(|item| item.id == group) {
        return Err("目標分組已不存在。".into());
    }
    if timestamp_ms < 0 {
        return Err("系統時間早於Unix epoch，請先校正電腦時間。".into());
    }
    if let Some(serial) = &material.serial
        && normalize_serial(serial)?.as_deref() != Some(serial.as_str())
    {
        return Err("匯入序號尚未正規化。".into());
    }
    if let Some(code) = &material.restore_code {
        validate_restore(code)?;
    }
    valid_name(&material.source, 256, "來源")?;
    let mut candidate = vault.clone();
    if let Some(index) = vault
        .authenticators
        .iter()
        .position(|item| item.secret.as_ref() == Some(&material.secret))
    {
        let original = &vault.authenticators[index];
        if matches!((&original.serial, &material.serial), (Some(a), Some(b)) if a != b) {
            return Err("同一密鑰已有不同序號，未覆寫既有資料；請先核對來源。".into());
        }
        if matches!((&original.restore_code, &material.restore_code), (Some(a), Some(b)) if a != b)
        {
            return Err("同一密鑰已有不同還原碼，未覆寫既有資料；請先核對來源。".into());
        }
        let entry = &mut candidate.authenticators[index];
        let mut fields = Vec::new();
        if entry.serial.is_none() && material.serial.is_some() {
            entry.serial = material.serial;
            fields.push("序號");
        }
        if entry.restore_code.is_none() && material.restore_code.is_some() {
            entry.restore_code = material.restore_code;
            entry.restore_code_source = Some("manual".into());
            fields.push("還原碼");
        }
        if entry.description.trim().is_empty() && !description.trim().is_empty() {
            entry.description = description.to_owned();
            fields.push("備註");
        }
        let changed = !fields.is_empty();
        if changed {
            entry.updated_at_ms = timestamp_ms;
            candidate.updated_at_ms = timestamp_ms;
        }
        let summary = if changed {
            format!(
                "密鑰已存在；確認後只補齊{}。保留既有名稱與分組。",
                fields.join("、")
            )
        } else {
            "密鑰已存在，沒有需要補齊的資料；不重複新增。".into()
        };
        candidate.validate()?;
        return Ok(ImportPreview {
            candidate,
            existing_id: Some(original.id),
            changed,
            new_secret: false,
            summary,
        });
    }
    if vault.authenticators.len() >= MAX_ENTRIES {
        return Err("已達1000筆驗證器上限。".into());
    }
    let count = vault
        .authenticators
        .iter()
        .filter(|entry| entry.group_id == group)
        .count();
    if count >= MAX_GROUP_ENTRIES {
        return Err("目標分組已達100筆上限。".into());
    }
    let serial_warning = material.serial.as_ref().is_some_and(|serial| {
        vault
            .authenticators
            .iter()
            .any(|entry| entry.serial.as_ref() == Some(serial))
    });
    candidate.authenticators.push(Authenticator {
        id: Uuid::new_v4(),
        group_id: group,
        sort_order: count as u32,
        name: name.to_owned(),
        description: description.to_owned(),
        secret: Some(material.secret),
        serial: material.serial,
        restore_code_source: material.restore_code.as_ref().map(|_| "manual".into()),
        restore_code: material.restore_code,
        digits: 8,
        algorithm: "SHA1".into(),
        period_secs: 30,
        created_at_ms: timestamp_ms,
        updated_at_ms: timestamp_ms,
        source: material.source,
        recovery_history: Vec::new(),
        legacy_metadata: Vec::new(),
    });
    candidate.updated_at_ms = timestamp_ms;
    candidate.validate()?;
    let summary = if serial_warning {
        "將新增1個驗證器。注意：已有相同序號但密鑰不同的資料，請核對配套資料；不會覆寫既有密鑰。"
    } else {
        "將新增1個驗證器。匯入只檢查格式，不表示戰網已驗證或綁定。"
    }
    .into();
    Ok(ImportPreview {
        candidate,
        existing_id: None,
        changed: true,
        new_secret: true,
        summary,
    })
}
