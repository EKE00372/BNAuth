use super::*;

#[test]
fn group_preview_counts_the_whole_saved_group_and_cancel_keeps_data_and_selection() {
    let mut ui = GroupUi::new();
    let original = ui.app.runtime.app.current.clone();
    let group = ui.app.model.groups[2].clone();
    let id = ui.app.model.entries[0].id;
    ui.app.selected.insert(id);
    ui.app.query = "目前不匹配任何驗證器".into();
    ui.app.open_group_export(&ui.ctx, group.id);
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    assert_eq!(ui.points("遊戲帳號").len(), 1);
    assert_eq!(ui.points("將匯出此分組的全部 6 個驗證器。").len(), 1);
    assert_eq!(ui.points("另存分組檔…").len(), 1);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.runtime.app.current, original);
    assert_eq!(ui.app.selected, BTreeSet::from([id]));
    assert_eq!(ui.app.group, 0);
    assert_eq!(ui.app.query, "目前不匹配任何驗證器");
}

fn export_ui() -> (GroupUi, u64) {
    let mut ui = GroupUi::new();
    let empty = bnauth_core::VaultSnapshot::new_empty(IMPORT_TEST_TIME);
    let vault = bnauth_core::prepare_import(
        &empty,
        "已保存的匯出標題",
        "已保存備註",
        empty.groups[0].id,
        bnauth_core::parse_manual("3132333435363738393031323334353637383930", "", "").unwrap(),
        IMPORT_TEST_TIME,
    )
    .unwrap()
    .candidate;
    ui.use_memory_vault(vault);
    let id = ui.app.model.entries[0].id;
    ui.app.open_xml_export(&ui.ctx, id);
    ui.frame(Vec::new());
    (ui, id)
}

#[test]
fn export_preview_uses_committed_record_and_close_preserves_vault_selection_and_clock() {
    let (mut ui, id) = export_ui();
    let original = ui.app.runtime.app.current.clone();
    ui.app.selected.insert(id);
    ui.app
        .model
        .update_entry(id, "尚未保存的標題", "尚未保存的備註")
        .unwrap();
    ui.app.open_xml_export(&ui.ctx, id);
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    assert_eq!(ui.points("已保存的匯出標題").len(), 1);
    assert!(ui.points("尚未保存的標題").is_empty());
    assert_eq!(ui.points("另存 XML…").len(), 1);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.runtime.app.current, original);
    assert_eq!(ui.app.selected, BTreeSet::from([id]));
    assert!(ui.app.runtime.export_rx.is_none());
    assert!(ui.app.runtime.sync_rx.is_none());
}

#[test]
fn native_dialog_cancel_and_worker_failure_allow_retry_without_saving_the_vault() {
    let (mut ui, _) = export_ui();
    let original = ui.app.runtime.app.current.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.export_rx = Some(rx);
    assert!(ui.app.runtime.close_blocked());
    tx.send(Ok(None)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(matches!(ui.app.dialog, Some(Dialog::Export(_))));
    assert!(!ui.app.runtime.close_blocked());

    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.export_rx = Some(rx);
    ui.app.runtime.close_requested = true;
    tx.send(Err("人工磁碟寫入失敗".into())).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(!ui.app.runtime.close_requested);
    ui.frame(Vec::new());
    assert_eq!(ui.points("人工磁碟寫入失敗").len(), 1);
    assert_eq!(ui.points("另存 XML…").len(), 1);
    assert_eq!(ui.app.runtime.app.current, original);
    assert!(!ui.app.runtime.app.saving);
    assert!(ui.app.runtime.sync_rx.is_none());
}

#[test]
fn worker_success_closes_preview_and_disconnect_releases_the_close_guard() {
    let (mut ui, _) = export_ui();
    let original = ui.app.runtime.app.current.clone();
    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.export_rx = Some(rx);
    ui.frame(Vec::new());
    // The preview cannot be dismissed while the native window owns the worker.
    ui.click(ui.point("×"));
    assert!(matches!(ui.app.dialog, Some(Dialog::Export(_))));
    // The worker supplies the actual installed location, which can differ from
    // the path selected through a junction in the Save As dialog.
    tx.send(Ok(Some(std::path::PathBuf::from(
        r"\\?\D:\實際保存目錄\人工匯出.xml",
    ))))
    .unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(ui.app.dialog.is_none());
    assert!(ui.app.runtime.export_rx.is_none());
    assert_eq!(ui.app.runtime.app.current, original);
    assert_eq!(
        ui.app.notifications.visible(Instant::now()),
        Some(r"已匯出XML：D:\實際保存目錄\人工匯出.xml")
    );
    let (_tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.export_rx = Some(rx);
    drop(_tx);
    ui.app.poll_runtime(&ui.ctx);
    assert!(!ui.app.runtime.close_blocked());
}

#[test]
fn export_location_display_preserves_unc_and_volume_guid_paths() {
    use crate::app::file_export::display_export_path;
    for (actual, displayed) in [
        (r"\\?\D:\繁體目錄\匯出.xml", r"D:\繁體目錄\匯出.xml"),
        (
            r"\\?\UNC\server\share\繁體目錄\匯出.xml",
            r"\\server\share\繁體目錄\匯出.xml",
        ),
        (
            r"\\?\Volume{12345678-1234-1234-1234-123456789012}\匯出.xml",
            r"\\?\Volume{12345678-1234-1234-1234-123456789012}\匯出.xml",
        ),
        (r"\\?\D:\保留尾點.\匯出.xml", r"\\?\D:\保留尾點.\匯出.xml"),
        (
            r"\\?\UNC\server\share\保留空白 \匯出.xml",
            r"\\?\UNC\server\share\保留空白 \匯出.xml",
        ),
        (r"\\?\D:\NUL\匯出.xml", r"\\?\D:\NUL\匯出.xml"),
        (r"\\?\D:\繁體目錄\NUL.xml", r"\\?\D:\繁體目錄\NUL.xml"),
        (r"\\server\share\匯出.xml", r"\\server\share\匯出.xml"),
    ] {
        assert_eq!(display_export_path(std::path::Path::new(actual)), displayed);
    }
    let long = format!(r"\\?\D:\{}\匯出.xml", "長".repeat(260));
    assert_eq!(display_export_path(std::path::Path::new(&long)), long);
}
