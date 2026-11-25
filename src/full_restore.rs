use super::*;
use bnauth_adapters::{export_file, native};
use bnauth_core::{ErrorCode, FullRestorePreview, FullRestoreTarget, clock::ClockObservation};
use std::{path::PathBuf, sync::mpsc::TryRecvError};

pub(super) use export_file::FULL_RESTORE_TITLE as TITLE;

pub(crate) struct RestoreFile {
    pub path: PathBuf,
    pub preview: FullRestorePreview,
}

#[derive(Default)]
pub(super) struct RestoreForm {
    pub loaded: Option<RestoreFile>,
    pub confirmed: bool,
    pub error: String,
}

impl BnAuthApp {
    pub(super) fn open_full_restore(&mut self, ctx: &egui::Context) {
        if self.runtime.close_requested
            || self.runtime.full_restore_rx.is_some()
            || self.runtime.export_job.is_some()
            || self.runtime.group_import_rx.is_some()
        {
            return;
        }
        self.open_dialog(ctx, Dialog::FullRestore(Box::default()));
    }

    fn begin_full_restore(&mut self) -> Result<(), String> {
        let target = self
            .runtime
            .app
            .current
            .as_deref()
            .map(FullRestoreTarget::from);
        let primary = self.runtime.path.clone();
        let directory = self
            .runtime
            .path
            .parent()
            .ok_or_else(|| ErrorCode::ReadFailed.notice().to_string())?
            .to_path_buf();
        let owner = export_file::active_window();
        let (tx, rx) = crate::runtime::worker_channel(&self.runtime.ui_ctx);
        std::thread::Builder::new()
            .name("bnauth-full-restore".into())
            .spawn(move || {
                let result = (|| {
                    let Some(path) = export_file::choose_full_restore_path(&directory, owner)?
                    else {
                        return Ok(None);
                    };
                    let same_resolved = match (
                        std::fs::canonicalize(&path),
                        std::fs::canonicalize(&primary),
                    ) {
                        (Ok(source), Ok(target)) => {
                            source.as_os_str().eq_ignore_ascii_case(target.as_os_str())
                        }
                        _ => false,
                    };
                    if path.as_os_str().eq_ignore_ascii_case(primary.as_os_str()) || same_resolved {
                        return Err("請勿重覆匯入。".into());
                    }
                    let source = native::read_full_for_restore(&path)?;
                    let preview = bnauth_core::prepare_full_restore_from_validated(target, source)?;
                    Ok(Some(RestoreFile { path, preview }))
                })();
                tx.send(result);
            })
            .map_err(|_| ErrorCode::ReadFailed.notice().to_string())?;
        self.runtime.full_restore_rx = Some(rx);
        Ok(())
    }

    pub(super) fn poll_full_restore(&mut self) {
        let result = self
            .runtime
            .full_restore_rx
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err(ErrorCode::ReadFailed.notice().to_string()))
                }
            });
        let Some(result) = result else { return };
        self.runtime.full_restore_rx = None;
        match result {
            Ok(Some(loaded)) => {
                if let Some(Dialog::FullRestore(form)) = &mut self.dialog {
                    form.loaded = Some(loaded);
                    form.confirmed = false;
                    form.error.clear();
                }
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(Dialog::FullRestore(form)) = &mut self.dialog {
                    form.loaded = None;
                    form.confirmed = false;
                    form.error = error;
                } else {
                    self.warn(error);
                }
            }
        }
    }

    pub(super) fn full_restore_dialog(
        &mut self,
        ui: &mut egui::Ui,
        form: &mut RestoreForm,
    ) -> bool {
        ui.add_space(8.0);
        ui.label("選擇完整的 .bnauth 備份檔案，匯入全部分組名稱、驗證器資料及排序。");
        ui.colored_label(RED, "完整還原會取代目前資料庫。");
        if !ErrorCode::GroupBackupSelected.matches_message(&form.error) {
            ui.label(ErrorCode::GroupBackupSelected.default_detail().unwrap_or_default());
        }
        ui.add_space(8.0);
        let busy = self.runtime.full_restore_rx.is_some();
        if busy {
            ui.spinner();
            ui.label("請選擇完整備份，正在讀取與驗證");
        }
        if ui
            .add_enabled(!busy, egui::Button::new("選擇檔案"))
            .clicked()
            && let Err(error) = self.begin_full_restore()
        {
            form.error = error;
        }
        if let Some(loaded) = &mut form.loaded {
            let preview = &loaded.preview;
            let snapshot = &preview.candidate;
            ui.add_space(8.0);
            ui.label(format!(
                "檔案：{}",
                loaded
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));
            ui.label(format!(
                "備份包含 {} 個分組、{} 個驗證器。",
                snapshot.groups.len(),
                snapshot.authenticators.len()
            ));
            if self.runtime.app.current.is_some() {
                ui.colored_label(RED, "目前資料將被全部取代。");
            } else {
                ui.label("將在 EXE 同目錄建立 BNAuth.bnauth。");
            }
            ui.add_space(8.0);
            for (index, group) in snapshot.groups.iter().enumerate() {
                let count = preview
                    .group_counts
                    .get(&group.id)
                    .copied()
                    .unwrap_or_default();
                ui.label(format!("{}. {} · {} 個", index + 1, group.name, count));
            }
            ui.add_space(12.0);
            if preview.base.is_some() {
                ui.checkbox(&mut form.confirmed, "我已知曉目前資料將被全部取代。");
            }
            if ui
                .add_enabled(
                    !busy && (preview.base.is_none() || form.confirmed),
                    egui::Button::new(egui::RichText::new("還原").color(ON_ACCENT)).fill(BLUE),
                )
                .clicked()
            {
                let current_base = self
                    .runtime
                    .app
                    .current
                    .as_ref()
                    .map(|v| (v.vault_id, v.revision));
                if current_base != preview.base {
                    form.error = "資料庫已變更，請重新選擇備份並預覽。".into();
                } else {
                    self.observe_shared_clock(ClockObservation::now());
                    let invalid_backup_clock = crate::runtime::RuntimeState::unusable_backup_clock(
                        preview.candidate.service_time_sync.battlenet.as_ref(),
                    );
                    let replacement_clock = invalid_backup_clock
                        .then(|| self.runtime.clock.last_success().cloned())
                        .flatten();
                    let submitted = self.runtime.app.restore_from_preview(|| {
                        let mut candidate = form
                            .loaded
                            .take()
                            .expect("confirmed restore preview")
                            .preview
                            .candidate;
                        if invalid_backup_clock {
                            candidate
                                .service_time_sync
                                .battlenet
                                .clone_from(&replacement_clock);
                        }
                        candidate
                    });
                    if form.loaded.is_none() {
                        self.runtime.pending_full_restore = true;
                        self.runtime.pending_clock_merge = invalid_backup_clock;
                        self.runtime.clock_save_manual =
                            replacement_clock.is_some() && self.runtime.pending_clock_manual;
                        self.runtime.clock_save_inflight = replacement_clock;
                        if let Err(error) = submitted {
                            self.runtime.app.error = Some(error);
                        }
                        return false;
                    }
                    if let Err(error) = submitted {
                        form.error = error;
                    }
                }
            }
        }
        if !form.error.is_empty() {
            ui.colored_label(RED, &form.error);
        }
        true
    }
}
