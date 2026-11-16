//! View keys and editing projection; persisted identities and secrets live in the core snapshot.
use bnauth_core::{Authenticator, VaultSnapshot};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use uuid::Uuid;

pub const UNGROUPED_ID: u64 = 0;
pub const MAX_GROUPS: usize = 20;
pub const MAX_GROUP_ENTRIES: usize = 100;
#[cfg(test)]
pub const MAX_ENTRIES: usize = 1_000;

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Group {
    pub id: u64,
    pub name: String,
    pub persistent_id: Uuid,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Entry {
    pub id: u64,
    pub group_id: u64,
    pub name: String,
    pub description: String,
    pub record: Arc<Authenticator>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultModel {
    pub groups: Vec<Group>,
    pub entries: Vec<Entry>,
    next_group_id: u64,
    next_entry_id: u64,
    change_token: u64,
    // Only vault metadata is retained here. Rows and groups are rebuilt on save,
    // so the editing projection does not retain a second full set of records.
    base: Arc<VaultSnapshot>,
}

impl VaultModel {
    #[cfg(test)]
    pub fn test_fixture() -> Self {
        let mut model = Self::from_snapshot(&VaultSnapshot::new_empty(1_700_000_000_000));
        for name in ["主帳號", "遊戲帳號", "備用", "待整理"] {
            model.add_group(name).unwrap();
        }
        let names = [
            "星港#2037",
            "霜溪#4182",
            "旅行者",
            "週末小隊",
            "月影#7604",
            "這是一個很長的人工驗證器名稱・窄視窗排列測試",
            "繁體與简体驗證器",
            "晴空#9026",
        ];
        for (group_id, count) in [(0, 8), (1, 12), (2, 6), (3, 3)] {
            for row in 0..count {
                let description = match row % 4 {
                    0 => format!("fixture-{}-{}@example.invalid", group_id + 1, row + 1),
                    1 => String::new(),
                    2 => "人工測試驗證器".to_owned(),
                    _ => "可自行修改的說明文字".to_owned(),
                };
                model
                    .add_test_entry(names[row % names.len()], &description, group_id)
                    .unwrap();
            }
        }
        model.snapshot().validate().unwrap();
        model
    }

    /// Changes after mutations and snapshot replacement; reading it is constant time.
    pub fn change_token(&self) -> u64 {
        self.change_token
    }

    fn mark_changed(&mut self) {
        self.change_token = self.change_token.wrapping_add(1);
    }

    /// Returns IDs in TAB order, then in the original row order within each TAB.
    /// Only names participate in matching; description and code are excluded.
    pub fn filtered(&self, current_group: u64, query: &str, all_groups: bool) -> Vec<u64> {
        self.filtered_indices(current_group, query, all_groups)
            .into_iter()
            .map(|index| self.entries[index].id)
            .collect()
    }

    /// Indices stay valid until change_token changes. Rendering can index visible
    /// rows directly instead of searching the entire model again for every ID.
    pub fn filtered_indices(
        &self,
        current_group: u64,
        query: &str,
        all_groups: bool,
    ) -> Vec<usize> {
        let query = query.trim().to_lowercase();
        let group_indices: HashMap<u64, usize> = self
            .groups
            .iter()
            .enumerate()
            .filter(|(_, group)| all_groups || group.id == current_group)
            .map(|(index, group)| (group.id, index))
            .collect();
        let mut matching = vec![Vec::new(); self.groups.len()];
        for (index, entry) in self.entries.iter().enumerate() {
            let Some(&group_index) = group_indices.get(&entry.group_id) else {
                continue;
            };
            if query.is_empty() || entry.name.to_lowercase().contains(&query) {
                matching[group_index].push(index);
            }
        }
        matching.into_iter().flatten().collect()
    }

    pub fn entry(&self, id: u64) -> Option<&Entry> {
        self.entries.iter().find(|entry| entry.id == id)
    }

    pub fn group_name(&self, id: u64) -> Option<&str> {
        self.groups
            .iter()
            .find(|group| group.id == id)
            .map(|group| group.name.as_str())
    }

    pub fn group_count(&self, group: u64) -> usize {
        self.entries
            .iter()
            .filter(|entry| entry.group_id == group)
            .count()
    }

    /// Current and post-move counts by group, computed once for the whole menu.
    pub fn move_target_counts(&self, ids: &[u64]) -> HashMap<u64, (usize, usize)> {
        let selected: HashSet<_> = ids.iter().copied().collect();
        let mut counts: HashMap<_, _> = self.groups.iter().map(|g| (g.id, (0, 0))).collect();
        let mut selected_count = 0;
        for entry in &self.entries {
            let (count, selected_here) =
                counts.get_mut(&entry.group_id).expect("entry has a group");
            *count += 1;
            if selected.contains(&entry.id) {
                *selected_here += 1;
                selected_count += 1;
            }
        }
        for (count, after_move) in counts.values_mut() {
            *after_move = *count + selected_count - *after_move;
        }
        counts
    }

    /// All validation happens before mutation. IDs already at the target stay put.
    pub fn move_entries(&mut self, ids: &[u64], target: u64) -> Result<usize, String> {
        if self.group_name(target).is_none() {
            return Err("找不到目標分組。".to_owned());
        }
        let selected: HashSet<u64> = ids.iter().copied().collect();
        if selected.iter().any(|&id| self.entry(id).is_none()) {
            return Err("選取的驗證器已不存在，請重新選取。".to_owned());
        }
        let mut moving = Vec::new();
        for group in &self.groups {
            for entry in &self.entries {
                if entry.group_id == group.id
                    && entry.group_id != target
                    && selected.contains(&entry.id)
                {
                    moving.push(entry.clone());
                }
            }
        }
        if self.group_count(target) + moving.len() > MAX_GROUP_ENTRIES {
            return Err(format!(
                "目標分組最多容納{MAX_GROUP_ENTRIES}筆，無法整批移動。"
            ));
        }
        let moved_count = moving.len();
        if moved_count == 0 {
            return Ok(0);
        }
        let moving_ids: HashSet<u64> = moving.iter().map(|entry| entry.id).collect();
        self.entries.retain(|entry| !moving_ids.contains(&entry.id));
        for entry in &mut moving {
            entry.group_id = target;
        }
        self.entries.extend(moving);
        self.mark_changed();
        Ok(moved_count)
    }

    /// The UI owns selection and confirmation; deleting duplicate IDs counts once.
    pub fn delete_entries(&mut self, ids: &[u64]) -> usize {
        let selected: HashSet<u64> = ids.iter().copied().collect();
        let previous_len = self.entries.len();
        self.entries.retain(|entry| !selected.contains(&entry.id));
        let deleted = previous_len - self.entries.len();
        if deleted != 0 {
            self.mark_changed();
        }
        deleted
    }

    pub fn add_group(&mut self, name: &str) -> Result<u64, String> {
        let name = self.valid_group_name(name, None)?;
        if self.groups.len() >= MAX_GROUPS {
            return Err(format!("最多建立{MAX_GROUPS}個分組，包含未分組。"));
        }
        let id = self.next_group_id;
        self.groups.push(Group {
            id,
            name,
            persistent_id: Uuid::new_v4(),
        });
        self.next_group_id += 1;
        self.mark_changed();
        Ok(id)
    }

    pub fn rename_group(&mut self, id: u64, name: &str) -> Result<(), String> {
        if id == UNGROUPED_ID {
            return Err("未分組不可改名。".to_owned());
        }
        let index = self
            .groups
            .iter()
            .position(|group| group.id == id)
            .ok_or("找不到分組。")?;
        // A valid, manually edited data file may retain surrounding whitespace.
        if self.groups[index].name == name {
            return Ok(());
        }
        let name = self.valid_group_name(name, Some(id))?;
        let group = &mut self.groups[index];
        if group.name != name {
            group.name = name;
            self.mark_changed();
        }
        Ok(())
    }

    /// Deletes a custom TAB only after all its rows fit in the system TAB.
    pub fn delete_group(&mut self, id: u64) -> Result<usize, String> {
        if id == UNGROUPED_ID {
            return Err("未分組不可刪除。".to_owned());
        }
        if self.group_name(id).is_none() {
            return Err("找不到分組。".to_owned());
        }
        let ids = self.filtered(id, "", false);
        let moved = self.move_entries(&ids, UNGROUPED_ID)?;
        self.groups.retain(|group| group.id != id);
        self.mark_changed();
        Ok(moved)
    }

    pub fn update_entry(&mut self, id: u64, name: &str, description: &str) -> Result<(), String> {
        let entry = self
            .entries
            .iter_mut()
            .find(|entry| entry.id == id)
            .ok_or("找不到驗證器。")?;
        // Imported names may contain surrounding whitespace. Editing notes
        // must not normalize a name that was left untouched.
        if entry.name == name && entry.description == description {
            return Ok(());
        }
        let edited_name = if entry.name == name {
            None
        } else {
            Some(valid_entry_name(name)?)
        };
        if description.len() > 16 * 1024 {
            return Err("備註最多16KiB。".into());
        }
        if edited_name.as_ref().is_some_and(|name| entry.name != *name)
            || entry.description != description
        {
            if let Some(name) = edited_name {
                entry.name = name;
            }
            entry.description = description.to_owned();
            self.mark_changed();
        }
        Ok(())
    }

    /// Builds a valid row from a unique public test seed; no fixture state exists in release builds.
    #[cfg(test)]
    pub fn add_test_entry(
        &mut self,
        name: &str,
        description: &str,
        group: u64,
    ) -> Result<u64, String> {
        let name = valid_entry_name(name)?;
        if self.group_name(group).is_none() {
            return Err("找不到目標分組。".to_owned());
        }
        if self.entries.len() >= MAX_ENTRIES {
            return Err(format!("最多保存{MAX_ENTRIES}筆驗證器。"));
        }
        if self.group_count(group) >= MAX_GROUP_ENTRIES {
            return Err(format!("每個分組最多容納{MAX_GROUP_ENTRIES}筆。"));
        }
        let id = self.next_entry_id;
        let next_id = id.checked_add(1).ok_or("驗證器識別碼已用盡。")?;
        let mut seed = [0; 20];
        seed[..8].copy_from_slice(&id.to_le_bytes());
        let empty = VaultSnapshot::new_empty(1_700_000_000_000);
        let material =
            bnauth_core::parse_manual(&bnauth_core::Secret::from_bytes(seed).hex(), "", "")?;
        let mut record = bnauth_core::prepare_import(
            &empty,
            &name,
            description,
            empty.groups[0].id,
            material,
            empty.created_at_ms,
        )?
        .candidate
        .authenticators
        .remove(0);
        record.group_id = self.persistent_group(group).unwrap();
        record.sort_order = self.group_count(group) as u32;
        self.entries.push(Entry {
            id,
            group_id: group,
            name,
            description: description.to_owned(),
            record: Arc::new(record),
        });
        self.next_entry_id = next_id;
        self.mark_changed();
        Ok(id)
    }

    /// Moves a row before another row in its own TAB; None appends to that TAB.
    pub fn reorder_entry(&mut self, id: u64, before: Option<u64>) -> Result<(), String> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .ok_or("找不到驗證器。")?;
        let group = self.entries[index].group_id;
        if let Some(before_id) = before {
            let before_entry = self.entry(before_id).ok_or("找不到目標驗證器。")?;
            if before_entry.group_id != group {
                return Err("組內排序只能使用同一分組的驗證器。".to_owned());
            }
            if before_id == id {
                return Ok(());
            }
        }
        let next_in_group = self.entries[index + 1..]
            .iter()
            .find(|entry| entry.group_id == group)
            .map(|entry| entry.id);
        if next_in_group == before {
            return Ok(());
        }
        let entry = self.entries.remove(index);
        let insert_at = match before {
            Some(before_id) => self
                .entries
                .iter()
                .position(|other| other.id == before_id)
                .expect("target was validated before removing a different row"),
            None => self
                .entries
                .iter()
                .rposition(|other| other.group_id == group)
                .map_or(self.entries.len(), |index| index + 1),
        };
        self.entries.insert(insert_at, entry);
        self.mark_changed();
        Ok(())
    }

    /// Custom TABs can move by a signed offset; the system TAB remains first.
    pub fn move_group(&mut self, id: u64, delta: isize) -> Result<(), String> {
        if id == UNGROUPED_ID {
            return Err("未分組固定在第一個分頁。".to_owned());
        }
        let index = self
            .groups
            .iter()
            .position(|group| group.id == id)
            .ok_or("找不到分組。")?;
        let target = index
            .checked_add_signed(delta)
            .filter(|&target| target > 0 && target < self.groups.len())
            .ok_or("分組已到可移動範圍的邊界。")?;
        if target != index {
            let group = self.groups.remove(index);
            self.groups.insert(target, group);
            self.mark_changed();
        }
        Ok(())
    }

    fn valid_group_name(&self, name: &str, except: Option<u64>) -> Result<String, String> {
        let name = name.trim();
        if name.len() > 256 {
            return Err("分組名稱最多256個UTF-8位元組。".into());
        }
        if name.is_empty() {
            return Err("請輸入分組名稱。".to_owned());
        }
        let key = name.to_lowercase();
        if self
            .groups
            .iter()
            .any(|group| Some(group.id) != except && group.name.trim().to_lowercase() == key)
        {
            return Err("已有同名分組，請使用其他名稱。".to_owned());
        }
        Ok(name.to_owned())
    }
}

fn valid_entry_name(name: &str) -> Result<String, String> {
    let name = name.trim();
    if name.len() > 1024 {
        return Err("名稱最多1KiB。".into());
    }
    if name.is_empty() {
        Err("請輸入名稱。".to_owned())
    } else {
        Ok(name.to_owned())
    }
}

impl VaultModel {
    pub fn from_snapshot(snapshot: &VaultSnapshot) -> Self {
        let mut model = Self {
            groups: Vec::new(),
            entries: Vec::new(),
            next_group_id: 1,
            next_entry_id: 1001,
            change_token: 0,
            base: Arc::new(snapshot_metadata(snapshot)),
        };
        model.replace_rows(snapshot);
        model
    }
    pub fn apply_snapshot(&mut self, snapshot: &VaultSnapshot) {
        self.base = Arc::new(snapshot_metadata(snapshot));
        self.replace_rows(snapshot);
    }

    fn replace_rows(&mut self, snapshot: &VaultSnapshot) {
        let previous_groups: HashMap<Uuid, u64> = self
            .groups
            .iter()
            .map(|group| (group.persistent_id, group.id))
            .collect();
        let previous_entries: HashMap<Uuid, u64> = self
            .entries
            .iter()
            .map(|entry| (entry.record.id, entry.id))
            .collect();
        let mut groups: Vec<_> = snapshot.groups.iter().collect();
        groups.sort_by_key(|g| g.sort_order);
        let mut mapped = Vec::with_capacity(groups.len());
        for group in groups {
            let id = if group.is_ungrouped {
                0
            } else if let Some(&id) = previous_groups
                .get(&group.id)
                .filter(|&&id| id != UNGROUPED_ID)
            {
                id
            } else {
                let id = self.next_group_id;
                self.next_group_id += 1;
                id
            };
            mapped.push(Group {
                id,
                name: group.name.clone(),
                persistent_id: group.id,
            });
        }
        let mapped_groups: HashMap<Uuid, (usize, u64)> = mapped
            .iter()
            .enumerate()
            .map(|(index, group)| (group.persistent_id, (index, group.id)))
            .collect();
        let mut entries: Vec<_> = snapshot.authenticators.iter().collect();
        entries.sort_by_key(|entry| (mapped_groups[&entry.group_id].0, entry.sort_order));
        let mut mapped_entries = Vec::with_capacity(entries.len());
        for entry in entries {
            let id = if let Some(&id) = previous_entries.get(&entry.id) {
                id
            } else {
                let id = self.next_entry_id;
                self.next_entry_id += 1;
                id
            };
            mapped_entries.push(Entry {
                id,
                group_id: mapped_groups[&entry.group_id].1,
                name: entry.name.clone(),
                description: entry.description.clone(),
                record: Arc::new(entry.clone()),
            });
        }
        self.groups = mapped;
        self.entries = mapped_entries;
        self.mark_changed();
    }
    pub fn snapshot(&self) -> VaultSnapshot {
        let mut vault = (*self.base).clone();
        vault.groups = self
            .groups
            .iter()
            .enumerate()
            .map(|(index, group)| bnauth_core::Group {
                id: group.persistent_id,
                name: group.name.clone(),
                sort_order: index as u32,
                is_ungrouped: group.id == 0,
            })
            .collect();
        let now = bnauth_core::now_ms();
        let mut entries = Vec::with_capacity(self.entries.len());
        for group in &self.groups {
            for (index, entry) in self
                .entries
                .iter()
                .filter(|e| e.group_id == group.id)
                .enumerate()
            {
                let mut record = (*entry.record).clone();
                if record.name != entry.name
                    || record.description != entry.description
                    || record.group_id != group.persistent_id
                    || record.sort_order != index as u32
                {
                    record.updated_at_ms = now;
                }
                record.name.clone_from(&entry.name);
                record.description.clone_from(&entry.description);
                record.group_id = group.persistent_id;
                record.sort_order = index as u32;
                entries.push(record);
            }
        }
        vault.authenticators = entries;
        vault
    }
    pub fn persistent_group(&self, id: u64) -> Option<Uuid> {
        self.groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.persistent_id)
    }
}

fn snapshot_metadata(snapshot: &VaultSnapshot) -> VaultSnapshot {
    VaultSnapshot {
        schema_version: snapshot.schema_version,
        scope: snapshot.scope.clone(),
        vault_id: snapshot.vault_id,
        revision: snapshot.revision,
        created_at_ms: snapshot.created_at_ms,
        updated_at_ms: snapshot.updated_at_ms,
        groups: Vec::new(),
        authenticators: Vec::new(),
        service_time_sync: snapshot.service_time_sync.clone(),
        provenance: snapshot.provenance.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_model() -> VaultModel {
        VaultModel::from_snapshot(&VaultSnapshot::new_empty(1_700_000_000_000))
    }

    fn assert_changed<T>(model: &mut VaultModel, mutate: impl FnOnce(&mut VaultModel) -> T) -> T {
        let previous = model.change_token();
        let result = mutate(model);
        assert_ne!(model.change_token(), previous);
        result
    }

    #[test]
    fn successful_mutations_invalidate_cached_rows() {
        let mut model = empty_model();
        let a = assert_changed(&mut model, |model| model.add_group("A")).unwrap();
        let b = assert_changed(&mut model, |model| model.add_group("B")).unwrap();
        assert_changed(&mut model, |model| model.rename_group(a, "Renamed")).unwrap();
        let first =
            assert_changed(&mut model, |model| model.add_test_entry("First", "", a)).unwrap();
        let second =
            assert_changed(&mut model, |model| model.add_test_entry("Second", "", a)).unwrap();
        assert_changed(&mut model, |model| {
            model.update_entry(first, "Updated", "Note")
        })
        .unwrap();
        assert_changed(&mut model, |model| model.reorder_entry(first, None)).unwrap();
        assert_changed(&mut model, |model| model.move_group(b, -1)).unwrap();
        assert_changed(&mut model, |model| model.move_entries(&[first], b)).unwrap();
        assert_eq!(
            assert_changed(&mut model, |model| model.delete_entries(&[second])),
            1
        );
        // Empty group deletion must invalidate the cache even though no rows move.
        assert_eq!(
            assert_changed(&mut model, |model| model.delete_group(a)),
            Ok(0)
        );
        assert_eq!(
            assert_changed(&mut model, |model| model.delete_group(b)),
            Ok(1)
        );
    }

    #[test]
    fn no_op_edits_keep_change_token_and_interleaved_rows_stable() {
        let mut model = empty_model();
        let a = model.add_group("A").unwrap();
        let b = model.add_group("B").unwrap();
        let first = model.add_test_entry("First", "Note", a).unwrap();
        model.add_test_entry("Other group", "", b).unwrap();
        let last = model.add_test_entry("Last", "", a).unwrap();
        let previous = model.clone();
        model.rename_group(a, "  A  ").unwrap();
        model.update_entry(first, " First ", "Note").unwrap();
        model.reorder_entry(first, Some(first)).unwrap();
        model.reorder_entry(first, Some(last)).unwrap();
        model.reorder_entry(last, None).unwrap();
        model.move_group(a, 0).unwrap();
        assert_eq!(model.move_entries(&[first, last], a), Ok(0));
        assert_eq!(model.delete_entries(&[u64::MAX]), 0);
        assert_eq!(model, previous);
    }

    #[test]
    fn loaded_group_names_use_the_same_duplicate_rules_as_storage() {
        for name in ["  Group", "Group  ", "  Group  ", "\u{3000}Group\u{3000}"] {
            let mut vault = VaultSnapshot::new_empty(1_700_000_000_000);
            for name in [name, "Other"] {
                vault.groups.push(bnauth_core::Group {
                    id: Uuid::new_v4(),
                    name: name.into(),
                    sort_order: vault.groups.len() as u32,
                    is_ungrouped: false,
                });
            }
            vault.validate().unwrap();
            let mut model = VaultModel::from_snapshot(&vault);
            let original = model.clone();
            let other = model.groups[2].id;
            for duplicate in ["group", " GROUP ", "\u{3000}Group\u{3000}"] {
                assert!(model.add_group(duplicate).unwrap_err().contains("同名"));
                assert!(
                    model
                        .rename_group(other, duplicate)
                        .unwrap_err()
                        .contains("同名")
                );
                assert_eq!(model, original);
            }
            let id = model.groups[1].id;
            model.rename_group(id, name).unwrap();
            assert_eq!(model, original);
            model.rename_group(id, "Changed").unwrap();
            model.snapshot().validate().unwrap();
        }
    }

    #[test]
    fn move_menu_counts_match_actual_moves_and_capacity_without_mutating() {
        let mut model = empty_model();
        let full = model.add_group("Full").unwrap();
        let source = model.add_group("Source").unwrap();
        model.add_group("Empty").unwrap();
        for group in [UNGROUPED_ID, full, source] {
            for _ in 0..if group == full { 100 } else { 2 } {
                model.add_test_entry("Row", "", group).unwrap();
            }
        }
        let selected = [
            model.filtered(full, "", false)[0],
            model.filtered(source, "", false)[0],
        ];
        let original = model.clone();
        let counts = model.move_target_counts(&selected);
        assert_eq!(model, original);
        assert_eq!(counts[&full], (100, 101));
        for group in &model.groups {
            let (current, after_move) = counts[&group.id];
            assert_eq!(current, model.group_count(group.id));
            let mut moved = model.clone();
            let result = moved.move_entries(&selected, group.id);
            assert_eq!(result.is_ok(), after_move <= MAX_GROUP_ENTRIES);
            if result.is_ok() {
                assert_eq!(moved.group_count(group.id), after_move);
                moved.snapshot().validate().unwrap();
            } else {
                assert_eq!(moved, original);
            }
        }
    }

    #[test]
    fn filtered_indices_follow_group_order_and_survive_reads() {
        let mut model = empty_model();
        let a = model.add_group("A").unwrap();
        let b = model.add_group("B").unwrap();
        let first = model.add_test_entry("ALPHA", "", b).unwrap();
        model.add_test_entry("Other", "alpha", a).unwrap();
        let last = model.add_test_entry("Alpha", "", a).unwrap();
        let token = model.change_token();
        let indices = model.filtered_indices(0, "  alpha  ", true);
        assert_eq!(indices, vec![2, 0]);
        assert_eq!(
            indices
                .iter()
                .map(|&index| model.entries[index].id)
                .collect::<Vec<_>>(),
            vec![last, first]
        );
        assert_eq!(model.filtered_indices(b, "alpha", false), vec![0]);
        assert_eq!(model.change_token(), token);
        model.move_group(b, -1).unwrap();
        assert_ne!(model.change_token(), token);
        assert_eq!(model.filtered_indices(0, "alpha", true), vec![0, 2]);
    }

    #[test]
    fn model_clones_share_records_without_a_second_snapshot_copy() {
        let timestamp = 1_700_000_000_000;
        let vault = VaultSnapshot::new_empty(timestamp);
        let material = bnauth_core::parse_manual(&"01".repeat(20), "", "").unwrap();
        let vault = bnauth_core::prepare_import(
            &vault,
            "Authenticator",
            &"x".repeat(16 * 1024),
            vault.ungrouped_id().unwrap(),
            material,
            timestamp,
        )
        .unwrap()
        .candidate;
        let mut model = VaultModel::from_snapshot(&vault);
        let cloned = model.clone();
        assert!(Arc::ptr_eq(
            &model.entries[0].record,
            &cloned.entries[0].record
        ));
        assert!(Arc::ptr_eq(&model.base, &cloned.base));
        assert!(model.base.authenticators.is_empty());
        assert!(model.base.groups.is_empty());
        assert_eq!(model.snapshot(), vault);
        let token = model.change_token();
        model.apply_snapshot(&vault);
        assert_ne!(model.change_token(), token);
        assert_eq!(model.entries[0].id, cloned.entries[0].id);
    }

    #[test]
    fn reloaded_group_uuid_roles_never_reuse_the_system_ui_id() {
        let timestamp = 1_700_000_000_000;
        let mut original = VaultSnapshot::new_empty(timestamp);
        for name in ["Custom", "Unchanged"] {
            original.groups.push(bnauth_core::Group {
                id: Uuid::new_v4(),
                name: name.into(),
                sort_order: original.groups.len() as u32,
                is_ungrouped: false,
            });
        }
        for index in 0..original.groups.len() {
            let material =
                bnauth_core::parse_manual(&format!("{:02X}", index + 1).repeat(20), "", "")
                    .unwrap();
            original = bnauth_core::prepare_import(
                &original,
                &format!("Authenticator {index}"),
                "",
                original.groups[index].id,
                material,
                timestamp,
            )
            .unwrap()
            .candidate;
        }
        let mut model = VaultModel::from_snapshot(&original);
        let unchanged_group_id = model.groups[2].id;
        let entry_ids: HashMap<_, _> = model
            .entries
            .iter()
            .map(|entry| (entry.record.id, entry.id))
            .collect();

        // Both documents are valid. Only the two group UUIDs change roles.
        let mut reloaded = original.clone();
        reloaded.groups[0].id = original.groups[1].id;
        reloaded.groups[1].id = original.groups[0].id;
        reloaded.validate().unwrap();
        model.apply_snapshot(&reloaded);
        assert_eq!(
            model
                .groups
                .iter()
                .map(|group| group.id)
                .collect::<HashSet<_>>()
                .len(),
            model.groups.len(),
            "the reserved system UI ID must not also identify a custom group"
        );
        assert_eq!(model.groups[0].id, UNGROUPED_ID);
        assert_ne!(model.groups[1].id, UNGROUPED_ID);
        assert_eq!(model.groups[2].id, unchanged_group_id);
        for entry in &model.entries {
            assert_eq!(entry.id, entry_ids[&entry.record.id]);
        }

        let saved = model.snapshot();
        saved.validate().unwrap();
        let custom_uuid = model.persistent_group(model.groups[1].id).unwrap();
        assert_eq!(custom_uuid, reloaded.groups[1].id);
        let imported = bnauth_core::prepare_import(
            &saved,
            "Imported into Custom",
            "",
            custom_uuid,
            bnauth_core::parse_manual(&"04".repeat(20), "", "").unwrap(),
            timestamp,
        )
        .unwrap()
        .candidate;
        assert_eq!(
            imported.authenticators.last().unwrap().group_id,
            custom_uuid
        );
        assert_ne!(custom_uuid, imported.ungrouped_id().unwrap());
    }

    #[test]
    fn name_only_search_keeps_same_names_and_ignores_notes() {
        let mut model = empty_model();
        let other = model.add_group("其他").unwrap();
        let a = model
            .add_test_entry("旅行者Alpha", "only-in-description", 0)
            .unwrap();
        let b = model.add_test_entry("旅行者Alpha", "", other).unwrap();
        assert_eq!(model.filtered(0, "  ALPHA ", true), vec![a, b]);
        assert_eq!(model.filtered(0, "旅行者", false), vec![a]);
        assert!(model.filtered(0, "only-in-description", true).is_empty());
        assert!(
            model
                .filtered(
                    0,
                    &bnauth_core::totp(
                        model.entry(a).unwrap().record.secret.as_ref().unwrap(),
                        1_700_000_000_000
                    )
                    .unwrap(),
                    true
                )
                .is_empty()
        );
        model.update_entry(a, "旅行者Alpha", "").unwrap();
        let original = model.clone();
        assert!(model.update_entry(a, " \t ", "changed").is_err());
        assert_eq!(model, original);
    }

    #[test]
    fn batch_move_uses_source_order_and_keeps_existing_target_rows() {
        let mut model = empty_model();
        let a = model.add_group("A").unwrap();
        let b = model.add_group("B").unwrap();
        let target = model.add_group("目標").unwrap();
        let a1 = model.add_test_entry("A1", "", a).unwrap();
        let a2 = model.add_test_entry("A2", "", a).unwrap();
        let b1 = model.add_test_entry("B1", "", b).unwrap();
        let existing = model.add_test_entry("既有", "", target).unwrap();
        assert_eq!(
            model.move_entries(&[b1, existing, a2, a1, a1], target),
            Ok(3)
        );
        assert_eq!(
            model.filtered(target, "", false),
            vec![existing, a1, a2, b1]
        );
        assert_eq!(model.move_entries(&[existing], target), Ok(0));
        let original = model.clone();
        assert!(model.move_entries(&[a1, u64::MAX], a).is_err());
        assert_eq!(model, original);
    }

    #[test]
    fn move_and_delete_group_reject_capacity_overflow_atomically() {
        let mut model = empty_model();
        let source = model.add_group("來源").unwrap();
        for _ in 0..99 {
            model.add_test_entry("既有", "", 0).unwrap();
        }
        let first = model.add_test_entry("第一筆", "", source).unwrap();
        let second = model.add_test_entry("第二筆", "", source).unwrap();
        let original = model.clone();
        assert!(model.move_entries(&[first, second], 0).is_err());
        assert_eq!(model, original);
        assert!(model.delete_group(source).is_err());
        assert_eq!(model, original);
        assert_eq!(model.delete_entries(&[second, second]), 1);
        assert_eq!(model.delete_group(source), Ok(1));
        assert_eq!(model.group_count(0), 100);
        assert_eq!(model.entry(first).unwrap().group_id, 0);
        assert!(model.group_name(source).is_none());
    }

    #[test]
    fn all_three_capacity_limits_are_enforced_without_partial_mutation() {
        let mut model = empty_model();
        for index in 1..MAX_GROUPS {
            model.add_group(&format!("分組{index}")).unwrap();
        }
        let original = model.clone();
        assert!(model.add_group("超出分組").is_err());
        assert_eq!(model, original);
        for group in 0..10 {
            for _ in 0..MAX_GROUP_ENTRIES {
                model.add_test_entry("可同名", "", group).unwrap();
            }
            if group == 0 {
                let original = model.clone();
                assert!(model.add_test_entry("單組超限", "", 0).is_err());
                assert_eq!(model, original);
            }
        }
        let original = model.clone();
        assert!(model.add_test_entry("全庫超限", "", 10).is_err());
        assert_eq!(model, original);
        assert_eq!(model.entries.len(), MAX_ENTRIES);
    }

    #[test]
    fn row_and_tab_reordering_preserve_identity_and_system_group() {
        let mut model = empty_model();
        let a = model.add_group("A").unwrap();
        let b = model.add_group("B").unwrap();
        let a1 = model.add_test_entry("A1", "", a).unwrap();
        let a2 = model.add_test_entry("A2", "", a).unwrap();
        let a3 = model.add_test_entry("A3", "", a).unwrap();
        let b1 = model.add_test_entry("B1", "", b).unwrap();
        let original = model.clone();
        assert!(model.reorder_entry(a1, Some(b1)).is_err());
        assert_eq!(model, original);
        model.reorder_entry(a3, Some(a1)).unwrap();
        assert_eq!(model.filtered(a, "", false), vec![a3, a1, a2]);
        model.reorder_entry(a3, None).unwrap();
        assert_eq!(model.filtered(a, "", false), vec![a1, a2, a3]);
        model.move_group(b, -1).unwrap();
        assert_eq!(
            model
                .groups
                .iter()
                .map(|group| group.id)
                .collect::<Vec<_>>(),
            vec![0, b, a]
        );
        let original = model.clone();
        assert!(model.move_group(b, -1).is_err());
        assert!(model.move_group(0, 1).is_err());
        assert!(model.rename_group(0, "改名").is_err());
        assert!(model.delete_group(0).is_err());
        assert_eq!(model, original);
    }
}

#[cfg(test)]
#[path = "snapshot_bridge_tests.rs"]
mod snapshot_bridge_tests;
