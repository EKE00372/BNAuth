use super::*;
use bnauth_adapters::{export_file, native, winauth2};
use bnauth_core::{Authenticator, VaultSnapshot};
use export_file::Format;
use std::sync::mpsc::TryRecvError;
use zeroize::Zeroizing;

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

#[derive(Clone)]
enum Contents {
    Xml(Authenticator),
    Group(VaultSnapshot),
    Full(VaultSnapshot),
}

impl Contents {
    fn encode(&self, offset: Option<i64>) -> Result<Zeroizing<Vec<u8>>, String> {
        match self {
            Self::Xml(record) => Ok(Zeroizing::new(
                winauth2::encode(record, offset)?.as_bytes().to_vec(),
            )),
            Self::Group(snapshot) => native::encode_group(snapshot),
            Self::Full(snapshot) => native::encode(snapshot),
        }
    }
}

pub(super) struct ExportForm {
    contents: Contents,
    error: String,
}

impl ExportForm {
    pub(super) fn format(&self) -> Format {
        match self.contents {
            Contents::Xml(_) => Format::WinAuth2Xml,
            Contents::Group(_) => Format::Group,
            Contents::Full(_) => Format::Full,
        }
    }

    pub(super) fn name(&self) -> &str {
        match &self.contents {
            Contents::Xml(record) => &record.name,
            Contents::Group(snapshot) => &snapshot.groups[0].name,
            Contents::Full(_) => "BNAuth-full",
        }
    }
}

impl BnAuthApp {
    pub(super) fn open_full_backup(&mut self, ctx: &egui::Context) {
        if self.runtime.export_rx.is_some() || self.runtime.close_requested {
            return;
        }
        let Some(snapshot) = &self.runtime.app.current else {
            return;
        };
        self.open_dialog(
            ctx,
            Dialog::Export(Box::new(ExportForm {
                contents: Contents::Full(snapshot.clone()),
                error: String::new(),
            })),
        );
    }

    pub(super) fn open_xml_export(&mut self, ctx: &egui::Context, id: u64) {
        if self.runtime.export_rx.is_some() || self.runtime.close_requested {
            return;
        }
        let Some(entry) = self.model.entry(id) else {
            self.notify("找不到驗證器。");
            return;
        };
        let Some(record) = self.runtime.app.current.as_ref().and_then(|snapshot| {
            snapshot
                .authenticators
                .iter()
                .find(|record| record.id == entry.record.id)
        }) else {
            self.notify("請先保存驗證器，再匯出 XML。");
            return;
        };
        let error = winauth2::encode(record, None).err().unwrap_or_default();
        let form = ExportForm {
            contents: Contents::Xml(record.clone()),
            error,
        };
        self.open_dialog(ctx, Dialog::Export(Box::new(form)));
    }

    pub(super) fn open_group_export(&mut self, ctx: &egui::Context, id: u64) {
        if self.runtime.export_rx.is_some() || self.runtime.close_requested {
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
        match snapshot.export_group(group.persistent_id) {
            Ok(snapshot) => self.open_dialog(
                ctx,
                Dialog::Export(Box::new(ExportForm {
                    contents: Contents::Group(snapshot),
                    error: String::new(),
                })),
            ),
            Err(error) => self.warn(error),
        }
    }

    pub(super) fn file_export_dialog(&mut self, ui: &mut egui::Ui, form: &mut ExportForm) {
        let format = form.format();
        ui.add_space(8.0);
        ui.label(egui::RichText::new(form.name()).size(18.0));
        ui.add_space(6.0);
        match &form.contents {
            Contents::Full(snapshot) => {
                ui.label(format!(
                    "將備份全部 {} 個分組、{} 個驗證器。",
                    snapshot.groups.len(),
                    snapshot.authenticators.len()
                ));
                ui.label("保留所有分組名稱、完整驗證器資料、分頁順序及組內排序，包含空分組與尚未取得密鑰的紀錄。");
                ui.add_space(8.0);
                ui.colored_label(RED, "完整備份為明文，內含密鑰與還原資料。");
                ui.label("可從啟動畫面或分頁右鍵選單選「還原完整資料庫…」。");
            }
            Contents::Xml(record) => {
                ui.label("每個 XML 包含一筆驗證器，可用 WinAuth2 開啟並產碼。");
                ui.label("標題、備註、序號與還原碼會一併寫入 XML。");
                ui.label(
                    "未校時時，XML 使用本機時間；請先確認電腦時間正確，或在 BNAuth 校時後再匯出。",
                );
                ui.add_space(8.0);
                ui.label(egui::RichText::new("XML 為明文，內含密鑰與還原資料。").color(RED));
                ui.label("WinAuth2 會忽略附加的標題、備註與保存的還原碼；用 WinAuth2 重新保存後，這些附加資料會丟失，請保留原匯出檔。");
                ui.label(
                    egui::RichText::new(
                        "WinAuth2 畫面顯示的還原碼由它自行推算，可能與 XML 保存的值不同。",
                    )
                    .size(12.0)
                    .color(MUTED),
                );
                if record.serial.is_none() {
                    ui.add_space(6.0);
                    ui.label("這筆資料沒有序號，會匯出純密鑰並保留空白序號。");
                }
            }
            Contents::Group(snapshot) => {
                ui.label(format!(
                    "將匯出此分組的全部 {} 個驗證器。",
                    snapshot.authenticators.len()
                ));
                ui.label(
                    "完整保留標題、備註、密鑰、序號、還原碼及組內順序，包含尚未取得密鑰的紀錄。",
                );
                ui.add_space(8.0);
                ui.label(egui::RichText::new("分組檔為明文，內含密鑰與還原資料。").color(RED));
                ui.label(
                    egui::RichText::new("可在分頁右鍵選單選「匯入分組檔…」，預覽後匯入。")
                        .size(12.0)
                        .color(MUTED),
                );
            }
        }
        if !form.error.is_empty() {
            ui.add_space(8.0);
            ui.colored_label(RED, &form.error);
        }
        ui.add_space(12.0);
        if self.runtime.export_rx.is_some() {
            ui.horizontal(|ui| {
                ui.spinner();
                ui.label(if self.runtime.close_requested {
                    "將在檔案匯出結束後關閉。"
                } else {
                    "正在準備匯出；請在另存視窗選擇位置，或取消另存。"
                });
            });
        } else if ui
            .add_enabled(
                !matches!(&form.contents, Contents::Xml(record) if record.secret.is_none()),
                egui::Button::new(
                    egui::RichText::new(match format {
                        Format::WinAuth2Xml => "另存 XML…",
                        Format::Group => "另存分組檔…",
                        Format::Full => "另存完整備份…",
                    })
                    .color(ON_ACCENT),
                )
                .fill(BLUE),
            )
            .clicked()
        {
            let offset = self.runtime.clock.offset_ms();
            let filename = match &form.contents {
                Contents::Xml(record) => winauth2::suggested_filename(record),
                Contents::Group(_) | Contents::Full(_) => {
                    export_file::suggested_filename(form.name(), format)
                }
            };
            let contents = form.contents.clone();
            let primary = self.runtime.path.clone();
            let owner = export_file::active_window();
            let (tx, rx) = std::sync::mpsc::channel();
            let result = std::thread::Builder::new()
                .name("bnauth-file-export".into())
                .spawn(move || {
                    let result = (|| {
                        let bytes = contents.encode(offset)?;
                        let directory = primary.parent().ok_or("無法取得程式所在目錄。")?;
                        let path = export_file::choose_path(directory, &filename, owner, format)?;
                        path.map(|path| export_file::write(&path, &bytes, format, Some(&primary)))
                            .transpose()
                    })();
                    let _ = tx.send(result);
                });
            match result {
                Ok(_) => {
                    self.runtime.export_rx = Some(rx);
                    form.error.clear();
                }
                Err(error) => form.error = format!("無法啟動檔案匯出（{error}）。"),
            }
        }
    }

    pub(super) fn poll_file_export(&mut self, ctx: &egui::Context) {
        let result = self
            .runtime
            .export_rx
            .as_ref()
            .and_then(|rx| match rx.try_recv() {
                Ok(result) => Some(result),
                Err(TryRecvError::Empty) => None,
                Err(TryRecvError::Disconnected) => {
                    Some(Err("檔案匯出工作中斷，請檢查目標檔案後重試。".into()))
                }
            });
        let Some(result) = result else { return };
        self.runtime.export_rx = None;
        match result {
            Ok(Some(path)) => {
                let label = if let Some(Dialog::Export(form)) = &self.dialog {
                    form.format().label()
                } else {
                    "檔案"
                };
                if matches!(self.dialog, Some(Dialog::Export(_))) {
                    self.dialog = None;
                    reset_dialog_editors(ctx);
                }
                self.notify(format!("已匯出{label}：{}", display_export_path(&path)));
            }
            Ok(None) => {}
            Err(error) => {
                if let Some(Dialog::Export(form)) = &mut self.dialog {
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
}
