//! Preview an atomic group-file import into a new group.
use crate::{
    Authenticator, Group, MAX_ENTRIES, MAX_GROUP_NAME_BYTES, MAX_GROUPS, Uuid, ValidatedBackup,
    VaultSnapshot,
};
use std::collections::HashSet;

#[derive(Clone, Debug)]
pub struct GroupImportPreview {
    pub candidate: VaultSnapshot,
    pub base_vault_id: Uuid,
    pub base_revision: u64,
    pub group_id: Uuid,
    imported_start: usize,
}

impl GroupImportPreview {
    /// The new group's records in their original group order.
    pub fn imported_records(&self) -> &[Authenticator] {
        &self.candidate.authenticators[self.imported_start..]
    }
}

/// Check the capacity to append a validated group export before copying the vault.
/// The group export's own size has already been checked by its reader.
pub fn check_group_import_capacity(
    target: &VaultSnapshot,
    source: &VaultSnapshot,
) -> Result<(), String> {
    if target.groups.len() >= MAX_GROUPS {
        return Err(format!(
            "資料庫最多可有 {MAX_GROUPS} 個分組，無法建立新分組。"
        ));
    }
    let current_entries = target.authenticators.len();
    if current_entries >= MAX_ENTRIES {
        return Err(format!("已達 {MAX_ENTRIES} 筆驗證器上限，無法匯入分組。"));
    }
    if source.authenticators.is_empty() {
        return Err("分組為空，無可匯入的驗證器".into());
    }
    if current_entries + source.authenticators.len() > MAX_ENTRIES {
        return Err(format!("匯入後的驗證器數量將超過 {MAX_ENTRIES} 筆上限。"));
    }
    Ok(())
}

pub fn prepare_group_import(
    target: &VaultSnapshot,
    source: &VaultSnapshot,
) -> Result<GroupImportPreview, String> {
    let target_index = target.checked_index(false)?;
    let source = ValidatedBackup::new(source.clone(), true)?;
    check_group_import_capacity(target, source.snapshot())?;
    Ok(append_group(target.clone(), target_index.ids, source))
}

/// Prepare an import from an acknowledged full vault and a validated group export.
/// The caller must validate both snapshots and check capacity before copying the vault.
pub fn prepare_group_import_from_validated(
    candidate: VaultSnapshot,
    source: ValidatedBackup,
) -> GroupImportPreview {
    let occupied = identities(&candidate);
    append_group(candidate, occupied, source)
}

fn append_group(
    mut candidate: VaultSnapshot,
    mut occupied: HashSet<Uuid>,
    source: ValidatedBackup,
) -> GroupImportPreview {
    let ValidatedBackup {
        snapshot: mut source,
        index,
    } = source;
    let reserved = index.ids;
    let mut group = source.groups.pop().expect("validated group export");
    group.name = available_group_name(&candidate.groups, group.name);
    if !occupied.insert(group.id) {
        group.id = fresh_id(&mut occupied, &reserved);
    }
    group.sort_order = candidate.groups.len() as u32;
    group.is_ungrouped = false;
    let group_id = group.id;
    candidate.groups.append_items(std::iter::once(group));

    source
        .authenticators
        .sort_unstable_by_key(|record| record.sort_order);
    for record in source.authenticators.iter_mut() {
        if !occupied.insert(record.id) {
            record.id = fresh_id(&mut occupied, &reserved);
        }
        record.group_id = group_id;
    }
    let imported_start = candidate.authenticators.len();
    candidate
        .authenticators
        .append_items(source.authenticators.drain(..));

    GroupImportPreview {
        base_vault_id: candidate.vault_id,
        base_revision: candidate.revision,
        candidate,
        group_id,
        imported_start,
    }
}

pub(super) fn available_group_name(groups: &[Group], source_name: String) -> String {
    let existing_names: HashSet<_> = groups
        .iter()
        .map(|group| group.name.to_lowercase())
        .collect();
    if !existing_names.contains(&source_name.to_lowercase()) {
        return source_name;
    }
    for number in 2.. {
        let suffix = format!("({number})");
        let end = source_name.floor_char_boundary(MAX_GROUP_NAME_BYTES - suffix.len());
        let name = format!("{}{suffix}", &source_name[..end]);
        if !existing_names.contains(&name.to_lowercase()) {
            return name;
        }
    }
    unreachable!("a finite set of names cannot use every suffix")
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

#[cfg(test)]
#[path = "group_import_tests.rs"]
mod tests;
