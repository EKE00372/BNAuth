//! Preview an atomic group-file merge without mutating either input.
use crate::{Authenticator, Provenance, Uuid, VaultSnapshot};
use std::collections::{HashMap, HashSet};

#[derive(Clone, Debug)]
pub struct GroupImportRow {
    pub name: String,
    pub action: String,
}

#[derive(Clone, Debug)]
pub struct GroupImportPreview {
    pub candidate: VaultSnapshot,
    pub base_vault_id: Uuid,
    pub base_revision: u64,
    pub group_id: Uuid,
    pub group_name: String,
    pub created_group: bool,
    pub added: usize,
    pub supplemented: usize,
    pub existing: usize,
    pub moved: usize,
    pub reordered: bool,
    pub serial_warnings: usize,
    pub rows: Vec<GroupImportRow>,
    pub changed: bool,
    pub new_secret: bool,
}

pub fn prepare_group_import(
    target: &VaultSnapshot,
    source: &VaultSnapshot,
    timestamp_ms: i64,
) -> Result<GroupImportPreview, String> {
    target.validate()?;
    source.validate_group_export()?;
    if timestamp_ms < 0 {
        return Err("系統時間早於Unix epoch，請先校正電腦時間。".into());
    }
    let source_group = &source.groups[0];
    let mut candidate = target.clone();
    let mut occupied = identities(target);
    let reserved = identities(source);
    let matching_group = candidate.groups.iter().position(|group| {
        if source_group.is_ungrouped {
            group.is_ungrouped
        } else {
            group.name.trim().to_lowercase() == source_group.name.trim().to_lowercase()
        }
    });
    let created_group = matching_group.is_none();
    let group_id = if let Some(index) = matching_group {
        let group = &candidate.groups[index];
        if group.name != source_group.name {
            return Err(format!(
                "來源分組「{}」與既有「{}」只有大小寫或空白差異，請先調整分組名稱再匯入。",
                source_group.name, group.name
            ));
        }
        group.id
    } else {
        let mut group = source_group.clone();
        if !occupied.insert(group.id) {
            group.id = fresh_id(&mut occupied, &reserved);
        }
        group.sort_order = candidate.groups.len() as u32;
        let id = group.id;
        candidate.groups.push(group);
        id
    };
    let mut preview = GroupImportPreview {
        candidate,
        base_vault_id: target.vault_id,
        base_revision: target.revision,
        group_id,
        group_name: source_group.name.clone(),
        created_group,
        added: 0,
        supplemented: 0,
        existing: 0,
        moved: 0,
        reordered: false,
        serial_warnings: 0,
        rows: Vec::new(),
        changed: false,
        new_secret: false,
    };
    let mut source_records: Vec<_> = source.authenticators.iter().collect();
    source_records.sort_by_key(|record| record.sort_order);
    let mut imported_ids = Vec::with_capacity(source_records.len());
    let mut seen = HashSet::new();
    for source in source_records {
        let by_seed = source.secret.as_ref().and_then(|secret| {
            preview
                .candidate
                .authenticators
                .iter()
                .position(|record| record.secret.as_ref() == Some(secret))
        });
        let by_id = preview.candidate.authenticators.iter().position(|record| {
            record.id == source.id
                && !matches!((&record.secret, &source.secret), (Some(left), Some(right)) if left != right)
        });
        let mut actions = Vec::new();
        let index = if let Some(index) = by_seed.or(by_id) {
            let original = &preview.candidate.authenticators[index];
            if !seen.insert(original.id) {
                return Err(format!(
                    "來源中多筆資料對應到同一個既有驗證器「{}」，請先核對來源。",
                    source.name
                ));
            }
            let mut merged = merge_record(original, source)?;
            preview.existing += 1;
            if &merged != original {
                preview.supplemented += 1;
                actions.push("補齊資料");
                merged.updated_at_ms = timestamp_ms;
            } else {
                actions.push("已存在");
            }
            preview.new_secret |= original.secret.is_none() && merged.secret.is_some();
            if merged.group_id != group_id {
                preview.moved += 1;
                actions.push("移入此分組");
            }
            merged.group_id = group_id;
            preview.candidate.authenticators[index] = merged;
            index
        } else {
            let mut record = source.clone();
            if !occupied.insert(record.id) {
                if record.secret.is_none() {
                    return Err(format!(
                        "「{}」沒有密鑰，且識別碼與既有資料衝突，無法安全判重；請先核對來源。",
                        record.name
                    ));
                }
                record.id = fresh_id(&mut occupied, &reserved);
            }
            record.group_id = group_id;
            seen.insert(record.id);
            preview.new_secret |= record.secret.is_some();
            preview.candidate.authenticators.push(record);
            preview.added += 1;
            actions.push("新增");
            preview.candidate.authenticators.len() - 1
        };
        let record = &preview.candidate.authenticators[index];
        if record.serial.as_ref().is_some_and(|serial| {
            preview.candidate.authenticators.iter().any(|other| {
                other.id != record.id
                    && other.serial.as_ref() == Some(serial)
                    && other.secret != record.secret
            })
        }) {
            preview.serial_warnings += 1;
        }
        imported_ids.push(record.id);
        preview.rows.push(GroupImportRow {
            name: source.name.clone(),
            action: actions.join("、"),
        });
    }
    // Keep unrelated records in their original relative order. The imported
    // block follows them in exactly the source file's order, including duplicates.
    let imported: HashSet<_> = imported_ids.iter().copied().collect();
    let mut unrelated: Vec<_> = preview
        .candidate
        .authenticators
        .iter()
        .filter(|record| record.group_id == group_id && !imported.contains(&record.id))
        .collect();
    unrelated.sort_by_key(|record| record.sort_order);
    let ordered: Vec<_> = unrelated
        .iter()
        .map(|record| record.id)
        .chain(imported_ids)
        .collect();
    let positions: HashMap<_, _> = ordered
        .into_iter()
        .enumerate()
        .map(|(index, id)| (id, index as u32))
        .collect();
    let mut other_orders: HashMap<Uuid, Vec<usize>> = HashMap::new();
    for (index, record) in preview.candidate.authenticators.iter_mut().enumerate() {
        if record.group_id == group_id {
            record.sort_order = positions[&record.id];
        } else {
            other_orders.entry(record.group_id).or_default().push(index);
        }
    }
    for indices in other_orders.values_mut() {
        indices.sort_by_key(|&index| preview.candidate.authenticators[index].sort_order);
        for (order, &index) in indices.iter().enumerate() {
            preview.candidate.authenticators[index].sort_order = order as u32;
        }
    }
    preview.reordered = preview.candidate.authenticators.iter().any(|record| {
        target.authenticators.iter().any(|old| {
            old.id == record.id
                && old.group_id == group_id
                && record.group_id == group_id
                && old.sort_order != record.sort_order
        })
    });
    // Preserve source history as metadata, never as this process's live clock.
    for provenance in &source.provenance {
        if !preview.candidate.provenance.contains(provenance) {
            preview.candidate.provenance.push(provenance.clone());
        }
    }
    preview.changed = &preview.candidate != target;
    if preview.changed {
        let provenance = Provenance {
            source_vault_id: source.vault_id,
            source_revision: source.revision,
            scope: "group".into(),
            imported_at_ms: timestamp_ms,
            source_time_sync: source.service_time_sync.battlenet.clone(),
        };
        // Importing the same group again without any data change is a no-op.
        preview.candidate.provenance.push(provenance);
        preview.candidate.updated_at_ms = timestamp_ms;
    }
    preview.candidate.validate()?;
    Ok(preview)
}

fn identities(snapshot: &VaultSnapshot) -> HashSet<Uuid> {
    std::iter::once(snapshot.vault_id)
        .chain(snapshot.groups.iter().map(|group| group.id))
        .chain(snapshot.authenticators.iter().map(|record| record.id))
        .collect()
}

fn fresh_id(occupied: &mut HashSet<Uuid>, reserved: &HashSet<Uuid>) -> Uuid {
    loop {
        let id = Uuid::new_v4();
        if !reserved.contains(&id) && occupied.insert(id) {
            return id;
        }
    }
}

fn merge_record(original: &Authenticator, source: &Authenticator) -> Result<Authenticator, String> {
    let conflict = |field| {
        format!(
            "「{}」的{field}與既有資料不同，未匯入；請先核對兩份資料。",
            source.name
        )
    };
    if original.name != source.name {
        return Err(conflict("標題"));
    }
    let mut merged = original.clone();
    if merged.secret.is_none() {
        merged.secret.clone_from(&source.secret);
    }
    if merged.description.trim().is_empty() {
        if !source.description.trim().is_empty() {
            merged.description.clone_from(&source.description);
        }
    } else if !source.description.trim().is_empty() && merged.description != source.description {
        return Err(conflict("備註"));
    }
    if let (Some(left), Some(right)) = (&merged.serial, &source.serial) {
        if left != right {
            return Err(conflict("序號"));
        }
    } else if merged.serial.is_none() {
        merged.serial.clone_from(&source.serial);
    }
    if let (Some(left), Some(right)) = (&merged.restore_code, &source.restore_code) {
        if left != right {
            return Err(conflict("還原碼"));
        }
    } else if merged.restore_code.is_none() {
        merged.restore_code.clone_from(&source.restore_code);
        merged
            .restore_code_source
            .clone_from(&source.restore_code_source);
    }
    for event in &source.recovery_history {
        if !merged.recovery_history.contains(event) {
            merged.recovery_history.push(event.clone());
        }
    }
    for metadata in &source.legacy_metadata {
        if !merged.legacy_metadata.contains(metadata) {
            merged.legacy_metadata.push(metadata.clone());
        }
    }
    Ok(merged)
}

#[cfg(test)]
#[path = "group_import_tests.rs"]
mod tests;
