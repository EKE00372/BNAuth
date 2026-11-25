use super::*;
use crate::runtime::{ExportJob, ExportStage};
use bnauth_adapters::export_file::Format;
use std::{path::PathBuf, sync::mpsc};

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
    assert_eq!(ui.points("匯出此分組的驗證器，共 6 個。").len(), 1);
    assert_eq!(ui.points("分組名稱及組內排序會一併保存。").len(), 1);
    assert_eq!(ui.points("另存新檔").len(), 1);
    ui.click(ui.point("×"));
    assert!(ui.app.dialog.is_none());
    assert_eq!(ui.app.runtime.app.current, original);
    assert_eq!(*ui.app.selected, BTreeSet::from([id]));
    assert_eq!(ui.app.group, 0);
    assert_eq!(ui.app.query, "目前不匹配任何驗證器");
}

fn export_ui() -> GroupUi {
    let mut ui = GroupUi::new();
    let empty = bnauth_core::VaultSnapshot::new_empty();
    let vault = bnauth_core::prepare_import(
        &empty,
        "已保存的匯出標題",
        "已保存備註",
        empty.groups[0].id,
        bnauth_core::parse_manual("3132333435363738393031323334353637383930", "", "").unwrap(),
    )
    .unwrap()
    .candidate(&empty)
    .unwrap();
    ui.use_memory_vault(vault);
    ui.app.open_xml_export(&ui.ctx, 0);
    ui.frame(Vec::new());
    ui
}

fn mock_export_worker(
    ui: &mut GroupUi,
    stage: ExportStage,
) -> mpsc::Sender<Result<Option<PathBuf>, String>> {
    let (sender, result) = mpsc::channel();
    ui.app.runtime.export_job = Some(ExportJob { stage, result });
    sender
}

#[test]
fn native_dialog_cancel_and_worker_failure_allow_retry_without_saving_the_vault() {
    let mut ui = export_ui();
    let original = ui.app.runtime.app.current.clone();
    let error = bnauth_core::ErrorCode::DeleteFailed
        .manual_delete(r"C:\備份\.BNAuth-export-test.tmp")
        .to_string();
    let tx = mock_export_worker(&mut ui, ExportStage::WriteFile);
    ui.app.runtime.close_requested = true;
    tx.send(Err(error.clone())).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(!ui.app.runtime.close_requested);

    let tx = mock_export_worker(&mut ui, ExportStage::ChoosePath);
    assert!(ui.app.runtime.close_blocked());
    tx.send(Ok(None)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(matches!(ui.app.dialog, Some(Dialog::Export(_))));
    assert!(!ui.app.runtime.close_blocked());

    ui.frame(Vec::new());
    assert_eq!(ui.points(&error).len(), 1);
    assert_eq!(ui.points("另存新檔").len(), 1);
    assert_eq!(ui.app.runtime.app.current, original);
    assert!(!ui.app.runtime.app.saving);
    assert!(ui.app.runtime.sync_rx.is_none());
}

#[test]
fn exports_use_the_clock_at_path_selection_and_release_the_close_guard_after_writing() {
    for format in [Format::WinAuth2Xml, Format::Group, Format::Full] {
        let mut ui = export_ui();
        match format {
            Format::WinAuth2Xml => {}
            Format::Group => ui.app.open_group_export(&ui.ctx, 0),
            Format::Full => ui.app.open_full_backup(&ui.ctx),
        }
        let Some(Dialog::Export(form)) = &ui.app.dialog else {
            panic!("Expected export preview");
        };
        match format {
            Format::Group => {
                assert_eq!(form.name(), "預設");
                assert_eq!(form.filename_stem(), "Default");
            }
            Format::WinAuth2Xml => assert_eq!(form.filename_stem(), "已保存的匯出標題"),
            Format::Full => assert_eq!(form.filename_stem(), "BNAuth"),
        }
        let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let directory = std::env::temp_dir();
        ui.app.runtime.path = directory.join("BNAuth-test-primary.bnauth");
        let path = directory.join(format!(
            "BNAuth-test-{}.{}",
            uuid::Uuid::new_v4(),
            format.extension()
        ));
        let chooser = mock_export_worker(&mut ui, ExportStage::ChoosePath);
        ui.frame(Vec::new());
        ui.click(ui.point("×"));
        assert!(matches!(ui.app.dialog, Some(Dialog::Export(_))));

        // Both replies are ready on the first poll after the native chooser returns.
        let instant = Instant::now();
        let wall = bnauth_core::now_ms();
        let sample =
            bnauth_core::clock::validate_sample(wall + 820, wall, wall, instant, instant).unwrap();
        let record = sample.record.clone();
        let (tx, rx) = mpsc::channel();
        ui.app.runtime.sync_rx = Some(rx);
        ui.app.runtime.sync_manual = true;
        tx.send(Ok(sample)).unwrap();
        chooser.send(Ok(Some(path.clone()))).unwrap();
        ui.app.poll_runtime(&ui.ctx);
        assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
        assert!(ui.app.runtime.close_blocked());

        let deadline = Instant::now() + Duration::from_secs(5);
        while ui.app.runtime.export_job.is_some() {
            assert!(Instant::now() < deadline, "export did not finish");
            ui.app.poll_file_export(&ui.ctx);
            std::thread::yield_now();
        }
        let installed = std::fs::canonicalize(&path).unwrap();
        let bytes = std::fs::read(&path).unwrap();
        std::fs::remove_file(path).unwrap();
        assert!(ui.app.dialog.is_none());
        let notice = format!(
            "已匯出{}：{}",
            format.label(),
            file_export::display_export_path(&installed)
        );
        assert_eq!(
            ui.app.notifications.visible(Instant::now()),
            Some(notice.as_str())
        );
        match format {
            Format::WinAuth2Xml => {
                let xml = String::from_utf8(bytes).unwrap();
                assert!(xml.contains("<servertimediff>820</servertimediff>"));
                assert!(xml.contains(&original.authenticators[0].name));
            }
            Format::Group | Format::Full => {
                let snapshot = if format == Format::Group {
                    bnauth_adapters::native::decode_group(&bytes)
                } else {
                    bnauth_adapters::native::decode(&bytes)
                }
                .unwrap();
                assert_eq!(snapshot.service_time_sync.battlenet.as_ref(), Some(&record));
                assert_eq!(snapshot.authenticators, original.authenticators);
            }
        }
        // The earlier manual sync is the only reason for a vault commit.
        wait_runtime(&mut ui);
        let current = ui.app.runtime.app.current.as_deref().unwrap();
        assert_eq!(current.revision, original.revision + 1);
        assert_eq!(current.service_time_sync.battlenet.as_ref(), Some(&record));
        assert_eq!(ui.app.runtime.sync_requests, 0);
        assert!(!ui.app.runtime.close_blocked());
    }
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
