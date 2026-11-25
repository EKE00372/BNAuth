use super::*;
use crate::runtime::{GroupImportFile, GroupImportForm, GroupImportResponse};
use bnauth_adapters::{export_file, native};
use bnauth_core::{DEFAULT_GROUP_NAME, ErrorCode, ValidatedBackup};
use std::sync::mpsc::TryRecvError;

pub(super) use export_file::GROUP_IMPORT_TITLE as TITLE;

impl BnAuthApp {
    pub(super) fn group_import_menu(&mut self, ui: &mut egui::Ui) {
        let read_only = !self.runtime.app.repository_state.allows_writes();
        let entry_limit_reached = self.model.entry_limit_reached();
        let group_limit = crate::model::MAX_GROUPS;
        let group_limit_reached = self.model.groups.len() >= group_limit;
        let response = ui.add_enabled(
            !read_only && !entry_limit_reached && !group_limit_reached,
            egui::Button::new(TITLE),
        );
        if read_only {
            response.on_disabled_hover_text(READ_ONLY_UPDATE_NOTICE);
        } else if entry_limit_reached {
            response.on_disabled_hover_text(ENTRY_LIMIT_NOTICE);
        } else if group_limit_reached {
            response.on_disabled_hover_text(format!(
                "已達 {group_limit} 個分組上限（含{DEFAULT_GROUP_NAME}）。"
            ));
        } else if response.clicked() {
            self.open_group_import(ui.ctx());
            ui.close();
        }
    }

    pub(super) fn open_group_import(&mut self, ctx: &egui::Context) {
        if self.runtime.group_import_rx.is_some()
            || self.runtime.export_job.is_some()
            || self.runtime.close_requested
        {
            return;
        }
        self.open_dialog(ctx, Dialog::GroupImport(Box::default()));
    }

    pub(super) fn begin_group_import(&mut self) -> Result<(), String> {
        if self.runtime.app.current.is_none() {
            return Err("請先建立或開啟資料庫。".into());
        }
        let directory = self
            .runtime
            .path
            .parent()
            .ok_or_else(|| ErrorCode::ReadFailed.notice().to_string())?
            .to_path_buf();
        let owner = export_file::active_window();
        self.start_group_import_job(move || {
            let Some(path) = export_file::choose_group_import_path(&directory, owner)? else {
                return Ok(None);
            };
            let source = Box::new(native::read_group_for_import(&path)?);
            Ok(Some(GroupImportResponse::Selected { path, source }))
        })
    }

    fn prepare_group_import_preview(&mut self, source: Box<ValidatedBackup>) -> Result<(), String> {
        let target = self
            .runtime
            .app
            .current
            .as_ref()
            .ok_or("請先建立或開啟資料庫。")?;
        bnauth_core::check_group_import_capacity(target, source.snapshot())?;
        let target = std::sync::Arc::clone(target);
        self.start_group_import_job(move || {
            let preview = bnauth_core::prepare_group_import_from_validated(
                std::sync::Arc::unwrap_or_clone(target),
                *source,
            );
            Ok(Some(GroupImportResponse::Prepared(Box::new(preview))))
        })
    }

    fn start_group_import_job(
        &mut self,
        job: impl FnOnce() -> Result<Option<GroupImportResponse>, String> + Send + 'static,
    ) -> Result<(), String> {
        let (tx, rx) = crate::runtime::worker_channel(&self.runtime.ui_ctx);
        std::thread::Builder::new()
            .name("bnauth-group-import".into())
            .spawn(move || {
                tx.send(job());
            })
            .map_err(|_| ErrorCode::ReadFailed.notice().to_string())?;
        self.runtime.group_import_rx = Some(rx);
        Ok(())
    }

    pub(super) fn poll_group_import(&mut self) {
        let result = self
            .runtime
            .group_import_rx
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err(ErrorCode::ReadFailed.notice().to_string()))
                }
            });
        let Some(result) = result else { return };
        self.runtime.group_import_rx = None;
        match result {
            Ok(Some(GroupImportResponse::Selected { path, source })) => {
                if !matches!(self.dialog, Some(Dialog::GroupImport(_))) {
                    return;
                }
                let source_count = source.snapshot().authenticators.len();
                let loaded = (source_count > 0).then(|| GroupImportFile {
                    path,
                    source_group: source.snapshot().groups[0].name.clone(),
                    source_count,
                    preview: None,
                    preview_code: Default::default(),
                });
                let error = self
                    .prepare_group_import_preview(source)
                    .err()
                    .unwrap_or_default();
                if let Some(Dialog::GroupImport(form)) = &mut self.dialog {
                    form.loaded = loaded;
                    form.error = error;
                }
            }
            Ok(Some(GroupImportResponse::Prepared(preview))) => {
                if let Some(Dialog::GroupImport(form)) = &mut self.dialog
                    && let Some(loaded) = &mut form.loaded
                {
                    loaded.preview = Some(preview);
                }
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(Dialog::GroupImport(form)) = &mut self.dialog {
                    form.loaded = None;
                    form.error.clone_from(&error);
                }
                self.warn(error);
            }
        }
    }

    pub(super) fn group_import_dialog(
        &mut self,
        ui: &mut egui::Ui,
        form: &mut GroupImportForm,
    ) -> bool {
        ui.add_space(8.0);
        ui.label("選擇單個分組的 .bnauth 備份檔案，匯入該分組名稱、驗證器資料及排序。");
        ui.add_space(8.0);
        let busy = self.runtime.group_import_rx.is_some();
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("正在準備分組匯入");
            });
        }
        if ui
            .add_enabled(!busy, egui::Button::new("選擇檔案"))
            .clicked()
            && let Err(error) = self.begin_group_import()
        {
            form.error = error;
        }
        if let Some(loaded) = &mut form.loaded {
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
                "分組 {} 共 {} 個驗證器。",
                loaded.source_group, loaded.source_count
            ));
            if let Some(preview) = &loaded.preview {
                let group = preview.candidate.groups.last().expect("new imported group");
                let first = &preview.imported_records()[0];
                ui.label(format!("將建立「{}」分組。", group.name));
                ui.add_space(6.0);
                ui.label(format!("第一筆驗證器：{}", first.name));
                dynamic_hover_text(ui.label("目前驗證碼"), || {
                    crate::runtime::clock_status_tooltip(
                        &self.runtime.clock,
                        self.runtime.sync_rx.is_some(),
                        ui.style(),
                        RED,
                    )
                });
                let now_ms = self.runtime.frame_time;
                let code = loaded.preview_code.at(&first.secret, now_ms);
                Self::show_preview_code(ui, code, now_ms);
                ui.add_space(12.0);
                if ui
                    .add_enabled(
                        !busy,
                        egui::Button::new(egui::RichText::new("匯入").color(ON_ACCENT)).fill(BLUE),
                    )
                    .clicked()
                {
                    let runtime = &mut self.runtime;
                    if runtime
                        .app
                        .current
                        .as_ref()
                        .map(|v| (v.vault_id, v.revision))
                        != Some((preview.base_vault_id, preview.base_revision))
                    {
                        form.error = "資料庫已變更，請重新選擇檔案並預覽。".into();
                    } else {
                        let import = crate::runtime::PendingImport::Group;
                        let submitted =
                            runtime.commit_prepared_with_pending_clock(Some(import), || {
                                loaded
                                    .preview
                                    .take()
                                    .expect("confirmed group preview")
                                    .candidate
                            });
                        if loaded.preview.is_none() {
                            if let Err(error) = submitted {
                                runtime.app.error = Some(error);
                            }
                            return false;
                        }
                        if let Err(error) = submitted {
                            form.error = error;
                        }
                    }
                }
            }
        }
        if !form.error.is_empty() {
            ui.add_space(8.0);
            ui.colored_label(RED, &form.error);
        }
        true
    }
}
