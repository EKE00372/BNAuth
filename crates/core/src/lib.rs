//! Independent BNAuth domain contracts. No filesystem, network or GUI dependencies.

pub mod clock;
mod full_restore;
mod group_import;
mod manual;
mod native_input;
mod operation_error;
mod recovery;
mod secret;

pub use full_restore::{
    FullRestorePreview, FullRestoreTarget, prepare_full_restore,
    prepare_full_restore_from_validated,
};
pub use group_import::{
    GroupImportPreview, check_group_import_capacity, prepare_group_import,
    prepare_group_import_from_validated,
};
pub use manual::{
    ImportGroup, ManualMaterial, SubmittedSerial, display_serial, format_serial_input,
    normalize_serial, parse_manual, prepare_import, submit_restore_input, submit_serial_input,
};
pub use operation_error::{ErrorCode, OperationError};
pub use recovery::{
    ImportPreview, PreparedImport, RecoveryDestination, RecoveryMaterial, RecoveryTarget,
    prepare_recovery,
};
pub use secret::{Secret, totp};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};
pub use uuid::Uuid;

pub const MAX_GROUPS: usize = 20;
pub const DEFAULT_GROUP_NAME: &str = "預設";
const LEGACY_DEFAULT_GROUP_NAME: &str = "未分組";
pub const READ_ONLY_UPDATE_NOTICE: &str = "設定檔處於唯讀狀態，僅能查看和匯出。";
pub const WRITE_FAILED_NOTICE: &str = ErrorCode::WriteFailed.description();
pub const MAX_ENTRIES: usize = 1_000;
pub const MAX_GROUP_ENTRIES: usize = 100;
pub const MAX_NAME_BYTES: usize = 1_024;
pub const MAX_GROUP_NAME_BYTES: usize = 256;
pub const MAX_DESCRIPTION_BYTES: usize = 16 * 1_024;
pub const TIME_SOURCE: &str = "https://authenticator-rest-api.bnet-identity.blizzard.net/v1/time";

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct TimeSyncRecord {
    pub source: String,
    pub server_time_ms: i64,
    pub local_wall_ms_at_sample: i64,
    pub offset_ms: i64,
    pub rtt_ms: u64,
}

impl TimeSyncRecord {
    pub fn validate(&self) -> Result<(), String> {
        if self.source != TIME_SOURCE || self.rtt_ms > clock::MAX_RTT_MS {
            return Err("校時紀錄中的來源網址或回應耗時不正確。".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTimeSync {
    pub battlenet: Option<TimeSyncRecord>,
}

impl ServiceTimeSync {
    pub fn validate(&self) -> Result<(), String> {
        if let Some(sample) = &self.battlenet {
            sample.validate()?;
        }
        Ok(())
    }
}

// Earlier v1 files carry these extra keys. Consume their values without allocating
// a history or allowing unknown fields elsewhere in the file.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
struct LegacyField;

impl<'de> Deserialize<'de> for LegacyField {
    fn deserialize<D: serde::Deserializer<'de>>(value: D) -> Result<Self, D::Error> {
        serde::de::IgnoredAny::deserialize(value)?;
        Ok(Self)
    }
}

pub type Group = GroupFields<String>;

/// The same native row can own text when loaded or borrow it for save verification.
#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GroupFields<Text> {
    pub id: Uuid,
    pub name: Text,
    pub sort_order: u32,
    pub is_ungrouped: bool,
}

/// Share unchanged tables and authenticator records between the saved vault and drafts.
/// The file contains the underlying value.
#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(transparent)]
pub struct Shared<T>(Arc<T>);

impl<T> From<T> for Shared<T> {
    fn from(value: T) -> Self {
        Self(Arc::new(value))
    }
}

impl<T> Deref for Shared<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.0
    }
}

impl<T: Clone> DerefMut for Shared<T> {
    fn deref_mut(&mut self) -> &mut T {
        Arc::make_mut(&mut self.0)
    }
}

impl<T: Clone> Shared<Vec<T>> {
    /// When a saved table is shared with a draft, copy into its final capacity.
    pub fn append_items(&mut self, items: impl ExactSizeIterator<Item = T>) {
        if let Some(table) = Arc::get_mut(&mut self.0) {
            table.extend(items);
        } else {
            let mut table = Vec::with_capacity(self.len() + items.len());
            table.extend_from_slice(self);
            table.extend(items);
            *self = table.into();
        }
    }
}

pub type Authenticator = Shared<AuthenticatorRecord>;
pub type AuthenticatorRecord = AuthenticatorFields<String>;

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AuthenticatorFields<Text> {
    pub id: Uuid,
    pub group_id: Uuid,
    pub sort_order: u32,
    pub name: Text,
    pub description: Text,
    pub secret: Secret,
    pub serial: Option<Text>,
    pub restore_code: Option<Text>,
    pub digits: u8,
    pub algorithm: Text,
    pub period_secs: u32,
    #[serde(
        default,
        skip_serializing,
        deserialize_with = "native_input::row_legacy"
    )]
    created_at_ms: LegacyField,
    #[serde(
        default,
        skip_serializing,
        deserialize_with = "native_input::row_legacy"
    )]
    updated_at_ms: LegacyField,
    #[serde(
        default,
        skip_serializing,
        deserialize_with = "native_input::row_legacy"
    )]
    source: LegacyField,
    #[serde(
        default,
        skip_serializing,
        deserialize_with = "native_input::row_legacy"
    )]
    restore_code_source: LegacyField,
    #[serde(default, skip_serializing)]
    recovery_history: LegacyField,
    #[serde(default, skip_serializing)]
    legacy_metadata: LegacyField,
}

impl AuthenticatorRecord {
    /// The editor owns both text fields. Move them into the pending record.
    pub fn with_text(&self, name: String, description: String) -> Authenticator {
        Self {
            id: self.id,
            group_id: self.group_id,
            sort_order: self.sort_order,
            name,
            description,
            secret: self.secret.clone(),
            serial: self.serial.clone(),
            restore_code: self.restore_code.clone(),
            digits: self.digits,
            algorithm: self.algorithm.clone(),
            period_secs: self.period_secs,
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

#[derive(Clone, Debug, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultSnapshot {
    pub schema_version: u32,
    pub scope: String,
    pub vault_id: Uuid,
    pub revision: u64,
    #[serde(deserialize_with = "native_input::rows")]
    pub groups: Shared<Vec<Group>>,
    #[serde(deserialize_with = "native_input::rows")]
    pub authenticators: Shared<Vec<Authenticator>>,
    pub service_time_sync: ServiceTimeSync,
    #[serde(default, deserialize_with = "native_input::vault_legacy")]
    created_at_ms: LegacyField,
    #[serde(default, deserialize_with = "native_input::vault_legacy")]
    updated_at_ms: LegacyField,
    #[serde(default)]
    provenance: LegacyField,
}

/// The fields written to a native file. A save can advance the revision and a
/// backup can select the live clock while both borrow the original records.
#[derive(Serialize, PartialEq, Eq)]
pub struct VaultFile<'a> {
    schema_version: u32,
    scope: &'a str,
    vault_id: Uuid,
    pub revision: u64,
    groups: &'a [Group],
    authenticators: &'a [Authenticator],
    pub service_time_sync: &'a ServiceTimeSync,
}

impl Serialize for VaultSnapshot {
    fn serialize<S: serde::Serializer>(&self, serializer: S) -> Result<S::Ok, S::Error> {
        self.file_view().serialize(serializer)
    }
}

/// Keep the index built while validating an external file for its import preview.
#[derive(Debug)]
pub struct ValidatedBackup {
    snapshot: VaultSnapshot,
    index: VaultIndex,
}

#[derive(Debug)]
struct VaultIndex {
    ids: HashSet<Uuid>,
    group_orders: HashMap<Uuid, Vec<u32>>,
}

impl ValidatedBackup {
    pub fn new(mut snapshot: VaultSnapshot, group: bool) -> Result<Self, String> {
        let actual_group = match snapshot.scope.as_str() {
            "full" => false,
            "group" => true,
            _ => return Err(ErrorCode::CorruptFile.notice().to_string()),
        };
        let legacy_name = snapshot.groups.first().is_some_and(|group| {
            group.is_ungrouped && group.name == LEGACY_DEFAULT_GROUP_NAME
        });
        let default_name = if legacy_name {
            LEGACY_DEFAULT_GROUP_NAME
        } else {
            DEFAULT_GROUP_NAME
        };
        let index = snapshot
            .checked_index_with_default_name(actual_group, default_name)
            .map_err(|_| ErrorCode::CorruptFile.notice().to_string())?;
        if actual_group != group {
            let code = if actual_group {
                ErrorCode::GroupBackupSelected
            } else {
                ErrorCode::FullBackupSelected
            };
            return Err(code.notice().to_string());
        }
        if legacy_name {
            if let Some(conflict) = snapshot
                .groups
                .iter()
                .position(|group| group.name == DEFAULT_GROUP_NAME)
            {
                snapshot.groups[conflict].name = group_import::available_group_name(
                    &snapshot.groups,
                    DEFAULT_GROUP_NAME.to_owned(),
                );
            }
            snapshot.groups[0].name = DEFAULT_GROUP_NAME.to_owned();
        }
        Ok(Self { snapshot, index })
    }

    pub fn snapshot(&self) -> &VaultSnapshot {
        &self.snapshot
    }

    pub fn into_snapshot(self) -> VaultSnapshot {
        self.snapshot
    }
}

impl VaultSnapshot {
    pub fn file_view(&self) -> VaultFile<'_> {
        VaultFile {
            schema_version: self.schema_version,
            scope: &self.scope,
            vault_id: self.vault_id,
            revision: self.revision,
            groups: &self.groups,
            authenticators: &self.authenticators,
            service_time_sync: &self.service_time_sync,
        }
    }

    pub fn new_empty() -> Self {
        Self {
            schema_version: 1,
            scope: "full".into(),
            vault_id: Uuid::new_v4(),
            revision: 0,
            groups: vec![Group {
                id: Uuid::new_v4(),
                name: DEFAULT_GROUP_NAME.to_owned(),
                sort_order: 0,
                is_ungrouped: true,
            }]
            .into(),
            authenticators: Shared::default(),
            service_time_sync: ServiceTimeSync::default(),
            created_at_ms: LegacyField,
            updated_at_ms: LegacyField,
            provenance: LegacyField,
        }
    }

    /// Retain the header and shared clock when rebuilding the editable rows.
    pub fn metadata_only(&self) -> Self {
        self.with_contents(
            Shared::default(),
            Shared::default(),
            self.service_time_sync.clone(),
        )
    }

    /// Prepare a clock-only save while retaining both tables from the acknowledged vault.
    pub fn with_battlenet_clock(&self, record: Option<TimeSyncRecord>) -> Self {
        self.with_contents(
            self.groups.clone(),
            self.authenticators.clone(),
            ServiceTimeSync { battlenet: record },
        )
    }

    fn with_contents(
        &self,
        groups: Shared<Vec<Group>>,
        authenticators: Shared<Vec<Authenticator>>,
        service_time_sync: ServiceTimeSync,
    ) -> Self {
        Self {
            schema_version: self.schema_version,
            scope: self.scope.clone(),
            vault_id: self.vault_id,
            revision: self.revision,
            groups,
            authenticators,
            service_time_sync,
            created_at_ms: LegacyField,
            updated_at_ms: LegacyField,
            provenance: LegacyField,
        }
    }

    pub fn ungrouped_id(&self) -> Option<Uuid> {
        self.groups
            .iter()
            .find(|group| group.is_ungrouped)
            .map(|group| group.id)
    }

    /// Validate a full working snapshot before any persistence or display.
    pub fn validate(&self) -> Result<(), String> {
        self.checked_index(false).map(drop)
    }

    /// Validate metadata after a checked edit to an existing full vault.
    /// The repository independently matches its ID and revision to the primary.
    pub fn validate_edit_metadata(&self) -> Result<(), String> {
        self.validate_full_header()?;
        self.service_time_sync.validate()
    }

    fn validate_full_header(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.scope != "full" {
            return Err(ErrorCode::CorruptFile.notice().to_string());
        }
        Ok(())
    }

    /// A group export is deliberately separate from a complete working file.
    /// It may contain a custom group without the system ungrouped group.
    pub fn validate_group_export(&self) -> Result<(), String> {
        self.checked_index(true).map(drop)
    }

    fn checked_index(&self, group: bool) -> Result<VaultIndex, String> {
        self.checked_index_with_default_name(group, DEFAULT_GROUP_NAME)
    }

    fn checked_index_with_default_name(
        &self,
        group: bool,
        default_name: &str,
    ) -> Result<VaultIndex, String> {
        if group {
            self.validate_group_export_header()?;
        } else {
            self.validate_full_header()?;
        }
        self.validate_contents(!group, default_name)
    }

    fn validate_group_export_header(&self) -> Result<(), String> {
        if self.schema_version != 1 || self.scope != "group" || self.groups.len() != 1 {
            return Err(ErrorCode::CorruptFile.notice().to_string());
        }
        Ok(())
    }

    /// Extract every record in the selected group from a committed full vault.
    /// Source identity, metadata and per-record fields are retained unchanged.
    pub fn export_group(&self, group_id: Uuid) -> Result<Self, String> {
        self.validate()?;
        self.export_group_from_validated(group_id)
    }

    /// Check the new group header. The full vault already validated the records.
    pub fn export_group_from_validated(&self, group_id: Uuid) -> Result<Self, String> {
        let group = self
            .groups
            .iter()
            .find(|group| group.id == group_id)
            .ok_or("找不到要匯出的分組。")?
            .clone();
        let mut authenticators: Vec<_> = self
            .authenticators
            .iter()
            .filter(|record| record.group_id == group_id)
            .cloned()
            .collect();
        authenticators.sort_by_key(|record| record.sort_order);
        self.export_ordered_group(group, authenticators)
    }

    /// The UI already indexes the committed file slots in group display order.
    pub fn export_ordered_group(
        &self,
        mut group: Group,
        authenticators: Vec<Authenticator>,
    ) -> Result<Self, String> {
        group.sort_order = 0;
        let exported = Self {
            schema_version: self.schema_version,
            scope: "group".into(),
            vault_id: self.vault_id,
            revision: self.revision,
            groups: vec![group].into(),
            authenticators: authenticators.into(),
            service_time_sync: self.service_time_sync.clone(),
            created_at_ms: LegacyField,
            updated_at_ms: LegacyField,
            provenance: LegacyField,
        };
        exported.validate_group_export_header()?;
        Ok(exported)
    }

    fn validate_contents(
        &self,
        require_ungrouped: bool,
        default_name: &str,
    ) -> Result<VaultIndex, String> {
        if self.vault_id.is_nil() {
            return Err("資料檔缺少有效識別碼。".into());
        }
        if self.groups.is_empty() || self.groups.len() > MAX_GROUPS {
            return Err("資料檔必須包含 1～20 個分組。".into());
        }
        if self.authenticators.len() > MAX_ENTRIES {
            return Err("驗證器數量超過 1000 筆上限。".into());
        }
        let mut all_ids = HashSet::new();
        all_ids.insert(self.vault_id);
        let mut names = HashSet::new();
        let mut group_orders = HashMap::with_capacity(self.groups.len());
        let mut system_count = 0;
        for (index, group) in self.groups.iter().enumerate() {
            if group.id.is_nil() || !all_ids.insert(group.id) {
                return Err("分組識別碼為空或重複。".into());
            }
            group_orders.insert(group.id, Vec::new());
            valid_name(&group.name, MAX_GROUP_NAME_BYTES, "分組名稱")?;
            if !names.insert(group.name.to_lowercase()) {
                return Err("分組名稱重複。".into());
            }
            if group.sort_order != index as u32 {
                return Err("分組的排序編號必須從 0 開始，且不能重複或跳號。".into());
            }
            if group.is_ungrouped {
                system_count += 1;
                if index != 0 || group.name != default_name {
                    return Err(format!("「{default_name}」必須固定為第一個分組。"));
                }
            }
        }
        if require_ungrouped && system_count != 1 {
            return Err(format!("資料檔必須包含一個「{default_name}」，不能缺少或重複。"));
        }
        for entry in self.authenticators.iter() {
            if entry.id.is_nil() || !all_ids.insert(entry.id) {
                return Err("驗證器識別碼為空或重複。".into());
            }
            let Some(orders) = group_orders.get_mut(&entry.group_id) else {
                return Err("驗證器所屬分組不存在。".into());
            };
            orders.push(entry.sort_order);
            valid_name(&entry.name, MAX_NAME_BYTES, "驗證器名稱")?;
            bounded(&entry.description, MAX_DESCRIPTION_BYTES, "備註")?;
            if entry.digits != 8 || entry.algorithm != "SHA1" || entry.period_secs != 30 {
                return Err(
                    "戰網驗證器的驗證碼設定必須為 8 位數、SHA1 計算方式，並每 30 秒更新一次。"
                        .into(),
                );
            }
            if let Some(serial) = &entry.serial
                && normalize_serial(serial)?.as_deref() != Some(serial.as_str())
            {
                return Err("資料檔中的序列號必須使用大寫英文字母與數字，不含空格或連字號，例如 US123456789012。".into());
            }
            if let Some(code) = &entry.restore_code {
                manual::validate_restore(code)?;
            }
        }
        for values in group_orders.values() {
            if values.len() > MAX_GROUP_ENTRIES {
                return Err("每個分組最多 100 筆驗證器。".into());
            }
            let mut seen = [false; MAX_GROUP_ENTRIES];
            for &order in values {
                let position = order as usize;
                if position >= values.len() || seen[position] {
                    return Err("分組內的驗證器排序編號必須從 0 開始，且不能重複或跳號。".into());
                }
                seen[position] = true;
            }
        }
        self.service_time_sync.validate()?;
        Ok(VaultIndex {
            ids: all_ids,
            group_orders,
        })
    }
}

pub fn now_ms() -> i64 {
    match SystemTime::now().duration_since(UNIX_EPOCH) {
        Ok(duration) => i64::try_from(duration.as_millis()).unwrap_or(i64::MAX),
        Err(error) => -i64::try_from(error.duration().as_millis()).unwrap_or(i64::MAX),
    }
}

pub fn valid_name(value: &str, limit: usize, label: &str) -> Result<(), String> {
    if value.trim().is_empty() {
        return Err(format!("{label}不可為空白。"));
    }
    bounded(value, limit, label)
}

pub fn bounded(value: &str, limit: usize, label: &str) -> Result<(), String> {
    if value.len() > limit {
        return Err(format!(
            "{label}的文字大小為 {} 位元組，超過 {limit} 位元組上限。",
            value.len()
        ));
    }
    if value.contains('\0') {
        return Err(format!("{label}含有不支援的隱藏字元（U+0000）。"));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
