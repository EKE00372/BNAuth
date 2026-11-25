use crate::model::{Entry, GroupDraft, GroupEditError, GroupOrderEdit, MovePlan, VaultModel};
use crate::runtime::ListEdit;
use bnauth_core::READ_ONLY_UPDATE_NOTICE;
#[path = "app_runtime.rs"]
mod app_runtime;
#[path = "delete_confirmation.rs"]
mod delete_confirmation;
use delete_confirmation::DeleteConfirmation;
#[path = "clock_persistence.rs"]
mod clock_persistence;
#[path = "email_recovery.rs"]
pub(crate) mod email_recovery;
#[path = "file_export.rs"]
mod file_export;
#[path = "full_restore.rs"]
pub(crate) mod full_restore;
#[path = "group_import.rs"]
mod group_import;
#[path = "limited_edit.rs"]
mod limited_edit;
#[path = "text_preview.rs"]
mod text_preview;
use eframe::egui::{self, Align2, Color32, FontId, Pos2, Rect, Sense, Stroke, Vec2};
use std::{
    collections::BTreeSet,
    sync::Arc,
    time::{Duration, Instant},
};

const BG: Color32 = Color32::from_rgb(16, 24, 35);
const PANEL: Color32 = Color32::from_rgb(22, 34, 49);
const ROW_A: Color32 = Color32::from_rgb(25, 39, 56);
const ROW_B: Color32 = Color32::from_rgb(32, 49, 71);
const HOVER: Color32 = Color32::from_rgb(41, 63, 87);
const SELECTED_A: Color32 = Color32::from_rgb(25, 60, 93);
const SELECTED_B: Color32 = Color32::from_rgb(32, 72, 107);
const INK: Color32 = Color32::from_rgb(231, 237, 245);
const NAME_COLOR: Color32 = Color32::from_rgb(140, 200, 255);
const MUTED: Color32 = Color32::from_rgb(154, 170, 192);
const LINE: Color32 = Color32::from_rgb(51, 70, 94);
const BLUE: Color32 = Color32::from_rgb(23, 99, 167);
const ACCENT: Color32 = Color32::from_rgb(71, 168, 245);
const TINT: Color32 = Color32::from_rgb(36, 70, 99);
const RED: Color32 = Color32::from_rgb(248, 132, 137);
const DANGER: Color32 = Color32::from_rgb(160, 48, 58);
const ON_ACCENT: Color32 = Color32::WHITE;
const ROW_HEIGHT: f32 = 68.0;
const FOOTER_HEIGHT: f32 = 44.0;
const TEXT_INPUT_LIMIT: usize = 256;
const EDIT_ENTRY_TITLE: &str = "編輯名稱與備註";
const ENTRY_LIMIT_NOTICE: &str = "資料庫已達 1,000 筆驗證器上限。";

#[cfg(test)]
#[path = "group_ui_tests.rs"]
mod group_ui_tests;

#[cfg(test)]
#[path = "tab_ui_tests.rs"]
mod tab_ui_tests;

#[derive(Clone, Copy)]
enum Icon {
    Copy,
    Clock,
    More,
}

struct GroupRename {
    id: u64,
    name: String,
    focus: bool,
    error: GroupNameFeedback,
}

enum GroupAction {
    Rename(u64),
    Move(u64, isize),
    Delete(u64),
    Add,
}

#[derive(Clone, Copy, Debug, Hash)]
enum DialogEditor {
    ImportName,
    ImportDescription,
    ImportSecret,
    ImportSerial,
    ImportRestore,
    RecoveryEmail,
    RecoverySerial,
    RecoveryCode,
    RecoveryName,
    RecoveryDescription,
    EditName,
    EditDescription,
    NewGroupName,
    GroupRename,
}

impl DialogEditor {
    fn id(self) -> egui::Id {
        egui::Id::unique(("dialog_editor", self))
    }

    fn reset(self, ctx: &egui::Context) {
        ctx.data_mut(|data| data.remove::<egui::text_edit::TextEditState>(self.id()));
        limited_edit::reset(ctx, self.id());
        ctx.memory_mut(|memory| memory.surrender_focus(self.id()));
    }
}

enum Dialog {
    Import(Box<crate::runtime::ImportForm>),
    EmailRecovery(Box<email_recovery::Form>),
    GroupImport(Box<crate::runtime::GroupImportForm>),
    FullRestore(Box<full_restore::RestoreForm>),
    Edit {
        id: u64,
        name: String,
        description: String,
        error: String,
    },
    Details {
        record: bnauth_core::Authenticator,
        group_name: String,
    },
    Export(Box<file_export::ExportForm>),
    Delete(DeleteConfirmation),
    Groups {
        draft: Box<GroupDraft>,
        name: String,
        rename: Option<GroupRename>,
        error: GroupNameFeedback,
    },
    Info,
}

impl Dialog {
    fn clear_editors(&self, ctx: &egui::Context) {
        use DialogEditor::*;
        let fields: &[DialogEditor] = match self {
            Self::Import(_) => &[
                ImportName,
                ImportDescription,
                ImportSecret,
                ImportSerial,
                ImportRestore,
            ],
            Self::EmailRecovery(_) => &[
                RecoveryEmail,
                RecoverySerial,
                RecoveryCode,
                RecoveryName,
                RecoveryDescription,
            ],
            Self::Edit { .. } => &[EditName, EditDescription],
            Self::Groups { .. } => &[NewGroupName, GroupRename],
            Self::Details { .. }
            | Self::Info
            | Self::Delete(_)
            | Self::Export(_)
            | Self::GroupImport(_)
            | Self::FullRestore(_) => &[],
        };
        for field in fields {
            field.reset(ctx);
        }
    }

    fn details(model: &VaultModel, entry: &Entry) -> Self {
        Self::Details {
            record: entry.record.clone(),
            group_name: model
                .group_name(entry.group_id)
                .unwrap_or_default()
                .to_owned(),
        }
    }

    fn groups(model: &VaultModel) -> Self {
        Self::Groups {
            draft: Box::new(model.group_draft()),
            name: String::new(),
            rename: None,
            error: GroupNameFeedback::default(),
        }
    }

    fn delete(ids: Vec<u64>) -> Self {
        Self::Delete(DeleteConfirmation::new(ids))
    }

    fn requires_writable_settings(&self) -> bool {
        !matches!(self, Self::Details { .. } | Self::Export(_) | Self::Info)
    }
}

#[derive(Default)]
struct EntryProjection {
    token: Option<u64>,
    group: Option<u64>,
    query: String,
    folded_query: String,
    ids: Vec<u64>,
    // Search results occupy consecutive runs in tab order.
    group_hits: std::collections::HashMap<u64, usize>,
    reset_search_scroll: bool,
    notes: text_preview::NotePreviews,
}

impl EntryProjection {
    fn is_current(&self, model: &VaultModel, group: u64, query: &str) -> bool {
        self.token == Some(model.projection_token())
            && self.group == query.trim().is_empty().then_some(group)
            && self.query == query.trim()
    }

    fn position(&self, model: &VaultModel, row: usize) -> Result<usize, usize> {
        let key = |entry: &Entry| {
            let group = model
                .groups
                .iter()
                .position(|group| group.id == entry.group_id)
                .expect("displayed row belongs to a group");
            (group, entry.record.sort_order)
        };
        self.ids
            .binary_search_by_key(&key(&model.entries[row]), |&id| {
                key(&model.entries[model.entry_slot(id)])
            })
    }

    /// Only this row's name or membership can have changed.
    fn update_row(&mut self, model: &VaultModel, row: usize, visible: bool) -> bool {
        let id = model.entries[row].id;
        match (self.position(model, row), visible) {
            (Ok(index), false) => {
                self.ids.remove(index);
                *self
                    .group_hits
                    .get_mut(&model.entries[row].group_id)
                    .expect("visible search result") -= 1;
                true
            }
            (Err(index), true) => {
                self.ids.insert(index, id);
                *self
                    .group_hits
                    .entry(model.entries[row].group_id)
                    .or_default() += 1;
                true
            }
            _ => false,
        }
    }

    fn append_row(&mut self, model: &VaultModel, row: usize) -> bool {
        let entry = &model.entries[row];
        if let Some(group) = self.group {
            if group != entry.group_id {
                return false;
            }
            self.ids.push(entry.id);
            return true;
        }
        crate::model::name_matches(&entry.record.name, &self.folded_query)
            && self.update_row(model, row, true)
    }

    fn remove_rows(&mut self, model: &VaultModel, rows: &[usize]) {
        let mut positions: Vec<_> = rows
            .iter()
            .filter_map(|&row| {
                let entry = &model.entries[row];
                match self.group {
                    Some(group) => {
                        (entry.group_id == group).then_some(entry.record.sort_order as usize)
                    }
                    None => {
                        let index = self.position(model, row).ok()?;
                        *self
                            .group_hits
                            .get_mut(&entry.group_id)
                            .expect("visible search result") -= 1;
                        Some(index)
                    }
                }
            })
            .collect();
        positions.sort_unstable();
        if let (Some(&first), Some(&last)) = (positions.first(), positions.last()) {
            let mut removed = positions.into_iter().peekable();
            let mut index = first;
            self.ids
                .extract_if(first..=last, |_| {
                    let remove = removed.next_if_eq(&index).is_some();
                    index += 1;
                    remove
                })
                .for_each(drop);
        }
    }

    fn retain_existing(&mut self, model: &VaultModel) {
        self.group_hits.clear();
        self.ids.retain(|id| {
            let Some(entry) = model.entry(*id) else {
                return false;
            };
            if self.group.is_none() {
                *self.group_hits.entry(entry.group_id).or_default() += 1;
            }
            true
        });
    }

    fn reorder_tabs(&mut self, model: &VaultModel, draft: &GroupDraft) {
        if self.group.is_some() || self.ids.len() < 2 {
            return;
        }
        let visible = |id| self.group_hits.get(&id).copied().unwrap_or(0) > 0;
        let before = model.groups.iter().map(|g| g.id).filter(|&id| visible(id));
        let after = draft.groups.iter().map(|g| g.id).filter(|&id| visible(id));
        if before.clone().eq(after.clone()) {
            return;
        }
        let mut start = 0;
        let ranges: std::collections::HashMap<_, _> = before
            .map(|id| {
                let end = start + self.group_hits[&id];
                let range = start..end;
                start = end;
                (id, range)
            })
            .collect();
        let mut offset = 0;
        let mut changed = Vec::new();
        for id in after {
            let old = ranges[&id].clone();
            let new = offset..offset + old.len();
            if old != new {
                changed.push((new, self.ids[old.clone()].to_vec()));
            }
            offset += old.len();
        }
        for (new, ids) in changed {
            self.ids[new].copy_from_slice(&ids);
        }
    }

    /// Row order changes, but search membership does not. Only relocate matches in edited spans.
    fn change_order(&mut self, model: &VaultModel, change: &GroupOrderEdit, forward: bool) {
        if let Some(group) = self.group {
            if let Some(span) = change.spans.iter().find(|span| span.group == group) {
                let old_len = if forward {
                    span.previous.len()
                } else {
                    span.new_len
                };
                self.ids.splice(
                    span.start..span.start + old_len,
                    span.rows_after(model, forward)
                        .iter()
                        .map(|&row| model.entries[row].id),
                );
            }
            return;
        }
        if self.ids.is_empty() {
            return;
        }
        let mut ranges = Vec::with_capacity(change.spans.len());
        let mut matched = std::collections::HashSet::new();
        let mut offset = 0;
        for group in &model.groups {
            let count = self.group_hits.get(&group.id).copied().unwrap_or(0);
            if let Some(span) = change.spans.iter().find(|span| span.group == group.id) {
                let old_len = if forward {
                    span.previous.len()
                } else {
                    span.new_len
                };
                let ids = &self.ids[offset..offset + count];
                let position = |order| {
                    ids.partition_point(|&id| {
                        model.entries[model.entry_slot(id)].record.sort_order < order
                    })
                };
                let start = offset + position(span.start as u32);
                let end = offset + position((span.start + old_len) as u32);
                matched.extend(self.ids[start..end].iter().copied());
                ranges.push((span, start..end));
            }
            offset += count;
        }
        if matched.is_empty() {
            return;
        }
        for (span, old) in ranges.into_iter().rev() {
            let rows = span.rows_after(model, forward);
            let ids: Vec<_> = rows
                .iter()
                .map(|&row| model.entries[row].id)
                .filter(|id| matched.contains(id))
                .collect();
            if self.ids[old.clone()] != ids {
                let count = self.group_hits.entry(span.group).or_default();
                *count = *count - old.len() + ids.len();
                self.ids.splice(old, ids);
            }
        }
    }
}

#[derive(Default)]
struct RowSelection {
    ids: BTreeSet<u64>,
    move_plan: Option<MovePlan>,
    all_visible: Option<bool>,
}

impl RowSelection {
    fn includes_all(&mut self, mut visible: impl ExactSizeIterator<Item = u64>) -> bool {
        *self.all_visible.get_or_insert_with(|| {
            visible.len() > 0
                && self.ids.len() >= visible.len()
                && visible.all(|id| self.ids.contains(&id))
        })
    }

    fn move_plan(&mut self, model: &VaultModel) -> &MovePlan {
        if let Some(plan) = &self.move_plan
            && !plan.is_current(model)
        {
            self.move_plan = None;
        }
        self.move_plan
            .get_or_insert_with(|| model.plan_move(self.ids.iter().copied()))
    }
}

impl std::ops::Deref for RowSelection {
    type Target = BTreeSet<u64>;

    fn deref(&self) -> &Self::Target {
        &self.ids
    }
}

impl std::ops::DerefMut for RowSelection {
    fn deref_mut(&mut self) -> &mut Self::Target {
        self.move_plan = None;
        self.all_visible = None;
        &mut self.ids
    }
}

enum DropTarget {
    Group { id: u64, after_move: usize },
    GroupSlot(usize),
}

struct TabLabel {
    galley: Arc<egui::Galley>,
    width: f32,
}

#[derive(Default)]
struct TabLabels {
    token: Option<u64>,
    title_font: Option<Arc<egui::Galley>>,
    labels: Vec<(String, TabLabel)>,
    total_width: f32,
}

impl TabLabels {
    fn observe_font(&mut self, title: Arc<egui::Galley>) {
        // The title is laid out on every page, including while saving. A fresh
        // title layout signals a font atlas or scale change even across pages.
        if !self
            .title_font
            .as_ref()
            .is_some_and(|old| Arc::ptr_eq(old, &title))
        {
            self.token = None;
            self.labels.clear();
            self.title_font = Some(title);
        }
    }

    fn update(&mut self, ui: &egui::Ui, model: &VaultModel) {
        const MAX_TEXT_WIDTH: f32 = 104.0;
        let token = model.group_token();
        if self.token == Some(token) {
            return;
        }
        self.token = Some(token);
        let mut previous: std::collections::HashMap<_, _> = self.labels.drain(..).collect();
        self.total_width = 0.0;
        for group in &model.groups {
            let (name, label) = previous.remove_entry(&group.name).unwrap_or_else(|| {
                let galley = single_line(
                    ui.painter(),
                    &group.name,
                    13.0,
                    Color32::PLACEHOLDER,
                    MAX_TEXT_WIDTH,
                );
                let width = if galley.elided {
                    MAX_TEXT_WIDTH
                } else {
                    galley.size().x.min(MAX_TEXT_WIDTH)
                } + 12.0;
                (group.name.clone(), TabLabel { galley, width })
            });
            self.total_width += label.width;
            self.labels.push((name, label));
        }
    }
}

pub struct BnAuthApp {
    window_chrome: crate::window_chrome::WindowChrome,
    pub(crate) model: VaultModel,
    group: u64,
    tab_reveal: Option<u64>,
    tab_scroll: Option<f32>,
    tab_labels: TabLabels,
    query: String,
    selected: RowSelection,
    dialog: Option<Dialog>,
    notifications: crate::notifications::Notifications,
    drag_origin: Option<u64>,
    dragging: Option<usize>,
    drop: Option<DropTarget>,
    runtime: crate::runtime::RuntimeState,
    otp_cache: crate::otp_cache::OtpCache,
    projection: EntryProjection,
}

impl BnAuthApp {
    pub fn new(cc: &eframe::CreationContext<'_>) -> Self {
        let mut fonts = egui::FontDefinitions::empty();
        fonts.font_data.insert(
            "Nowar Sans TW".into(),
            egui::FontData::from_static(include_bytes!("../assets/fonts/NowarSansTW-Regular.ttf"))
                .into(),
        );
        fonts
            .families
            .get_mut(&egui::FontFamily::Proportional)
            .unwrap()
            .push("Nowar Sans TW".into());
        fonts
            .families
            .get_mut(&egui::FontFamily::Monospace)
            .unwrap()
            .push("Nowar Sans TW".into());
        cc.egui_ctx.set_fonts(fonts);
        cc.egui_ctx.set_theme(egui::Theme::Dark);
        let mut style = (*cc.egui_ctx.global_style()).clone();
        style.visuals = egui::Visuals::dark();
        style.visuals.override_text_color = Some(INK);
        style.visuals.panel_fill = BG;
        style.visuals.window_fill = PANEL;
        style.visuals.window_stroke = Stroke::new(1.0, LINE);
        style.visuals.extreme_bg_color = BG;
        style.visuals.faint_bg_color = ROW_B;
        style.visuals.window_corner_radius = egui::CornerRadius::ZERO;
        style.visuals.menu_corner_radius = egui::CornerRadius::ZERO;
        style.visuals.selection.bg_fill = TINT;
        style.visuals.selection.stroke = Stroke::new(1.0, INK);
        for w in [
            &mut style.visuals.widgets.inactive,
            &mut style.visuals.widgets.hovered,
            &mut style.visuals.widgets.active,
            &mut style.visuals.widgets.noninteractive,
            &mut style.visuals.widgets.open,
        ] {
            w.corner_radius = egui::CornerRadius::ZERO;
            w.bg_fill = PANEL;
            w.weak_bg_fill = PANEL;
            w.bg_stroke = Stroke::new(1.0, LINE);
            w.fg_stroke = Stroke::new(1.0, INK);
        }
        style.visuals.widgets.hovered.bg_fill = HOVER;
        style.visuals.widgets.hovered.weak_bg_fill = HOVER;
        style.visuals.widgets.hovered.bg_stroke = Stroke::new(1.0, ACCENT);
        style.visuals.widgets.active.bg_fill = TINT;
        style.visuals.widgets.active.weak_bg_fill = TINT;
        style.visuals.widgets.active.bg_stroke = Stroke::new(1.0, ACCENT);
        style.visuals.widgets.open.bg_fill = TINT;
        style.visuals.widgets.open.weak_bg_fill = TINT;
        style.visuals.widgets.noninteractive.fg_stroke = Stroke::new(1.0, MUTED);
        style.spacing.button_padding = Vec2::new(10.0, 7.0);
        style.spacing.item_spacing = Vec2::new(6.0, 6.0);
        style
            .text_styles
            .insert(egui::TextStyle::Body, FontId::proportional(14.0));
        style
            .text_styles
            .insert(egui::TextStyle::Button, FontId::proportional(13.0));
        cc.egui_ctx.set_global_style(style);
        Self {
            window_chrome: crate::window_chrome::WindowChrome::default(),
            model: VaultModel::from_snapshot(&bnauth_core::VaultSnapshot::new_empty()),
            group: 0,
            tab_reveal: Some(0),
            tab_scroll: None,
            tab_labels: TabLabels::default(),
            query: String::new(),
            selected: RowSelection::default(),
            dialog: None,
            notifications: crate::notifications::Notifications::default(),
            drag_origin: None,
            dragging: None,
            drop: None,
            runtime: crate::runtime::RuntimeState::new(cc.egui_ctx.clone()),
            otp_cache: crate::otp_cache::OtpCache::default(),
            projection: EntryProjection::default(),
        }
    }
    fn notify(&mut self, text: impl Into<String>) {
        self.notifications.status(text);
    }
    fn warn(&mut self, text: impl Into<String>) {
        self.notifications.warning(text);
    }
    fn searching(&self) -> bool {
        !self.query.trim().is_empty()
    }
    fn refresh_projection(&mut self) {
        if !self
            .projection
            .is_current(&self.model, self.group, &self.query)
        {
            let searching = self.searching();
            let query = self.query.trim();
            if self.projection.query != query {
                let folded = query.to_lowercase();
                query.clone_into(&mut self.projection.query);
                if self.projection.folded_query == folded
                    && self.projection.is_current(&self.model, self.group, query)
                {
                    return;
                }
                self.projection.reset_search_scroll |= self.projection.folded_query != folded;
                self.projection.folded_query = folded;
            }
            self.selected.all_visible = None;
            self.projection.group_hits.clear();
            self.projection.ids = self.model.filtered_with_counts(
                self.group,
                &self.projection.folded_query,
                searching,
                |id, count| {
                    self.projection.group_hits.insert(id, count);
                },
            );
            self.projection.token = Some(self.model.projection_token());
            self.projection.group = (!searching).then_some(self.group);
        }
    }
    fn select_all_visible(&mut self) {
        self.refresh_projection();
        if self.selected.len() == self.projection.ids.len()
            && (self.selected.is_empty()
                || self
                    .selected
                    .includes_all(self.projection.ids.iter().copied()))
        {
            return;
        }
        *self.selected = self.projection.ids.iter().copied().collect();
        self.selected.all_visible = Some(!self.selected.is_empty());
    }
    #[cfg(test)]
    fn visible_ids(&mut self) -> Vec<u64> {
        self.refresh_projection();
        self.projection.ids.clone()
    }
    fn open_dialog(&mut self, ctx: &egui::Context, dialog: Dialog) {
        if !self.runtime.app.repository_state.allows_writes() && dialog.requires_writable_settings() {
            return;
        }
        self.cancel_drag();
        if let Some(previous) = self.dialog.replace(dialog) {
            previous.clear_editors(ctx);
        }
    }
    fn edit(&mut self, ctx: &egui::Context, id: Option<u64>) {
        let Some(id) = id else {
            self.open_dialog(
                ctx,
                Dialog::Import(Box::new(crate::runtime::ImportForm::new(self.group))),
            );
            return;
        };
        let Some(entry) = self.model.entry(id) else {
            self.notify("找不到驗證器。");
            return;
        };
        let dialog = Dialog::Edit {
            id,
            name: entry.record.name.clone(),
            description: entry.record.description.clone(),
            error: String::new(),
        };
        self.open_dialog(ctx, dialog);
    }
    fn save_entry_edit(
        &mut self,
        id: u64,
        name: &mut String,
        description: &mut String,
    ) -> Result<(), String> {
        let Some(row) = self.model.check_entry_edit(id, name, description)? else {
            return Ok(());
        };
        self.save_list_edit(|_, _, current| {
            let mut candidate = current.clone();
            candidate.authenticators[row] = current.authenticators[row]
                .with_text(std::mem::take(name), std::mem::take(description));
            (candidate, ListEdit::EditEntry(row))
        })
    }
    fn save_list_edit(
        &mut self,
        prepare: impl FnOnce(
            &mut VaultModel,
            &mut EntryProjection,
            &bnauth_core::VaultSnapshot,
        ) -> (bnauth_core::VaultSnapshot, ListEdit),
    ) -> Result<(), String> {
        let runtime = &mut self.runtime;
        let current = Arc::clone(runtime.app.current.as_ref().expect("editing an open vault"));
        let mut submitted = None;
        let result = runtime.commit_prepared_with_pending_clock(None, || {
            let (candidate, edit) = prepare(&mut self.model, &mut self.projection, &current);
            submitted = Some(edit);
            candidate
        });
        let Some(edit) = submitted else {
            return result;
        };
        runtime.pending_list_edit = Some(edit);
        runtime.app.error = result.err();
        self.notifications.clear_status();
        Ok(())
    }
    fn switch_group(&mut self, group: u64) {
        self.group = group;
        self.tab_reveal = Some(group);
        self.selected.clear();
        self.cancel_drag();
    }
    fn cancel_drag(&mut self) {
        self.drag_origin = None;
        self.dragging = None;
        self.drop = None;
    }
    fn dragged_entry(&self) -> Option<&Entry> {
        self.model
            .entries
            .get(self.dragging?)
            .filter(|entry| Some(entry.id) == self.drag_origin)
    }
    fn finish_group_order(
        &mut self,
        result: Result<Option<GroupOrderEdit>, String>,
        edit: fn(GroupOrderEdit) -> ListEdit,
        ctx: &egui::Context,
    ) {
        let change = match result {
            Ok(Some(change)) => change,
            Ok(None) => return,
            Err(error) => return self.notify(error),
        };
        // Keep the original display order until the worker accepts this submission.
        if change.group_count_changed(self.projection.group) {
            self.selected.all_visible = None;
        }
        let mut change = Some(change);
        let result = self.save_list_edit(|model, projection, current| {
            let change = change.take().expect("prepare an order change once");
            if projection.token == Some(change.previous_token) {
                projection.change_order(model, &change, true);
                projection.token = Some(model.projection_token());
            }
            let candidate = model.snapshot_for_group_order(current, &change);
            (candidate, edit(change))
        });
        if let Err(error) = result {
            if let Some(change) = change {
                self.rollback_list_edit(edit(change));
            }
            self.notify(error);
        }
        if self.runtime.close_requested {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
    }
    fn rollback_list_edit(&mut self, edit: ListEdit) {
        if let ListEdit::Move(change) | ListEdit::Reorder(change) = edit
            && let Some(snapshot) = &self.runtime.app.current
        {
            let current = self.projection.token == Some(self.model.projection_token());
            if current {
                self.projection.change_order(&self.model, &change, false);
                if change.group_count_changed(self.projection.group) {
                    self.selected.all_visible = None;
                }
            }
            let original_token = change.previous_token;
            self.model.rollback_group_order(snapshot, change);
            if current || self.projection.token == Some(original_token) {
                self.projection.token = Some(self.model.projection_token());
            }
        }
    }
    fn move_disabled_reason(&self) -> Option<&'static str> {
        if !self.runtime.app.repository_state.allows_writes() {
            Some(READ_ONLY_UPDATE_NOTICE)
        } else if self.model.groups.len() < 2 {
            Some("目前沒有其他分組。")
        } else {
            None
        }
    }

    fn group_menu(&mut self, ui: &mut egui::Ui, row: Option<usize>) {
        enum Source<'a> {
            Row(usize),
            Batch(&'a MovePlan),
        }
        let source = match row {
            Some(index) => Source::Row(index),
            None => Source::Batch(self.selected.move_plan(&self.model)),
        };
        ui.set_min_width(190.0);
        let target = egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                for group in &self.model.groups {
                    let (count, after_move) = match source {
                        Source::Row(index) => {
                            let count = self.model.group_count(group.id);
                            (
                                count,
                                count + usize::from(self.model.entries[index].group_id != group.id),
                            )
                        }
                        Source::Batch(plan) => plan.counts()[&group.id],
                    };
                    if after_move == count {
                        continue;
                    }
                    let reason = (after_move > crate::model::MAX_GROUP_ENTRIES)
                        .then_some("目標分組的剩餘容量不足。");
                    if group_target_button(ui, &group.name, count, reason).clicked() {
                        ui.close();
                        return Some((group.id, after_move));
                    }
                }
                None
            })
            .inner;
        if let Some((target, after_move)) = target {
            let result = match source {
                Source::Row(index) => self.model.move_entry_at(index, target, after_move),
                Source::Batch(plan) => self.model.move_entries_from_plan(plan, target),
            };
            self.finish_group_order(result, ListEdit::Move, ui.ctx());
        }
    }
    fn copy_group_menu(&mut self, ui: &mut egui::Ui, index: usize) {
        let source = self.model.entries[index].group_id;
        ui.set_min_width(190.0);
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                for group in &self.model.groups {
                    if group.id == source {
                        continue;
                    }
                    let count = self.model.group_count(group.id);
                    let reason = self.model.copy_target_reason(source, group.id, count);
                    if group_target_button(ui, &group.name, count, reason).clicked() {
                        let target = group.id;
                        let result = self.model.prepare_copy(index, target).and_then(|copy| {
                            self.save_list_edit(|_, _, current| {
                                let mut candidate = current.clone();
                                candidate.authenticators.append_items(std::iter::once(copy));
                                (candidate, ListEdit::Copy(target))
                            })
                        });
                        if let Err(error) = result {
                            self.notify(error);
                        }
                        ui.close();
                        break;
                    }
                }
            });
    }
    fn tab_context_menu(&mut self, tab: &egui::Response, group: u64, writable: bool) {
        let ctx = &tab.ctx;
        let mut popup = egui::Popup::context_menu(tab);
        let open_by_key = self.dialog.is_none()
            && tab.has_focus()
            && ctx.input_mut(|input| {
                input.consume_key(egui::Modifiers::SHIFT, egui::Key::F10)
                    || input.consume_key(egui::Modifiers::NONE, egui::Key::Menu)
            });
        if open_by_key {
            egui::Popup::open_id(ctx, popup.get_id());
        }
        // Pointer-opened context menus store their original click position.
        // A keyboard-opened menu follows its tab until dismissed.
        let keyboard_menu = popup.is_open()
            && !tab.secondary_clicked()
            && egui::Popup::position_of_id(ctx, popup.get_id()).is_none();
        if keyboard_menu {
            popup = popup.anchor(tab);
        }

        popup.show(|ui| {
            let manage = ui.add_enabled(writable, egui::Button::new("管理分組"));
            let has_entries = self.model.group_count(group) > 0;
            let export = ui.add_enabled(has_entries, egui::Button::new("匯出此分組"));
            if open_by_key && (writable || has_entries) {
                if writable { &manage } else { &export }.request_focus();
            }
            if manage.clicked() {
                self.open_dialog(ctx, Dialog::groups(&self.model));
                ui.close();
            }
            if export.clicked() {
                self.open_group_export(ctx, group);
                ui.close();
            }
            self.group_import_menu(ui);
            ui.separator();
            let backup = ui.button("備份完整資料庫");
            if open_by_key && !writable && !has_entries {
                backup.request_focus();
            }
            if backup.clicked() {
                self.open_full_backup(ctx);
                ui.close();
            }
            if ui
                .add_enabled(writable, egui::Button::new(full_restore::TITLE))
                .clicked()
            {
                self.open_full_restore(ctx);
                ui.close();
            }
        });
        if keyboard_menu
            && ctx.input_mut(|input| input.consume_key(egui::Modifiers::NONE, egui::Key::Escape))
        {
            tab.request_focus();
        }
    }

    fn tabs(&mut self, ui: &mut egui::Ui) -> bool {
        let writable = self.runtime.app.repository_state.allows_writes();
        let mut tab_focused = false;
        egui::Frame::NONE
            .fill(PANEL)
            .stroke(Stroke::new(1.0, LINE))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    self.tab_labels.update(ui, &self.model);
                    let available = (ui.available_width() - 38.0).max(0.0);
                    let overflow = self.tab_labels.total_width > available + 0.5;
                    let width = (available - if overflow { 38.0 } else { 0.0 }).max(1.0);
                    let reveal = self.tab_reveal.take();
                    let pending_scroll = self.tab_scroll.take();
                    let output = ui
                        .push_id("group_tabs", |ui| {
                            ui.allocate_ui_with_layout(
                                Vec2::new(width, 36.0),
                                egui::Layout::top_down(egui::Align::Min),
                                |ui| {
                                    ui.style_mut().always_scroll_the_only_direction = true;
                                    let mut scroll = egui::ScrollArea::horizontal()
                                        .id_salt("tabs")
                                        .max_width(width)
                                        .min_scrolled_width(0.0)
                                        .auto_shrink([false, true])
                                        .animated(false)
                                        .scroll_bar_visibility(
                                            egui::scroll_area::ScrollBarVisibility::AlwaysHidden,
                                        )
                                        .scroll_source(egui::scroll_area::ScrollSource {
                                            drag: egui::scroll_area::DragScroll::Never,
                                            ..Default::default()
                                        });
                                    if let Some(offset) = pending_scroll {
                                        scroll = scroll.horizontal_scroll_offset(offset);
                                    }
                                    scroll.show(ui, |ui| {
                                        ui.horizontal(|ui| {
                                            ui.spacing_mut().item_spacing.x = 0.0;
                                            for index in 0..self.model.groups.len() {
                                                let group = self.model.groups[index].id;
                                                ui.push_id(group, |ui| {
                                                    let (_, TabLabel { galley: name, width: tab_width }) = &self.tab_labels.labels[index];
                                                    let active = self.group == group;
                                                    let (rect, response) = ui.allocate_exact_size(
                                                        Vec2::new(*tab_width, 36.0),
                                                        Sense::click(),
                                                    );
                                                    response.widget_info(|| {
                                                        egui::WidgetInfo::selected(
                                                            egui::Role::Tab,
                                                            ui.is_enabled(),
                                                            active,
                                                            &self.model.groups[index].name,
                                                        )
                                                    });
                                                    let color = if active { ACCENT } else { MUTED };
                                                    ui.painter().rect_filled(
                                                        rect,
                                                        0,
                                                        if response.hovered() {
                                                            HOVER
                                                        } else if active {
                                                            TINT
                                                        } else if index % 2 == 0 {
                                                            ROW_A
                                                        } else {
                                                            ROW_B
                                                        },
                                                    );
                                                    ui.painter().galley(
                                                        Pos2::new(
                                                            rect.left() + 6.0,
                                                            rect.center().y - name.size().y / 2.0,
                                                        ),
                                                        Arc::clone(name),
                                                        color,
                                                    );

                                                    if active {
                                                        ui.painter().line_segment(
                                                            [
                                                                rect.left_bottom(),
                                                                rect.right_bottom(),
                                                            ],
                                                            Stroke::new(2.0, ACCENT),
                                                        );
                                                    }
                                                    if response.has_focus() {
                                                        tab_focused = true;
                                                        ui.painter().rect_stroke(
                                                            rect.shrink(1.0),
                                                            0,
                                                            Stroke::new(1.0, ACCENT),
                                                            egui::StrokeKind::Inside,
                                                        );
                                                    }
                                                    if reveal == Some(group)
                                                        || response.gained_focus()
                                                    {
                                                        response.scroll_to_me(None);
                                                    }
                                                    if response.clicked() {
                                                        if self.group != group || !self.selected.is_empty() {
                                                            // The search controls above already drew the previous selection.
                                                            ui.ctx().request_discard("refresh selection controls after tab click");
                                                        }
                                                        self.switch_group(group);
                                                    }
                                                    if response.contains_pointer()
                                                        && self.dragged_entry().is_some_and(
                                                            |entry| entry.group_id != group,
                                                        )
                                                    {
                                                        let after_move = self.model.group_count(group) + 1;
                                                        if after_move > crate::model::MAX_GROUP_ENTRIES {
                                                            ui.ctx().set_cursor_icon(egui::CursorIcon::NoDrop);
                                                        } else {
                                                            self.drop = Some(DropTarget::Group { id: group, after_move });
                                                            ui.painter().rect_stroke(
                                                                rect.shrink(1.0),
                                                                0,
                                                                Stroke::new(2.0, ACCENT),
                                                                egui::StrokeKind::Inside,
                                                            );
                                                        }
                                                    }
                                                    let response = dynamic_hover_text(response, || self.model.groups[index].name.clone());
                                                    self.tab_context_menu(
                                                        &response, group, writable,
                                                    );
                                                });
                                            }
                                        });
                                    })
                                },
                            )
                            .inner
                        })
                        .inner;
                    if overflow {
                        let offset = output.state.clamped_offset().x;
                        let max_offset =
                            (output.content_size.x - output.inner_rect.width()).max(0.0);
                        let drag_step = ui.input(|input| {
                            if self.dragging.is_some() && input.pointer.primary_down() {
                                320.0 * input.stable_dt.min(0.05)
                            } else {
                                0.0
                            }
                        });
                        let viewport = output.inner_rect;
                        let mut scroll_by = match ui
                            .ctx()
                            .pointer_hover_pos()
                            .filter(|p| viewport.contains(*p))
                        {
                            Some(pos) if pos.x < viewport.left() + 24.0 => -drag_step,
                            Some(pos) if pos.x > viewport.right() - 24.0 => drag_step,
                            _ => 0.0,
                        };
                        ui.allocate_ui_with_layout(
                            Vec2::splat(32.0),
                            egui::Layout::top_down(egui::Align::Center),
                            |ui| {
                                ui.spacing_mut().item_spacing.y = 6.0;
                                for (label, direction, enabled, hint) in [
                                    (
                                        "←",
                                        -1.0,
                                        offset > 0.5,
                                        "向左捲動分頁；也可在分頁列使用滾輪",
                                    ),
                                    (
                                        "→",
                                        1.0,
                                        offset < max_offset - 0.5,
                                        "向右捲動分頁；也可在分頁列使用滾輪",
                                    ),
                                ] {
                                    let response = ui
                                        .add_enabled_ui(enabled, |ui| {
                                            let (rect, response) = ui.allocate_exact_size(
                                                Vec2::new(32.0, 13.0),
                                                Sense::click(),
                                            );
                                            response.widget_info(|| {
                                                egui::WidgetInfo::labeled(
                                                    egui::Role::Button,
                                                    ui.is_enabled(),
                                                    hint,
                                                )
                                            });
                                            let fill = if response.is_pointer_button_down_on() {
                                                TINT
                                            } else if response.hovered() {
                                                HOVER
                                            } else {
                                                PANEL
                                            };
                                            ui.painter().rect_filled(rect, 0, fill);
                                            ui.painter().rect_stroke(
                                                rect,
                                                0,
                                                Stroke::new(
                                                    1.0,
                                                    if response.hovered() { ACCENT } else { LINE },
                                                ),
                                                egui::StrokeKind::Inside,
                                            );
                                            ui.painter().text(
                                                rect.center(),
                                                Align2::CENTER_CENTER,
                                                label,
                                                FontId::proportional(11.0),
                                                MUTED,
                                            );
                                            response
                                        })
                                        .inner;
                                    let response = response.on_hover_text(hint);
                                    if response.clicked() {
                                        scroll_by = direction * width * 0.8;
                                    } else if response.contains_pointer() {
                                        scroll_by = direction * drag_step;
                                    }
                                }
                            },
                        );
                        let next_offset = (offset + scroll_by).clamp(0.0, max_offset);
                        if next_offset != offset {
                            self.tab_scroll = Some(next_offset);
                            ui.ctx().request_repaint();
                        }
                    }
                    let clock = ui
                        .add_enabled_ui(writable, |ui| icon_button(ui, Icon::Clock, 32.0))
                        .inner;
                    if dynamic_hover_text(clock, || {
                        crate::runtime::clock_status_tooltip(
                            &self.runtime.clock,
                            self.runtime.sync_rx.is_some(),
                            ui.style(),
                            RED,
                        )
                    })
                    .clicked()
                    {
                        self.begin_time_sync();
                    }
                });
            });
        tab_focused
    }
    fn search(&mut self, ui: &mut egui::Ui) -> egui::Response {
        egui::Frame::NONE
            .inner_margin(egui::Margin {
                left: 12,
                right: 12,
                top: 8,
                bottom: 6,
            })
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    let reserve = if self.query.is_empty() { 82.0 } else { 116.0 };
                    let w = (ui.available_width() - reserve).max(60.0);
                    let search_id = egui::Id::unique("vault_search");
                    let paste =
                        limited_edit::prepare_paste(ui, search_id, TEXT_INPUT_LIMIT, |_| true);
                    let search = ui.add_sized(
                        [w, 32.0],
                        egui::TextEdit::singleline(&mut self.query)
                            .id(search_id)
                            .char_limit(TEXT_INPUT_LIMIT)
                            .hint_text("根據名稱查找"),
                    );
                    drop(paste);
                    if search.changed() {
                        self.selected.clear();
                        self.cancel_drag();
                    }
                    if !self.query.is_empty()
                        && ui
                            .add_sized([28.0, 32.0], egui::Button::new("×"))
                            .on_hover_text("清空搜尋")
                            .clicked()
                    {
                        self.query.clear();
                        self.selected.clear();
                        self.cancel_drag();
                    }
                    self.refresh_projection();
                    let count = self.projection.ids.len();
                    let all_selected = self
                        .selected
                        .includes_all(self.projection.ids.iter().copied());
                    let button = egui::Button::new(if all_selected {
                        "取消全選"
                    } else {
                        "全選"
                    })
                    .min_size(Vec2::new(76.0, 32.0));
                    if ui
                        .add_enabled(count > 0, button)
                        .on_hover_text("選取或取消目前列表的全部結果")
                        .clicked()
                    {
                        if all_selected {
                            self.selected.clear();
                        } else {
                            self.select_all_visible();
                        }
                    }
                    search
                })
                .inner
            })
            .inner
    }
    fn row(
        &mut self,
        ui: &mut egui::Ui,
        index: usize,
        primary_press: Option<Pos2>,
        focus: bool,
    ) -> bool {
        let writable = self.runtime.app.repository_state.allows_writes();
        let h = ROW_HEIGHT;
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
        let searching = self.searching();
        let model_index = self.model.entry_slot(self.projection.ids[index]);
        let entry = &self.model.entries[model_index];
        let entry_id = entry.id;
        let group_name = if searching {
            self.model.group_name(entry.group_id).unwrap_or_default()
        } else {
            ""
        };
        let has_description = !entry.record.description.is_empty();
        let y = r.bottom() - 21.0;
        let action_width = 32.0 + 4.0 + 32.0;
        let actions = Rect::from_min_size(
            Pos2::new(r.right() - 12.0 - action_width, y - 16.0),
            Vec2::new(action_width, 32.0),
        );
        let body = Rect::from_min_max(
            Pos2::new(r.left(), r.top() + 28.0),
            Pos2::new(actions.left() - 4.0, r.bottom()),
        );
        let title = Rect::from_min_max(r.min, Pos2::new(r.right(), r.top() + 28.0));
        let row_sense = if writable {
            Sense::click_and_drag()
        } else {
            Sense::click()
        };
        let title_response =
            ui.interact(title, egui::Id::unique(("row_title", entry_id)), row_sense);
        let body_response = ui.interact(body, egui::Id::unique(("row", entry_id)), row_sense);
        if focus {
            title_response.request_focus();
        }
        let focused = title_response.has_focus() || body_response.has_focus();
        let resp = dynamic_hover_text(title_response | body_response, || {
            let mut hint = entry.record.name.clone();
            if has_description {
                hint.push('\n');
                hint.push_str(&entry.record.description);
            }
            if searching {
                hint.push_str("\n分組：");
                hint.push_str(group_name);
            }
            hint
        });
        // A drag's source is the row under the actual left-button press,
        // not the row under a later right press while the left button is held.
        if writable
            && let Some(pos) = primary_press
            && ui.clip_rect().contains(pos)
            && (title.contains(pos) || body.contains(pos))
            && ui.ctx().layer_id_at(pos) == Some(ui.layer_id())
        {
            self.drag_origin = Some(entry_id);
        }
        let selected = self.selected.contains(&entry_id);
        ui.painter().rect_filled(
            r,
            0,
            if selected {
                if index.is_multiple_of(2) {
                    SELECTED_A
                } else {
                    SELECTED_B
                }
            } else if resp.hovered() {
                HOVER
            } else if index.is_multiple_of(2) {
                ROW_A
            } else {
                ROW_B
            },
        );
        if selected {
            ui.painter().rect_filled(
                Rect::from_min_size(r.min, Vec2::new(3.0, r.height())),
                0,
                ACCENT,
            );
        }
        if focused {
            ui.painter().rect_stroke(
                r.shrink(1.0),
                0,
                Stroke::new(1.0, ACCENT),
                egui::StrokeKind::Inside,
            );
        }
        let p = ui.painter().with_clip_rect(r.shrink2(Vec2::new(12.0, 0.0)));
        let inner_width = (r.width() - 24.0).max(0.0);
        let header_y = r.top() + 18.0;
        // Notes use their natural width up to 52%; names keep the remaining space.
        let description = has_description.then(|| {
            self.projection.notes.layout(
                &p,
                entry_id,
                self.model.change_token(),
                &entry.record.description,
                12.0,
                MUTED,
                inner_width * 0.52,
            )
        });
        let note_width = description.as_ref().map_or(0.0, |g| g.size().x + 12.0);
        let name_width = (inner_width - note_width).max(0.0);
        let name = single_line(&p, &entry.record.name, 16.0, NAME_COLOR, name_width);
        let name_rect = Rect::from_min_max(
            r.min + Vec2::new(12.0, 0.0),
            r.min + Vec2::new(12.0 + name_width, 28.0),
        );
        p.with_clip_rect(name_rect).galley(
            Pos2::new(name_rect.left(), header_y - name.size().y / 2.0),
            name,
            NAME_COLOR,
        );
        if let Some(note) = description {
            let note_rect = Rect::from_min_max(
                Pos2::new(r.right() - 12.0 - note.size().x, r.top()),
                Pos2::new(r.right() - 12.0, r.top() + 28.0),
            );
            p.with_clip_rect(note_rect).galley(
                Pos2::new(note_rect.left(), header_y - note.size().y / 2.0),
                note,
                MUTED,
            );
        }
        let code = self
            .otp_cache
            .display(&entry.record, self.runtime.frame_time);
        let (code_font, code_color) = if code.digits.len() == 8 {
            (FontId::monospace(27.0), INK)
        } else {
            (FontId::proportional(12.0), RED)
        };
        let code_galley = p.layout_no_wrap(code.formatted.clone(), code_font, code_color);
        let code_right = r.left() + 12.0 + code_galley.size().x;
        p.galley(
            Pos2::new(r.left() + 12.0, y - code_galley.size().y / 2.0),
            code_galley,
            code_color,
        );
        if searching {
            let group_rect = Rect::from_min_max(
                Pos2::new(code_right + 10.0, y - 10.0),
                Pos2::new(actions.left() - 8.0, y + 10.0),
            );
            if group_rect.width() > 16.0 {
                let group = single_line(&p, group_name, 11.0, MUTED, group_rect.width());
                p.with_clip_rect(group_rect).galley(
                    Pos2::new(
                        group_rect.right() - group.size().x,
                        y - group.size().y / 2.0,
                    ),
                    group,
                    MUTED,
                );
            }
        }
        // The row has already reserved its full height. Its buttons must not
        // advance the parent cursor again and overlap the next row's boundary.
        let mut action_ui = ui.new_child(
            egui::UiBuilder::new()
                .id_salt(("row_actions", entry_id))
                .max_rect(actions),
        );
        action_ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui
                .add_enabled_ui(code.digits.len() == 8, |ui| {
                    icon_button(ui, Icon::Copy, 32.0).on_hover_text("複製驗證碼")
                })
                .inner
                .clicked()
            {
                ui.ctx().copy_text(code.digits.clone());
                self.notify("已複製 8 位驗證碼");
            }
            let more = icon_button(ui, Icon::More, 32.0).on_hover_text("更多操作");
            egui::Popup::menu(&more).show(|ui| {
                if ui
                    .add_enabled(writable, egui::Button::new(EDIT_ENTRY_TITLE))
                    .clicked()
                {
                    self.edit(ui.ctx(), Some(entry_id));
                    ui.close();
                }
                let move_disabled = self.move_disabled_reason();
                ui.add_enabled_ui(move_disabled.is_none(), |ui| {
                    ui.menu_button("移動至分組", |ui| {
                        self.group_menu(ui, Some(model_index))
                    })
                })
                .inner
                .response
                .on_disabled_hover_text(move_disabled.unwrap_or_default());
                let copy_disabled = move_disabled.or(self
                    .model
                    .entry_limit_reached()
                    .then_some(ENTRY_LIMIT_NOTICE));
                ui.add_enabled_ui(copy_disabled.is_none(), |ui| {
                    ui.menu_button("複製到分組", |ui| {
                        self.copy_group_menu(ui, model_index)
                    })
                })
                .inner
                .response
                .on_disabled_hover_text(copy_disabled.unwrap_or_default());
                if ui.button("查看資料").clicked() {
                    let dialog = Dialog::details(&self.model, &self.model.entries[model_index]);
                    self.open_dialog(ui.ctx(), dialog);
                    ui.close();
                }
                if ui.button("匯出 WinAuth2 XML").clicked() {
                    self.open_xml_export(ui.ctx(), model_index);
                    ui.close();
                }
                ui.separator();
                if ui
                    .add_enabled(
                        writable,
                        egui::Button::new(egui::RichText::new("刪除").color(RED)),
                    )
                    .clicked()
                {
                    self.open_dialog(ui.ctx(), Dialog::delete(vec![entry_id]));
                    ui.close();
                }
            });
        });
        // Plain click toggles a single selection; Ctrl toggles only this row.
        // Editing remains available in the More menu.
        if resp.clicked() {
            if ui.input(|i| i.modifiers.ctrl) {
                if !self.selected.insert(entry_id) {
                    self.selected.remove(&entry_id);
                }
            } else {
                let was_only_selection =
                    self.selected.len() == 1 && self.selected.contains(&entry_id);
                self.selected.clear();
                if !was_only_selection {
                    self.selected.insert(entry_id);
                }
            }
        }
        if self.dragging.is_none()
            && self.drag_origin == Some(entry_id)
            && resp.drag_started_by(egui::PointerButton::Primary)
            && !ui.input(|i| i.modifiers.ctrl)
        {
            self.dragging = Some(model_index);
        }
        if resp.contains_pointer()
            && !searching
            && self
                .dragged_entry()
                .is_some_and(|dragged| dragged.id != entry_id)
        {
            let below = ui.input(|i| i.pointer.hover_pos().is_some_and(|p| p.y > r.center().y));
            self.drop = Some(DropTarget::GroupSlot(index + usize::from(below)));
            let y = if below { r.bottom() } else { r.top() };
            ui.painter().line_segment(
                [Pos2::new(r.left(), y), Pos2::new(r.right(), y)],
                Stroke::new(2.0, ACCENT),
            );
        }
        focused
    }
    fn entry_list(&mut self, ui: &mut egui::Ui, focus_first: bool) -> bool {
        if !window_hidden(ui.ctx()) {
            ui.ctx().request_repaint_after(Duration::from_millis(100));
        }
        let (bar, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.0), Sense::hover());
        ui.painter().rect_filled(bar, 0, LINE);
        let progress = self.runtime.frame_time.rem_euclid(30_000) as f32 / 30_000.0;
        ui.painter().rect_filled(
            Rect::from_min_size(bar.min, Vec2::new(bar.width() * progress, bar.height())),
            0,
            ACCENT,
        );
        self.refresh_projection();
        let primary_press = ui.input(|input| {
            input.events.iter().rev().find_map(|event| match event {
                egui::Event::PointerButton {
                    pos,
                    button: egui::PointerButton::Primary,
                    pressed: true,
                    ..
                } => Some(*pos),
                _ => None,
            })
        });
        let token = self.model.change_token();
        let list_height = (ui.available_height() - FOOTER_HEIGHT).max(60.0);
        ui.spacing_mut().item_spacing.y = 0.0;
        // Search covers every group, so it needs only one current scroll state.
        // Ordinary lists retain their individual group positions.
        let searching = self.searching();
        let scroll_key = ("entries", (!searching).then_some(self.group));
        let reset_search_scroll = std::mem::take(&mut self.projection.reset_search_scroll);
        if searching && reset_search_scroll {
            // Match ScrollArea's conversion of its salt before composing the ID.
            let id = ui.make_persistent_id(egui::IdSalt::new(scroll_key));
            ui.ctx()
                .data_mut(|data| data.remove::<egui::scroll_area::State>(id));
        }
        let mut scroll = egui::ScrollArea::vertical()
            .id_salt(scroll_key)
            .auto_shrink([false, false])
            .max_height(list_height);
        if self.projection.ids.is_empty() {
            scroll.show(ui, |ui| {
                ui.add_space(45.0);
                ui.vertical_centered(|ui| {
                    ui.label("沒有符合的驗證器");
                    ui.label(egui::RichText::new("換個名稱搜尋，或匯入驗證器。").color(MUTED));
                });
            });
            return false;
        }
        if focus_first {
            scroll = scroll.vertical_scroll_offset(0.0);
        }
        scroll
            .show_rows(ui, ROW_HEIGHT, self.projection.ids.len(), |ui, range| {
                let mut row_focused = false;
                for index in range {
                    row_focused |= self.row(ui, index, primary_press, focus_first && index == 0);
                    // Row menus can move entries. Refresh the projection next frame
                    // instead of using indices from before that mutation.
                    if self.model.change_token() != token {
                        ui.ctx().request_repaint();
                        break;
                    }
                }
                if self.dragging.is_some()
                    && let Some(pos) = ui.input(|i| i.pointer.hover_pos())
                {
                    let clip = ui.clip_rect();
                    if clip.contains(pos) {
                        let delta = if pos.y < clip.top() + 24.0 {
                            8.0
                        } else if pos.y > clip.bottom() - 24.0 {
                            -8.0
                        } else {
                            0.0
                        };
                        if delta != 0.0 {
                            ui.scroll_with_delta(Vec2::new(0.0, delta));
                        }
                    }
                }
                row_focused
            })
            .inner
    }
    fn footer(&mut self, ui: &mut egui::Ui) {
        let writable = self.runtime.app.repository_state.allows_writes();
        egui::Frame::NONE
            .fill(PANEL)
            .inner_margin(egui::Margin::symmetric(10, 6))
            .show(ui, |ui| {
                ui.set_min_width(ui.available_width());
                let narrow = ui.available_width() < 340.0;
                let add_width = if narrow { 64.0 } else { 76.0 };
                let action_width = if narrow { 44.0 } else { 52.0 };
                let left_width = ui.available_width() - add_width - 6.0;
                ui.horizontal(|ui| {
                    ui.allocate_ui_with_layout(
                        Vec2::new(left_width, 32.0),
                        egui::Layout::left_to_right(egui::Align::Center),
                        |ui| {
                            ui.set_min_size(Vec2::new(left_width, 32.0));
                            ui.spacing_mut().item_spacing.x = 4.0;
                            if self.selected.is_empty() {
                                let count_label = if self.searching() {
                                    self.refresh_projection();
                                    format!("找到 {} 個驗證器", self.projection.ids.len())
                                } else {
                                    format!(
                                        "總共 {} 個，本組 {} 個",
                                        self.model.entries.len(),
                                        self.model.group_count(self.group)
                                    )
                                };
                                ui.label(egui::RichText::new(count_label).size(11.0).color(MUTED));
                                if ui
                                    .small_button("說明")
                                    .on_hover_text("版本、專案與授權")
                                    .clicked()
                                {
                                    self.open_dialog(ui.ctx(), Dialog::Info);
                                }
                            } else {
                                ui.label(
                                    egui::RichText::new(format!("已選 {}", self.selected.len()))
                                        .size(12.0),
                                );
                                let move_disabled = self.move_disabled_reason();
                                ui.add_enabled_ui(move_disabled.is_none(), |ui| {
                                    let move_button = ui
                                        .add_sized([action_width, 32.0], egui::Button::new("移動"));
                                    egui::Popup::menu(&move_button)
                                        .show(|ui| self.group_menu(ui, None));
                                })
                                .response
                                .on_disabled_hover_text(move_disabled.unwrap_or_default());
                                if ui
                                    .add_enabled_ui(writable, |ui| {
                                        ui.add_sized(
                                            [action_width, 32.0],
                                            egui::Button::new(
                                                egui::RichText::new("刪除").color(RED),
                                            ),
                                        )
                                    })
                                    .inner
                                    .clicked()
                                {
                                    self.open_dialog(
                                        ui.ctx(),
                                        Dialog::delete(self.selected.iter().copied().collect()),
                                    );
                                }
                                if ui
                                    .add_sized([action_width, 32.0], egui::Button::new("取消"))
                                    .clicked()
                                {
                                    self.selected.clear();
                                }
                            }
                        },
                    );
                    let can_add = writable && !self.model.entry_limit_reached();
                    let label = egui::RichText::new("＋ 新增");
                    let button = if can_add {
                        egui::Button::new(label.color(ON_ACCENT)).fill(BLUE)
                    } else {
                        egui::Button::new(label)
                    };
                    let add = ui
                        .add_enabled_ui(can_add, |ui| ui.add_sized([add_width, 32.0], button))
                        .inner
                        .on_disabled_hover_text(if writable {
                            ENTRY_LIMIT_NOTICE
                        } else {
                            READ_ONLY_UPDATE_NOTICE
                        });
                    if can_add {
                        egui::Popup::menu(&add)
                            .align(egui::emath::RectAlign::TOP_END)
                            .show(|ui| {
                                if ui.button("從密鑰恢復").clicked() {
                                    self.edit(ui.ctx(), None);
                                    ui.close();
                                }
                                if ui.button(email_recovery::TITLE).clicked() {
                                    self.open_email_recovery(ui.ctx());
                                    ui.close();
                                }
                            });
                    }
                });
            });
    }
    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        if !self.runtime.app.repository_state.allows_writes() && dialog.requires_writable_settings() {
            dialog.clear_editors(ctx);
            return;
        }
        let mut keep = true;
        let mut submitted_groups = false;
        let (title, close_label) = match &dialog {
            Dialog::Import(_) => ("匯入驗證器", "取消恢復"),
            Dialog::EmailRecovery(_) => (email_recovery::TITLE, "取消恢復"),
            Dialog::GroupImport(_) => (group_import::TITLE, "關閉分組匯入"),
            Dialog::FullRestore(_) => (full_restore::TITLE, "關閉完整還原"),
            Dialog::Edit { .. } => (EDIT_ENTRY_TITLE, "關閉編輯"),
            Dialog::Details { .. } => ("驗證器資料", "關閉驗證器資料"),
            Dialog::Export(form) => (form.format().title(), "關閉匯出"),
            Dialog::Delete(_) => ("確定要刪除下列驗證器？", "關閉刪除確認"),
            Dialog::Groups { .. } => ("分組管理", "取消分組變更"),
            Dialog::Info => ("BNAuth", "關閉說明"),
        };
        let mut close_requested = false;
        let width = (ctx.content_rect().width() - 40.0).clamp(240.0, 370.0);
        let height = (ctx.content_rect().height() - 100.0).max(220.0);
        let id = egui::Id::unique(("dialog", std::mem::discriminant(&dialog)));
        let previous_size = ctx.memory(|memory| memory.area_rect(id).map(|rect| rect.size()));
        let response = egui::Modal::new(id).show(ctx, |ui| {
            ui.set_width(width);
            ui.set_max_height(height);
            let header = ui.horizontal_top(|ui| {
                // Keep the full title column so the close button stays at the right edge.
                let title_width =
                    (ui.available_width() - 28.0 - ui.spacing().item_spacing.x).max(1.0);
                ui.allocate_ui_with_layout(
                    Vec2::new(title_width, 28.0),
                    egui::Layout::top_down(egui::Align::Min),
                    |ui| {
                        ui.set_width(title_width);
                        ui.add(egui::Label::new(egui::RichText::new(title).heading()).wrap());
                    },
                );
                if ui
                    .add_enabled_ui(
                        (!matches!(dialog, Dialog::Export(_)) || self.runtime.export_job.is_none())
                            && (!matches!(dialog, Dialog::GroupImport(_))
                                || self.runtime.group_import_rx.is_none())
                            && (!matches!(dialog, Dialog::FullRestore(_))
                                || self.runtime.full_restore_rx.is_none()),
                        |ui| ui.add_sized([28.0, 28.0], egui::Button::new("×")),
                    )
                    .inner
                    .on_hover_text(close_label)
                    .clicked()
                {
                    close_requested = true;
                }
            });
            let content_height =
                (height - header.response.rect.height() - ui.spacing().item_spacing.y).max(1.0);
            let content_height = if matches!(dialog, Dialog::Groups { .. }) {
                (content_height - 32.0 - ui.spacing().item_spacing.y).max(1.0)
            } else {
                content_height
            };
            egui::ScrollArea::vertical()
                .max_height(content_height)
                .show(ui, |ui| match &mut dialog {
                    Dialog::Import(form) => {
                        keep = self.import_dialog(ui, form);
                    }
                    Dialog::EmailRecovery(form) => keep = self.email_recovery_dialog(ui, form),
                    Dialog::Export(form) => self.file_export_dialog(ui, form),
                    Dialog::GroupImport(form) => keep = self.group_import_dialog(ui, form),
                    Dialog::FullRestore(form) => keep = self.full_restore_dialog(ui, form),
                    Dialog::Edit {
                        id,
                        name,
                        description,
                        error,
                    } => {
                        ui.add_space(10.0);
                        limited_edit::name_and_note(
                            ui,
                            (name, DialogEditor::EditName.id()),
                            (description, DialogEditor::EditDescription.id()),
                            Some("BattleTag 或自訂名稱"),
                        );
                        if !error.is_empty() {
                            ui.colored_label(RED, error.as_str());
                        }
                        ui.add_space(12.0);
                        let (apply, _) = app_runtime::ActionButtons::Apply.show_row(ui, true, |_| {});
                        if apply {
                            match self.save_entry_edit(*id, name, description) {
                                Ok(()) => keep = false,
                                Err(e) => *error = e,
                            }
                        }
                    }
                    Dialog::Details { record, group_name } => {
                        ui.add_space(12.0);
                        ui.label(egui::RichText::new(&record.name).size(18.0));
                        if !record.description.is_empty() {
                            ui.label(&record.description);
                        }
                        let serial = record.serial.as_deref().map(bnauth_core::display_serial);
                        let mut secret = record.secret.hex();
                        secret.make_ascii_uppercase();
                        for (label, value) in [
                            ("分組", group_name.as_str()),
                            ("序號", serial.as_deref().unwrap_or("尚未提供")),
                            (
                                "還原碼",
                                record.restore_code.as_deref().unwrap_or("尚未提供"),
                            ),
                            ("長密鑰", secret.as_str()),
                        ] {
                            ui.separator();
                            ui.label(egui::RichText::new(label).size(12.0).color(MUTED));
                            ui.add(egui::Label::new(value).selectable(true).wrap());
                        }
                    }
                    Dialog::Delete(form) => {
                        form.show(ui, &self.model);
                        ui.colored_label(RED, "此操作無法復原。");
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new("確認").color(ON_ACCENT))
                                        .fill(DANGER),
                                )
                                .clicked()
                            {
                                if let Some(change) = self.model.prepare_deletion(&form.ids)
                                    && let Err(error) = self.save_list_edit(|_, _, current| {
                                        (change.snapshot(current), ListEdit::Delete(change))
                                    })
                                {
                                    self.notify(error);
                                }
                                keep = false;
                            }
                        });
                    }
                    Dialog::Groups {
                        draft,
                        name,
                        rename,
                        error,
                    } => {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} / 20 組 · 每組最多 100 筆",
                                draft.groups.len()
                            ))
                            .size(12.0)
                            .color(MUTED),
                        );
                        if rename.is_some()
                            && ui.input_mut(|i| {
                                i.consume_key(egui::Modifiers::NONE, egui::Key::Escape)
                            })
                        {
                            *rename = None;
                            DialogEditor::GroupRename.reset(ctx);
                        }
                        let mut action = None;
                        let mut rename_response = None;
                        let mut newly_focused = false;
                        for g in &draft.groups {
                            ui.scope_builder(
                                egui::UiBuilder::new()
                                    .scope_id(ui.make_persistent_id(("managed_group", g.id))),
                                |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
                                        let name_width = (ui.available_width() - 130.0).max(40.0);
                                        let count = draft.group_count(g.id);
                                        let label = format!("{}  ({count})", g.name);
                                        if let Some(edit) =
                                            rename.as_mut().filter(|edit| edit.id == g.id)
                                        {
                                            let paste = limited_edit::prepare_paste(
                                                ui,
                                                DialogEditor::GroupRename.id(),
                                                TEXT_INPUT_LIMIT,
                                                |_| true,
                                            );
                                            let mut output =
                                                egui::TextEdit::singleline(&mut edit.name)
                                                    .id(DialogEditor::GroupRename.id())
                                                    .char_limit(TEXT_INPUT_LIMIT)
                                                    .font(FontId::proportional(13.0))
                                                    .align(Align2::LEFT_CENTER)
                                                    .margin(egui::Margin::symmetric(6, 4))
                                                    .desired_width(name_width)
                                                    .min_size(Vec2::new(name_width, 28.0))
                                                    .show(ui);
                                            drop(paste);
                                            edit.error.show_border(
                                                ui,
                                                &output.response,
                                                draft,
                                                &edit.name,
                                                Some(edit.id),
                                            );
                                            if edit.focus {
                                                output.response.request_focus();
                                                output.state.cursor.set_char_range(Some(
                                                    egui::text::CCursorRange::two(
                                                        egui::text::CCursor::new(0),
                                                        egui::text::CCursor::new(
                                                            edit.name.chars().count(),
                                                        ),
                                                    ),
                                                ));
                                                output.state.store(ui.ctx(), output.response.id);
                                                output
                                                    .response
                                                    .scroll_to_me(Some(egui::Align::Center));
                                                edit.focus = false;
                                                newly_focused = true;
                                            }
                                            rename_response = Some(output.response);
                                        } else {
                                            let group_name = ui
                                                .add_sized(
                                                    [name_width, 28.0],
                                                    egui::Button::selectable(false, ())
                                                        .frame_when_inactive(true)
                                                        .left_text(clipped_button_label(
                                                            ui, &label, name_width,
                                                        )),
                                                )
                                                .on_hover_text(if g.id == 0 {
                                                    format!("{label}\n系統分組，不能改名或刪除")
                                                } else {
                                                    format!("{label}\n雙擊名稱以改名")
                                                });
                                            if g.id != 0 && group_name.double_clicked() {
                                                action = Some(GroupAction::Rename(g.id));
                                            }
                                        }
                                        if g.id != 0 {
                                            ui.scope_builder(
                                                egui::UiBuilder::new()
                                                    .scope_id(
                                                        ui.make_persistent_id("group_actions"),
                                                    )
                                                    .layout(egui::Layout::right_to_left(
                                                        egui::Align::Center,
                                                    )),
                                                |ui| {
                                                    if ui
                                                        .add_enabled(
                                                            count == 0,
                                                            egui::Button::new("刪除")
                                                                .min_size(Vec2::new(48.0, 28.0)),
                                                        )
                                                        .clicked()
                                                    {
                                                        if rename
                                                            .as_ref()
                                                            .is_some_and(|edit| edit.id == g.id)
                                                        {
                                                            *rename = None;
                                                            DialogEditor::GroupRename.reset(ctx);
                                                        }
                                                        action = Some(GroupAction::Delete(g.id));
                                                    }
                                                    if ui
                                                        .add_sized(
                                                            [32.0, 28.0],
                                                            egui::Button::new("↓"),
                                                        )
                                                        .on_hover_text("向下移動分組")
                                                        .clicked()
                                                    {
                                                        action = Some(GroupAction::Move(g.id, 1));
                                                    }
                                                    if ui
                                                        .add_sized(
                                                            [32.0, 28.0],
                                                            egui::Button::new("↑"),
                                                        )
                                                        .on_hover_text("向上移動分組")
                                                        .clicked()
                                                    {
                                                        action = Some(GroupAction::Move(g.id, -1));
                                                    }
                                                },
                                            );
                                        }
                                    })
                                },
                            );
                            if let Some(message) = rename
                                .as_ref()
                                .filter(|edit| edit.id == g.id)
                                .and_then(|edit| edit.error.message.as_deref())
                            {
                                ui.colored_label(RED, message);
                            }
                        }
                        ui.separator();
                        ui.label("新增分組");
                        let paste = limited_edit::prepare_paste(
                            ui,
                            DialogEditor::NewGroupName.id(),
                            TEXT_INPUT_LIMIT,
                            |_| true,
                        );
                        let new_group = egui::TextEdit::singleline(name)
                            .id(DialogEditor::NewGroupName.id())
                            .char_limit(TEXT_INPUT_LIMIT)
                            .desired_width(f32::INFINITY)
                            .hint_text("輸入名稱後按 Enter 新增")
                            .show(ui)
                            .response;
                        drop(paste);
                        error.show_border(ui, &new_group, draft, name, None);
                        if new_group.gained_focus() {
                            new_group.scroll_to_me(None);
                        }
                        if new_group.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            action = Some(GroupAction::Add);
                        }
                        if let Some(message) = &error.message {
                            ui.colored_label(RED, message);
                        }
                        // Check focus after all controls have rendered. A later widget can take it.
                        let lost_focus = !newly_focused
                            && rename_response.as_ref().is_some_and(|response| {
                                response.lost_focus() && !response.has_focus()
                            });
                        if (lost_focus || action.is_some())
                            && finish_group_rename(draft, rename, ctx)
                        {
                            match action {
                                Some(GroupAction::Rename(id)) => {
                                    if let Some(group_name) = draft.group_name(id) {
                                        DialogEditor::GroupRename.reset(ctx);
                                        *rename = Some(GroupRename {
                                            id,
                                            name: group_name.to_owned(),
                                            focus: true,
                                            error: GroupNameFeedback::default(),
                                        });
                                        error.message = None;
                                    }
                                }
                                Some(GroupAction::Move(id, delta)) => {
                                    error.message = draft.move_group(id, delta).err();
                                }
                                Some(GroupAction::Delete(id)) => {
                                    error.message = draft.delete_group(id).err();
                                }
                                Some(GroupAction::Add) => {
                                    let duplicate = error.name_in_use(draft, name, None);
                                    match draft.add_group_checked(name, duplicate) {
                                        Ok(_) => {
                                            name.clear();
                                            DialogEditor::NewGroupName.reset(ctx);
                                            *error = GroupNameFeedback::default();
                                        }
                                        Err(e) => error.reject(e),
                                    }
                                    new_group.request_focus();
                                }
                                None => {}
                            }
                        }
                    }
                    Dialog::Info => {
                        ui.label(concat!("本地戰網驗證器，版本 ", env!("CARGO_PKG_VERSION")));
                        ui.label("Copyright © 2026 EK · MIT 授權");
                        ui.hyperlink("https://github.com/EKE00372/BNAuth");
                        ui.label("資料自動保存於EXE同目錄的BNAuth.bnauth。");
                        ui.add_space(12.0);
                        ui.label("字型授權");
                        ui.label("字型來源：有愛黑體（Nowar Sans）");
                        ui.label("授權：SIL Open Font License 1.1")
                            .on_hover_text("右鍵可複製完整授權")
                            .context_menu(|ui| {
                                if ui.button("複製完整授權").clicked() {
                                    ctx.copy_text(
                                        concat!(
                                            include_str!("../LICENSE"),
                                            "\n\n",
                                            include_str!("../THIRD_PARTY_NOTICES.txt")
                                        )
                                        .to_owned(),
                                    );
                                    ui.close();
                                }
                            });
                    }
                });
            if let Dialog::Groups {
                draft,
                name,
                rename,
                error,
            } = &mut dialog
            {
                ui.allocate_ui_with_layout(
                    Vec2::new(ui.available_width(), 32.0),
                    egui::Layout::right_to_left(egui::Align::Center),
                    |ui| {
                        if ui
                            .add_sized([64.0, 32.0], egui::Button::new("套用").fill(BLUE))
                            .clicked()
                            && finish_group_rename(draft, rename, ctx)
                        {
                            if !name.is_empty()
                                && let Err(e) = draft
                                    .add_group_checked(name, error.name_in_use(draft, name, None))
                            {
                                error.reject(e);
                                ctx.memory_mut(|m| {
                                    m.request_focus(DialogEditor::NewGroupName.id())
                                });
                            } else if !draft.changed_from(&self.model) {
                                keep = false;
                            } else {
                                name.clear();
                                let current = self
                                    .runtime
                                    .app
                                    .current
                                    .as_ref()
                                    .expect("group management requires an open vault");
                                let candidate = draft.snapshot(current);
                                match self.runtime.commit_with_pending_clock(candidate, None) {
                                    Err(e) if !self.runtime.app.has_pending() => {
                                        error.message = Some(e)
                                    }
                                    result => {
                                        submitted_groups = true;
                                        keep = false;
                                        self.runtime.app.error = result.err();
                                    }
                                }
                            }
                        }
                        if ui
                            .add_sized([64.0, 32.0], egui::Button::new("取消"))
                            .clicked()
                        {
                            keep = false;
                        }
                    },
                );
            }
        });
        // Modal anchors use the previous measured size. Reposition changed layouts
        // before presenting the frame; stable layouts still need only one pass.
        if previous_size.is_none_or(|size| (size - response.response.rect.size()).length() > 0.5) {
            ctx.request_discard("Modal size changed; recenter before painting");
        }
        if keep && close_requested {
            // Only the explicit close button dismisses a dialog. Backdrop clicks
            // and Escape must never discard an import draft or its preview.
            if matches!(dialog, Dialog::EmailRecovery(_)) {
                self.runtime.recovery.cancel_draft();
            }
            keep = false;
        }
        if keep {
            self.dialog = Some(dialog);
        } else {
            dialog.clear_editors(ctx);
            if submitted_groups && let Dialog::Groups { draft, .. } = dialog {
                self.runtime.pending_groups = Some(draft);
            }
        }
    }
}

impl BnAuthApp {
    fn app_ui(&mut self, root: &mut egui::Ui) {
        let ctx = root.ctx().clone();
        if ctx.input(|input| input.pointer.button_pressed(egui::PointerButton::Primary)) {
            self.cancel_drag();
        }
        root.painter().rect_filled(root.max_rect(), 0, BG);
        self.tab_labels.observe_font(self.window_chrome.show(root));
        if self.runtime.close_requested && !self.runtime.close_blocked() {
            self.cancel_drag();
            root.label("正在關閉");
            return;
        }
        if self.startup_ui(root) {
            self.cancel_drag();
            self.show_dialog(&ctx);
            return;
        }
        if self.runtime.app.shows_save_status() {
            self.cancel_drag();
            self.saving_ui(root);
            return;
        }
        self.drop = None;
        root.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
        let repository = self.runtime.app.repository_state;
        let writable = repository.allows_writes();
        if !writable {
            self.cancel_drag();
            if app_runtime::read_only_banner(root, !repository.cleanup_required) {
                if let Err(error) = self.reload_with_clock_cancel() {
                    self.warn(error);
                }
                return;
            }
        }
        let search = self.search(root);
        let tab_focused = self.tabs(root);
        let focus_first_entry = (tab_focused || search.has_focus())
            && self.dialog.is_none()
            && !egui::Popup::is_any_open(&ctx)
            && ctx.input_mut(|input| {
                input.modifiers.is_none()
                    && input.consume_key(egui::Modifiers::NONE, egui::Key::ArrowDown)
            });
        if focus_first_entry {
            // egui queued a spatial focus move before dispatching this key.
            ctx.memory_mut(|memory| memory.move_focus(egui::FocusDirection::None));
        }
        let row_focused = self.entry_list(root, focus_first_entry);
        if focus_first_entry && tab_focused && self.projection.ids.is_empty() {
            search.request_focus();
        }
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_drag();
        }
        if ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
            if let (Some(index), Some(target)) = (self.dragging, self.drop.take())
                && self.dragged_entry().is_some()
            {
                match target {
                    DropTarget::GroupSlot(slot) => {
                        let result = self.model.reorder_entry_at(index, self.group, slot);
                        self.finish_group_order(result, ListEdit::Reorder, &ctx);
                    }
                    DropTarget::Group { id, after_move } => {
                        let result = self.model.move_entry_at(index, id, after_move);
                        self.finish_group_order(result, ListEdit::Move, &ctx);
                    }
                }
            }
            self.cancel_drag();
        }
        self.footer(root);
        let no_widget_focused = !ctx.egui_wants_keyboard_input();
        if self.dialog.is_none() && (row_focused || no_widget_focused) {
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::A)) {
                self.select_all_visible();
            }
            if no_widget_focused && ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selected.clear();
            }
            if writable
                && !self.selected.is_empty()
                && ctx.input(|i| i.key_pressed(egui::Key::Delete))
                && !egui::Popup::is_any_open(&ctx)
            {
                self.open_dialog(
                    &ctx,
                    Dialog::delete(self.selected.iter().copied().collect()),
                );
            }
        }
        self.show_dialog(&ctx);
    }
    fn show_notifications(&mut self, ctx: &egui::Context) {
        let Some((message, remaining)) = self.notifications.visible_for(Instant::now()) else {
            return;
        };
        ctx.request_repaint_after(remaining);
        egui::Area::new(egui::Id::unique("toast"))
            .anchor(Align2::CENTER_BOTTOM, [0.0, -53.0])
            .order(egui::Order::Tooltip)
            .interactable(false)
            .show(ctx, |ui| {
                egui::Frame::NONE
                    .fill(TINT)
                    .stroke(Stroke::new(1.0, LINE))
                    .inner_margin(10)
                    .show(ui, |ui| {
                        ui.set_max_width((ctx.content_rect().width() - 50.0).max(160.0));
                        ui.add(
                            egui::Label::new(egui::RichText::new(message).color(ON_ACCENT).size(12.0))
                                .halign(egui::Align::Center),
                        );
                    });
            });
    }
    fn schedule_recovery_timeout(&self, ctx: &egui::Context) {
        if let Some(deadline) = self.runtime.recovery.reply_deadline() {
            ctx.request_repaint_after(deadline.saturating_duration_since(Instant::now()));
        }
    }
}

impl eframe::App for BnAuthApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        self.window_chrome.enforce_window_state(ctx);
        let hidden = window_hidden(ctx);
        if hidden {
            self.cancel_drag();
            self.notifications.pause(Instant::now());
        }
        self.poll_runtime(ctx);
        if hidden {
            self.schedule_recovery_timeout(ctx);
        }
    }
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.app_ui(root);
        // One common exit: loading, saving, cleanup and the normal page all paint
        // important notifications before their display time starts to elapse.
        if !window_hidden(&ctx) {
            self.show_notifications(&ctx);
        }
        self.schedule_recovery_timeout(&ctx);
    }
}

fn window_hidden(ctx: &egui::Context) -> bool {
    ctx.input(|i| i.viewport().visible() == Some(false) || i.viewport().minimized == Some(true))
}

#[derive(Default)]
struct GroupNameFeedback {
    checked_revision: Option<u64>,
    checked_name: String,
    duplicate: bool,
    message: Option<String>,
}

fn show_invalid_field(ui: &egui::Ui, field: &egui::Response) {
    ui.painter().rect_stroke(
        field.rect,
        ui.visuals().widgets.inactive.corner_radius,
        Stroke::new(2.0, RED),
        egui::StrokeKind::Inside,
    );
}

fn show_invalid_fields<const N: usize>(
    ui: &egui::Ui,
    fields: [(bool, &egui::Response); N],
    submitted: bool,
) -> bool {
    let mut any_invalid = false;
    for (invalid, field) in fields {
        if invalid {
            show_invalid_field(ui, field);
            if submitted && !any_invalid {
                field.scroll_to_me(None);
            }
            any_invalid = true;
        }
    }
    any_invalid
}

impl GroupNameFeedback {
    fn name_in_use(&mut self, draft: &GroupDraft, name: &str, group: Option<u64>) -> bool {
        if self.checked_revision != Some(draft.change_token()) || self.checked_name != name {
            self.duplicate = !name.is_empty() && draft.group_name_in_use(name, group);
            self.checked_revision = Some(draft.change_token());
            name.clone_into(&mut self.checked_name);
        }
        self.duplicate
    }

    fn reject(&mut self, error: GroupEditError) {
        self.message = match error {
            GroupEditError::DuplicateName | GroupEditError::NameTooLong => None,
            GroupEditError::Message(message) => Some(message),
        };
    }

    fn show_border(
        &mut self,
        ui: &egui::Ui,
        field: &egui::Response,
        draft: &GroupDraft,
        name: &str,
        group_id: Option<u64>,
    ) {
        let revision = draft.change_token();
        if field.changed() || self.checked_revision != Some(revision) {
            self.name_in_use(draft, name, group_id);
        }
        if field.changed() {
            self.message = None;
        }
        if self.duplicate || name.len() > bnauth_core::MAX_GROUP_NAME_BYTES {
            show_invalid_field(ui, field);
        }
    }
}

fn finish_group_rename(
    draft: &mut GroupDraft,
    rename: &mut Option<GroupRename>,
    ctx: &egui::Context,
) -> bool {
    let Some(edit) = rename.as_mut() else {
        return true;
    };
    let duplicate = edit.error.name_in_use(draft, &edit.name, Some(edit.id));
    match draft.rename_group_checked(edit.id, &edit.name, duplicate) {
        Ok(()) => {
            *rename = None;
            DialogEditor::GroupRename.reset(ctx);
            true
        }
        Err(error) => {
            edit.error.reject(error);
            edit.focus = true;
            false
        }
    }
}

fn single_line(
    painter: &egui::Painter,
    text: &str,
    size: f32,
    color: Color32,
    width: f32,
) -> std::sync::Arc<egui::Galley> {
    // Keep stored text intact; line breaks become spaces only in the list preview.
    let mut job = egui::text::LayoutJob::simple_singleline(
        text.replace(['\r', '\n', '\t'], " "),
        FontId::proportional(size),
        color,
    );
    job.wrap = egui::text::TextWrapping {
        max_width: width.max(0.0),
        max_rows: 1,
        break_anywhere: true,
        overflow_character: None,
    };
    painter.layout_job(job)
}

fn clipped_button_label(ui: &egui::Ui, text: &str, width: f32) -> egui::WidgetText {
    let spacing = ui.spacing();
    let text_width = (width - 2.0 * spacing.button_padding.x - spacing.icon_spacing).max(0.0);
    let mut wrap = egui::text::TextWrapping::truncate_at_width(text_width);
    wrap.overflow_character = None;
    egui::WidgetText::from(text)
        .into_galley_impl(
            ui.ctx(),
            ui.style(),
            wrap,
            egui::TextStyle::Button.into(),
            ui.text_valign(),
        )
        .into()
}

fn group_target_button(
    ui: &mut egui::Ui,
    name: &str,
    count: usize,
    disabled_reason: Option<&str>,
) -> egui::Response {
    let limit = crate::model::MAX_GROUP_ENTRIES;
    let response = ui.add_enabled(
        disabled_reason.is_none(),
        egui::Button::new(format!("{name}    {count}/{limit}")).min_size(Vec2::new(180.0, 32.0)),
    );
    if let Some(reason) = disabled_reason {
        response.on_disabled_hover_text(reason)
    } else {
        response
    }
}

fn dynamic_hover_text<T: Into<egui::WidgetText>>(
    response: egui::Response,
    text: impl FnOnce() -> T,
) -> egui::Response {
    // Screen readers can request a widget's description without showing its tooltip.
    if response.enabled() && response.ctx.has_accesskit_node(response.id) {
        return response.on_hover_text(text());
    }
    let tooltip = if response.enabled() {
        egui::Tooltip::for_enabled(&response)
    } else {
        egui::Tooltip::for_disabled(&response)
    };
    tooltip.show(|ui| {
        ui.set_max_width(ui.spacing().tooltip_width);
        ui.label(text());
    });
    response
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, size: f32) -> egui::Response {
    let (r, resp) = ui.allocate_exact_size(Vec2::new(size, 32.0), Sense::click());
    let fill = if resp.is_pointer_button_down_on() {
        TINT
    } else if resp.hovered() {
        HOVER
    } else {
        PANEL
    };
    ui.painter().rect_filled(r, 0, fill);
    ui.painter().rect_stroke(
        r,
        0,
        Stroke::new(1.0, if resp.hovered() { ACCENT } else { LINE }),
        egui::StrokeKind::Inside,
    );
    let c = r.center();
    let s = Stroke::new(1.3, MUTED);
    let p = ui.painter();
    match icon {
        Icon::Copy => {
            p.rect_stroke(
                Rect::from_center_size(c + Vec2::new(2.0, 2.0), Vec2::new(10.0, 12.0)),
                0,
                s,
                egui::StrokeKind::Inside,
            );
            p.line_segment([c + Vec2::new(-6.0, 3.0), c + Vec2::new(-6.0, -7.0)], s);
            p.line_segment([c + Vec2::new(-6.0, -7.0), c + Vec2::new(3.0, -7.0)], s);
        }
        Icon::Clock => {
            p.circle_stroke(c, 7.0, s);
            p.line_segment([c, c + Vec2::new(0.0, -4.0)], s);
            p.line_segment([c, c + Vec2::new(4.0, 2.0)], s);
        }
        Icon::More => {
            for dy in [-5.0, 0.0, 5.0] {
                p.circle_filled(c + Vec2::new(0.0, dy), 1.3, MUTED);
            }
        }
    }
    resp
}
