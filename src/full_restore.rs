use super::*;
use bnauth_adapters::{export_file, native};
use bnauth_core::{FullRestorePreview, clock::ClockObservation};
use std::{path::PathBuf, sync::mpsc::TryRecvError};

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
            || self.runtime.export_rx.is_some()
            || self.runtime.group_import_rx.is_some()
        {
            return;
        }
        self.open_dialog(ctx, Dialog::FullRestore(Box::default()));
    }

    fn begin_full_restore(&mut self) -> Result<(), String> {
        let target = self.runtime.app.current.clone();
        let primary = self.runtime.path.clone();
        let directory = self
            .runtime
            .path
            .parent()
            .ok_or("無法取得程式所在目錄。")?
            .to_path_buf();
        let owner = export_file::active_window();
        let (tx, rx) = std::sync::mpsc::channel();
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
                        return Err(
                            "還原來源不能是目前的 BNAuth.bnauth 主檔，請選擇獨立的完整備份。"
                                .into(),
                        );
                    }
                    let source = native::read_full(&path)?;
                    let preview = bnauth_core::prepare_full_restore(
                        target.as_ref(),
                        &source,
                        bnauth_core::now_ms(),
                    )?;
                    native::encode(&preview.candidate)?;
                    Ok(Some(RestoreFile { path, preview }))
                })();
                let _ = tx.send(result);
            })
            .map_err(|error| format!("無法啟動完整還原（{error}）。"))?;
        self.runtime.full_restore_rx = Some(rx);
        Ok(())
    }

    pub(super) fn poll_full_restore(&mut self, ctx: &egui::Context) {
        let result = self
            .runtime
            .full_restore_rx
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("備份讀取工作中斷，請重新選擇檔案。".into()))
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

    pub(super) fn full_restore_dialog(
        &mut self,
        ui: &mut egui::Ui,
        form: &mut RestoreForm,
    ) -> bool {
        ui.add_space(8.0);
        ui.label("選擇完整 .bnauth 備份，保留全部分組名稱、驗證器完整資料及排序。");
        ui.label("分組檔請使用「匯入分組檔…」。完整還原會取代目前資料庫。");
        ui.add_space(8.0);
        let busy = self.runtime.full_restore_rx.is_some();
        if busy {
            ui.spinner();
            ui.label("請選擇完整備份，正在讀取與驗證…");
        }
        if ui
            .add_enabled(!busy, egui::Button::new("選擇完整備份…"))
            .clicked()
        {
            form.error = self.begin_full_restore().err().unwrap_or_default();
        }
        if let Some(loaded) = &form.loaded {
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
            if let Some(current) = &self.runtime.app.current {
                ui.colored_label(
                    RED,
                    format!(
                        "目前 {} 個分組、{} 個驗證器將由備份內容完整取代。",
                        current.groups.len(),
                        current.authenticators.len()
                    ),
                );
                ui.label("取代前會在 EXE 同目錄保存並驗證「BNAuth-before-restore-….bnauth」副本；副本失敗就停止還原。");
            } else {
                ui.label("將在 EXE 同目錄建立 BNAuth.bnauth。");
            }
            ui.add_space(8.0);
            for (index, group) in snapshot.groups.iter().enumerate() {
                let count = snapshot
                    .authenticators
                    .iter()
                    .filter(|record| record.group_id == group.id)
                    .count();
                ui.label(format!("{}. {} · {} 個", index + 1, group.name, count));
            }
            ui.add_space(12.0);
            if preview.base.is_some() {
                ui.checkbox(&mut form.confirmed, "我確認以備份取代目前全部資料");
            }
            if ui
                .add_enabled(
                    !busy && (preview.base.is_none() || form.confirmed),
                    egui::Button::new(egui::RichText::new("確認還原並保存").color(ON_ACCENT))
                        .fill(BLUE),
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
                    let context = self.runtime.clock_storage_context();
                    self.observe_shared_clock(ClockObservation::now(), &context);
                    let mut candidate = preview.candidate.clone();
                    if candidate
                        .service_time_sync
                        .battlenet
                        .as_ref()
                        .is_some_and(|record| {
                            self.runtime.invalidated_clock_records.contains(record)
                        })
                    {
                        // The restore provenance already preserves the source
                        // result; do not activate and then rewrite a known-invalid one.
                        candidate.service_time_sync.battlenet = None;
                    }
                    match self.runtime.app.restore(candidate, preview.new_secret) {
                        Ok(()) => {
                            self.runtime.pending_full_restore = true;
                            return false;
                        }
                        Err(error) => form.error = error,
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
