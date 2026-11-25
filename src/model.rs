//! View keys and editing projection; persisted identities and secrets live in the core snapshot.
use bnauth_core::{Authenticator, DEFAULT_GROUP_NAME, VaultSnapshot};
pub use bnauth_core::{MAX_ENTRIES, MAX_GROUP_ENTRIES, MAX_GROUPS};
use std::collections::{HashMap, HashSet};
#[cfg(test)]
use std::sync::Arc;
use uuid::Uuid;

pub const UNGROUPED_ID: u64 = 0;

#[derive(Debug, PartialEq, Eq)]
pub enum GroupEditError {
    DuplicateName,
    NameTooLong,
    Message(String),
}

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
    pub record: Authenticator,
}

#[derive(Debug)]
pub struct GroupOrderEdit {
    pub previous_token: u64,
    pub spans: Vec<GroupOrderSpan>,
}

#[derive(Debug)]
pub struct GroupOrderSpan {
    pub group: u64,
    pub start: usize,
    pub previous: Vec<usize>,
    pub new_len: usize,
}

impl GroupOrderEdit {
    pub fn group_count_changed(&self, group: Option<u64>) -> bool {
        self.spans
            .iter()
            .any(|s| Some(s.group) == group && s.new_len != s.previous.len())
    }
}

impl GroupOrderSpan {
    pub fn rows_after<'a>(&'a self, model: &'a VaultModel, forward: bool) -> &'a [usize] {
        if forward {
            &model.rows_in_group(self.group)[self.start..self.start + self.new_len]
        } else {
            &self.previous
        }
    }
}

pub struct EntryDeletion {
    // File slots before deletion, sorted so surviving records keep their file order.
    slots: Vec<usize>,
    groups: HashMap<u64, GroupDeletion>,
}

struct GroupDeletion {
    first_order: usize,
    // Surviving rows after the first deletion, in display order, using their new file slots.
    reordered_rows: Vec<usize>,
}

impl EntryDeletion {
    pub fn deleted_slots(&self) -> &[usize] {
        &self.slots
    }

    pub fn snapshot(&self, current: &VaultSnapshot) -> VaultSnapshot {
        let source = &current.authenticators;
        let mut records = Vec::with_capacity(source.len() - self.slots.len());
        let mut start = 0;
        for &deleted in &self.slots {
            records.extend_from_slice(&source[start..deleted]);
            start = deleted + 1;
        }
        records.extend_from_slice(&source[start..]);
        for group in self.groups.values() {
            for (order, &slot) in (group.first_order..).zip(&group.reordered_rows) {
                records[slot].sort_order = order as u32;
            }
        }
        let mut candidate = current.clone();
        candidate.authenticators = records.into();
        candidate
    }
}

impl Entry {
    fn from_record(id: u64, group_id: u64, record: &Authenticator) -> Self {
        Self {
            id,
            group_id,
            record: record.clone(),
        }
    }

    fn set_record_order(&mut self, group: Uuid, order: usize) -> bool {
        let order = order as u32;
        if self.record.group_id == group && self.record.sort_order == order {
            return false;
        }
        self.record.group_id = group;
        self.record.sort_order = order;
        true
    }
}

/// The dialog stages group changes without copying any authenticator.
#[derive(Debug)]
pub struct GroupDraft {
    pub groups: Vec<Group>,
    group_counts: HashMap<u64, usize>,
    next_group_id: u64,
    revision: u64,
}

impl GroupDraft {
    pub fn group_name(&self, id: u64) -> Option<&str> {
        self.groups
            .iter()
            .find(|group| group.id == id)
            .map(|group| group.name.as_str())
    }

    pub fn group_count(&self, id: u64) -> usize {
        self.group_counts.get(&id).copied().unwrap_or_default()
    }

    pub fn change_token(&self) -> u64 {
        self.revision
    }

    pub fn changed_from(&self, model: &VaultModel) -> bool {
        self.groups != model.groups
    }

    pub fn snapshot(&self, current: &VaultSnapshot) -> VaultSnapshot {
        let mut snapshot = current.clone();
        snapshot.groups = snapshot_groups(&self.groups).into();
        snapshot
    }

    pub fn add_group_checked(
        &mut self,
        name: &str,
        duplicate: bool,
    ) -> Result<u64, GroupEditError> {
        let name = Self::valid_group_name(name, duplicate)?;
        if self.groups.len() >= MAX_GROUPS {
            return Err(GroupEditError::Message(format!(
                "最多建立{MAX_GROUPS}個分組，包含{DEFAULT_GROUP_NAME}。"
            )));
        }
        let id = self.next_group_id;
        self.groups.push(Group {
            id,
            name,
            persistent_id: Uuid::new_v4(),
        });
        self.next_group_id += 1;
        self.revision += 1;
        Ok(id)
    }

    pub fn rename_group_checked(
        &mut self,
        id: u64,
        name: &str,
        duplicate: bool,
    ) -> Result<(), GroupEditError> {
        if id == UNGROUPED_ID {
            return Err(GroupEditError::Message(format!("{DEFAULT_GROUP_NAME}不可改名。")));
        }
        let index = self
            .groups
            .iter()
            .position(|group| group.id == id)
            .ok_or_else(|| GroupEditError::Message("找不到分組。".into()))?;
        if self.groups[index].name == name {
            return Ok(());
        }
        self.groups[index].name = Self::valid_group_name(name, duplicate)?;
        self.revision += 1;
        Ok(())
    }

    pub fn delete_group(&mut self, id: u64) -> Result<(), String> {
        if id == UNGROUPED_ID {
            return Err(format!("{DEFAULT_GROUP_NAME}不可刪除。"));
        }
        if self.group_count(id) != 0 {
            return Err("只能刪除空分組。".to_owned());
        }
        let index = self
            .groups
            .iter()
            .position(|group| group.id == id)
            .ok_or("找不到分組。")?;
        self.groups.remove(index);
        self.revision += 1;
        Ok(())
    }

    /// Custom TABs can move by a signed offset; the system TAB remains first.
    pub fn move_group(&mut self, id: u64, delta: isize) -> Result<(), String> {
        if id == UNGROUPED_ID {
            return Err(format!("{DEFAULT_GROUP_NAME}固定在第一個分頁。"));
        }
        let index = self
            .groups
            .iter()
            .position(|group| group.id == id)
            .ok_or("找不到分組。")?;
        if let Some(target) = index
            .checked_add_signed(delta)
            .filter(|&target| target > 0 && target < self.groups.len())
            && target != index
        {
            if index < target {
                self.groups[index..=target].rotate_left(1);
            } else {
                self.groups[target..=index].rotate_right(1);
            }
            self.revision += 1;
        }
        Ok(())
    }

    pub fn group_name_in_use(&self, name: &str, except: Option<u64>) -> bool {
        let key = name.to_lowercase();
        self.groups
            .iter()
            .any(|group| Some(group.id) != except && group.name.to_lowercase() == key)
    }

    #[cfg(test)]
    pub fn add_group(&mut self, name: &str) -> Result<u64, GroupEditError> {
        self.add_group_checked(name, self.group_name_in_use(name, None))
    }

    #[cfg(test)]
    pub fn rename_group(&mut self, id: u64, name: &str) -> Result<(), GroupEditError> {
        self.rename_group_checked(id, name, self.group_name_in_use(name, Some(id)))
    }

    fn valid_group_name(name: &str, duplicate: bool) -> Result<String, GroupEditError> {
        if name.len() > bnauth_core::MAX_GROUP_NAME_BYTES {
            return Err(GroupEditError::NameTooLong);
        }
        if name.trim().is_empty() {
            return Err(GroupEditError::Message("請輸入分組名稱。".into()));
        }
        bnauth_core::bounded(name, bnauth_core::MAX_GROUP_NAME_BYTES, "分組名稱")
            .map_err(GroupEditError::Message)?;
        if duplicate {
            return Err(GroupEditError::DuplicateName);
        }
        Ok(name.to_owned())
    }
}

pub struct MovePlan {
    token: u64,
    selected: HashSet<u64>,
    // TAB order, with the first selected row in each source.
    source_groups: Vec<(u64, usize, usize)>,
    selection_exists: bool,
    counts: HashMap<u64, (usize, usize)>,
}

impl MovePlan {
    pub fn is_current(&self, model: &VaultModel) -> bool {
        self.token == model.move_token
    }

    pub fn counts(&self) -> &HashMap<u64, (usize, usize)> {
        &self.counts
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct VaultModel {
    pub groups: Vec<Group>,
    // File slots are stable across text edits, moves, and reorders.
    pub entries: Vec<Entry>,
    entry_slots: HashMap<u64, usize>,
    // Each group's visible order, independent of how records interleave on disk.
    group_rows: HashMap<u64, Vec<usize>>,
    next_group_id: u64,
    next_entry_id: u64,
    change_token: u64,
    projection_token: u64,
    move_token: u64,
    group_token: u64,
    // Test fixtures can materialize a complete snapshot from the display model.
    #[cfg(test)]
    base: Arc<VaultSnapshot>,
}

impl VaultModel {
    #[cfg(test)]
    pub fn test_fixture() -> Self {
        let mut model = Self::from_snapshot(&VaultSnapshot::new_empty());
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

    pub fn projection_token(&self) -> u64 {
        self.projection_token
    }

    pub fn group_token(&self) -> u64 {
        self.group_token
    }

    fn mark_changed(&mut self) {
        self.mark_projection_changed();
        self.move_token = self.change_token;
    }

    fn mark_projection_changed(&mut self) {
        self.mark_data_changed();
        self.projection_token = self.change_token;
    }

    fn mark_data_changed(&mut self) {
        self.change_token = self.change_token.wrapping_add(1);
    }

    /// Returns IDs in TAB order, then in the original row order within each TAB.
    /// Only names participate in matching; description and code are excluded.
    #[cfg(test)]
    pub fn filtered(&self, current_group: u64, query: &str, all_groups: bool) -> Vec<u64> {
        self.filtered_with_counts(
            current_group,
            &query.trim().to_lowercase(),
            all_groups,
            |_, _| {},
        )
    }

    pub fn filtered_with_counts(
        &self,
        current_group: u64,
        query: &str,
        all_groups: bool,
        mut group_hits: impl FnMut(u64, usize),
    ) -> Vec<u64> {
        if !all_groups && query.is_empty() {
            return self
                .rows_in_group(current_group)
                .iter()
                .map(|&row| self.entries[row].id)
                .collect();
        }
        let mut ids = Vec::new();
        for group in self
            .groups
            .iter()
            .filter(|group| all_groups || group.id == current_group)
        {
            let start = ids.len();
            ids.extend(
                self.rows_in_group(group.id)
                    .iter()
                    .map(|&row| &self.entries[row])
                    .filter(|entry| name_matches(&entry.record.name, query))
                    .map(|entry| entry.id),
            );
            if ids.len() > start {
                group_hits(group.id, ids.len() - start);
            }
        }
        ids
    }

    pub fn entry_slot(&self, id: u64) -> usize {
        self.entry_slots[&id]
    }

    pub fn entry(&self, id: u64) -> Option<&Entry> {
        self.entry_slots.get(&id).map(|&slot| &self.entries[slot])
    }

    pub fn group_name(&self, id: u64) -> Option<&str> {
        self.groups
            .iter()
            .find(|group| group.id == id)
            .map(|group| group.name.as_str())
    }

    pub fn group_count(&self, group: u64) -> usize {
        self.rows_in_group(group).len()
    }

    pub fn rows_in_group(&self, group: u64) -> &[usize] {
        self.group_rows
            .get(&group)
            .map(Vec::as_slice)
            .unwrap_or_default()
    }

    pub fn entry_limit_reached(&self) -> bool {
        self.entries.len() >= MAX_ENTRIES
    }

    fn push_entry(&mut self, entry: Entry) {
        let slot = self.entries.len();
        self.entry_slots.insert(entry.id, slot);
        self.group_rows
            .entry(entry.group_id)
            .or_default()
            .push(slot);
        self.entries.push(entry);
    }

    pub fn copy_target_reason(
        &self,
        source: u64,
        target: u64,
        count: usize,
    ) -> Option<&'static str> {
        if source == target {
            Some("驗證器已在這個分組。")
        } else if count >= MAX_GROUP_ENTRIES {
            Some("目標分組已達容量上限。")
        } else if self.entry_limit_reached() {
            Some("資料庫已達容量上限。")
        } else {
            None
        }
    }

    pub fn prepare_copy(&self, index: usize, target: u64) -> Result<Authenticator, String> {
        let entry = &self.entries[index];
        let group_id = self.persistent_group(target).ok_or("找不到目標分組。")?;
        let count = self.group_count(target);
        if let Some(reason) = self.copy_target_reason(entry.group_id, target, count) {
            return Err(reason.into());
        }

        let mut copy = entry.record.clone();
        copy.id = Uuid::new_v4();
        copy.group_id = group_id;
        copy.sort_order = count as u32;
        Ok(copy)
    }

    pub fn acknowledge_copy(&mut self, target: u64, record: &Authenticator) -> u64 {
        let id = self.next_entry_id;
        self.next_entry_id += 1;
        self.push_entry(Entry::from_record(id, target, record));
        self.mark_changed();
        id
    }

    /// Validate the selection and count all destinations for the current list.
    pub fn plan_move(&self, ids: impl IntoIterator<Item = u64>) -> MovePlan {
        let selected: HashSet<_> = ids.into_iter().collect();
        let mut sources = HashMap::new();
        let mut selected_count = 0;
        for &id in &selected {
            if let Some(entry) = self.entry(id) {
                let (first, selected_here) =
                    sources.entry(entry.group_id).or_insert((usize::MAX, 0));
                *first = (*first).min(entry.record.sort_order as usize);
                *selected_here += 1;
                selected_count += 1;
            }
        }
        let mut counts = HashMap::with_capacity(self.groups.len());
        let mut source_groups = Vec::with_capacity(sources.len());
        for group in &self.groups {
            let selected_here = sources.get(&group.id).map_or(0, |&(first, count)| {
                source_groups.push((group.id, first, count));
                count
            });
            let count = self.group_count(group.id);
            counts.insert(group.id, (count, count + selected_count - selected_here));
        }
        MovePlan {
            token: self.move_token,
            selection_exists: selected_count == selected.len(),
            selected,
            source_groups,
            counts,
        }
    }

    pub fn move_entries_from_plan(
        &mut self,
        plan: &MovePlan,
        target: u64,
    ) -> Result<Option<GroupOrderEdit>, String> {
        if !plan.is_current(self) || !plan.selection_exists {
            return Err("選取的驗證器已不存在，請重新選取。".to_owned());
        }
        let &(before, after_move) = plan.counts.get(&target).ok_or("找不到目標分組。")?;
        self.check_move_capacity(target, after_move)?;
        let moved_count = after_move - before;
        if moved_count == 0 {
            return Ok(None);
        }
        let sources = plan
            .source_groups
            .iter()
            .copied()
            .filter(|&(group, _, _)| group != target);
        let mut edit = GroupOrderEdit {
            previous_token: self.projection_token,
            spans: Vec::with_capacity(plan.source_groups.len() + 1),
        };
        edit.spans
            .push(self.order_span(target, before, before, moved_count));
        let mut destination = self.group_rows.remove(&target).unwrap_or_default();
        destination.reserve(moved_count);
        for (source, first, selected_here) in sources {
            let end = self.group_count(source);
            edit.spans
                .push(self.order_span(source, first, end, end - first - selected_here));
            let rows = self
                .group_rows
                .get_mut(&source)
                .expect("selected source group has rows");
            rows.extract_if(first.., |row| {
                let entry = &mut self.entries[*row];
                if plan.selected.contains(&entry.id) {
                    entry.group_id = target;
                    return true;
                }
                false
            })
            .for_each(|row| destination.push(row));
            if rows.is_empty() {
                self.group_rows.remove(&source);
            }
        }
        self.group_rows.insert(target, destination);
        self.mark_changed();
        Ok(Some(edit))
    }

    /// The row menu or drag gesture already identifies the row and destination.
    pub fn move_entry_at(
        &mut self,
        index: usize,
        target: u64,
        after_move: usize,
    ) -> Result<Option<GroupOrderEdit>, String> {
        let source = self.entries[index].group_id;
        if source == target {
            return Ok(None);
        }
        self.check_move_capacity(target, after_move)?;
        let first = self.entries[index].record.sort_order as usize;
        let end = self.group_count(source);
        let edit = GroupOrderEdit {
            previous_token: self.projection_token,
            spans: vec![
                self.order_span(source, first, end, end - first - 1),
                self.order_span(target, after_move - 1, after_move - 1, 1),
            ],
        };
        let rows = self
            .group_rows
            .get_mut(&source)
            .expect("moved row belongs to a group");
        rows.remove(first);
        if rows.is_empty() {
            self.group_rows.remove(&source);
        }
        self.entries[index].group_id = target;
        self.group_rows.entry(target).or_default().push(index);
        self.mark_changed();
        Ok(Some(edit))
    }

    fn check_move_capacity(&self, target: u64, after_move: usize) -> Result<(), String> {
        if after_move > MAX_GROUP_ENTRIES {
            let name = self.group_name(target).ok_or("找不到目標分組。")?;
            return Err(format!(
                "移入後，「{name}」將超過每組 {MAX_GROUP_ENTRIES} 筆驗證器的上限。"
            ));
        }
        Ok(())
    }

    pub fn prepare_deletion(&self, ids: &[u64]) -> Option<EntryDeletion> {
        let mut slots: Vec<_> = ids
            .iter()
            .filter_map(|id| self.entry_slots.get(id).copied())
            .collect();
        if slots.is_empty() {
            return None;
        }
        slots.sort_unstable();
        let mut groups = HashMap::new();
        for &slot in &slots {
            let entry = &self.entries[slot];
            let order = entry.record.sort_order as usize;
            let group = groups
                .entry(entry.group_id)
                .or_insert_with(|| GroupDeletion {
                    first_order: order,
                    reordered_rows: Vec::new(),
                });
            group.first_order = group.first_order.min(order);
        }
        for (&id, group) in &mut groups {
            group.reordered_rows = self.rows_in_group(id)[group.first_order + 1..]
                .iter()
                .filter_map(|row| slots.binary_search(row).err().map(|before| row - before))
                .collect();
        }
        Some(EntryDeletion { slots, groups })
    }

    pub fn acknowledge_deletion(&mut self, change: EntryDeletion, saved: &VaultSnapshot) {
        let first = change.slots[0];
        let mut deleted = change.slots.into_iter().peekable();
        let mut old_slot = first;
        let mut new_slot = first;
        // Preserve file order. Only the suffix after the first removal changes slots.
        self.entries
            .extract_if(first.., |entry| {
                let remove = deleted.next_if_eq(&old_slot).is_some();
                old_slot += 1;
                if remove {
                    self.entry_slots.remove(&entry.id);
                } else {
                    self.entry_slots.insert(entry.id, new_slot);
                    let order = entry.record.sort_order as usize;
                    if change
                        .groups
                        .get(&entry.group_id)
                        .is_none_or(|group| order < group.first_order)
                    {
                        let rows = self
                            .group_rows
                            .get_mut(&entry.group_id)
                            .expect("existing group rows");
                        rows[order] = new_slot;
                    }
                    new_slot += 1;
                }
                remove
            })
            .for_each(drop);
        for (id, group) in change.groups {
            let rows = self.group_rows.get_mut(&id).expect("existing group rows");
            rows.truncate(group.first_order);
            for &slot in &group.reordered_rows {
                self.entries[slot]
                    .record
                    .clone_from(&saved.authenticators[slot]);
            }
            rows.extend(group.reordered_rows);
            if rows.is_empty() {
                self.group_rows.remove(&id);
            }
        }
        self.mark_changed();
    }

    pub fn group_draft(&self) -> GroupDraft {
        GroupDraft {
            groups: self.groups.clone(),
            group_counts: self
                .group_rows
                .iter()
                .map(|(&id, rows)| (id, rows.len()))
                .collect(),
            next_group_id: self.next_group_id,
            revision: 0,
        }
    }

    pub fn acknowledge_groups(&mut self, draft: GroupDraft) {
        let same_tabs = self
            .groups
            .iter()
            .map(|g| g.id)
            .eq(draft.groups.iter().map(|g| g.id));
        self.groups = draft.groups;
        self.group_token = self.group_token.wrapping_add(1);
        self.next_group_id = draft.next_group_id;
        if same_tabs {
            self.mark_projection_changed();
        } else {
            self.mark_changed();
        }
    }

    pub fn check_entry_edit(
        &self,
        id: u64,
        name: &str,
        description: &str,
    ) -> Result<Option<usize>, String> {
        let &index = self.entry_slots.get(&id).ok_or("找不到驗證器。")?;
        let record = &self.entries[index].record;
        let name_changed = name != record.name;
        let note_changed = description != record.description;
        if !name_changed && !note_changed {
            return Ok(None);
        }
        if name_changed {
            valid_entry_name(name)?;
        }
        if note_changed {
            bnauth_core::bounded(description, bnauth_core::MAX_DESCRIPTION_BYTES, "備註")?;
        }
        Ok(Some(index))
    }

    pub fn acknowledge_entry_edit(&mut self, row: usize, saved: &Authenticator) -> bool {
        let entry = &mut self.entries[row];
        let renamed = entry.record.name != saved.name;
        entry.record.clone_from(saved);
        if renamed {
            self.mark_projection_changed();
        } else {
            self.mark_data_changed();
        }
        renamed
    }

    /// Builds a valid row from a unique public test seed; no fixture state exists in release builds.
    #[cfg(test)]
    pub fn add_test_entry(
        &mut self,
        name: &str,
        description: &str,
        group: u64,
    ) -> Result<u64, String> {
        valid_entry_name(name)?;
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
        let empty = VaultSnapshot::new_empty();
        let material =
            bnauth_core::parse_manual(&bnauth_core::Secret::from_bytes(seed).hex(), "", "")?;
        let mut record =
            bnauth_core::prepare_import(&empty, name, description, empty.groups[0].id, material)?
                .record();
        record.group_id = self.persistent_group(group).unwrap();
        record.sort_order = self.group_count(group) as u32;
        self.push_entry(Entry {
            id,
            group_id: group,
            record,
        });
        self.next_entry_id = next_id;
        self.mark_changed();
        Ok(id)
    }

    /// Insert at a gap in the displayed group's row order.
    pub fn reorder_entry_at(
        &mut self,
        index: usize,
        visible_group: u64,
        slot: usize,
    ) -> Result<Option<GroupOrderEdit>, String> {
        let (above, below) = self.rows_in_group(visible_group).split_at(slot);
        let previous = above.last().copied();
        let next = below.first().copied();
        if previous == Some(index) || next == Some(index) {
            return Ok(None);
        }
        let neighbor = next.or(previous).expect("drop on a nonempty group");
        let group = self.entries[index].group_id;
        if self.entries[neighbor].group_id != group {
            return Err("組內排序只能使用同一分組的驗證器。".to_owned());
        }
        let from = self.entries[index].record.sort_order as usize;
        let start = from.min(slot);
        let end = if from < slot { slot } else { from + 1 };
        let edit = GroupOrderEdit {
            previous_token: self.projection_token,
            spans: vec![self.order_span(group, start, end, end - start)],
        };
        let rows = self
            .group_rows
            .get_mut(&group)
            .expect("reordered group has rows");
        if from < slot {
            rows[from..slot].rotate_left(1);
        } else {
            rows[slot..=from].rotate_right(1);
        }
        self.mark_changed();
        Ok(Some(edit))
    }
}

#[cfg(test)]
impl VaultModel {
    pub fn copy_entry_at(&mut self, index: usize, target: u64) -> Result<u64, String> {
        let record = self.prepare_copy(index, target)?;
        Ok(self.acknowledge_copy(target, &record))
    }

    pub fn delete_entries(&mut self, ids: &[u64]) -> bool {
        let current = self.snapshot();
        let Some(change) = self.prepare_deletion(ids) else {
            return false;
        };
        let saved = change.snapshot(&current);
        self.acknowledge_deletion(change, &saved);
        true
    }

    pub fn update_entry(&mut self, id: u64, name: &str, description: &str) -> Result<(), String> {
        if let Some(row) = self.check_entry_edit(id, name, description)? {
            let edited = self.entries[row]
                .record
                .with_text(name.to_owned(), description.to_owned());
            self.acknowledge_entry_edit(row, &edited);
        }
        Ok(())
    }

    pub fn move_entries(&mut self, ids: &[u64], target: u64) -> Result<usize, String> {
        let before = self.group_count(target);
        if let Some(change) =
            self.move_entries_from_plan(&self.plan_move(ids.iter().copied()), target)?
        {
            self.apply_group_order(&change, |_, _| {});
        }
        Ok(self.group_count(target) - before)
    }

    pub fn reorder_entry(&mut self, id: u64, before: Option<u64>) -> Result<(), String> {
        let index = self
            .entries
            .iter()
            .position(|entry| entry.id == id)
            .unwrap();
        let group = match before {
            Some(id) => self.entry(id).ok_or("找不到目標驗證器。")?.group_id,
            None => self.entries[index].group_id,
        };
        let rows = self.rows_in_group(group);
        let slot = before.map_or(rows.len(), |id| {
            rows.iter()
                .position(|&row| self.entries[row].id == id)
                .unwrap()
        });
        if let Some(change) = self.reorder_entry_at(index, group, slot)? {
            self.apply_group_order(&change, |_, _| {});
        }
        Ok(())
    }

    fn edit_test_groups<T, E>(
        &mut self,
        edit: impl FnOnce(&mut GroupDraft) -> Result<T, E>,
    ) -> Result<T, E> {
        let mut draft = self.group_draft();
        let result = edit(&mut draft)?;
        if draft.changed_from(self) {
            self.acknowledge_groups(draft);
        }
        Ok(result)
    }

    pub fn add_group(&mut self, name: &str) -> Result<u64, GroupEditError> {
        self.edit_test_groups(|draft| draft.add_group(name))
    }

    pub fn rename_group(&mut self, id: u64, name: &str) -> Result<(), GroupEditError> {
        self.edit_test_groups(|draft| draft.rename_group(id, name))
    }

    pub fn move_group(&mut self, id: u64, delta: isize) -> Result<(), String> {
        self.edit_test_groups(|draft| draft.move_group(id, delta))
    }

    pub fn delete_group(&mut self, id: u64) -> Result<(), String> {
        self.edit_test_groups(|draft| draft.delete_group(id))
    }
}

fn valid_entry_name(name: &str) -> Result<(), String> {
    if name.trim().is_empty() {
        return Err("請輸入名稱。".to_owned());
    }
    bnauth_core::bounded(name, bnauth_core::MAX_NAME_BYTES, "名稱")
}

pub fn name_matches(name: &str, folded_query: &str) -> bool {
    folded_query.is_empty() || name.to_lowercase().contains(folded_query)
}

impl VaultModel {
    pub fn from_snapshot(snapshot: &VaultSnapshot) -> Self {
        let mut model = Self {
            groups: Vec::new(),
            entries: Vec::new(),
            entry_slots: HashMap::new(),
            group_rows: HashMap::new(),
            next_group_id: 1,
            next_entry_id: 1001,
            change_token: 0,
            projection_token: 0,
            move_token: 0,
            group_token: 0,
            #[cfg(test)]
            base: Arc::new(snapshot.metadata_only()),
        };
        model.replace_rows(snapshot);
        model
    }
    pub fn apply_snapshot(&mut self, snapshot: &VaultSnapshot) {
        #[cfg(test)]
        self.apply_snapshot_metadata(snapshot);
        self.replace_rows(snapshot);
    }

    #[cfg(test)]
    pub fn apply_snapshot_metadata(&mut self, snapshot: &VaultSnapshot) {
        self.base = Arc::new(snapshot.metadata_only());
    }

    /// Append one recovered record or one imported group, returning the new tab if created.
    pub fn acknowledge_import(&mut self, snapshot: &VaultSnapshot) -> Option<u64> {
        #[cfg(test)]
        self.apply_snapshot_metadata(snapshot);
        let new_group = snapshot.groups.get(self.groups.len()).map(|group| {
            let id = self.next_group_id;
            self.next_group_id += 1;
            self.group_token = self.group_token.wrapping_add(1);
            self.groups.push(Group {
                id,
                name: group.name.clone(),
                persistent_id: group.id,
            });
            id
        });
        let records = &snapshot.authenticators[self.entries.len()..];
        if let Some(first) = records.first() {
            let target = new_group.unwrap_or_else(|| {
                self.groups
                    .iter()
                    .find(|group| group.persistent_id == first.group_id)
                    .expect("recovered record belongs to a saved group")
                    .id
            });
            self.entries.reserve(records.len());
            for record in records {
                let id = self.next_entry_id;
                self.next_entry_id += 1;
                self.push_entry(Entry::from_record(id, target, record));
            }
        }
        self.mark_changed();
        new_group
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
        let mut mapped = Vec::with_capacity(snapshot.groups.len());
        for group in snapshot.groups.iter() {
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
        let mapped_groups: HashMap<Uuid, u64> = mapped
            .iter()
            .map(|group| (group.persistent_id, group.id))
            .collect();
        self.entries.clear();
        self.entries.reserve(snapshot.authenticators.len());
        self.entry_slots.clear();
        self.entry_slots.reserve(snapshot.authenticators.len());
        self.group_rows.clear();
        for entry in snapshot.authenticators.iter() {
            let id = if let Some(&id) = previous_entries.get(&entry.id) {
                id
            } else {
                let id = self.next_entry_id;
                self.next_entry_id += 1;
                id
            };
            self.push_entry(Entry::from_record(
                id,
                mapped_groups[&entry.group_id],
                entry,
            ));
        }
        for rows in self.group_rows.values_mut() {
            rows.sort_unstable_by_key(|&slot| self.entries[slot].record.sort_order);
        }
        if self.groups != mapped {
            self.groups = mapped;
            self.group_token = self.group_token.wrapping_add(1);
        }
        self.mark_changed();
    }
    fn order_span(&self, group: u64, start: usize, end: usize, new_len: usize) -> GroupOrderSpan {
        GroupOrderSpan {
            group,
            start,
            new_len,
            previous: self.rows_in_group(group)[start..end].to_vec(),
        }
    }

    pub fn snapshot_for_group_order(
        &mut self,
        current: &VaultSnapshot,
        change: &GroupOrderEdit,
    ) -> VaultSnapshot {
        let mut candidate = current.clone();
        self.apply_group_order(change, |slot, record| {
            candidate.authenticators[slot] = record.clone()
        });
        candidate
    }

    fn apply_group_order(
        &mut self,
        change: &GroupOrderEdit,
        mut update: impl FnMut(usize, &Authenticator),
    ) {
        for span in &change.spans {
            let Some(rows) = self.group_rows.get(&span.group) else {
                continue;
            };
            let persistent_id = self.persistent_group(span.group).expect("existing group");
            for (order, &slot) in (span.start..).zip(&rows[span.start..span.start + span.new_len]) {
                let entry = &mut self.entries[slot];
                if entry.set_record_order(persistent_id, order) {
                    update(slot, &entry.record);
                }
            }
        }
    }

    pub fn rollback_group_order(&mut self, current: &VaultSnapshot, change: GroupOrderEdit) {
        for span in change.spans {
            let group = span.group;
            for &slot in &span.previous {
                let entry = &mut self.entries[slot];
                let saved = &current.authenticators[slot];
                if entry.group_id != group || entry.record.sort_order != saved.sort_order {
                    entry.group_id = group;
                    entry.record.clone_from(saved);
                }
            }
            let rows = self.group_rows.entry(group).or_default();
            rows.splice(span.start..span.start + span.new_len, span.previous);
            if rows.is_empty() {
                self.group_rows.remove(&group);
            }
        }
        self.mark_changed();
    }

    #[cfg(test)]
    pub fn snapshot(&mut self) -> VaultSnapshot {
        let mut vault = (*self.base).clone();
        vault.groups = snapshot_groups(&self.groups).into();
        for group in &self.groups {
            let Some(rows) = self.group_rows.get(&group.id) else {
                continue;
            };
            for (order, &slot) in rows.iter().enumerate() {
                self.entries[slot].set_record_order(group.persistent_id, order);
            }
        }
        vault.authenticators = self
            .entries
            .iter()
            .map(|entry| entry.record.clone())
            .collect::<Vec<_>>()
            .into();
        vault
    }
    pub fn import_group(&self, id: u64) -> Option<bnauth_core::ImportGroup> {
        self.persistent_group(id)
            .map(|group| bnauth_core::ImportGroup::with_count(group, self.group_count(id)))
    }

    pub fn persistent_group(&self, id: u64) -> Option<Uuid> {
        self.groups
            .iter()
            .find(|g| g.id == id)
            .map(|g| g.persistent_id)
    }
}

fn snapshot_groups(groups: &[Group]) -> Vec<bnauth_core::Group> {
    groups
        .iter()
        .enumerate()
        .map(|(index, group)| bnauth_core::Group {
            id: group.persistent_id,
            name: group.name.clone(),
            sort_order: index as u32,
            is_ungrouped: group.id == UNGROUPED_ID,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn empty_model() -> VaultModel {
        VaultModel::from_snapshot(&VaultSnapshot::new_empty())
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
        assert!(assert_changed(&mut model, |model| model.delete_entries(&[second])));
        assert!(model.entry(first).is_some());
        assert!(model.entry(second).is_none());
        // Empty group deletion must invalidate the cache even though no rows move.
        assert_changed(&mut model, |model| model.delete_group(a)).unwrap();
        assert!(model.delete_group(b).is_err());
    }

    #[test]
    fn no_op_edits_keep_change_token_and_interleaved_rows_stable() {
        let mut model = empty_model();
        let a = model.add_group("  A  ").unwrap();
        let b = model.add_group("B").unwrap();
        let first = model.add_test_entry(" First ", "Note", a).unwrap();
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
        assert!(!model.delete_entries(&[u64::MAX]));
        assert_eq!(model, previous);
    }

    #[test]
    fn loaded_group_names_use_the_same_duplicate_rules_as_storage() {
        for name in ["  Group", "Group  ", "  Group  ", "\u{3000}Group\u{3000}"] {
            let mut vault = VaultSnapshot::new_empty();
            for name in [name, "Other"] {
                let sort_order = vault.groups.len() as u32;
                vault.groups.push(bnauth_core::Group {
                    id: Uuid::new_v4(),
                    name: name.into(),
                    sort_order,
                    is_ungrouped: false,
                });
            }
            vault.validate().unwrap();
            let mut model = VaultModel::from_snapshot(&vault);
            let original = model.clone();
            let other = model.groups[2].id;
            let duplicate = name.to_uppercase();
            assert_eq!(
                model.add_group(&duplicate),
                Err(GroupEditError::DuplicateName)
            );
            assert_eq!(
                model.rename_group(other, &duplicate),
                Err(GroupEditError::DuplicateName)
            );
            let id = model.groups[1].id;
            model.rename_group(id, name).unwrap();
            assert_eq!(model, original);
            let added = model.add_group("group").unwrap();
            assert_eq!(model.group_name(added), Some("group"));
            model.rename_group(other, " Changed ").unwrap();
            assert_eq!(model.group_name(other), Some(" Changed "));
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
        let plan = model.plan_move(selected);
        let counts = plan.counts();
        assert_eq!(model, original);
        assert_eq!(counts[&full], (100, 101));
        for group in &model.groups {
            let (current, after_move) = counts[&group.id];
            assert_eq!(current, model.group_count(group.id));
            let mut moved = model.clone();
            let result = moved.move_entries_from_plan(&plan, group.id);
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
        assert_eq!(model.filtered(0, "  alpha  ", true), vec![last, first]);
        assert_eq!(model.filtered(b, "alpha", false), vec![first]);
        assert_eq!(model.change_token(), token);
        model.move_group(b, -1).unwrap();
        assert_ne!(model.change_token(), token);
        assert_eq!(model.filtered(0, "alpha", true), vec![first, last]);
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
                    &bnauth_core::totp(&model.entry(a).unwrap().record.secret, 1_700_000_000_000)
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
    fn full_group_rejects_moves_and_only_empty_groups_can_be_deleted() {
        let mut model = empty_model();
        let source = model.add_group("來源").unwrap();
        let empty = model.add_group("空組").unwrap();
        for _ in 0..MAX_GROUP_ENTRIES {
            model.add_test_entry("既有", "", UNGROUPED_ID).unwrap();
        }
        let first = model.add_test_entry("驗證器", "", source).unwrap();
        let original = model.clone();
        assert!(model.move_entries(&[first], UNGROUPED_ID).is_err());
        let mut draft = model.group_draft();
        assert!(draft.delete_group(source).is_err());
        assert!(!draft.changed_from(&model));
        draft.delete_group(empty).unwrap();
        let saved = draft.snapshot(&model.snapshot());
        assert_eq!(model, original);
        model.acknowledge_groups(draft);
        assert_eq!(model.snapshot().groups, saved.groups);
        assert_eq!(model.entries, original.entries);
        assert_eq!(model.group_count(source), 1);
        assert_eq!(model.group_count(UNGROUPED_ID), MAX_GROUP_ENTRIES);
        assert!(model.group_name(empty).is_none());
    }

    #[test]
    fn copy_move_and_group_deletion_keep_same_key_records_independent() {
        let mut model = empty_model();
        let source = model.add_group("來源").unwrap();
        let other = model.add_group("其他").unwrap();
        let original = model.add_test_entry("來源項目", "原備註", source).unwrap();
        let first = model.add_test_entry("既有項目", "", UNGROUPED_ID).unwrap();
        let copy = model.copy_entry_at(0, UNGROUPED_ID).unwrap();
        assert_eq!(model.filtered(UNGROUPED_ID, "", false), [first, copy]);
        model.update_entry(copy, "副本", "新備註").unwrap();
        assert_eq!(model.entry(original).unwrap().record.name, "來源項目");
        assert_eq!(model.entry(original).unwrap().record.description, "原備註");
        model.snapshot().validate().unwrap();

        let before = model.clone();
        assert_eq!(model.group_count(source), 1);
        assert_eq!(model.group_count(UNGROUPED_ID), 2);
        assert_eq!(model.group_count(other), 0);
        assert!(model.copy_entry_at(0, source).is_err());
        assert_eq!(model, before);

        let second_copy = model.copy_entry_at(0, UNGROUPED_ID).unwrap();
        assert_eq!(
            model.filtered(UNGROUPED_ID, "", false),
            [first, copy, second_copy]
        );
        assert_eq!(model.move_entries(&[original, copy], other), Ok(2));
        assert_eq!(model.filtered(other, "", false), [copy, original]);
        assert_eq!(model.move_entries(&[original], UNGROUPED_ID), Ok(1));
        model.move_entries(&[copy], UNGROUPED_ID).unwrap();
        model.delete_group(other).unwrap();
        assert_eq!(
            model.filtered(UNGROUPED_ID, "", false),
            [first, second_copy, original, copy]
        );
        assert_eq!(model.entry(copy).unwrap().record.description, "新備註");
        model.snapshot().validate().unwrap();
        assert!(model.delete_entries(&[copy]));
        assert_eq!(
            model.filtered(UNGROUPED_ID, "", false),
            [first, second_copy, original]
        );
        model.snapshot().validate().unwrap();
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
        assert_eq!(
            model.copy_entry_at(100, 0).unwrap_err(),
            "目標分組已達容量上限。"
        );
        assert_eq!(
            model.copy_entry_at(0, 10).unwrap_err(),
            "資料庫已達容量上限。"
        );
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
        model.move_group(b, -1).unwrap();
        model.move_group(a, 1).unwrap();
        assert!(model.move_group(0, 1).is_err());
        assert!(model.rename_group(0, "改名").is_err());
        assert!(model.delete_group(0).is_err());
        assert_eq!(model, original);
    }
}

#[cfg(test)]
#[path = "snapshot_bridge_tests.rs"]
mod snapshot_bridge_tests;
