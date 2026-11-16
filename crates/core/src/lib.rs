//! Independent BNAuth domain contracts. No filesystem, network or GUI dependencies.

pub mod clock;
mod manual;
mod secret;

pub use manual::{
    ImportPreview, ManualMaterial, display_serial, normalize_serial, parse_manual, prepare_import,
};
pub use secret::{Secret, totp};
use serde::{Deserialize, Serialize};
use std::collections::{HashMap, HashSet};
use std::time::{SystemTime, UNIX_EPOCH};
pub use uuid::Uuid;

pub const MAX_GROUPS: usize = 20;
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
        if self.source != TIME_SOURCE
            || !(946_684_800_000..4_133_980_800_000).contains(&self.server_time_ms)
            || self.rtt_ms > 2_000
        {
            return Err("校時紀錄中的來源網址、時間或回應耗時不正確。".into());
        }
        Ok(())
    }
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ServiceTimeSync {
    pub battlenet: Option<TimeSyncRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    pub source_vault_id: Uuid,
    pub source_revision: u64,
    pub scope: String,
    pub imported_at_ms: i64,
    pub source_time_sync: Option<TimeSyncRecord>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryEvent {
    pub at_ms: i64,
    pub operation: String,
    pub outcome: String,
    pub confirmation: Option<String>,
}

#[derive(Clone, Debug, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LegacyMetadata {
    pub source: String,
    pub source_version: Option<String>,
    pub field: String,
    pub original_value: String,
    pub unit: Option<String>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Group {
    pub id: Uuid,
    pub name: String,
    pub sort_order: u32,
    pub is_ungrouped: bool,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Authenticator {
    pub id: Uuid,
    pub group_id: Uuid,
    pub sort_order: u32,
    pub name: String,
    pub description: String,
    pub secret: Option<Secret>,
    pub serial: Option<String>,
    pub restore_code: Option<String>,
    pub restore_code_source: Option<String>,
    pub digits: u8,
    pub algorithm: String,
    pub period_secs: u32,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub source: String,
    pub recovery_history: Vec<RecoveryEvent>,
    pub legacy_metadata: Vec<LegacyMetadata>,
}

#[derive(Clone, Debug, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VaultSnapshot {
    pub schema_version: u32,
    pub scope: String,
    pub vault_id: Uuid,
    pub revision: u64,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub groups: Vec<Group>,
    pub authenticators: Vec<Authenticator>,
    pub service_time_sync: ServiceTimeSync,
    pub provenance: Vec<Provenance>,
}

impl VaultSnapshot {
    pub fn new_empty(timestamp_ms: i64) -> Self {
        Self {
            schema_version: 1,
            scope: "full".into(),
            vault_id: Uuid::new_v4(),
            revision: 0,
            created_at_ms: timestamp_ms,
            updated_at_ms: timestamp_ms,
            groups: vec![Group {
                id: Uuid::new_v4(),
                name: "未分組".into(),
                sort_order: 0,
                is_ungrouped: true,
            }],
            authenticators: Vec::new(),
            service_time_sync: ServiceTimeSync::default(),
            provenance: Vec::new(),
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
        if self.schema_version != 1 || self.scope != "full" {
            return Err("資料格式版本不受支援，或檔案未包含完整資料。".into());
        }
        if self.vault_id.is_nil() {
            return Err("資料檔缺少有效識別碼。".into());
        }
        valid_times(self.created_at_ms, self.updated_at_ms)?;
        if self.groups.is_empty() || self.groups.len() > MAX_GROUPS {
            return Err("資料檔必須包含 1～20 個分組。".into());
        }
        if self.authenticators.len() > MAX_ENTRIES {
            return Err("驗證器數量超過 1000 筆上限。".into());
        }
        let mut all_ids = HashSet::new();
        all_ids.insert(self.vault_id);
        let mut names = HashSet::new();
        let mut group_ids = HashSet::new();
        let mut system_count = 0;
        for (index, group) in self.groups.iter().enumerate() {
            if group.id.is_nil() || !all_ids.insert(group.id) {
                return Err("分組識別碼為空或重複。".into());
            }
            group_ids.insert(group.id);
            valid_name(&group.name, MAX_GROUP_NAME_BYTES, "分組名稱")?;
            if !names.insert(group.name.trim().to_lowercase()) {
                return Err("分組名稱重複。".into());
            }
            if group.sort_order != index as u32 {
                return Err("分組的排序編號必須從 0 開始，且不能重複或跳號。".into());
            }
            if group.is_ungrouped {
                system_count += 1;
                if index != 0 || group.name != "未分組" {
                    return Err("「未分組」必須固定為第一個分組。".into());
                }
            }
        }
        if system_count != 1 {
            return Err("資料檔必須包含一個「未分組」，不能缺少或重複。".into());
        }
        let mut orders: HashMap<Uuid, Vec<u32>> = HashMap::new();
        let mut seeds = HashSet::new();
        for entry in &self.authenticators {
            if entry.id.is_nil() || !all_ids.insert(entry.id) {
                return Err("驗證器識別碼為空或重複。".into());
            }
            if !group_ids.contains(&entry.group_id) {
                return Err("驗證器所屬分組不存在。".into());
            }
            orders
                .entry(entry.group_id)
                .or_default()
                .push(entry.sort_order);
            valid_name(&entry.name, MAX_NAME_BYTES, "驗證器名稱")?;
            bounded(&entry.description, MAX_DESCRIPTION_BYTES, "備註")?;
            if entry.digits != 8 || entry.algorithm != "SHA1" || entry.period_secs != 30 {
                return Err(
                    "戰網驗證器的驗證碼設定必須為 8 位數、SHA1 計算方式，並每 30 秒更新一次。"
                        .into(),
                );
            }
            valid_times(entry.created_at_ms, entry.updated_at_ms)?;
            valid_name(&entry.source, 256, "來源")?;
            if let Some(secret) = &entry.secret
                && !seeds.insert(secret.bytes())
            {
                return Err("資料檔中有驗證器使用相同的長密鑰。".into());
            }
            if let Some(serial) = &entry.serial
                && normalize_serial(serial)?.as_deref() != Some(serial.as_str())
            {
                return Err("資料檔中的序列號必須使用大寫英文字母與數字，不含空格或連字號，例如 US123456789012。".into());
            }
            match (&entry.restore_code, &entry.restore_code_source) {
                (Some(code), Some(source)) => {
                    manual::validate_restore(code)?;
                    valid_name(source, 256, "還原碼來源")?;
                }
                (None, None) => {}
                _ => return Err("還原碼與還原碼來源必須一起填寫，或一起留空。".into()),
            }
            if entry.recovery_history.len() > 128 || entry.legacy_metadata.len() > 128 {
                return Err("歷史紀錄超過 128 筆上限。".into());
            }
            for item in &entry.recovery_history {
                if item.at_ms < 0 {
                    return Err("歷史時間不可為負值。".into());
                }
                valid_name(&item.operation, 128, "歷史操作")?;
                valid_name(&item.outcome, 128, "歷史結果")?;
                if let Some(value) = &item.confirmation {
                    bounded(value, 1024, "確認方式")?;
                }
            }
            for item in &entry.legacy_metadata {
                valid_name(&item.source, 256, "歷史來源")?;
                valid_name(&item.field, 256, "歷史欄位")?;
                bounded(&item.original_value, 64 * 1024, "歷史值")?;
                if let Some(value) = &item.source_version {
                    bounded(value, 256, "來源版本")?;
                }
                if let Some(value) = &item.unit {
                    bounded(value, 128, "歷史單位")?;
                }
            }
        }
        for values in orders.values_mut() {
            if values.len() > MAX_GROUP_ENTRIES {
                return Err("每個分組最多 100 筆驗證器。".into());
            }
            values.sort_unstable();
            if values
                .iter()
                .enumerate()
                .any(|(index, order)| *order != index as u32)
            {
                return Err("分組內的驗證器排序編號必須從 0 開始，且不能重複或跳號。".into());
            }
        }
        if let Some(sample) = &self.service_time_sync.battlenet {
            sample.validate()?;
        }
        if self.provenance.len() > 1000 {
            return Err("來源紀錄超過 1000 筆上限。".into());
        }
        for source in &self.provenance {
            if source.source_vault_id.is_nil()
                || source.imported_at_ms < 0
                || !matches!(source.scope.as_str(), "full" | "group")
            {
                return Err("匯入來源紀錄無效。".into());
            }
            if let Some(sample) = &source.source_time_sync {
                sample.validate()?;
            }
        }
        Ok(())
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

fn valid_times(created: i64, updated: i64) -> Result<(), String> {
    if created < 0 || updated < 0 {
        return Err("建立及更新時間不可為負值。".into());
    }
    // Wall-clock corrections may legitimately make a later edit's UTC timestamp smaller.
    Ok(())
}

#[cfg(test)]
mod tests;
