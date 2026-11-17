use super::*;
use crate::runtime::ImportForm;
use bnauth_app::{Event, Phase};
use bnauth_core::VaultSnapshot;

struct AsciiImportBuffer<'a> {
    text: &'a mut String,
    serial: bool,
}

impl egui::TextBuffer for AsciiImportBuffer<'_> {
    fn is_mutable(&self) -> bool {
        true
    }

    fn as_str(&self) -> &str {
        self.text
    }

    fn insert_text(&mut self, text: &str, char_index: egui::text::CharIndex) -> usize {
        let remaining =
            (if self.serial { 64_usize } else { 128 }).saturating_sub(self.text.chars().count());
        // Count accepted characters, so pasted separators or Unicode do not use
        // capacity. The serial's standard separators are added by its formatter.
        let accepted: String = text
            .chars()
            .filter(char::is_ascii_alphanumeric)
            .take(remaining)
            .map(|ch| ch.to_ascii_uppercase())
            .collect();
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

fn show_ascii_import_field(
    ui: &mut egui::Ui,
    text: &mut String,
    editor: DialogEditor,
    serial: bool,
) -> egui::text_edit::TextEditOutput {
    // egui deletes a selection before calling insert_text_at. Hide completely
    // rejected input from this widget so it leaves both text and selection alone.
    // Other fields receive the original events as soon as this widget returns.
    let events = ui.input_mut(|input| std::mem::take(&mut input.events));
    let field_events =
        events
            .iter()
            .filter(|event| {
                let text: &str = match event {
                    egui::Event::Text(text) | egui::Event::Paste(text) => text,
                    // Accept IME commits only. Keeping composition text out of an
                    // ASCII field also avoids a rejected candidate leaving an older
                    // accepted preedit behind in the draft.
                    egui::Event::Ime(egui::ImeEvent::Preedit { .. }) => return false,
                    egui::Event::Ime(egui::ImeEvent::Commit(text)) => text,
                    egui::Event::AccessKitActionRequest(egui::accesskit::ActionRequest {
                        action:
                            egui::accesskit::Action::SetValue
                            | egui::accesskit::Action::ReplaceSelectedText,
                        data: Some(egui::accesskit::ActionData::Value(text)),
                        ..
                    }) => text,
                    _ => return true,
                };
                text.is_empty() || text.chars().any(|ch| ch.is_ascii_alphanumeric())
            })
            .cloned()
            .collect();
    ui.input_mut(|input| input.events = field_events);
    let mut buffer = AsciiImportBuffer { text, serial };
    let mut edit = egui::TextEdit::singleline(&mut buffer)
        .id(editor.id())
        .desired_width(f32::INFINITY);
    if serial {
        edit = edit.hint_text("US-1404-0311-8994");
    }
    let output = edit.show(ui);
    ui.input_mut(|input| input.events = events);
    output
}

fn serial_cursor_after_formatting(
    original: &str,
    formatted: &str,
    mut cursor: egui::text::CCursor,
) -> egui::text::CCursor {
    let original_index = usize::from(cursor.index);
    let compact_index = original
        .chars()
        .take(original_index)
        .filter(char::is_ascii_alphanumeric)
        .count();
    let mut formatted_index = formatted
        .chars()
        .enumerate()
        .filter(|(_, ch)| ch.is_ascii_alphanumeric())
        .nth(compact_index)
        .map_or_else(|| formatted.chars().count(), |(index, _)| index);
    // Keep either side of an existing separator; a newly inserted separator
    // belongs before the next compact character. Trimmed edge spaces count as none.
    if original.chars().nth(original_index) == Some('-') {
        formatted_index -= 1;
    }
    cursor.index = formatted_index.into();
    cursor
}

impl BnAuthApp {
    pub(super) fn begin_time_sync(&mut self) {
        self.begin_time_sync_for(true);
    }

    fn begin_time_sync_for(&mut self, manual: bool) {
        let runtime = &mut self.runtime;
        if runtime.close_requested {
            return;
        }
        if runtime.sync_rx.is_some() {
            if manual && !runtime.sync_manual {
                runtime.clock_notices.begin_sync();
            }
            runtime.sync_manual |= manual;
            if manual {
                self.notify("正在校時…");
            }
            return;
        }
        runtime.clock_notices.begin_sync();
        let reference = runtime.clock.reference();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::spawn(move || {
            let _ = tx.send(bnauth_adapters::time_sync::synchronize(reference));
        });
        runtime.sync_rx = Some(rx);
        runtime.sync_manual = manual;
        if manual {
            self.notify("正在校時…");
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
        let mut clock_context = self.runtime.clock_storage_context();
        // Observe continuity before ACKs can replace the old saved/in-flight
        // values. Reuse this reading if an ACK then rebases the shared clock.
        self.observe_shared_clock(observation, &clock_context);
        let (raw_close, hidden) = ctx.input(|i| {
            (
                i.viewport().close_requested(),
                i.viewport().visible() == Some(false),
            )
        });
        self.runtime.close_requested |= raw_close;
        self.poll_file_export(ctx);
        self.poll_group_import(ctx);
        self.poll_full_restore(ctx);
        let runtime = &mut self.runtime;
        let mut sync_after_commit = false;
        let mut storage_completed = false;
        let mut notice = None;
        let mut warnings = Vec::new();
        let mut load_clock = false;
        let mut cancel_clock = false;
        for event in runtime.app.poll() {
            storage_completed = true;
            match event {
                Event::Loaded | Event::Committed { .. } => {
                    load_clock |= matches!(event, Event::Loaded);
                    if let Some(snapshot) = &runtime.app.current {
                        self.model.apply_snapshot(snapshot);
                        self.otp_cache.clear();
                    }
                    if let Event::Committed { new_secret } = event {
                        let clock_commit = runtime.clock_save_inflight.is_some()
                            || runtime.clock_invalidation_inflight;
                        if std::mem::take(&mut runtime.pending_full_restore) {
                            self.group = 0;
                            self.tab_reveal = Some(0);
                            self.query.clear();
                            self.selected.clear();
                            load_clock = true;
                            notice = Some(if let Some(path) = &runtime.app.restore_backup {
                                format!("已還原；還原前副本：{}", path.display())
                            } else {
                                "已還原完整資料庫".into()
                            });
                        }
                        if let Some(group) = runtime.pending_import_group.take()
                            && let Some(group) = self
                                .model
                                .groups
                                .iter()
                                .find(|entry| entry.persistent_id == group)
                        {
                            self.group = group.id;
                            self.tab_reveal = Some(group.id);
                            self.query.clear();
                            self.selected.clear();
                        }
                        if let Some(warning) = &runtime.app.warning
                            && (!clock_commit || runtime.clock_save_manual)
                        {
                            warnings.push(warning.clone());
                        } else if notice.is_none() && !clock_commit {
                            notice = Some("已保存".to_owned());
                        }
                        sync_after_commit |= new_secret;
                        if std::mem::take(&mut runtime.clock_invalidation_inflight)
                            && runtime.app.current.as_ref().is_some_and(|snapshot| {
                                snapshot.service_time_sync.battlenet.is_none()
                            })
                        {
                            runtime.pending_clock_invalidation = false;
                        }
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
                                notice = Some("校時成功並已保存".into());
                            }
                        }
                    }
                }
                Event::Cleaned => {
                    notice = Some(
                        if runtime.app.has_pending() {
                            "暫存檔已移除，但這次變更尚未保存。請按「重試保存」。"
                        } else {
                            "暫存檔已移除。"
                        }
                        .into(),
                    );
                }
                Event::Failed => cancel_clock |= runtime.app.phase == Phase::Error,
            }
        }

        if !runtime.app.has_pending() && !runtime.app.saving {
            runtime.pending_import_group = None;
            runtime.pending_full_restore = false;
            // Also handle a caller explicitly discarding a failed clock write.
            load_clock |= runtime.clock_save_inflight.is_some();
        }
        if load_clock {
            self.load_shared_clock_observed(observation, &clock_context);
            clock_context = self.runtime.clock_storage_context();
        } else if cancel_clock {
            self.cancel_clock_work();
        }
        let runtime = &mut self.runtime;
        let sync_feedback = runtime.sync_manual;
        let mut specific_sync_rejection = None;
        let sync_result = runtime.sync_rx.as_ref().and_then(|rx| rx.try_recv().ok());
        if let Some(result) = sync_result {
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
                            runtime.pending_clock_invalidation = false;
                            runtime.pending_clock_manual = sync_feedback || awaiting_manual_save;
                            if sync_feedback {
                                notice = Some("校時成功，正在保存…".into());
                            }
                        }
                        Err(error) => {
                            // apply already records the error and observes clock continuity.
                            specific_sync_rejection = Some(error);
                        }
                    }
                }
                Err(failure) => {
                    let clock_discontinuity = failure.is_clock_discontinuity();
                    let error = failure.into_message();
                    runtime.clock.note_failure(error.clone());
                    let message = format!("校時失敗：{error}");
                    if clock_discontinuity {
                        specific_sync_rejection = Some(message);
                    } else if sync_feedback {
                        warnings.push(message);
                    }
                }
            }
        }
        // A fresh result's apply() can observe a later Instant than poll start.
        // Read again here; never move its last_tick backwards to the first read.
        runtime.frame_time = runtime.clock.observe(observation_source());
        if runtime.clock.take_invalidation() {
            runtime.note_clock_invalidation_from(runtime.clock.is_synced(), &clock_context);
        }
        if let Some(message) = runtime
            .clock_notices
            .observe(runtime.clock.status(), specific_sync_rejection)
            && sync_feedback
        {
            warnings.push(message);
        }
        let closing = runtime.close_requested;
        for warning in warnings {
            self.warn(warning);
        }
        if let Some(message) = notice {
            self.notify(message);
        }
        if sync_after_commit && !closing {
            self.begin_time_sync_for(false);
        }

        self.save_shared_clock_if_ready();

        let runtime = &self.runtime;
        if closing {
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
            && (runtime.app.saving
                || runtime.app.cleaning
                || runtime.app.has_pending()
                || runtime.app.warning.is_some()
                || runtime.app.repository_state.needs_attention())
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
        egui::Frame::NONE.fill(BG).inner_margin(18).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            ui.heading("BNAuth");
            ui.add_space(12.0);
            match runtime.app.phase {
                Phase::Loading => {
                    ui.spinner();
                    ui.label("正在讀取資料…");
                }
                Phase::Missing => {
                    ui.label("建立驗證器資料");
                    ui.label("資料保存於程式同目錄的 BNAuth.bnauth。");
                    ui.label("資料及備份使用明文保存，內含密鑰與還原資訊。密碼保護尚未提供。");
                    ui.add_space(12.0);
                    if ui.button("建立資料").clicked()
                        && let Err(error) = runtime
                            .app
                            .commit(VaultSnapshot::new_empty(bnauth_core::now_ms()), false)
                    {
                        runtime.app.error = Some(error);
                    }
                    restore = ui.button("從備份還原…").clicked();
                }
                Phase::Error => {
                    ui.colored_label(RED, "無法開啟資料檔");
                    if let Some(error) = &runtime.app.error {
                        ui.label(error);
                    }
                    if ui.button("重新讀取").clicked() {
                        reload = true;
                    }
                }
                Phase::Ready => {}
            }
            ui.add_space(12.0);
            ui.label(
                egui::RichText::new(runtime.path.display().to_string())
                    .size(11.0)
                    .color(MUTED),
            );
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
        let mut reload_after_discard = false;
        let mut reload = false;
        let runtime = &mut self.runtime;
        egui::Frame::NONE.fill(BG).inner_margin(18).show(ui, |ui| {
            ui.set_min_size(ui.available_size());
            if runtime.app.cleaning {
                ui.spinner();
                ui.label("正在移除暫存檔…");
                if runtime.close_requested {
                    ui.label("將在暫存檔移除後關閉");
                }
            } else if runtime.app.saving {
                ui.spinner();
                ui.label("正在保存…");
                if runtime.close_requested {
                    ui.label("保存完成後會關閉。");
                }
            } else if !runtime.app.has_pending()
                && (runtime.app.warning.is_some() || runtime.app.repository_state.needs_attention())
            {
                let state = runtime.app.repository_state;
                ui.heading(if state.blocked {
                    "資料檔需要檢查"
                } else if runtime.app.warning.is_some() {
                    "資料已保存"
                } else {
                    "仍有暫存檔未移除"
                });
                if let Some(warning) = &runtime.app.warning {
                    ui.colored_label(RED, warning);
                }
                if let Some(error) = &runtime.app.error {
                    ui.colored_label(RED, error);
                }
                ui.label(if state.blocked {
                    "目前無法確認檔案狀態，已停止自動處理。請保留資料檔和相關暫存檔，依上方原因檢查。"
                } else if runtime.app.warning.is_some() && state.cleanup_required {
                    "這次變更已保存。下方操作只會檢查並移除暫存檔，不會再次保存資料。"
                } else if runtime.app.warning.is_some() {
                    "這次變更已保存，請確認上方提示。"
                } else {
                    "這次未保存的變更已放棄，仍有暫存檔需要移除。"
                });
                if state.cleanup_required {
                    if ui
                        .add_enabled(!state.blocked, egui::Button::new("重試移除暫存檔"))
                        .clicked()
                        && let Err(error) = runtime.app.retry_cleanup()
                    {
                        runtime.app.error = Some(error);
                    }
                } else if !state.blocked && ui.button("繼續").clicked() {
                    runtime.app.warning = None;
                }
                if state.blocked && !state.cleanup_required && ui.button("重新讀取資料檔").clicked() {
                    reload = true;
                }
                if state.needs_attention() {
                    ui.label("若仍有暫存檔，關閉後會保留；下次開啟時會再次提醒處理。");
                    if ui.button("保留檔案並關閉").clicked() {
                        runtime.allow_close_with_residue = true;
                        runtime.close_requested = true;
                        ui.ctx().send_viewport_cmd(egui::ViewportCommand::Close);
                    }
                }
            } else {
                ui.heading("變更尚未確認保存");
                if let Some(error) = &runtime.app.error {
                    ui.colored_label(RED, error);
                }
                if let Some(warning) = &runtime.app.warning {
                    ui.colored_label(RED, warning);
                }
                ui.label("這次變更尚未確認保存。請依上方提示重試，或放棄這次變更。");
                if ui
                    .add_enabled(
                        !runtime.app.repository_state.needs_attention(),
                        egui::Button::new("重試保存"),
                    )
                    .clicked()
                    && let Err(error) = runtime.app.retry()
                {
                    runtime.app.error = Some(error);
                }
                if runtime.app.repository_state.cleanup_required
                    && ui
                    .add_enabled(
                        !runtime.app.repository_state.blocked,
                        egui::Button::new("只移除暫存檔"),
                    )
                    .clicked()
                    && let Err(error) = runtime.app.retry_cleanup()
                {
                    runtime.app.error = Some(error);
                }
                if ui.button("放棄這次未保存變更").clicked() {
                    discard = true;
                }
                if ui
                    .add_enabled(
                        !runtime.app.repository_state.cleanup_required,
                        egui::Button::new("放棄變更並重新讀取資料檔"),
                    )
                    .clicked()
                {
                    discard = true;
                    reload_after_discard = true;
                }
            }
        });
        if discard {
            self.discard_unsaved_changes();
            reload |= reload_after_discard;
        }
        if reload && let Err(error) = self.reload_with_clock_cancel() {
            self.runtime.app.error = Some(error);
        }
    }
    pub(super) fn persist_frame_changes(&mut self, before: u64, ctx: &egui::Context) {
        let runtime = &mut self.runtime;
        if runtime.app.phase != Phase::Ready
            || runtime.allow_close_with_residue
            || runtime.app.saving
            || runtime.app.cleaning
            || runtime.app.has_pending()
            || runtime.app.warning.is_some()
            || runtime.app.repository_state.needs_attention()
            || self.model.change_token() == before
        {
            return;
        }
        let closing = ctx.input(|i| i.viewport().close_requested()) || runtime.close_requested;
        // Construct a full candidate only for an actual submission. Retain UI
        // identities until the pending edit is committed or explicitly discarded.
        let candidate = self.model.snapshot();
        match runtime.app.commit(candidate, false) {
            Ok(()) => {
                self.notifications.clear_status();
            }
            Err(error) => {
                if let Some(snapshot) = &runtime.app.current {
                    self.model.apply_snapshot(snapshot);
                }
                self.notify(error);
            }
        }
        if closing {
            self.runtime.close_requested = true;
            ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
        }
    }
    pub(super) fn import_dialog(&mut self, ui: &mut egui::Ui, form: &mut ImportForm) -> bool {
        if let Some(preview) = &form.preview {
            ui.label(&preview.summary);
            ui.separator();
            let entry = preview
                .existing_id
                .and_then(|id| {
                    preview
                        .candidate
                        .authenticators
                        .iter()
                        .find(|entry| entry.id == id)
                })
                .or_else(|| preview.candidate.authenticators.last());
            if let Some(entry) = entry {
                let runtime = &self.runtime;
                ui.label("目前驗證碼").on_hover_ui(|ui| {
                    ui.label(crate::runtime::clock_status_tooltip(
                        &runtime.clock,
                        runtime.sync_rx.is_some(),
                    ));
                });
                let code = self.otp_cache.code(entry, runtime.frame_time);
                if code.len() == 8 {
                    let remaining = 30 - runtime.frame_time.rem_euclid(30_000) / 1_000;
                    ui.horizontal_wrapped(|ui| {
                        ui.add(
                            egui::Label::new(
                                egui::RichText::new(format!("{} {}", &code[..4], &code[4..]))
                                    .monospace()
                                    .size(27.0)
                                    .color(INK),
                            )
                            .selectable(true),
                        );
                        ui.label(
                            egui::RichText::new(format!("剩餘 {remaining} 秒"))
                                .size(12.0)
                                .color(MUTED),
                        );
                    });
                } else {
                    ui.colored_label(RED, "無法產生驗證碼，請檢查系統時間。");
                }
                ui.add_space(8.0);
                let group = preview
                    .candidate
                    .groups
                    .iter()
                    .find(|group| group.id == entry.group_id);
                ui.label(format!("名稱：{}", entry.name));
                ui.label(format!(
                    "分組：{}",
                    group.map(|group| group.name.as_str()).unwrap_or("未分組")
                ));
                ui.label(format!(
                    "序號：{}",
                    entry
                        .serial
                        .as_deref()
                        .map(bnauth_core::display_serial)
                        .unwrap_or_else(|| "未提供".into())
                ));
                ui.add(
                    egui::Label::new(format!(
                        "長密鑰：{}",
                        entry
                            .secret
                            .as_ref()
                            .map(|secret| secret.hex().to_ascii_uppercase())
                            .unwrap_or_else(|| "尚未取得密鑰".into())
                    ))
                    .selectable(true)
                    .wrap(),
                );
                ui.add(
                    egui::Label::new(format!(
                        "還原碼：{}",
                        entry.restore_code.as_deref().unwrap_or("未提供")
                    ))
                    .selectable(true)
                    .wrap(),
                );
                if !entry.description.trim().is_empty() {
                    ui.label(format!("備註：{}", entry.description));
                }
            }
            ui.label("格式通過不代表帳號或綁定已獲官方確認。");
            let changed = preview.changed;
            let mut back = false;
            let mut confirm = false;
            ui.horizontal(|ui| {
                back = ui.button("返回修改").clicked();
                confirm = ui
                    .button(if changed {
                        "確認匯入並保存"
                    } else {
                        "完成（未變更）"
                    })
                    .clicked();
            });
            if back {
                form.preview = None;
            }
            if confirm && let Some(preview) = form.preview.take() {
                if !preview.changed {
                    return false;
                }
                match self
                    .runtime
                    .app
                    .commit(preview.candidate, preview.new_secret)
                {
                    Ok(()) => {
                        return false;
                    }
                    Err(error) => form.error = error,
                }
            }
        } else {
            ui.label("名稱（必填）");
            ui.add(
                egui::TextEdit::singleline(&mut form.name)
                    .id(DialogEditor::ImportName.id())
                    .desired_width(f32::INFINITY)
                    .char_limit(TEXT_INPUT_LIMIT),
            );
            ui.label("Email／備註（選填）");
            ui.add(
                egui::TextEdit::singleline(&mut form.description)
                    .id(DialogEditor::ImportDescription.id())
                    .desired_width(f32::INFINITY)
                    .char_limit(TEXT_INPUT_LIMIT),
            );
            ui.label("分組");
            egui::ComboBox::from_id_salt("manual_group")
                .selected_text(self.model.group_name(form.group).unwrap_or("未分組"))
                .show_ui(ui, |ui| {
                    for group in &self.model.groups {
                        ui.selectable_value(&mut form.group, group.id, &group.name);
                    }
                });
            ui.label("長密鑰（40／74位十六進位）");
            let edited = ui
                .add(
                    egui::TextEdit::singleline(&mut form.secret)
                        .id(DialogEditor::ImportSecret.id())
                        .desired_width(f32::INFINITY)
                        .char_limit(4096),
                )
                .changed();
            if edited {
                form.secret.make_ascii_uppercase();
                if let Ok(material) = bnauth_core::parse_manual(&form.secret, "", "") {
                    let inferred_serial =
                        material.serial.as_deref().map(bnauth_core::display_serial);
                    if form.serial.is_empty() || form.inferred_serial.as_ref() == Some(&form.serial)
                    {
                        form.serial = inferred_serial.clone().unwrap_or_default();
                    }
                    form.inferred_serial = inferred_serial;
                }
            }
            ui.label("序號（選填；74位資料會自動帶入）");
            let mut serial_output =
                show_ascii_import_field(ui, &mut form.serial, DialogEditor::ImportSerial, true);
            if serial_output.response.changed() {
                let formatted = bnauth_core::display_serial(&form.serial);
                if formatted != form.serial {
                    if let Some(mut range) = serial_output.cursor_range {
                        range.primary =
                            serial_cursor_after_formatting(&form.serial, &formatted, range.primary);
                        range.secondary = serial_cursor_after_formatting(
                            &form.serial,
                            &formatted,
                            range.secondary,
                        );
                        serial_output.state.cursor.set_char_range(Some(range));
                    }
                    form.serial = formatted;
                    serial_output
                        .state
                        .store(ui.ctx(), serial_output.response.id);
                }
            }
            ui.label("還原碼（選填）");
            show_ascii_import_field(ui, &mut form.restore, DialogEditor::ImportRestore, false);
            ui.label(
                egui::RichText::new("資料以明文儲存，匯入後會嘗試一次校時。")
                    .size(12.0)
                    .color(MUTED),
            );
            let next = ui.button("預覽匯入").clicked();
            if next {
                let prepared = (|| {
                    let vault = self
                        .runtime
                        .app
                        .current
                        .as_ref()
                        .ok_or("資料檔尚未開啟。")?;
                    let group = self
                        .model
                        .persistent_group(form.group)
                        .ok_or("分組已不存在。")?;
                    form.prepare_preview(vault, group, bnauth_core::now_ms())
                })();
                match prepared {
                    Ok(()) => {}
                    Err(error) => form.error = error,
                }
            }
        }
        if !form.error.is_empty() {
            ui.colored_label(RED, &form.error);
        }
        true
    }
}
