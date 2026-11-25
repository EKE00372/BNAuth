use super::*;
use crate::otp_cache::PreviewCode;
use bnauth_core::{
    ErrorCode, ImportPreview, RecoveryDestination, RecoveryMaterial, RecoveryTarget, Secret,
};
#[cfg(not(test))]
use std::sync::Arc;
use std::sync::mpsc::{Receiver, TryRecvError};
use zeroize::Zeroizing;

pub(super) const TITLE: &str = "從 Email 恢復";
const REQUEST_LIMIT: Duration = Duration::from_secs(30);
pub(super) type Reply = Result<Secret, String>;
#[cfg(test)]
type Dispatch =
    Box<dyn Fn(&str, &RecoveryMaterial, Instant) -> Result<Receiver<Reply>, &'static str>>;

#[derive(Default)]
pub(crate) struct Recovery {
    #[cfg(test)]
    pub(super) dispatch: Option<Dispatch>,
    work: Option<Work>,
}

#[derive(Default)]
pub(super) struct Form {
    email: String,
    serial: String,
    restore: Zeroizing<String>,
    invalid_email: bool,
    invalid_serial: bool,
    invalid_restore: bool,
    invalid_name: bool,
    name: String,
    description: String,
    keep_email: bool,
    group: u64,
    error: String,
}

fn valid_email(email: &str) -> bool {
    email.len() <= 254
        && email.split_once('@').is_some_and(|(name, domain)| {
            !name.is_empty()
                && !domain.is_empty()
                && !domain.contains(['@', ',', '，', ';', '；'])
                && !domain.contains("..")
                && !email.chars().any(|c| c.is_whitespace() || c.is_control())
        })
}

enum Work {
    Waiting {
        reply: Receiver<Reply>,
        deadline: Instant,
        // Dropping the pending job also tells its worker to stop consuming the response.
        #[cfg(not(test))]
        _active: Arc<()>,
        destination: RecoveryDestination,
        material: RecoveryMaterial,
    },
    Received(Box<Received>),
}

struct Received {
    preview: Result<ImportPreview, String>,
    validation_error: Option<String>,
    save_error: Option<String>,
    code: PreviewCode,
    reveal_preview: bool,
}

impl Recovery {
    pub(super) fn reply_deadline(&self) -> Option<Instant> {
        match &self.work {
            Some(Work::Waiting { deadline, .. }) => Some(*deadline),
            _ => None,
        }
    }

    pub(crate) fn in_progress(&self) -> bool {
        self.work.is_some()
    }

    pub(super) fn cancel_draft(&mut self) {
        self.work = None;
    }
}

impl BnAuthApp {
    pub(super) fn open_email_recovery(&mut self, ctx: &egui::Context) {
        self.open_dialog(
            ctx,
            Dialog::EmailRecovery(Box::new(Form {
                group: self.group,
                keep_email: true,
                ..Form::default()
            })),
        );
    }

    pub(super) fn email_recovery_dialog(&mut self, ui: &mut egui::Ui, form: &mut Form) -> bool {
        let recovering = self.runtime.recovery.in_progress();
        let editable = !self.runtime.close_requested;
        ui.label("憑帳號 Email、序列號和還原碼，取回綁定於該帳號的驗證器。");
        let (email_field, serial_field, restore_field) = ui
            .add_enabled_ui(editable && !recovering, |ui| {
                ui.label("Email *");
                let paste =
                    limited_edit::prepare_paste(ui, DialogEditor::RecoveryEmail.id(), 255, |_| {
                        true
                    });
                let email = ui.add(
                    egui::TextEdit::singleline(&mut form.email)
                        .id(DialogEditor::RecoveryEmail.id())
                        .hint_text("example@mail.com")
                        .desired_width(f32::INFINITY)
                        .char_limit(255),
                );
                drop(paste);
                if email.changed() {
                    form.invalid_email = false;
                    if form.keep_email {
                        form.email.trim().clone_into(&mut form.description);
                    }
                }
                let (serial, restore) = app_runtime::show_serial_and_restore(
                    ui,
                    &mut form.serial,
                    &mut form.restore,
                    app_runtime::RecoveryFields::Email,
                );
                if serial.changed() {
                    form.invalid_serial = false;
                }
                if restore.changed() {
                    form.invalid_restore = false;
                }
                (email, serial, restore)
            })
            .inner;
        let (name_field, metadata_changed) = ui
            .add_enabled_ui(editable, |ui| {
                let (name_field, note_field) = limited_edit::name_and_note(
                    ui,
                    (&mut form.name, DialogEditor::RecoveryName.id()),
                    (
                        &mut form.description,
                        DialogEditor::RecoveryDescription.id(),
                    ),
                    None,
                );
                if name_field.changed() {
                    form.invalid_name = false;
                }
                let note_changed = note_field.changed();
                if note_changed {
                    form.keep_email = false;
                }
                let keep_changed = ui
                    .checkbox(&mut form.keep_email, "將 Email 存為備註")
                    .changed();
                if keep_changed {
                    if form.keep_email {
                        form.email.trim().clone_into(&mut form.description);
                    } else {
                        form.description.clear();
                    }
                }
                let changed = name_field.changed() || note_changed || keep_changed;
                (name_field, changed)
            })
            .inner;
        if recovering {
            self.email_recovery_progress(ui, form, &name_field, metadata_changed);
            return self.runtime.recovery.work.is_some();
        }

        let submit = ui
            .add_enabled_ui(editable, |ui| {
                app_runtime::recovery_footer(
                    ui,
                    &self.model,
                    "恢復至",
                    &mut form.group,
                    app_runtime::ActionButtons::Single("開始恢復"),
                    true,
                )
                .0
            })
            .inner;
        let mut submitted_serial = None;
        if submit {
            form.invalid_email = !valid_email(form.email.trim());
            let serial = bnauth_core::submit_serial_input(&mut form.serial, true);
            form.invalid_serial = serial.is_err();
            submitted_serial = serial.ok().flatten();
            form.invalid_restore =
                bnauth_core::submit_restore_input(&mut form.restore, true).is_err();
            form.invalid_name = form.name.trim().is_empty();
        }
        let invalid = show_invalid_fields(
            ui,
            [
                (form.invalid_email, &email_field),
                (form.invalid_serial, &serial_field),
                (form.invalid_restore, &restore_field),
                (form.invalid_name, &name_field),
            ],
            submit,
        );
        if submit {
            if invalid {
                form.error.clear();
            } else if let Some(serial) = submitted_serial {
                form.error = self
                    .start_email_recovery(form, serial)
                    .err()
                    .unwrap_or_default();
            }
        }
        if !form.error.is_empty() {
            ui.colored_label(RED, &form.error);
        }
        true
    }

    fn start_email_recovery(
        &mut self,
        form: &Form,
        serial: bnauth_core::SubmittedSerial,
    ) -> Result<(), String> {
        let runtime = &mut self.runtime;
        if runtime.close_requested || runtime.recovery.in_progress() {
            return Err("請先完成目前的恢復工作。".into());
        }
        if !runtime.app.repository_state.allows_writes() {
            return Err(bnauth_core::READ_ONLY_UPDATE_NOTICE.into());
        }
        if runtime.app.phase != bnauth_app::Phase::Ready
            || runtime.app.saving
            || runtime.app.cleaning
            || runtime.app.has_pending()
            || runtime.app.warning.is_some()
        {
            return Err("請先確認資料檔可保存。".into());
        }
        let vault = runtime.app.current.as_ref().ok_or("資料檔尚未開啟。")?;
        let group = self
            .model
            .import_group(form.group)
            .ok_or("分組已不存在。")?;
        let email = form.email.trim();
        let material = RecoveryMaterial::from_submitted_serial(serial, &form.restore)?;
        let destination = RecoveryTarget {
            group,
            name: &form.name,
            description: &form.description,
        }
        .prepare(vault)?;
        let deadline = Instant::now() + REQUEST_LIMIT;
        #[cfg(not(test))]
        let active = Arc::new(());
        #[cfg(not(test))]
        let reply = start_battle_net_recovery(
            &runtime.ui_ctx,
            email,
            &form.serial,
            &material,
            deadline,
            Arc::downgrade(&active),
        )?;
        #[cfg(test)]
        let reply = {
            let dispatch = runtime
                .recovery
                .dispatch
                .as_ref()
                .ok_or("測試未設定人工恢復回覆。")?;
            dispatch(email, &material, deadline)?
        };
        runtime.recovery.work = Some(Work::Waiting {
            reply,
            deadline,
            #[cfg(not(test))]
            _active: active,
            destination,
            material,
        });
        Ok(())
    }

    pub(super) fn poll_email_recovery(&mut self) -> bool {
        let work = &mut self.runtime.recovery.work;
        let Some(Work::Waiting {
            reply, deadline, ..
        }) = work.as_ref()
        else {
            return false;
        };
        let reply = match reply.try_recv() {
            Ok(reply) => reply,
            Err(TryRecvError::Empty) if Instant::now() < *deadline => return false,
            Err(TryRecvError::Empty | TryRecvError::Disconnected) => {
                Err(ErrorCode::retry_connection().to_string())
            }
        };
        let Some(Work::Waiting {
            destination,
            material,
            ..
        }) = work.take()
        else {
            unreachable!("polled a waiting recovery")
        };
        match reply {
            Ok(secret) => {
                let preview = self
                    .runtime
                    .app
                    .current
                    .as_ref()
                    .ok_or_else(|| "恢復時使用的資料庫已變更。".to_owned())
                    .and_then(|vault| destination.preview(vault, material, secret));
                *work = Some(Work::Received(Box::new(Received {
                    preview,
                    validation_error: None,
                    save_error: None,
                    code: PreviewCode::default(),
                    reveal_preview: true,
                })));
            }
            Err(error) => {
                if let Some(Dialog::EmailRecovery(form)) = &mut self.dialog {
                    form.error = error;
                } else {
                    self.warn(error);
                }
            }
        }
        true
    }

    fn recovery_save_available(&self) -> bool {
        let runtime = &self.runtime;
        let app = &runtime.app;
        app.phase == bnauth_app::Phase::Ready
            && !app.saving
            && !app.cleaning
            && !app.has_pending()
            && app.warning.is_none()
            && app.repository_state.allows_writes()
    }

    fn save_email_recovery(&mut self, form: &Form) {
        let runtime = &mut self.runtime;
        let Some(Work::Received(received)) = runtime.recovery.work.as_mut() else {
            return;
        };
        let Ok(preview) = &received.preview else {
            return;
        };
        let Some(vault) = runtime.app.current.as_ref() else {
            return;
        };
        let candidate = self
            .model
            .import_group(form.group)
            .ok_or_else(|| "目標分組已不存在。".to_owned())
            .and_then(|group| {
                preview.confirm(
                    vault,
                    RecoveryTarget {
                        name: &form.name,
                        description: &form.description,
                        group,
                    },
                )
            });
        let candidate = match candidate {
            Ok(candidate) => candidate,
            Err(error) => {
                received.validation_error = Some(error);
                return;
            }
        };
        received.validation_error = None;
        let import = crate::runtime::PendingImport::FromEmail;
        let submitted = runtime.commit_with_pending_clock(candidate, Some(import));
        if runtime.app.has_pending() {
            runtime.recovery.work = None;
        }
        if let Err(error) = submitted {
            if let Some(Work::Received(received)) = runtime.recovery.work.as_mut() {
                received.save_error = Some(error);
            } else {
                runtime.app.error = Some(error);
            }
        }
    }

    fn email_recovery_progress(
        &mut self,
        ui: &mut egui::Ui,
        form: &mut Form,
        name_field: &egui::Response,
        metadata_changed: bool,
    ) {
        let save_available = self.recovery_save_available();
        let Some(Work::Received(received)) = &mut self.runtime.recovery.work else {
            ui.spinner();
            ui.label("正在等待恢復結果");
            return;
        };
        let preview = match &received.preview {
            Ok(preview) => preview,
            Err(error) => {
                ui.colored_label(RED, error);
                if ui.button("放棄").clicked() {
                    self.runtime.recovery.cancel_draft();
                }
                return;
            }
        };
        ui.add_space(8.0);
        let now_ms = self.runtime.frame_time;
        let code = received.code.at(&preview.secret, now_ms);
        Self::show_preview_code(ui, code, now_ms);
        app_runtime::show_duplicate_hint(ui, &preview.matches);
        let buttons = if received.save_error.is_some() {
            app_runtime::ActionButtons::Retry
        } else {
            app_runtime::ActionButtons::Confirm
        };
        let previous_group = form.group;
        let (save, cancel) = app_runtime::recovery_footer(
            ui,
            &self.model,
            "恢復至",
            &mut form.group,
            buttons,
            save_available,
        );
        if form.group != previous_group || metadata_changed {
            received.validation_error = None;
        }
        if save {
            form.invalid_name = form.name.trim().is_empty();
        }
        let invalid = show_invalid_fields(ui, [(form.invalid_name, name_field)], save);
        if save && !invalid {
            self.save_email_recovery(form);
        } else if cancel {
            self.runtime.recovery.cancel_draft();
            return;
        }
        let Some(Work::Received(received)) = &mut self.runtime.recovery.work else {
            return;
        };
        if let Some(error) = received
            .validation_error
            .as_ref()
            .or(received.save_error.as_ref())
        {
            let error = ui.colored_label(RED, error);
            if save && !invalid {
                error.scroll_to_me(None);
            }
        }
        if std::mem::take(&mut received.reveal_preview) {
            ui.scroll_to_cursor(Some(egui::Align::Max));
        }
    }
}

#[cfg(not(test))]
fn start_battle_net_recovery(
    ctx: &egui::Context,
    email: &str,
    formatted_serial: &str,
    material: &RecoveryMaterial,
    deadline: Instant,
    active: std::sync::Weak<()>,
) -> Result<Receiver<Reply>, String> {
    let email = Zeroizing::new(email.to_owned());
    let serial = formatted_serial.to_owned();
    let restore_code = Zeroizing::new(material.restore_code().to_owned());
    let (sender, receiver) = crate::runtime::worker_channel(ctx);
    std::thread::Builder::new()
        .name("bnauth-email-recovery".into())
        .spawn(move || {
            let result = bnauth_adapters::email_recovery::recover_existing(
                &email,
                &serial,
                &restore_code,
                deadline,
                || active.strong_count() == 0,
            );
            sender.send(result);
        })
        .map_err(|_| ErrorCode::retry_connection().to_string())?;
    Ok(receiver)
}
