use super::*;
use crate::runtime::{ExportJob, ExportStage};
use bnauth_adapters::{export_file, native, winauth2};
use bnauth_core::{ErrorCode, Uuid};
use export_file::Format;
use std::{
    path::PathBuf,
    sync::{Arc, mpsc::TryRecvError},
};

fn export_job_error(stage: ExportStage) -> String {
    match stage {
        ExportStage::ChoosePath => ErrorCode::ReadFailed,
        ExportStage::WriteFile => ErrorCode::WriteFailed,
    }
    .notice()
    .to_string()
}

pub(super) fn display_export_path(path: &std::path::Path) -> String {
    let text = path.to_string_lossy();
    let ordinary = if let Some(unc) = text.strip_prefix(r"\\?\UNC\") {
        Some(format!(r"\\{unc}"))
    } else if let Some(dos) = text.strip_prefix(r"\\?\")
        && dos.as_bytes().first().is_some_and(u8::is_ascii_alphabetic)
        && dos.as_bytes().get(1) == Some(&b':')
        && dos.as_bytes().get(2) == Some(&b'\\')
    {
        Some(dos.to_owned())
    } else {
        None
    };
    // Keep verbatim syntax when removing it could change how the displayed
    // location is interpreted. The actual PathBuf always retains this syntax.
    if let Some(ordinary) = ordinary
        && ordinary.encode_utf16().count() < 260
        && !ordinary
            .split('\\')
            .any(|part| part.ends_with(['.', ' ']) || export_file::is_reserved_filename(part))
    {
        return ordinary;
    }
    text.into_owned()
}

enum Contents {
    Xml {
        slot: usize,
        record: bnauth_core::Authenticator,
        validation_error: Option<String>,
    },
    Group {
        view_id: u64,
        id: Uuid,
        name: String,
        is_ungrouped: bool,
        authenticators: usize,
    },
    Full {
        groups: usize,
        authenticators: usize,
    },
}

pub(super) struct ExportForm {
    contents: Contents,
    error: String,
}

impl ExportForm {
    pub(super) fn format(&self) -> Format {
        match self.contents {
            Contents::Xml { .. } => Format::WinAuth2Xml,
            Contents::Group { .. } => Format::Group,
            Contents::Full { .. } => Format::Full,
        }
    }

    pub(super) fn name(&self) -> &str {
        match &self.contents {
            Contents::Xml { record, .. } => &record.name,
            Contents::Group { name, .. } => name,
            Contents::Full { .. } => "BNAuth",
        }
    }

    pub(super) fn filename_stem(&self) -> &str {
        match &self.contents {
            Contents::Group {
                is_ungrouped: true, ..
            } => "Default",
            _ => self.name(),
        }
    }
}

impl BnAuthApp {
    pub(super) fn open_full_backup(&mut self, ctx: &egui::Context) {
        if self.runtime.export_job.is_some() || self.runtime.close_requested {
            return;
        }
        let Some(snapshot) = &self.runtime.app.current else {
            return;
        };
        self.open_dialog(
            ctx,
            Dialog::Export(Box::new(ExportForm {
                contents: Contents::Full {
                    groups: snapshot.groups.len(),
                    authenticators: snapshot.authenticators.len(),
                },
                error: String::new(),
            })),
        );
    }

    pub(super) fn open_xml_export(&mut self, ctx: &egui::Context, slot: usize) {
        if self.runtime.export_job.is_some() || self.runtime.close_requested {
            return;
        }
        let Some(record) = self
            .runtime
            .app
            .current
            .as_ref()
            .and_then(|vault| vault.authenticators.get(slot))
        else {
            return;
        };
        let validation_error = winauth2::validate(record).err();
        let form = ExportForm {
            error: validation_error.clone().unwrap_or_default(),
            contents: Contents::Xml {
                slot,
                record: record.clone(),
                validation_error,
            },
        };
        self.open_dialog(ctx, Dialog::Export(Box::new(form)));
    }

    pub(super) fn open_group_export(&mut self, ctx: &egui::Context, id: u64) {
        if self.runtime.export_job.is_some() || self.runtime.close_requested {
            return;
        }
        let Some(group) = self.model.groups.iter().find(|group| group.id == id) else {
            self.notify("找不到要匯出的分組。");
            return;
        };
        let Some(snapshot) = &self.runtime.app.current else {
            self.notify("請先保存資料，再匯出分組。");
            return;
        };
        let Some(saved) = snapshot.groups.iter().find(|g| g.id == group.persistent_id) else {
            self.warn("找不到要匯出的分組。");
            return;
        };
        let form = ExportForm {
            contents: Contents::Group {
                view_id: id,
                id: saved.id,
                name: saved.name.clone(),
                is_ungrouped: saved.is_ungrouped,
                authenticators: self.model.group_count(id),
            },
            error: String::new(),
        };
        self.open_dialog(ctx, Dialog::Export(Box::new(form)));
    }

    pub(super) fn file_export_dialog(&mut self, ui: &mut egui::Ui, form: &mut ExportForm) {
        let format = form.format();
        ui.add_space(8.0);
        ui.label(egui::RichText::new(form.name()).size(18.0));
        ui.add_space(6.0);
        match &form.contents {
            Contents::Full {
                groups,
                authenticators,
            } => {
                ui.label(format!(
                    "匯出所有資料，共 {groups} 個分組、{authenticators} 個驗證器。"
                ));
                ui.label("保留所有分組名稱、完整驗證器資料、分頁順序及組內排序，包含空分組。");
            }
            Contents::Xml { .. } => {
                ui.label("生成可用於 WinAuth2 的 XML 檔案。");
                ui.label("名稱、備註、序號與還原碼會以附加資料的形式一併寫入，明文儲存。");
                ui.add_space(8.0);
                ui.colored_label(MUTED, "WinAuth2 無法讀取附加資料，僅作為備份使用。用 WinAuth2 重新保存後，這些附加資料會丟失，請保留原匯出檔。");
                ui.label(
                    egui::RichText::new(
                        "WinAuth2 顯示的還原碼由它自行推算，可能與 XML 保存的值不同。",
                    )
                    .size(12.0)
                    .color(RED),
                );
            }
            Contents::Group { authenticators, .. } => {
                ui.label(format!(
                    "匯出此分組的驗證器，共 {authenticators} 個。"
                ));
                ui.label("分組名稱及組內排序會一併保存。");
            }
        }
        if format != Format::WinAuth2Xml {
            ui.add_space(8.0);
            ui.colored_label(RED, "檔案以明文儲存。");
        }
        if !form.error.is_empty() {
            ui.add_space(8.0);
            ui.colored_label(RED, &form.error);
        }
        ui.add_space(12.0);
        if let Some(job) = &self.runtime.export_job {
            ui.horizontal(|ui| {
                ui.spinner();
                if self.runtime.close_requested {
                    ui.label("將在檔案匯出結束後關閉。");
                } else if job.stage == ExportStage::WriteFile {
                    ui.label(format!("正在寫入{}", format.label()));
                } else {
                    ui.label("正在準備匯出；請在另存視窗選擇位置，或取消另存。");
                }
            });
        } else if ui
            .add(egui::Button::new(egui::RichText::new("另存新檔").color(ON_ACCENT)).fill(BLUE))
            .clicked()
        {
            let name = form.filename_stem().to_owned();
            let primary = self.runtime.path.clone();
            let owner = export_file::active_window();
            if let Err(error) = self.start_export_job(ExportStage::ChoosePath, move || {
                let directory = primary
                    .parent()
                    .ok_or_else(|| export_job_error(ExportStage::ChoosePath))?;
                export_file::choose_path(directory, &name, owner, format)
            }) {
                form.error = error;
            }
        }
    }

    fn start_export_job(
        &mut self,
        stage: ExportStage,
        job: impl FnOnce() -> Result<Option<PathBuf>, String> + Send + 'static,
    ) -> Result<(), String> {
        let (tx, rx) = crate::runtime::worker_channel(&self.runtime.ui_ctx);
        std::thread::Builder::new()
            .name("bnauth-file-export".into())
            .spawn(move || {
                tx.send(job());
            })
            .map_err(|_| export_job_error(stage))?;
        self.runtime.export_job = Some(ExportJob { stage, result: rx });
        Ok(())
    }

    fn write_selected_export(&mut self, path: PathBuf) -> Result<(), String> {
        let Some(Dialog::Export(form)) = &mut self.dialog else {
            return Err("請重新選擇要匯出的資料。".into());
        };
        form.error.clear();
        let format = form.format();
        let current = self
            .runtime
            .app
            .current
            .as_ref()
            .ok_or("請先建立或開啟資料庫。")?;
        let clock = self.runtime.clock.last_success();
        let snapshot = match &form.contents {
            Contents::Xml {
                slot,
                record: preview,
                validation_error,
            } => {
                let record = current
                    .authenticators
                    .get(*slot)
                    .filter(|record| record.id == preview.id)
                    .ok_or("找不到要匯出的驗證器。")?;
                if !std::ptr::eq(&**record, &**preview) {
                    winauth2::validate(record)?;
                } else if let Some(error) = validation_error {
                    return Err(error.clone());
                }
                let record = record.clone();
                let offset = clock.map(|record| record.offset_ms);
                return self.start_export_write(path, format, move || {
                    Ok(winauth2::encode_from_validated(&record, offset))
                });
            }
            Contents::Group { view_id, id, .. } => {
                let group = current
                    .groups
                    .iter()
                    .find(|group| group.id == *id)
                    .ok_or("找不到要匯出的分組。")?;
                let records = self
                    .model
                    .rows_in_group(*view_id)
                    .iter()
                    .map(|&slot| current.authenticators[slot].clone())
                    .collect();
                Arc::new(current.export_ordered_group(group.clone(), records)?)
            }
            Contents::Full { .. } => Arc::clone(current),
        };
        let service_time_sync = bnauth_core::ServiceTimeSync {
            battlenet: clock.cloned(),
        };
        self.start_export_write(path, format, move || {
            let mut file = snapshot.file_view();
            file.service_time_sync = &service_time_sync;
            native::encode_backup_from_validated(&file)
        })
    }

    fn start_export_write<B: AsRef<[u8]>>(
        &mut self,
        path: PathBuf,
        format: Format,
        encode: impl FnOnce() -> Result<B, String> + Send + 'static,
    ) -> Result<(), String> {
        let primary = self.runtime.path.clone();
        let cleanup = Arc::clone(&self.runtime.export_cleanup);
        self.start_export_job(ExportStage::WriteFile, move || {
            let mut cleanup = cleanup
                .lock()
                .map_err(|_| export_job_error(ExportStage::WriteFile))?;
            export_file::write_with_cleanup(&mut cleanup, &path, format, Some(&primary), encode)
                .map(Some)
        })
    }

    pub(super) fn poll_file_export(&mut self, ctx: &egui::Context) {
        let Some(job) = &self.runtime.export_job else {
            return;
        };
        let stage = job.stage;
        let result = match job.result.try_recv() {
            Ok(result) => result,
            Err(TryRecvError::Empty) => return,
            Err(TryRecvError::Disconnected) => Err(export_job_error(stage)),
        };
        self.runtime.export_job = None;
        let result = match (stage, result) {
            (ExportStage::ChoosePath, Ok(Some(path))) => match self.write_selected_export(path) {
                Ok(()) => return,
                Err(error) => Err(error),
            },
            (_, result) => result,
        };
        match result {
            Ok(Some(path)) => {
                let label = if let Some(Dialog::Export(form)) = &self.dialog {
                    form.format().label()
                } else {
                    "檔案"
                };
                if matches!(self.dialog, Some(Dialog::Export(_))) {
                    self.dialog = None;
                }
                self.notify(format!("已匯出{label}：{}", display_export_path(&path)));
            }
            Ok(None) => {}
            Err(error) => {
                if self.runtime.close_requested {
                    self.runtime.close_requested = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
                }
                if let Some(Dialog::Export(form)) = &mut self.dialog {
                    if error == ErrorCode::DiskFull.notice().to_string() {
                        form.error = error;
                        return;
                    }
                    form.error.clone_from(&error);
                }
                self.warn(error);
            }
        }
    }
}
