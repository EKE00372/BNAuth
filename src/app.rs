use crate::model::{Entry, VaultModel};
#[path = "app_runtime.rs"]
mod app_runtime;
#[path = "delete_confirmation.rs"]
mod delete_confirmation;
use delete_confirmation::DeleteConfirmation;
#[path = "clock_persistence.rs"]
mod clock_persistence;
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
    Groups,
}

struct GroupRename {
    id: u64,
    name: String,
    focus: bool,
    error: String,
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
    EditName,
    EditDescription,
    NewGroupName,
    GroupRename,
}

impl DialogEditor {
    const ALL: [Self; 9] = [
        Self::ImportName,
        Self::ImportDescription,
        Self::ImportSecret,
        Self::ImportSerial,
        Self::ImportRestore,
        Self::EditName,
        Self::EditDescription,
        Self::NewGroupName,
        Self::GroupRename,
    ];

    fn id(self) -> egui::Id {
        egui::Id::unique(("dialog_editor", self))
    }

    fn reset(self, ctx: &egui::Context) {
        ctx.data_mut(|data| data.remove::<egui::text_edit::TextEditState>(self.id()));
        limited_edit::reset(ctx, self.id());
        ctx.memory_mut(|memory| memory.surrender_focus(self.id()));
    }
}

fn reset_dialog_editors(ctx: &egui::Context) {
    for editor in DialogEditor::ALL {
        editor.reset(ctx);
    }
}

enum Dialog {
    Import(Box<crate::runtime::ImportForm>),
    GroupImport(Box<crate::runtime::GroupImportForm>),
    FullRestore(Box<full_restore::RestoreForm>),
    Edit {
        id: u64,
        name: String,
        description: String,
        error: String,
    },
    Details(u64),
    Export(Box<file_export::ExportForm>),
    Delete(DeleteConfirmation),
    Groups {
        name: String,
        rename: Option<GroupRename>,
        error: String,
    },
    DeleteGroup(u64),
    Info,
}

impl Dialog {
    fn delete(ids: Vec<u64>) -> Self {
        Self::Delete(DeleteConfirmation::new(ids))
    }
}

#[derive(Default)]
struct EntryProjection {
    token: Option<u64>,
    group: u64,
    query: String,
    indices: Vec<usize>,
    reset_search_scroll: bool,
    notes: text_preview::NotePreviews,
}

pub struct BnAuthApp {
    window_chrome: crate::window_chrome::WindowChrome,
    pub(crate) model: VaultModel,
    group: u64,
    tab_reveal: Option<u64>,
    tab_scroll: Option<f32>,
    query: String,
    selected: BTreeSet<u64>,
    dialog: Option<Dialog>,
    notifications: crate::notifications::Notifications,
    drag_origin: Option<u64>,
    dragging: Option<u64>,
    drop: Option<(u64, Option<u64>)>,
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
            model: VaultModel::from_snapshot(&bnauth_core::VaultSnapshot::new_empty(
                bnauth_core::now_ms(),
            )),
            group: 0,
            tab_reveal: Some(0),
            tab_scroll: None,
            query: String::new(),
            selected: BTreeSet::new(),
            dialog: None,
            notifications: crate::notifications::Notifications::default(),
            drag_origin: None,
            dragging: None,
            drop: None,
            runtime: crate::runtime::RuntimeState::new(),
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
        let token = self.model.change_token();
        let query_changed = self.projection.query != self.query;
        if self.projection.token != Some(token)
            || self.projection.group != self.group
            || query_changed
        {
            self.projection.reset_search_scroll |= query_changed;
            self.projection.indices =
                self.model
                    .filtered_indices(self.group, &self.query, self.searching());
            self.projection.token = Some(token);
            self.projection.group = self.group;
            self.projection.query.clone_from(&self.query);
        }
    }
    fn visible_ids(&mut self) -> Vec<u64> {
        self.refresh_projection();
        self.projection
            .indices
            .iter()
            .map(|&index| self.model.entries[index].id)
            .collect()
    }
    fn open_dialog(&mut self, ctx: &egui::Context, dialog: Dialog) {
        self.cancel_drag();
        // Fixed field IDs keep retained state bounded; each new draft owns its undo.
        reset_dialog_editors(ctx);
        self.dialog = Some(dialog);
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
            name: entry.name.clone(),
            description: entry.description.clone(),
            error: String::new(),
        };
        self.open_dialog(ctx, dialog);
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
    fn move_ids(&mut self, ids: &[u64], target: u64) {
        match self.model.move_entries(ids, target) {
            Ok(0) => {}
            Ok(_) => {
                self.selected.clear();
            }
            Err(e) => self.notify(e),
        }
    }
    fn group_menu(&mut self, ui: &mut egui::Ui, ids: &[u64]) {
        let counts = self.model.move_target_counts(ids);
        ui.set_min_width(190.0);
        egui::ScrollArea::vertical()
            .max_height(280.0)
            .show(ui, |ui| {
                for g in self.model.groups.clone() {
                    let (count, after_move) = counts[&g.id];
                    if ui
                        .add_enabled(
                            after_move <= crate::model::MAX_GROUP_ENTRIES,
                            egui::Button::new(format!("{}    {count}/100", g.name))
                                .min_size(Vec2::new(180.0, 32.0)),
                        )
                        .clicked()
                    {
                        self.move_ids(ids, g.id);
                        ui.close();
                    }
                }
            });
    }
    fn tabs(&mut self, ui: &mut egui::Ui) {
        egui::Frame::NONE
            .fill(PANEL)
            .stroke(Stroke::new(1.0, LINE))
            .show(ui, |ui| {
                ui.horizontal(|ui| {
                    ui.spacing_mut().item_spacing.x = 6.0;
                    let tabs: Vec<_> = self
                        .model
                        .groups
                        .iter()
                        .map(|g| {
                            let label_width = ui
                                .painter()
                                .layout_no_wrap(g.name.clone(), FontId::proportional(13.0), MUTED)
                                .size()
                                .x;
                            let width = (label_width + 12.0).min(116.0);
                            (g.clone(), width)
                        })
                        .collect();
                    let content_width: f32 = tabs.iter().map(|(_, width)| width).sum();
                    let available = (ui.available_width() - 38.0).max(0.0);
                    let overflow = content_width > available + 0.5;
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
                                            for (index, (g, tab_width)) in
                                                tabs.into_iter().enumerate()
                                            {
                                                ui.push_id(g.id, |ui| {
                                                    let active = self.group == g.id;
                                                    let (rect, response) = ui.allocate_exact_size(
                                                        Vec2::new(tab_width, 36.0),
                                                        Sense::click(),
                                                    );
                                                    let label = g.name.clone();
                                                    response.widget_info(|| {
                                                        egui::WidgetInfo::selected(
                                                            egui::Role::Tab,
                                                            ui.is_enabled(),
                                                            active,
                                                            &label,
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
                                                    let name = single_line(
                                                        ui.painter(),
                                                        &g.name,
                                                        13.0,
                                                        color,
                                                        (tab_width - 12.0).max(1.0),
                                                    );
                                                    ui.painter().galley(
                                                        Pos2::new(
                                                            rect.left() + 6.0,
                                                            rect.center().y - name.size().y / 2.0,
                                                        ),
                                                        name,
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
                                                    if reveal == Some(g.id) {
                                                        response.scroll_to_me(None);
                                                    }
                                                    if response.clicked() {
                                                        self.switch_group(g.id);
                                                    }
                                                    if self
                                                        .dragging
                                                        .and_then(|id| self.model.entry(id))
                                                        .is_some_and(|entry| entry.group_id != g.id)
                                                        && response.contains_pointer()
                                                    {
                                                        self.drop = Some((g.id, None));
                                                        ui.painter().rect_stroke(
                                                            rect.shrink(1.0),
                                                            0,
                                                            Stroke::new(2.0, ACCENT),
                                                            egui::StrokeKind::Inside,
                                                        );
                                                    }
                                                    let response = response.on_hover_text(label);
                                                    response.context_menu(|ui| {
                                                        if ui.button("管理分組").clicked() {
                                                            self.open_dialog(
                                                                ui.ctx(),
                                                                Dialog::Groups {
                                                                    name: String::new(),
                                                                    rename: None,
                                                                    error: String::new(),
                                                                },
                                                            );
                                                            ui.close();
                                                        }
                                                        if ui.button("匯出此分組…").clicked()
                                                        {
                                                            self.open_group_export(ui.ctx(), g.id);
                                                            ui.close();
                                                        }
                                                        if ui.button("匯入分組檔…").clicked()
                                                        {
                                                            self.open_group_import(ui.ctx());
                                                            ui.close();
                                                        }
                                                        ui.separator();
                                                        if ui.button("備份完整資料庫…").clicked()
                                                        {
                                                            self.open_full_backup(ui.ctx());
                                                            ui.close();
                                                        }
                                                        if ui.button("還原完整資料庫…").clicked()
                                                        {
                                                            self.open_full_restore(ui.ctx());
                                                            ui.close();
                                                        }
                                                    });
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
                                    if response.on_hover_text(hint).clicked() {
                                        self.tab_scroll = Some(
                                            (offset + direction * width * 0.8)
                                                .clamp(0.0, max_offset),
                                        );
                                        ui.ctx().request_repaint();
                                    }
                                }
                            },
                        );
                    }
                    if icon_button(ui, Icon::Groups, "全部分組", 32.0).clicked() {
                        self.open_dialog(
                            ui.ctx(),
                            Dialog::Groups {
                                name: String::new(),
                                rename: None,
                                error: String::new(),
                            },
                        );
                    }
                });
            });
    }
    fn search(&mut self, ui: &mut egui::Ui) {
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
                    if ui
                        .add_sized(
                            [w, 32.0],
                            egui::TextEdit::singleline(&mut self.query)
                                .char_limit(TEXT_INPUT_LIMIT)
                                .hint_text("搜尋所有分頁的名稱"),
                        )
                        .changed()
                    {
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
                    let count = self.projection.indices.len();
                    let all_selected =
                        count > 0
                            && self.selected.len() >= count
                            && self.projection.indices.iter().all(|&index| {
                                self.selected.contains(&self.model.entries[index].id)
                            });
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
                            self.selected = self.visible_ids().into_iter().collect();
                        }
                    }
                });
            });
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), 2.0), Sense::hover());
        ui.painter().rect_filled(r, 0, LINE);
        let progress = self.runtime.frame_time.rem_euclid(30_000) as f32 / 30_000.0;
        ui.painter().rect_filled(
            Rect::from_min_size(r.min, Vec2::new(r.width() * progress, 2.0)),
            0,
            ACCENT,
        );
    }
    fn row(&mut self, ui: &mut egui::Ui, e: &Entry, index: usize, primary_press: Option<Pos2>) {
        let h = ROW_HEIGHT;
        let (r, _) = ui.allocate_exact_size(Vec2::new(ui.available_width(), h), Sense::hover());
        let searching = self.searching();
        let group_name = self.model.group_name(e.group_id).unwrap_or("");
        let has_description = !e.description.trim().is_empty();
        let mut hint = e.name.clone();
        if has_description {
            hint.push('\n');
            hint.push_str(&e.description);
        }
        if searching {
            hint.push_str("\n分組：");
            hint.push_str(group_name);
        }
        let body = Rect::from_min_max(
            Pos2::new(r.left(), r.top() + 28.0),
            Pos2::new(r.right() - 120.0, r.bottom()),
        );
        let title = Rect::from_min_max(r.min, Pos2::new(r.right(), r.top() + 28.0));
        let resp = (ui.interact(
            title,
            egui::Id::unique(("row_title", e.id)),
            Sense::click_and_drag(),
        ) | ui.interact(
            body,
            egui::Id::unique(("row", e.id)),
            Sense::click_and_drag(),
        ))
        .on_hover_text(hint);
        // A drag's source is the row under the actual left-button press,
        // not the row under a later right press while the left button is held.
        if let Some(pos) = primary_press
            && ui.clip_rect().contains(pos)
            && (title.contains(pos) || body.contains(pos))
            && ui.ctx().layer_id_at(pos) == Some(ui.layer_id())
        {
            self.drag_origin = Some(e.id);
        }
        let selected = self.selected.contains(&e.id);
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
        let p = ui.painter().with_clip_rect(r.shrink2(Vec2::new(12.0, 0.0)));
        let inner_width = (r.width() - 24.0).max(0.0);
        let header_y = r.top() + 18.0;
        // Notes use their natural width up to 52%; names keep the remaining space.
        let description = has_description.then(|| {
            self.projection.notes.layout(
                &p,
                e.id,
                self.model.change_token(),
                &e.description,
                12.0,
                MUTED,
                inner_width * 0.52,
            )
        });
        let note_width = description.as_ref().map_or(0.0, |g| g.size().x + 12.0);
        let name_width = (inner_width - note_width).max(0.0);
        let name = single_line(&p, &e.name, 16.0, NAME_COLOR, name_width);
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
        let y = r.bottom() - 21.0;
        let code = self.otp_cache.code(&e.record, self.runtime.frame_time);
        let formatted = if code.len() == 8 {
            format!("{} {}", &code[..4], &code[4..])
        } else {
            "尚未取得密鑰".to_owned()
        };
        let code_galley = p.layout_no_wrap(formatted, FontId::monospace(27.0), INK);
        let code_right = r.left() + 12.0 + code_galley.size().x;
        p.galley(
            Pos2::new(r.left() + 12.0, y - code_galley.size().y / 2.0),
            code_galley,
            INK,
        );
        let actions = Rect::from_min_size(
            Pos2::new(r.right() - 116.0, y - 16.0),
            Vec2::new(104.0, 32.0),
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
                .id_salt(("row_actions", e.id))
                .max_rect(actions),
        );
        action_ui.horizontal(|ui| {
            ui.spacing_mut().item_spacing.x = 4.0;
            if ui
                .add_enabled_ui(code.len() == 8, |ui| {
                    icon_button(ui, Icon::Copy, "複製驗證碼", 32.0)
                })
                .inner
                .clicked()
            {
                ui.ctx().copy_text(code.clone());
                self.notify("已複製 8 位驗證碼");
            }
            let clock_hint = crate::runtime::clock_status_tooltip(
                &self.runtime.clock,
                self.runtime.sync_rx.is_some(),
            );
            if icon_button(ui, Icon::Clock, &clock_hint, 32.0).clicked() {
                self.begin_time_sync();
            }
            let more = icon_button(ui, Icon::More, "更多操作", 32.0);
            egui::Popup::menu(&more).show(|ui| {
                if ui.button("編輯標題與說明").clicked() {
                    self.edit(ui.ctx(), Some(e.id));
                    ui.close();
                }
                ui.menu_button("移動至分組", |ui| self.group_menu(ui, &[e.id]));
                if ui.button("查看驗證器資料").clicked() {
                    self.open_dialog(ui.ctx(), Dialog::Details(e.id));
                    ui.close();
                }
                if ui.button("匯出 WinAuth2 XML…").clicked() {
                    self.open_xml_export(ui.ctx(), e.id);
                    ui.close();
                }
                ui.separator();
                if ui
                    .button(egui::RichText::new("刪除驗證器…").color(RED))
                    .clicked()
                {
                    self.open_dialog(ui.ctx(), Dialog::delete(vec![e.id]));
                    ui.close();
                }
            });
        });
        // Plain click toggles a single selection; Ctrl toggles only this row.
        // Editing remains available in the More menu.
        if resp.clicked() {
            if ui.input(|i| i.modifiers.ctrl) {
                if !self.selected.insert(e.id) {
                    self.selected.remove(&e.id);
                }
            } else {
                let was_only_selection = self.selected.len() == 1 && self.selected.contains(&e.id);
                self.selected.clear();
                if !was_only_selection {
                    self.selected.insert(e.id);
                }
            }
        }
        if self.dragging.is_none()
            && self.drag_origin == Some(e.id)
            && resp.drag_started_by(egui::PointerButton::Primary)
            && !ui.input(|i| i.modifiers.ctrl)
        {
            self.dragging = Some(e.id);
        }
        if let Some(id) = self.dragging
            && resp.contains_pointer()
            && !self.searching()
            && id != e.id
        {
            let below = ui.input(|i| i.pointer.hover_pos().is_some_and(|p| p.y > r.center().y));
            let ids = self.model.filtered(self.group, "", false);
            let before = if below {
                ids.iter()
                    .position(|v| *v == e.id)
                    .and_then(|i| ids.get(i + 1))
                    .copied()
            } else {
                Some(e.id)
            };
            self.drop = Some((self.group, before));
            let y = if below { r.bottom() } else { r.top() };
            ui.painter().line_segment(
                [Pos2::new(r.left(), y), Pos2::new(r.right(), y)],
                Stroke::new(2.0, ACCENT),
            );
        }
    }
    fn entry_list(&mut self, ui: &mut egui::Ui) {
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
        let scroll = egui::ScrollArea::vertical()
            .id_salt(scroll_key)
            .auto_shrink([false, false])
            .max_height(list_height);
        if self.projection.indices.is_empty() {
            scroll.show(ui, |ui| {
                ui.add_space(45.0);
                ui.vertical_centered(|ui| {
                    ui.label("沒有符合的驗證器");
                    ui.label(egui::RichText::new("換個名稱搜尋，或匯入驗證器。").color(MUTED));
                });
            });
            return;
        }
        scroll.show_rows(
            ui,
            ROW_HEIGHT,
            self.projection.indices.len(),
            |ui, range| {
                for index in range {
                    let entry = self.model.entries[self.projection.indices[index]].clone();
                    self.row(ui, &entry, index, primary_press);
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
            },
        );
    }
    fn footer(&mut self, ui: &mut egui::Ui) {
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
                                    format!("找到 {} 個驗證器", self.projection.indices.len())
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
                                let ids: Vec<_> = self.selected.iter().copied().collect();
                                let move_button =
                                    ui.add_sized([action_width, 32.0], egui::Button::new("移動"));
                                egui::Popup::menu(&move_button)
                                    .show(|ui| self.group_menu(ui, &ids));
                                if ui
                                    .add_sized(
                                        [action_width, 32.0],
                                        egui::Button::new(egui::RichText::new("刪除").color(RED)),
                                    )
                                    .clicked()
                                {
                                    self.open_dialog(ui.ctx(), Dialog::delete(ids));
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
                    if ui
                        .add_sized(
                            [add_width, 32.0],
                            egui::Button::new(egui::RichText::new("＋ 新增").color(ON_ACCENT))
                                .fill(BLUE),
                        )
                        .clicked()
                    {
                        self.edit(ui.ctx(), None);
                    }
                });
            });
    }
    fn show_dialog(&mut self, ctx: &egui::Context) {
        let Some(mut dialog) = self.dialog.take() else {
            return;
        };
        let mut keep = true;
        let (title, close_label) = match &dialog {
            Dialog::Import(form) if form.preview.is_some() => ("預覽匯入", "關閉匯入"),
            Dialog::Import(_) => ("匯入驗證器", "關閉匯入"),
            Dialog::GroupImport(_) => ("匯入分組檔", "關閉分組匯入"),
            Dialog::FullRestore(_) => ("還原完整資料庫", "關閉完整還原"),
            Dialog::Edit { .. } => ("編輯標題與說明", "關閉編輯"),
            Dialog::Details(_) => ("驗證器資料", "關閉驗證器資料"),
            Dialog::Export(form) => (form.format().title(), "關閉匯出"),
            Dialog::Delete(_) => ("確定要刪除下列驗證器？", "關閉刪除確認"),
            Dialog::Groups { .. } => ("分組管理", "關閉分組管理"),
            Dialog::DeleteGroup(_) => ("刪除分組？", "關閉刪除分組確認"),
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
                        (!matches!(dialog, Dialog::Export(_)) || self.runtime.export_rx.is_none())
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
            egui::ScrollArea::vertical()
                .max_height(content_height)
                .show(ui, |ui| match &mut dialog {
                    Dialog::Import(form) => {
                        keep = self.import_dialog(ui, form);
                    }
                    Dialog::Export(form) => self.file_export_dialog(ui, form),
                    Dialog::GroupImport(form) => keep = self.group_import_dialog(ui, form),
                    Dialog::FullRestore(form) => keep = self.full_restore_dialog(ui, form),
                    Dialog::Edit {
                        id,
                        name,
                        description,
                        error,
                    } => {
                        ui.label(
                            egui::RichText::new("變更會保存到資料檔。")
                                .size(12.0)
                                .color(MUTED),
                        );
                        ui.add_space(10.0);
                        ui.label("名稱（必填）");
                        limited_edit::show(
                            ui,
                            name,
                            DialogEditor::EditName.id(),
                            Some("BattleTag 或自訂名稱"),
                        );
                        ui.label("Email／備註（選填）");
                        limited_edit::show(
                            ui,
                            description,
                            DialogEditor::EditDescription.id(),
                            None,
                        );
                        if !error.is_empty() {
                            ui.colored_label(RED, error.as_str());
                        }
                        ui.add_space(12.0);
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(egui::RichText::new("套用").color(ON_ACCENT))
                                        .fill(BLUE),
                                )
                                .clicked()
                            {
                                let result = self.model.update_entry(*id, name, description);
                                match result {
                                    Ok(()) => {
                                        keep = false;
                                    }
                                    Err(e) => *error = e,
                                }
                            }
                        });
                    }
                    Dialog::Details(id) => {
                        if let Some(e) = self.model.entry(*id).cloned() {
                            ui.add_space(12.0);
                            ui.label(egui::RichText::new(&e.name).size(18.0));
                            if !e.description.is_empty() {
                                ui.label(&e.description);
                            }
                            for (label, value) in [
                                (
                                    "分組",
                                    self.model.group_name(e.group_id).unwrap_or("").to_owned(),
                                ),
                                (
                                    "序號",
                                    e.record
                                        .serial
                                        .as_deref()
                                        .map(bnauth_core::display_serial)
                                        .unwrap_or("尚未提供".into()),
                                ),
                                (
                                    "還原碼",
                                    e.record.restore_code.clone().unwrap_or("尚未提供".into()),
                                ),
                                (
                                    "長密鑰",
                                    e.record
                                        .secret
                                        .as_ref()
                                        .map(|secret| secret.hex().to_ascii_uppercase())
                                        .unwrap_or("尚未取得密鑰".into()),
                                ),
                            ] {
                                ui.separator();
                                ui.label(egui::RichText::new(label).size(12.0).color(MUTED));
                                ui.add(egui::Label::new(value).selectable(true).wrap());
                            }
                        }
                    }
                    Dialog::Delete(form) => {
                        form.show(ui, &self.model);
                        ui.colored_label(RED, "此操作無法復原。");
                        ui.horizontal(|ui| {
                            if ui
                                .add(
                                    egui::Button::new(
                                        egui::RichText::new("確認刪除").color(ON_ACCENT),
                                    )
                                    .fill(DANGER),
                                )
                                .clicked()
                            {
                                self.model.delete_entries(&form.ids);
                                self.selected.clear();
                                keep = false;
                            }
                        });
                    }
                    Dialog::Groups {
                        name,
                        rename,
                        error,
                    } => {
                        ui.label(
                            egui::RichText::new(format!(
                                "{} / 20 組 · 每組最多 100 筆",
                                self.model.groups.len()
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
                        for g in self.model.groups.clone() {
                            ui.scope_builder(
                                egui::UiBuilder::new()
                                    .scope_id(ui.make_persistent_id(("managed_group", g.id))),
                                |ui| {
                                    ui.horizontal(|ui| {
                                        ui.spacing_mut().item_spacing.x = 6.0;
                                        ui.spacing_mut().button_padding = Vec2::new(6.0, 4.0);
                                        let name_width = (ui.available_width() - 130.0).max(40.0);
                                        let label = format!(
                                            "{}  ({})",
                                            g.name,
                                            self.model.group_count(g.id)
                                        );
                                        if let Some(edit) =
                                            rename.as_mut().filter(|edit| edit.id == g.id)
                                        {
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
                                                        .left_text(label.clone())
                                                        .truncate(),
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
                                                        .add_sized(
                                                            [48.0, 28.0],
                                                            egui::Button::new("刪除"),
                                                        )
                                                        .clicked()
                                                    {
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
                            if let Some(edit) = rename
                                .as_ref()
                                .filter(|edit| edit.id == g.id && !edit.error.is_empty())
                            {
                                ui.colored_label(RED, &edit.error);
                            }
                        }
                        ui.separator();
                        ui.label("新增分組");
                        let new_group = egui::TextEdit::singleline(name)
                            .id(DialogEditor::NewGroupName.id())
                            .char_limit(TEXT_INPUT_LIMIT)
                            .desired_width(f32::INFINITY)
                            .hint_text("輸入名稱後按 Enter 新增")
                            .show(ui)
                            .response;
                        if new_group.lost_focus() && ui.input(|i| i.key_pressed(egui::Key::Enter)) {
                            action = Some(GroupAction::Add);
                        }
                        if !error.is_empty() {
                            ui.colored_label(RED, error.as_str());
                        }
                        // Check focus after all controls have rendered. A later widget can take it.
                        let lost_focus = !newly_focused
                            && rename_response.as_ref().is_some_and(|response| {
                                response.lost_focus() && !response.has_focus()
                            });
                        if (lost_focus || action.is_some())
                            && finish_group_rename(&mut self.model, rename, ctx)
                        {
                            self.tab_reveal = Some(self.group);
                            match action {
                                Some(GroupAction::Rename(id)) => {
                                    if let Some(group_name) = self.model.group_name(id) {
                                        DialogEditor::GroupRename.reset(ctx);
                                        *rename = Some(GroupRename {
                                            id,
                                            name: group_name.to_owned(),
                                            focus: true,
                                            error: String::new(),
                                        });
                                        error.clear();
                                    }
                                }
                                Some(GroupAction::Move(id, delta)) => {
                                    match self.model.move_group(id, delta) {
                                        Ok(()) => error.clear(),
                                        Err(e) => *error = e,
                                    }
                                }
                                Some(GroupAction::Delete(id)) => {
                                    self.open_dialog(ctx, Dialog::DeleteGroup(id));
                                    keep = false;
                                }
                                Some(GroupAction::Add) => {
                                    match self.model.add_group(name) {
                                        Ok(_) => {
                                            name.clear();
                                            DialogEditor::NewGroupName.reset(ctx);
                                            error.clear();
                                        }
                                        Err(e) => *error = e,
                                    }
                                    new_group.request_focus();
                                }
                                None => {}
                            }
                        }
                    }
                    Dialog::DeleteGroup(id) => {
                        ui.label(format!(
                            "{} 的所有驗證器將移到未分組。",
                            self.model.group_name(*id).unwrap_or("")
                        ));
                        ui.horizontal(|ui| {
                            if ui.button("確認刪除分組").clicked() {
                                match self.model.delete_group(*id) {
                                    Ok(_) => {
                                        if self.group == *id {
                                            self.switch_group(0);
                                        }
                                        keep = false;
                                    }
                                    Err(e) => self.notify(e),
                                }
                            }
                        });
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
        });
        // Modal anchors use the previous measured size. Reposition changed layouts
        // before presenting the frame; stable layouts still need only one pass.
        if previous_size.is_none_or(|size| (size - response.response.rect.size()).length() > 0.5) {
            ctx.request_discard("Modal size changed; recenter before painting");
        }
        if keep && close_requested {
            // Only the explicit close button dismisses a dialog. Backdrop clicks
            // and Escape must never discard an import draft or its preview.
            keep = match &mut dialog {
                Dialog::Groups { rename, .. } => !finish_group_rename(&mut self.model, rename, ctx),
                _ => false,
            };
        }
        if keep {
            self.dialog = Some(dialog);
        } else {
            reset_dialog_editors(ctx);
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
        self.window_chrome.show(root);
        if self.runtime.close_requested && !self.runtime.close_blocked() {
            self.cancel_drag();
            root.label("正在關閉…");
            return;
        }
        if self.startup_ui(root) {
            self.cancel_drag();
            self.show_dialog(&ctx);
            return;
        }
        if self.runtime.app.saving
            || self.runtime.app.cleaning
            || self.runtime.app.has_pending()
            || self.runtime.app.warning.is_some()
            || self.runtime.app.repository_state.needs_attention()
        {
            self.cancel_drag();
            self.saving_ui(root);
            return;
        }
        let before = self.model.change_token();
        self.drop = None;
        root.spacing_mut().item_spacing = Vec2::new(6.0, 0.0);
        self.tabs(root);
        self.search(root);
        self.entry_list(root);
        if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
            self.cancel_drag();
        }
        if ctx.input(|i| i.pointer.button_released(egui::PointerButton::Primary)) {
            if let (Some(id), Some((group, before))) = (self.dragging.take(), self.drop.take()) {
                if self.model.entry(id).is_some_and(|e| e.group_id == group) {
                    if let Err(e) = self.model.reorder_entry(id, before) {
                        self.notify(e);
                    }
                } else {
                    self.move_ids(&[id], group);
                }
            }
            self.cancel_drag();
        }
        self.footer(root);
        if self.dialog.is_none() && !ctx.egui_wants_keyboard_input() {
            if ctx.input(|i| i.modifiers.ctrl && i.key_pressed(egui::Key::A)) {
                self.selected = self.visible_ids().into_iter().collect();
            }
            if ctx.input(|i| i.key_pressed(egui::Key::Escape)) {
                self.selected.clear();
            }
            if !self.selected.is_empty() && ctx.input(|i| i.key_pressed(egui::Key::Delete)) {
                self.open_dialog(
                    &ctx,
                    Dialog::delete(self.selected.iter().copied().collect()),
                );
            }
        }
        self.show_dialog(&ctx);
        self.persist_frame_changes(before, &ctx);
    }
    fn show_notifications(&mut self, ctx: &egui::Context) {
        let Some(message) = self.notifications.visible(Instant::now()) else {
            return;
        };
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
                        ui.label(egui::RichText::new(message).color(ON_ACCENT).size(12.0));
                    });
            });
    }
}

impl eframe::App for BnAuthApp {
    fn logic(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Async workers do not depend on painting to acknowledge saves or cleanup.
        ctx.request_repaint_after(Duration::from_millis(100));
        self.window_chrome.enforce_window_state(ctx);
        if ctx.input(|i| i.viewport().visible() == Some(false)) {
            self.cancel_drag();
            self.notifications.pause(Instant::now());
        }
        self.poll_runtime(ctx);
    }
    fn ui(&mut self, root: &mut egui::Ui, _frame: &mut eframe::Frame) {
        let ctx = root.ctx().clone();
        self.app_ui(root);
        // One common exit: loading, saving, cleanup and the normal page all paint
        // important notifications before their display time starts to elapse.
        self.show_notifications(&ctx);
    }
}

fn finish_group_rename(
    model: &mut VaultModel,
    rename: &mut Option<GroupRename>,
    ctx: &egui::Context,
) -> bool {
    let Some(edit) = rename.as_mut() else {
        return true;
    };
    match model.rename_group(edit.id, &edit.name) {
        Ok(()) => {
            *rename = None;
            DialogEditor::GroupRename.reset(ctx);
            true
        }
        Err(error) => {
            edit.error = error;
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
        ..Default::default()
    };
    painter.layout_job(job)
}

fn icon_button(ui: &mut egui::Ui, icon: Icon, hint: &str, size: f32) -> egui::Response {
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
        Icon::Groups => {
            for dy in [-5.0, 0.0, 5.0] {
                p.line_segment([c + Vec2::new(-5.0, dy), c + Vec2::new(7.0, dy)], s);
                p.circle_filled(c + Vec2::new(-8.0, dy), 1.0, MUTED);
            }
        }
    }
    resp.on_hover_text(hint)
}
