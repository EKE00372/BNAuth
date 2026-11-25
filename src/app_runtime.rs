use super::*;
use crate::runtime::{ImportForm, PendingImport, SYNCING_NOTICE};
use bnauth_app::{Event, Phase};
use bnauth_core::{DEFAULT_GROUP_NAME, ErrorCode, OperationError, VaultSnapshot};

fn error_parts(message: Option<&str>, fallback: ErrorCode) -> (&str, Option<&str>) {
    message
        .and_then(OperationError::display_parts)
        .unwrap_or_else(|| {
            (
                fallback.description(),
                message.or(fallback.default_detail()),
            )
        })
}

fn show_error_detail(ui: &mut egui::Ui, error: Option<&str>, warning: Option<&str>) {
    if error.is_none() && warning.is_none() {
        return;
    }
    egui::ScrollArea::vertical()
        .id_salt("operation_error_detail")
        .max_height((ui.available_height() * 0.5).clamp(0.0, 180.0))
        .min_scrolled_height(0.0)
        .auto_shrink([false, true])
        .show(ui, |ui| {
            for text in error.into_iter().chain(warning) {
                ui.colored_label(RED, text);
            }
        });
}

fn file_status_panel(ui: &mut egui::Ui, contents: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::NONE.fill(BG).inner_margin(18).show(ui, |ui| {
        ui.set_min_size(ui.available_size());
        contents(ui);
    });
}

fn show_file_error(
    ui: &mut egui::Ui,
    description: &str,
    detail: Option<&str>,
    warning: Option<&str>,
    buttons: ActionButtons,
    action_enabled: bool,
) -> (bool, bool) {
    ui.heading(description);
    show_error_detail(ui, detail, warning);
    buttons.show_row(ui, action_enabled, |_| {})
}

pub(super) fn read_only_banner(ui: &mut egui::Ui, can_reload: bool) -> bool {
    const RELOAD: &str = "重新讀取";
    egui::Frame::NONE
        .fill(PANEL)
        .inner_margin(egui::Margin::symmetric(12, 8))
        .show(ui, |ui| {
            let width = ui.available_width();
            ui.set_min_width(width);
            let font = egui::FontSelection::Default.resolve(ui.style());
            let notice = ui
                .fonts_mut(|fonts| fonts.layout_no_wrap(READ_ONLY_UPDATE_NOTICE.into(), font, RED));
            let mut row_size = notice.size();
            if can_reload {
                let style: egui::widget_style::ButtonStyle =
                    ui.widget_style(ui.next_auto_id(), &Default::default());
                let style = style.atom_layout;
                let margin = style.frame.total_margin().sum();
                let label = ui.fonts_mut(|fonts| {
                    fonts.layout_no_wrap(RELOAD.into(), style.text_style.font_id, INK)
                });
                let button_size = (label.size() + margin).max(style.min_size);
                row_size.x += ui.spacing().item_spacing.x + button_size.x;
                row_size.y = row_size
                    .y
                    .max(button_size.y)
                    .max(ui.spacing().interact_size.y);
            }
            ui.allocate_ui_with_layout(
                egui::vec2(width, row_size.y),
                egui::Layout::left_to_right(egui::Align::Center),
                |ui| {
                    ui.add_space(((width - row_size.x) * 0.5).max(0.0));
                    ui.label(notice);
                    can_reload && ui.button(RELOAD).clicked()
                },
            )
            .inner
        })
        .inner
}

pub(super) fn show_duplicate_hint(ui: &mut egui::Ui, matches: &[(String, String)]) {
    if matches.is_empty() {
        return;
    }
    ui.label("此驗證器密鑰與下列相同：");
    for (group, name) in matches {
        ui.label(format!("「{group}」的「{name}」"));
    }
}

#[derive(Clone, Copy)]
pub(super) enum ActionButtons {
    Single(&'static str),
    Apply,
    Setup,
    Confirm,
    Retry,
}

impl ActionButtons {
    /// Lay out the optional left-side controls and right-aligned actions in tab order.
    pub(super) fn show_row(
        self,
        ui: &mut egui::Ui,
        action_enabled: bool,
        add_left: impl FnOnce(&mut egui::Ui),
    ) -> (bool, bool) {
        let (action, other) = match self {
            Self::Single(label) => (label, None),
            Self::Apply => ("套用", None),
            Self::Setup => ("建立", Some("從備份還原")),
            Self::Confirm => ("確認", Some("取消")),
            Self::Retry => ("重試", Some("放棄")),
        };
        let gap = ui.spacing().item_spacing.x;
        let style: egui::widget_style::ButtonStyle =
            ui.widget_style(ui.next_auto_id(), &Default::default());
        let margin = style.atom_layout.frame.total_margin().sum();
        let font = style.atom_layout.text_style.font_id;
        let (height, buttons_width) = ui.fonts_mut(|fonts| {
            let mut size = |label: &str| {
                fonts
                    .layout_no_wrap(label.to_owned(), font.clone(), INK)
                    .size()
                    + margin
            };
            let action_size = size(action);
            let width = action_size.x + other.map_or(0.0, |label| gap + size(label).x);
            (action_size.y, width)
        });
        let height = height
            .max(ui.spacing().icon_width + margin.y)
            .max(ui.spacing().interact_size.y);
        ui.allocate_ui_with_layout(
            egui::vec2(ui.available_width(), height),
            egui::Layout::left_to_right(egui::Align::Center),
            |ui| {
                let left_width = (ui.available_width() - buttons_width - gap).max(0.0);
                ui.allocate_ui_with_layout(
                    egui::vec2(left_width, height),
                    egui::Layout::left_to_right(egui::Align::Center),
                    |ui| {
                        ui.set_min_width(left_width);
                        add_left(ui);
                    },
                );
                let primary = |ui: &mut egui::Ui| {
                    ui.push_id(action, |ui| {
                        let button = if matches!(self, Self::Apply) {
                            egui::Button::new(egui::RichText::new(action).color(ON_ACCENT))
                                .fill(BLUE)
                        } else {
                            egui::Button::new(action)
                        };
                        ui.add_enabled(action_enabled, button).clicked()
                    })
                    .inner
                };
                let secondary =
                    |ui: &mut egui::Ui| other.is_some_and(|label| ui.button(label).clicked());
                if matches!(self, Self::Confirm) {
                    let cancel = secondary(ui);
                    (primary(ui), cancel)
                } else {
                    (primary(ui), secondary(ui))
                }
            },
        )
        .inner
    }
}

pub(super) fn recovery_footer(
    ui: &mut egui::Ui,
    model: &VaultModel,
    label: &str,
    group: &mut u64,
    buttons: ActionButtons,
    action_enabled: bool,
) -> (bool, bool) {
    buttons.show_row(ui, action_enabled, |ui| {
        ui.label(label);
        let name = model.group_name(*group).unwrap_or(DEFAULT_GROUP_NAME);
        let width = ui.available_width().clamp(36.0, 144.0);
        ui.allocate_ui(egui::vec2(width, ui.available_height()), |ui| {
            egui::ComboBox::from_id_salt("recovery_destination")
                .selected_text(clipped_button_label(
                    ui,
                    name,
                    width - ui.spacing().icon_width,
                ))
                .width(width)
                .wrap_mode(egui::TextWrapMode::Truncate)
                .show_ui(ui, |ui| {
                    for target in &model.groups {
                        ui.selectable_value(group, target.id, &target.name);
                    }
                })
                .response
                .on_hover_text(name);
        });
    })
}

pub(super) enum RecoveryFields {
    Key { embedded_serial: bool },
    Email,
}

enum MaterialField {
    Secret,
    Serial,
    RestoreCode,
}

struct AsciiImportBuffer<'a> {
    text: &'a mut String,
    limit: usize,
    uppercase: bool,
}

impl egui::TextBuffer for AsciiImportBuffer<'_> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.text
    }

    fn insert_text(&mut self, text: &str, char_index: egui::text::CharIndex) -> usize {
        let remaining = self.limit.saturating_sub(self.text.len());
        // Apply the limit to accepted characters and preserve punctuation for
        // the actual format check when previewing or starting recovery.
        let mut accepted: String = text
            .chars()
            .filter(char::is_ascii_graphic)
            .take(remaining)
            .collect();
        if self.uppercase {
            accepted.make_ascii_uppercase();
        }
        egui::TextBuffer::insert_text(self.text, &accepted, char_index)
    }

    fn insert_text_at(&mut self, cursor: &mut egui::text::CCursor, text: &str, _char_limit: usize) {
        // The default implementation truncates the raw paste before filtering.
        // This buffer applies the field's limit to accepted characters instead.
        cursor.index += self.insert_text(text, cursor.index);
    }

    fn delete_char_range(&mut self, range: std::ops::Range<egui::text::CharIndex>) {
        egui::TextBuffer::delete_char_range(self.text, range);
    }

    fn replace_with(&mut self, text: &str) {
        // egui uses replacement for Undo/Redo. Restore the exact snapshot rather
        // than filtering a previous value or changing its cursor coordinates.
        text.clone_into(self.text);
    }

    fn type_id(&self) -> std::any::TypeId {
        std::any::TypeId::of::<AsciiImportBuffer<'static>>()
    }
}

fn show_material_field(
    ui: &mut egui::Ui,
    text: &mut String,
    field: MaterialField,
    editor: DialogEditor,
) -> egui::text_edit::TextEditOutput {
    let (limit, uppercase, hint) = match field {
        MaterialField::Secret => (4096, true, ""),
        MaterialField::Serial => (64, false, "US-1234-5678-9012"),
        MaterialField::RestoreCode => (128, false, "A2C4E6G8J0"),
    };
    let id = editor.id();
    let _paste = limited_edit::prepare_paste(ui, id, limit, |ch| ch.is_ascii_graphic());
    let (focused, owns_ime) =
        ui.memory(|memory| (memory.has_focus(id), memory.owns_ime_events(id)));
    // egui deletes a selection before calling insert_text_at. Hide completely
    // rejected input from this widget without copying another field's paste.
    let rejects =
        |text: &str| !text.is_empty() && !text.bytes().any(|byte| byte.is_ascii_graphic());
    let mut hidden = Vec::new();
    ui.input_mut(|input| {
        for (index, event) in input.events.iter_mut().enumerate() {
            let reject = match event {
                egui::Event::Ime(egui::ImeEvent::Preedit { .. }) if owns_ime => true,
                egui::Event::Text(s) | egui::Event::Paste(s) if focused => rejects(s),
                egui::Event::Ime(egui::ImeEvent::Commit(s)) if owns_ime => rejects(s),
                egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                    target_node,
                    target_tree,
                    action: egui::accesskit::Action::SetValue
                        | egui::accesskit::Action::ReplaceSelectedText,
                    data: Some(egui::accesskit::ActionData::Value(s)),
                }) if *target_node == id.accesskit_id()
                    && *target_tree == egui::accesskit::TreeId::ROOT => rejects(s),
                _ => false,
            };
            if reject {
                let rejected = std::mem::replace(event, egui::Event::Text(String::new()));
                hidden.push((index, rejected));
            }
        }
    });
    let mut buffer = AsciiImportBuffer {
        text,
        limit,
        uppercase,
    };
    let output = egui::TextEdit::singleline(&mut buffer)
        .id(id)
        .desired_width(f32::INFINITY)
        .hint_text(hint)
        .show(ui);
    if !hidden.is_empty() {
        ui.input_mut(|input| {
            for (index, event) in hidden {
                input.events[index] = event;
            }
        });
    }
    output
}

pub(super) fn show_serial_and_restore(
    ui: &mut egui::Ui,
    serial: &mut String,
    restore: &mut String,
    fields: RecoveryFields,
) -> (egui::Response, egui::Response) {
    let (serial_label, serial_editor, restore_label, restore_editor) = match fields {
        RecoveryFields::Key { .. } => (
            "序號",
            DialogEditor::ImportSerial,
            "還原碼",
            DialogEditor::ImportRestore,
        ),
        RecoveryFields::Email => (
            "序號 *",
            DialogEditor::RecoverySerial,
            "還原碼 *",
            DialogEditor::RecoveryCode,
        ),
    };
    ui.columns_const(|[left, right]| {
        left.label(serial_label);
        let output = match fields {
            RecoveryFields::Key {
                embedded_serial: true,
            } => egui::TextEdit::singleline(&mut serial.as_str())
                .id(serial_editor.id())
                .desired_width(f32::INFINITY)
                .show(left),
            _ => show_material_field(left, serial, MaterialField::Serial, serial_editor),
        };
        right.label(restore_label);
        let restore =
            show_material_field(right, restore, MaterialField::RestoreCode, restore_editor);
        (output.response.response, restore.response.response)
    })
}

impl BnAuthApp {
    pub(super) fn begin_time_sync(&mut self) {
        self.begin_time_sync_for(true);
    }

    fn begin_time_sync_for(&mut self, manual: bool) {
        let runtime = &mut self.runtime;
        if runtime.close_requested
            || !runtime.app.repository_state.allows_writes()
            || (!manual && runtime.clock.is_synced())
        {
            return;
        }
        if runtime.sync_rx.is_some() {
            if manual && !runtime.sync_manual {
                runtime.clock_notices.begin_sync();
            }
            runtime.sync_manual |= manual;
            if manual {
                self.notify(SYNCING_NOTICE);
            }
            return;
        }
        runtime.clock_notices.begin_sync();
        #[cfg(not(test))]
        {
            let reference = runtime.clock.reference();
            let (tx, rx) = crate::runtime::worker_channel(&runtime.ui_ctx);
            std::thread::spawn(move || {
                tx.send(bnauth_adapters::time_sync::synchronize(reference));
            });
            runtime.sync_rx = Some(rx);
            runtime.sync_manual = manual;
        }
        #[cfg(test)]
        {
            runtime.sync_requests += 1;
        }
        if manual {
            self.notify(SYNCING_NOTICE);
        }
    }
    pub(super) fn poll_runtime(&mut self, ctx: &egui::Context) {
        self.poll_runtime_with_clock(ctx, bnauth_core::clock::ClockObservation::now);
    }

    pub(super) fn poll_runtime_with_clock(
        &mut self,
        ctx: &egui::Context,
        mut observation_source: impl FnMut() -> bnauth_core::clock::ClockObservation,
    ) {
        let observation = observation_source();
        // Reuse this reading if an ACK rebases the clock during this poll.
        self.observe_shared_clock(observation);
        let sync_result = self
            .runtime
            .sync_rx
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(reply) => Some(reply),
                Err(std::sync::mpsc::TryRecvError::Empty) => None,
                Err(std::sync::mpsc::TryRecvError::Disconnected) => {
                    Some(Err(ErrorCode::retry_connection().into()))
                }
            });
        let (raw_close, hidden) = ctx.input(|i| {
            (
                i.viewport().close_requested(),
                i.viewport().visible() == Some(false),
            )
        });
        let raw_close = if raw_close
            && self
                .dialog
                .as_ref()
                .is_some_and(Dialog::requires_writable_settings)
        {
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
            if hidden || ctx.input(|i| i.viewport().minimized == Some(true)) {
                ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
            }
            self.notify("請先關閉目前的操作視窗。");
            false
        } else {
            raw_close
        };
        self.runtime.close_requested |= raw_close;
        self.poll_group_import();
        self.poll_full_restore();
        let runtime = &mut self.runtime;
        let mut sync_after_commit = false;
        let mut retry_after_cleanup = false;
        let mut storage_completed = false;
        let mut notice = None;
        let mut retained_notice = None;
        let mut warnings = Vec::new();
        let mut load_clock = false;
        let mut preserve_manual_clock = false;
        let mut cancel_clock = false;
        for event in runtime.app.poll() {
            storage_completed = true;
            match event {
                Event::Loaded | Event::Committed { .. } => {
                    let loaded = matches!(event, Event::Loaded);
                    let clock_commit = !loaded
                        && runtime.clock_save_inflight.is_some()
                        && !runtime.pending_clock_merge;
                    load_clock |= loaded;
                    #[cfg(test)]
                    if let Some(snapshot) = &runtime.app.current
                        && (clock_commit
                            || runtime.pending_groups.is_some()
                            || runtime.pending_list_edit.is_some())
                    {
                        self.model.apply_snapshot_metadata(snapshot);
                    }
                    if !clock_commit && let Some(snapshot) = &runtime.app.current {
                        let cached =
                            self.projection
                                .is_current(&self.model, self.group, &self.query);
                        if let Some(draft) = runtime.pending_groups.take() {
                            if cached {
                                self.projection.reorder_tabs(&self.model, &draft);
                            }
                            self.model.acknowledge_groups(*draft);
                            if cached {
                                self.projection.token = Some(self.model.projection_token());
                            }
                            if self.model.group_name(self.group).is_none() {
                                self.group = 0;
                                self.selected.clear();
                            }
                            self.tab_reveal = Some(self.group);
                        } else if !loaded && let Some(edit) = runtime.pending_list_edit.take() {
                            match edit {
                                ListEdit::EditEntry(row) => {
                                    let saved = &snapshot.authenticators[row];
                                    let renamed = self.model.acknowledge_entry_edit(row, saved);
                                    if renamed && !self.query.trim().is_empty() {
                                        let visible = crate::model::name_matches(
                                            &saved.name,
                                            &self.query.trim().to_lowercase(),
                                        );
                                        if cached
                                            && self.projection.update_row(&self.model, row, visible)
                                        {
                                            self.selected.all_visible = None;
                                        }
                                        let id = self.model.entries[row].id;
                                        if !visible && self.selected.contains(&id) {
                                            self.selected.remove(&id);
                                        }
                                    }
                                }
                                ListEdit::Copy(group) => {
                                    let record =
                                        snapshot.authenticators.last().expect("saved copy");
                                    self.model.acknowledge_copy(group, record);
                                    if cached
                                        && self
                                            .projection
                                            .append_row(&self.model, self.model.entries.len() - 1)
                                    {
                                        self.selected.all_visible = None;
                                    }
                                }
                                ListEdit::Delete(change) => {
                                    let mut retain_survivors = false;
                                    if cached {
                                        let deleted = change.deleted_slots();
                                        // The row menu can delete just one item even after Select All.
                                        if self.selected.all_visible == Some(true)
                                            && deleted.len() == self.projection.ids.len()
                                        {
                                            self.projection.ids.clear();
                                            self.projection.group_hits.clear();
                                        } else if deleted.len() > 1
                                            && deleted.len()
                                                >= self.projection.ids.len().div_ceil(2)
                                        {
                                            retain_survivors = true;
                                        } else {
                                            self.projection.remove_rows(&self.model, deleted);
                                        }
                                    }
                                    self.model.acknowledge_deletion(change, snapshot);
                                    if retain_survivors {
                                        self.projection.retain_existing(&self.model);
                                    }
                                    self.selected.clear();
                                }
                                ListEdit::Move(_) => self.selected.clear(),
                                ListEdit::Reorder(_) => {}
                            }
                            if cached {
                                self.projection.token = Some(self.model.projection_token());
                            }
                        } else if matches!(
                            event,
                            Event::Committed {
                                calibrate_if_needed: true
                            }
                        ) {
                            let first_added = self.model.entries.len();
                            if let Some(group) = self.model.acknowledge_import(snapshot) {
                                self.group = group;
                                self.tab_reveal = Some(group);
                                self.query.clear();
                                self.selected.clear();
                            } else if cached {
                                for row in first_added..self.model.entries.len() {
                                    if self.projection.append_row(&self.model, row) {
                                        self.selected.all_visible = None;
                                    }
                                }
                                self.projection.token = Some(self.model.projection_token());
                            }
                        } else {
                            self.model.apply_snapshot(snapshot);
                            self.otp_cache.clear();
                        }
                    }
                    if let Event::Committed {
                        calibrate_if_needed,
                    } = event
                    {
                        let merge_clock = std::mem::take(&mut runtime.pending_clock_merge);
                        if std::mem::take(&mut runtime.pending_full_restore) {
                            self.group = 0;
                            self.tab_reveal = Some(0);
                            self.query.clear();
                            self.selected.clear();
                            load_clock = true;
                            preserve_manual_clock = merge_clock;
                            retained_notice = Some("已還原完整資料庫".into());
                        }
                        if let Some(PendingImport::FromEmail) = runtime.pending_import.take() {
                            notice = Some("已保存恢復的驗證器".into());
                        }
                        if let Some(warning) = &runtime.app.warning
                            && (!clock_commit || runtime.clock_save_manual)
                        {
                            warnings.push(warning.clone());
                        } else if notice.is_none()
                            && retained_notice.is_none()
                            && !clock_commit
                            && !runtime.clock_save_manual
                        {
                            notice = Some("已保存".to_owned());
                        }
                        sync_after_commit |= calibrate_if_needed;
                        if let Some(saved) = runtime.clock_save_inflight.take()
                            && runtime
                                .app
                                .current
                                .as_ref()
                                .and_then(|snapshot| snapshot.service_time_sync.battlenet.as_ref())
                                == Some(&saved)
                        {
                            if runtime.pending_clock_record.as_ref() == Some(&saved) {
                                runtime.pending_clock_record = None;
                                runtime.pending_clock_manual = false;
                            }
                            if std::mem::take(&mut runtime.clock_save_manual) {
                                if retained_notice.is_none() {
                                    retained_notice = notice.take();
                                }
                                notice = Some("校時成功".into());
                            }
                        }
                    }
                }
                Event::Cleaned => {
                    if std::mem::take(&mut runtime.save_after_cleanup) {
                        retry_after_cleanup = runtime.app.has_pending();
                    } else {
                        notice = Some(
                            if runtime.app.has_pending() {
                                "暫存檔已移除，但這次變更尚未保存。請按「重試」。"
                            } else {
                                "暫存檔已移除。"
                            }
                            .into(),
                        );
                    }
                }
                Event::Failed => {
                    runtime.save_after_cleanup = false;
                    cancel_clock |= runtime.app.phase == Phase::Error;
                }
            }
        }

        if load_clock {
            self.rebase_shared_clock(observation, preserve_manual_clock);
        } else if cancel_clock {
            self.cancel_clock_work();
        }
        let runtime = &mut self.runtime;
        let sync_feedback = runtime.sync_manual;
        let mut rejected_for_discontinuity = false;
        // Loading another vault may have cancelled the round after we read its reply.
        if let Some(result) = sync_result.filter(|_| runtime.sync_rx.is_some()) {
            if sync_feedback {
                self.notifications.clear_status_if(SYNCING_NOTICE);
            }
            runtime.sync_rx = None;
            runtime.sync_manual = false;
            match result {
                Ok(sample) => {
                    let record = sample.record.clone();
                    match runtime.clock.apply(sample) {
                        Ok(()) => {
                            let awaiting_manual_save = runtime.pending_clock_manual
                                && runtime.pending_clock_record != runtime.clock_save_inflight;
                            runtime.pending_clock_record = Some(record);
                            runtime.pending_clock_manual = sync_feedback || awaiting_manual_save;
                        }
                        Err(_) => {
                            // apply already records the error and observes clock continuity.
                            rejected_for_discontinuity = true;
                        }
                    }
                }
                Err(failure) => {
                    rejected_for_discontinuity = failure.is_clock_discontinuity();
                    let error = failure.into_message();
                    if sync_feedback && !rejected_for_discontinuity {
                        warnings.push(error.clone());
                    }
                    runtime.clock.note_failure(error);
                }
            }
            // apply() and note_failure() read the clock while handling the reply.
            runtime.frame_time = runtime.clock.observe(observation_source());
        }
        // Include a clock reply received while the retry was cleaning its files.
        if retry_after_cleanup && let Err(error) = runtime.retry_pending_save() {
            runtime.app.error = Some(error);
        }
        if runtime.clock_notices.should_report_discontinuity(
            runtime.clock.take_clock_jump(),
            rejected_for_discontinuity && sync_feedback,
        ) {
            warnings.push(ErrorCode::time_deviation().to_string());
        }
        for warning in warnings {
            self.warn(warning);
        }
        if let Some(message) = retained_notice {
            self.notifications.retained_status(message);
        }
        if let Some(message) = notice {
            self.notify(message);
        }
        // Export with the acknowledged records and the clock checked in this poll.
        self.poll_file_export(ctx);
        storage_completed |= self.poll_email_recovery();
        if sync_after_commit {
            self.begin_time_sync_for(false);
        }

        self.save_shared_clock_if_ready();

        let runtime = &self.runtime;
        if runtime.close_requested {
            if runtime.close_blocked() {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                if (raw_close || storage_completed)
                    && hidden
                    && !runtime.app.saving
                    && !runtime.app.cleaning
                {
                    // A completed operation can still need cleanup or a decision
                    // about a pending candidate after a taskbar close.
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Focus);
                }
            } else if !raw_close {
                ctx.send_viewport_cmd(egui::ViewportCommand::Close);
            }
        }
    }
    pub(super) fn startup_ui(&mut self, ui: &mut egui::Ui) -> bool {
        let runtime = &self.runtime;
        if runtime.app.phase != Phase::Ready
            && runtime.app.phase != Phase::Loading
            && runtime.app.shows_save_status()
        {
            self.saving_ui(ui);
            return true;
        }
        let mut restore = false;
        let mut reload = false;
        let runtime = &mut self.runtime;
        if runtime.app.phase == Phase::Ready {
            return false;
        }
        file_status_panel(ui, |ui| {
            match runtime.app.phase {
                Phase::Loading => {
                    ui.spinner();
                    ui.label("正在讀取資料");
                }
                Phase::Missing => {
                    let state = runtime.app.repository_state;
                    let writable = state.allows_writes();
                    if !writable {
                        reload = read_only_banner(ui, !state.cleanup_required);
                    }
                    ui.heading("建立驗證器資料");
                    ui.add_space(12.0);
                    let (create, restore_from_file) = ui
                        .add_enabled_ui(writable, |ui| {
                            ActionButtons::Setup.show_row(ui, true, |_| {})
                        })
                        .inner;
                    if create
                        && let Err(error) = runtime.app.commit(VaultSnapshot::new_empty(), false)
                    {
                        runtime.app.error = Some(error);
                    }
                    restore = restore_from_file;
                    if let Some(error) = &runtime.app.error {
                        let (description, detail) =
                            error_parts(Some(error), ErrorCode::WriteFailed);
                        ui.colored_label(RED, description);
                        show_error_detail(ui, detail, None);
                    }
                }
                Phase::Error => {
                    let (description, detail) = runtime
                        .app
                        .error
                        .as_deref()
                        .and_then(OperationError::display_parts)
                        .unwrap_or((ErrorCode::ReadFailed.description(), None));
                    reload = show_file_error(
                        ui,
                        description,
                        detail,
                        None,
                        ActionButtons::Single("重新讀取"),
                        true,
                    )
                    .0;
                }
                Phase::Ready => {}
            }
            if runtime.app.phase != Phase::Error {
                ui.add_space(12.0);
                ui.label(
                    egui::RichText::new(runtime.path.display().to_string())
                        .size(11.0)
                        .color(MUTED),
                );
            }
        });
        if restore {
            self.open_full_restore(ui.ctx());
        }
        if reload && let Err(error) = self.reload_with_clock_cancel() {
            self.runtime.app.error = Some(error);
        }
        true
    }
    pub(super) fn saving_ui(&mut self, ui: &mut egui::Ui) {
        let mut discard = false;
        let mut retry = false;
        let runtime = &mut self.runtime;
        file_status_panel(ui, |ui| {
            let state = runtime.app.repository_state;
            let pending = runtime.app.has_pending();
            if runtime.app.saving || (runtime.app.cleaning && pending) {
                ui.spinner();
                ui.label("正在保存");
                return;
            }
            if runtime.app.cleaning {
                ui.spinner();
                ui.label("正在移除暫存檔");
                if runtime.close_requested {
                    ui.label("將在暫存檔移除後關閉");
                }
                return;
            }
            if !pending && runtime.app.warning.is_some() {
                ui.heading("資料已保存");
                show_error_detail(ui, runtime.app.error.as_deref(), runtime.app.warning.as_deref());
                if ui.button("繼續").clicked() {
                    runtime.app.warning = None;
                }
            } else {
                let fallback = if state.read_only && !state.blocked {
                    ErrorCode::PermissionDenied
                } else {
                    ErrorCode::WriteFailed
                };
                let error = runtime.app.error.as_deref();
                let (description, detail) = error_parts(error, fallback);
                let detail = detail.filter(|_| {
                    !state.blocked
                        || !error.is_some_and(|error| ErrorCode::WriteFailed.matches_message(error))
                });
                (retry, discard) = show_file_error(
                    ui,
                    description,
                    detail,
                    runtime.app.warning.as_deref(),
                    ActionButtons::Retry,
                    !state.blocked,
                );
            }
        });
        if retry && let Err(error) = runtime.retry_pending_save() {
            runtime.app.error = Some(error);
        }
        if discard {
            self.discard_unsaved_changes();
        }
    }
    pub(super) fn show_preview_code(
        ui: &mut egui::Ui,
        code: &crate::otp_cache::DisplayCode,
        now_ms: i64,
    ) {
        if code.digits.len() != 8 {
            ui.colored_label(RED, ErrorCode::TimeError.notice().to_string());
            return;
        }
        ui.add(
            egui::Label::new(
                egui::RichText::new(code.formatted.as_str())
                    .monospace()
                    .size(27.0)
                    .color(INK),
            )
            .selectable(true),
        );
        ui.add(
            egui::ProgressBar::new(now_ms.rem_euclid(30_000) as f32 / 30_000.0)
                .desired_height(4.0)
                .fill(ACCENT),
        );
    }

    pub(super) fn import_dialog(&mut self, ui: &mut egui::Ui, form: &mut ImportForm) -> bool {
        ui.label("接受 40 位密鑰或 74 位 WinAuth 未加密 secretdata，74位資料會自動帶入序號。");
        let (name_field, note_field) = limited_edit::name_and_note(
            ui,
            (&mut form.name, DialogEditor::ImportName.id()),
            (&mut form.description, DialogEditor::ImportDescription.id()),
            None,
        );
        if name_field.changed() {
            form.invalid_name = false;
        }
        ui.label("密鑰 *");
        let secret_field = show_material_field(
            ui,
            &mut form.secret,
            MaterialField::Secret,
            DialogEditor::ImportSecret,
        )
        .response;
        let secret_changed = secret_field.changed();
        if secret_changed {
            form.invalid_secret = false;
            form.packed_key = (form.secret.len() == 74)
                .then(|| bnauth_core::ManualMaterial::from_key(&form.secret));
            let embedded = form
                .packed_key
                .as_ref()
                .and_then(|key| key.as_ref().ok())
                .and_then(|key| key.embedded_serial_display());
            if embedded.is_some() || form.serial_is_embedded {
                DialogEditor::ImportSerial.reset(ui.ctx());
                form.serial_is_embedded = embedded.is_some();
                form.invalid_serial = false;
                form.serial = embedded.unwrap_or_default();
            }
        }
        let (serial_field, restore_field) = show_serial_and_restore(
            ui,
            &mut form.serial,
            &mut form.restore,
            RecoveryFields::Key {
                embedded_serial: form.serial_is_embedded,
            },
        );
        if !form.serial_is_embedded && serial_field.changed() {
            form.invalid_serial = false;
        }
        if restore_field.changed() {
            form.invalid_restore = false;
        }
        let recovery_fields_changed =
            (!form.serial_is_embedded && serial_field.changed()) || restore_field.changed();
        if secret_changed || recovery_fields_changed {
            drop(form.take_preview());
        }
        if name_field.changed() || note_field.changed() || secret_changed || recovery_fields_changed
        {
            form.error.clear();
        }

        if let Some(preview) = &form.preview {
            ui.add_space(8.0);
            let time = self.runtime.frame_time;
            let code = form.preview_code.at(&preview.secret, time);
            Self::show_preview_code(ui, code, time);
            show_duplicate_hint(ui, &preview.matches);
        }

        let has_preview = form.preview.is_some();
        let buttons = if has_preview {
            ActionButtons::Confirm
        } else {
            ActionButtons::Single("預覽匯入")
        };
        let previous_group = form.group;
        let (clicked, cancel) =
            recovery_footer(ui, &self.model, "匯入至", &mut form.group, buttons, true);
        if std::mem::take(&mut form.reveal_preview) {
            ui.scroll_to_cursor(Some(egui::Align::Max));
        }
        if form.group != previous_group {
            form.error.clear();
        }
        if cancel {
            return false;
        }
        if clicked {
            form.invalid_name = form.name.trim().is_empty();
            let result = (|| {
                let runtime = &mut self.runtime;
                let vault = runtime.app.current.as_ref().ok_or("資料檔尚未開啟。")?;
                let group = self
                    .model
                    .import_group(form.group)
                    .ok_or("分組已不存在。")?;
                if let Some(preview) = &form.preview {
                    let candidate = preview.confirm(
                        vault,
                        bnauth_core::RecoveryTarget {
                            name: &form.name,
                            description: &form.description,
                            group,
                        },
                    )?;
                    runtime.commit_with_pending_clock(candidate, Some(PendingImport::FromKey))
                } else {
                    form.prepare_preview(vault, group)
                }
            })();
            match result {
                Ok(()) if has_preview => return false,
                Ok(()) => {
                    form.reveal_preview = true;
                    ui.ctx().request_repaint();
                }
                Err(error) if has_preview && self.runtime.app.has_pending() => {
                    self.runtime.app.error = Some(error);
                    return false;
                }
                Err(error) => form.error = error,
            }
        }

        let invalid = show_invalid_fields(
            ui,
            [
                (form.invalid_name, &name_field),
                (form.invalid_secret, &secret_field),
                (form.invalid_serial, &serial_field),
                (form.invalid_restore, &restore_field),
            ],
            clicked,
        );
        if !invalid && !form.error.is_empty() {
            let error = ui.colored_label(RED, &form.error);
            if clicked && has_preview {
                error.scroll_to_me(None);
            }
        }
        true
    }
}
