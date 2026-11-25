use crate::{
    Authenticator, ImportGroup, LegacyField, MAX_ENTRIES, Secret, Uuid, VaultSnapshot,
    manual::{validate_authenticator_details, validate_restore},
    normalize_serial,
};
use zeroize::Zeroizing;

#[derive(Clone)]
pub struct RecoveryMaterial {
    serial: String,
    restore_code: Zeroizing<String>,
}

impl RecoveryMaterial {
    pub fn new(serial: &str, restore_code: &str) -> Result<Self, String> {
        let serial = normalize_serial(serial)?.ok_or("請輸入序號。")?;
        Self::from_submitted_serial(crate::SubmittedSerial(serial), restore_code)
    }

    pub fn from_submitted_serial(
        serial: crate::SubmittedSerial,
        restore_code: &str,
    ) -> Result<Self, String> {
        validate_restore(restore_code)?;
        Ok(Self {
            serial: serial.0,
            restore_code: Zeroizing::new(restore_code.to_owned()),
        })
    }

    pub fn serial(&self) -> &str {
        &self.serial
    }

    pub fn restore_code(&self) -> &str {
        self.restore_code.as_str()
    }
}

#[derive(Clone, Copy)]
pub struct RecoveryTarget<'a, G = Uuid> {
    pub name: &'a str,
    pub description: &'a str,
    pub group: G,
}

impl<G: Copy + Into<ImportGroup>> RecoveryTarget<'_, G> {
    /// Check the intended destination before starting a recovery job.
    pub fn validate(&self, vault: &VaultSnapshot) -> Result<(), String> {
        self.validated_sort_order(vault).map(|_| ())
    }

    /// Keep the validated placement for this committed revision during recovery.
    pub fn prepare(&self, vault: &VaultSnapshot) -> Result<RecoveryDestination, String> {
        let sort_order = self.validated_sort_order(vault)?;
        Ok(RecoveryDestination {
            base_version: (vault.vault_id, vault.revision),
            group: self.group.into().id,
            sort_order,
        })
    }

    fn validated_sort_order(&self, vault: &VaultSnapshot) -> Result<u32, String> {
        validate_authenticator_details(self.name, self.description)?;
        if vault.authenticators.len() >= MAX_ENTRIES {
            return Err("驗證器數量已達上限。".into());
        }
        self.group.into().append_order(vault)
    }
}

#[derive(Clone, PartialEq, Eq)]
pub struct RecoveryDestination {
    base_version: (Uuid, u64),
    group: Uuid,
    sort_order: u32,
}

impl RecoveryDestination {
    pub fn preview(
        self,
        vault: &VaultSnapshot,
        material: RecoveryMaterial,
        secret: Secret,
    ) -> Result<ImportPreview, String> {
        if (vault.vault_id, vault.revision) != self.base_version {
            return Err("恢復時使用的資料庫已變更。".into());
        }
        Ok(ImportPreview::new(
            vault,
            self,
            secret,
            Some(material.serial),
            Some(material.restore_code),
        ))
    }
}

/// A reviewed authenticator. Its record is created on confirmation.
#[derive(Clone, PartialEq, Eq)]
pub struct ImportPreview {
    destination: RecoveryDestination,
    pub secret: Secret,
    serial: Option<String>,
    restore_code: Option<Zeroizing<String>>,
    pub matches: Vec<(String, String)>,
}

impl ImportPreview {
    pub(crate) fn new(
        vault: &VaultSnapshot,
        destination: RecoveryDestination,
        secret: Secret,
        serial: Option<String>,
        restore_code: Option<Zeroizing<String>>,
    ) -> Self {
        let matches = vault.matching_authenticator_names(&secret);
        Self {
            destination,
            secret,
            serial,
            restore_code,
            matches,
        }
    }

    /// Apply the final form values and create one record at the time of confirmation.
    pub fn confirm(
        &self,
        vault: &VaultSnapshot,
        target: RecoveryTarget<'_, impl Copy + Into<ImportGroup>>,
    ) -> Result<VaultSnapshot, String> {
        if self.destination.base_version != (vault.vault_id, vault.revision) {
            return Err("資料庫已變更，請重新核對恢復結果。".into());
        }
        validate_authenticator_details(target.name, target.description)?;
        let group = target.group.into();
        let order = if group.id == self.destination.group {
            self.destination.sort_order
        } else {
            group.append_order(vault)?
        };
        let mut candidate = vault.clone();
        let record = self.record_in_group(target.name, target.description, group.id, order);
        candidate
            .authenticators
            .append_items(std::iter::once(record));
        Ok(candidate)
    }

    pub fn record(&self, name: &str, description: &str) -> Authenticator {
        self.record_in_group(
            name,
            description,
            self.destination.group,
            self.destination.sort_order,
        )
    }

    fn record_in_group(
        &self,
        name: &str,
        description: &str,
        group: Uuid,
        order: u32,
    ) -> Authenticator {
        crate::AuthenticatorRecord {
            id: Uuid::new_v4(),
            group_id: group,
            sort_order: order,
            name: name.to_owned(),
            description: description.to_owned(),
            secret: self.secret.clone(),
            serial: self.serial.clone(),
            restore_code: self
                .restore_code
                .as_ref()
                .map(|code| code.as_str().to_owned()),
            digits: 8,
            algorithm: "SHA1".into(),
            period_secs: 30,
            created_at_ms: LegacyField,
            updated_at_ms: LegacyField,
            source: LegacyField,
            restore_code_source: LegacyField,
            recovery_history: LegacyField,
            legacy_metadata: LegacyField,
        }
        .into()
    }
}

/// A preview paired with the caller's initial labels for a direct import.
/// Editable forms keep only the preview and supply their current labels on confirmation.
pub struct PreparedImport<'a> {
    pub preview: ImportPreview,
    pub(crate) name: &'a str,
    pub(crate) description: &'a str,
}

impl PreparedImport<'_> {
    pub fn candidate(&self, vault: &VaultSnapshot) -> Result<VaultSnapshot, String> {
        self.preview.confirm(
            vault,
            RecoveryTarget {
                name: self.name,
                description: self.description,
                group: self.preview.destination.group,
            },
        )
    }

    pub fn record(&self) -> Authenticator {
        self.preview.record(self.name, self.description)
    }
}

pub fn prepare_recovery<'a>(
    vault: &VaultSnapshot,
    target: RecoveryTarget<'a, impl Copy + Into<ImportGroup>>,
    material: &RecoveryMaterial,
    secret: &Secret,
) -> Result<PreparedImport<'a>, String> {
    let destination = target.prepare(vault)?;
    let preview = destination.preview(vault, material.clone(), secret.clone())?;
    Ok(PreparedImport {
        preview,
        name: target.name,
        description: target.description,
    })
}

#[cfg(test)]
#[path = "recovery_tests.rs"]
mod tests;
