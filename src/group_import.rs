use super::*;
use crate::runtime::{GroupImportFile, GroupImportForm};
use bnauth_adapters::{export_file, native};
use std::sync::mpsc::TryRecvError;

impl BnAuthApp {
    pub(super) fn open_group_import(&mut self, ctx: &egui::Context) {
        if self.runtime.group_import_rx.is_some()
            || self.runtime.export_rx.is_some()
            || self.runtime.close_requested
        {
            return;
        }
        self.open_dialog(ctx, Dialog::GroupImport(Box::default()));
    }

    pub(super) fn begin_group_import(&mut self) -> Result<(), String> {
        let target = self
            .runtime
            .app
            .current
            .clone()
            .ok_or("請先建立或開啟資料庫。")?;
        let directory = self
            .runtime
            .path
            .parent()
            .ok_or("無法取得程式所在目錄。")?
            .to_path_buf();
        let owner = export_file::active_window();
        let (tx, rx) = std::sync::mpsc::channel();
        std::thread::Builder::new()
            .name("bnauth-group-import".into())
            .spawn(move || {
                let result = (|| {
                    let Some(path) = export_file::choose_group_import_path(&directory, owner)?
                    else {
                        return Ok(None);
                    };
                    let source = native::read_group(&path)?;
                    let group_name = source.groups[0].name.clone();
                    let source_count = source.authenticators.len();
                    let preview =
                        bnauth_core::prepare_group_import(&target, &source, bnauth_core::now_ms())
                            .and_then(|preview| {
                                native::encode(&preview.candidate)?;
                                Ok(preview)
                            });
                    Ok(Some(GroupImportFile {
                        path,
                        group_name,
                        source_count,
                        preview,
                    }))
                })();
                let _ = tx.send(result);
            })
            .map_err(|error| format!("無法啟動分組匯入（{error}）。"))?;
        self.runtime.group_import_rx = Some(rx);
        Ok(())
    }

    pub(super) fn poll_group_import(&mut self, ctx: &egui::Context) {
        let result = self
            .runtime
            .group_import_rx
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("分組檔讀取工作中斷，請重新選擇檔案。".into()))
                }
            });
        let Some(result) = result else { return };
        self.runtime.group_import_rx = None;
        match result {
            Ok(Some(loaded)) => {
                if let Some(Dialog::GroupImport(form)) = &mut self.dialog {
                    form.loaded = Some(loaded);
                    form.error.clear();
                }
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(Dialog::GroupImport(form)) = &mut self.dialog {
                    form.loaded = None;
                    form.error.clone_from(&error);
                }
                if self.runtime.close_requested {
                    self.runtime.close_requested = false;
                    ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                    ctx.send_viewport_cmd(egui::ViewportCommand::Visible(true));
                    ctx.send_viewport_cmd(egui::ViewportCommand::Minimized(false));
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
        ui.label("選擇 .bnauth 分組檔，預覽後一次匯入並保存。");
        ui.label("保留來源分組名稱與驗證器完整資料，來源項目依原順序放在該組末尾。");
        ui.add_space(8.0);
        let busy = self.runtime.group_import_rx.is_some();
        if busy {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label("請選擇分組檔，正在讀取與準備預覽…");
            });
        }
        if ui
            .add_enabled(
                !busy,
                egui::Button::new(if form.loaded.is_some() {
                    "選擇其他分組檔…"
                } else {
                    "選擇分組檔…"
                }),
            )
            .clicked()
        {
            form.error = self.begin_group_import().err().unwrap_or_default();
        }
        if let Some(loaded) = &form.loaded {
            ui.add_space(8.0);
            ui.label(format!(
                "檔案：{}",
                loaded
                    .path
                    .file_name()
                    .unwrap_or_default()
                    .to_string_lossy()
            ));
            ui.label(format!("來源分組：{}", loaded.group_name));
            ui.label(format!("來源共 {} 個驗證器。", loaded.source_count));
            match &loaded.preview {
                Err(error) => {
                    ui.add_space(6.0);
                    ui.colored_label(RED, error);
                }
                Ok(preview) => {
                    ui.label(if preview.created_group {
                        "將建立來源分組。"
                    } else {
                        "將合併到現有同名分組。"
                    });
                    ui.label(format!(
                        "新增 {} 個，已存在 {} 個；補齊資料 {} 個，移入分組 {} 個。",
                        preview.added, preview.existing, preview.supplemented, preview.moved
                    ));
                    if preview.reordered {
                        ui.label("既有來源項目也會按檔案順序重新排列。");
                    }
                    if preview.moved > 0 {
                        ui.label("重複項目會移入來源分組，其他分組的剩餘項目維持相對順序。");
                    }
                    if preview.serial_warnings > 0 {
                        ui.colored_label(RED, "部分序號已由其他密鑰使用，請核對來源資料。");
                    }
                    if !preview.changed {
                        ui.label("資料已存在，沒有需要保存的變更。");
                    }
                    ui.add_space(6.0);
                    for (index, row) in preview.rows.iter().enumerate() {
                        ui.label(format!("{}. {} · {}", index + 1, row.name, row.action));
                    }
                    ui.add_space(12.0);
                    if ui
                        .add_enabled(
                            !busy,
                            egui::Button::new(
                                egui::RichText::new(if preview.changed {
                                    "確認匯入並保存"
                                } else {
                                    "完成（未變更）"
                                })
                                .color(ON_ACCENT),
                            )
                            .fill(BLUE),
                        )
                        .clicked()
                    {
                        let current = self.runtime.app.current.as_ref();
                        if !current.is_some_and(|current| {
                            current.vault_id == preview.base_vault_id
                                && current.revision == preview.base_revision
                        }) {
                            form.error = "資料庫已變更，請重新選擇檔案並預覽。".into();
                        } else if !preview.changed {
                            return false;
                        } else {
                            match self
                                .runtime
                                .app
                                .commit(preview.candidate.clone(), preview.new_secret)
                            {
                                Ok(()) => {
                                    self.runtime.pending_import_group = Some(preview.group_id);
                                    return false;
                                }
                                Err(error) => form.error = error,
                            }
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
