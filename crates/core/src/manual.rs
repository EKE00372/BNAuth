use crate::{
    ImportPreview, MAX_DESCRIPTION_BYTES, MAX_GROUP_ENTRIES, MAX_NAME_BYTES, PreparedImport,
    RecoveryTarget, Secret, Uuid, VaultSnapshot, bounded, valid_name,
};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ManualMaterial {
    pub secret: Secret,
    pub(crate) serial: Option<String>,
    pub(crate) restore_code: Option<String>,
}

impl VaultSnapshot {
    /// Return up to five group and record names for the duplicate reminder.
    pub fn matching_authenticator_names(&self, secret: &Secret) -> Vec<(String, String)> {
        self.authenticators
            .iter()
            .filter(|entry| &entry.secret == secret)
            .take(5)
            .map(|entry| {
                let group = self
                    .groups
                    .iter()
                    .find(|group| group.id == entry.group_id)
                    .expect("record belongs to an existing group");
                (group.name.clone(), entry.name.clone())
            })
            .collect()
    }
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
    let mut compact = if text.len() == 17 {
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
    compact.make_ascii_uppercase();
    Ok(Some(compact))
}

fn dashed_serial(serial: &str) -> String {
    format!(
        "{}-{}-{}-{}",
        &serial[..2],
        &serial[2..6],
        &serial[6..10],
        &serial[10..14]
    )
}

pub fn format_serial_input(serial: &str) -> Result<Option<String>, String> {
    Ok(normalize_serial(serial)?.as_deref().map(dashed_serial))
}

pub fn display_serial(serial: &str) -> String {
    format_serial_input(serial)
        .ok()
        .flatten()
        .unwrap_or_else(|| serial.to_owned())
}

/// Format a submitted recovery field only when the entered value is valid.
pub struct SubmittedSerial(pub(crate) String);

pub fn submit_serial_input(
    serial: &mut String,
    required: bool,
) -> Result<Option<SubmittedSerial>, String> {
    let compact = normalize_serial(serial)?;
    if let Some(value) = compact.as_deref() {
        *serial = dashed_serial(value);
    } else if required {
        return Err("請輸入序號。".into());
    }
    Ok(compact.map(SubmittedSerial))
}

fn material_format_error(field: &str, hint: &str) -> String {
    format!("{field}格式不正確。{hint}")
}

const MAX_RESTORE_BYTES: usize = 64;

pub fn submit_restore_input(code: &mut str, required: bool) -> Result<(), String> {
    if code.is_empty() && !required {
        return Ok(());
    }
    if code.len() != 10 || !code.bytes().all(|b| b.is_ascii_alphanumeric()) {
        return Err(material_format_error(
            "還原碼",
            "請輸入 10 位大寫英文字母或數字。",
        ));
    }
    code.make_ascii_uppercase();
    Ok(())
}

pub(crate) fn validate_restore(text: &str) -> Result<(), String> {
    // This is supplied recovery data, not a value derived or verified against the seed.
    // Preserve case and contents; the online recovery adapter owns provider validation.
    valid_name(text, MAX_RESTORE_BYTES, "還原碼")?;
    if text.chars().any(char::is_control) {
        return Err("還原碼不可包含控制字元。".into());
    }
    Ok(())
}

pub fn parse_manual(input: &str, serial: &str, restore: &str) -> Result<ManualMaterial, String> {
    ManualMaterial::from_key(input)?
        .with_fields(normalize_serial(serial)?.map(SubmittedSerial), restore)
}

impl ManualMaterial {
    pub fn from_key(input: &str) -> Result<Self, String> {
        let input = input.trim();
        if !matches!(input.len(), 40 | 74) || !input.is_char_boundary(40) {
            return Err(material_format_error("密鑰", ""));
        }
        let secret =
            Secret::from_hex(&input[..40]).map_err(|_| material_format_error("密鑰", ""))?;
        let embedded = if input.len() == 74 {
            let mut bytes = [0; 17];
            for (byte, pair) in bytes
                .iter_mut()
                .zip(input.as_bytes()[40..].as_chunks::<2>().0)
            {
                *byte = (crate::secret::nibble(pair[0])
                    .map_err(|_| material_format_error("密鑰", ""))?
                    << 4)
                    | crate::secret::nibble(pair[1])
                        .map_err(|_| material_format_error("密鑰", ""))?;
            }
            let value =
                std::str::from_utf8(&bytes).map_err(|_| "74位資料的序號不是有效ASCII文字。")?;
            let normalized = normalize_serial(value)?;
            if normalized.is_none() {
                return Err("74位資料缺少有效序號。".into());
            }
            normalized
        } else {
            None
        };
        Ok(Self {
            secret,
            serial: embedded,
            restore_code: None,
        })
    }

    pub fn embedded_serial_display(&self) -> Option<String> {
        self.serial.as_deref().map(dashed_serial)
    }

    pub fn with_fields(
        mut self,
        serial: Option<SubmittedSerial>,
        restore: &str,
    ) -> Result<Self, String> {
        if let (Some(a), Some(b)) = (&self.serial, &serial)
            && a != &b.0
        {
            return Err("74位資料內的序號與手填序號不同，請先核對。".into());
        }
        self.restore_code = if restore.trim().is_empty() {
            None
        } else {
            validate_restore(restore)?;
            Some(restore.to_owned())
        };
        self.serial = self.serial.or_else(|| serial.map(|s| s.0));
        Ok(self)
    }
}

pub(crate) fn validate_authenticator_details(name: &str, description: &str) -> Result<(), String> {
    valid_name(name, MAX_NAME_BYTES, "驗證器名稱")?;
    bounded(description, MAX_DESCRIPTION_BYTES, "備註")
}

/// An import destination, optionally carrying the group's count from the current vault.
#[derive(Clone, Copy)]
pub struct ImportGroup {
    pub(crate) id: Uuid,
    entry_count: Option<usize>,
}

impl From<Uuid> for ImportGroup {
    fn from(id: Uuid) -> Self {
        Self {
            id,
            entry_count: None,
        }
    }
}

impl ImportGroup {
    pub fn with_count(id: Uuid, entry_count: usize) -> Self {
        Self {
            id,
            entry_count: Some(entry_count),
        }
    }

    pub(crate) fn append_order(self, vault: &VaultSnapshot) -> Result<u32, String> {
        if !vault.groups.iter().any(|group| group.id == self.id) {
            return Err("目標分組已不存在。".into());
        }
        let count = self.entry_count.unwrap_or_else(|| {
            vault
                .authenticators
                .iter()
                .filter(|entry| entry.group_id == self.id)
                .count()
        });
        if count >= MAX_GROUP_ENTRIES {
            return Err("分組數量已達上限。".into());
        }
        Ok(count as u32)
    }
}

/// Preview one key against the committed vault without creating a stored record.
/// The caller must confirm the preview and submit its candidate through Repository.
pub fn prepare_import<'a>(
    vault: &VaultSnapshot,
    name: &'a str,
    description: &'a str,
    group: impl Into<ImportGroup>,
    material: ManualMaterial,
) -> Result<PreparedImport<'a>, String> {
    let target = RecoveryTarget {
        name,
        description,
        group: group.into(),
    };
    let preview = ImportPreview::new(
        vault,
        target.prepare(vault)?,
        material.secret,
        material.serial,
        material.restore_code.map(zeroize::Zeroizing::new),
    );
    Ok(PreparedImport {
        preview,
        name,
        description,
    })
}
