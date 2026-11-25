use super::*;
use std::time::{Duration, Instant};

#[test]
fn clock_tooltip_reports_local_fallback_valid_anchor_and_latest_failure() {
    let mut clock = RuntimeClock::new();
    let style = Style::default();
    let tooltip = |clock: &RuntimeClock, syncing| {
        clock_status_tooltip(clock, syncing, &style, Color32::RED)
    };
    let job = tooltip(&clock, false);
    assert_eq!(job.text, "所有驗證器共用校時。\n尚未校時");
    assert!(
        job.sections
            .iter()
            .all(|section| section.format.color == Color32::PLACEHOLDER)
    );
    clock.note_failure(bnauth_core::ErrorCode::retry_connection().to_string());
    let job = tooltip(&clock, true);
    assert_eq!(job.text, "所有驗證器共用校時。\n正在校時\nE11：連線異常，請重試");
    let (failure, context) = job.sections.split_last().unwrap();
    let bytes = &failure.byte_range;
    assert_eq!(&job.text[bytes.start.0..bytes.end.0], "E11：連線異常，請重試");
    assert_eq!(failure.format.color, Color32::RED);
    assert!(
        context
            .iter()
            .all(|section| section.format.color == Color32::PLACEHOLDER)
    );

    let sent = Instant::now();
    let wall = bnauth_core::now_ms();
    let sample = bnauth_core::clock::validate_sample(wall, wall, wall, sent, sent).unwrap();
    clock.apply(sample).unwrap();
    let text = tooltip(&clock, false).text;
    assert!(text.contains("上次校時："));
    assert!(text.contains("RTT 0 ms"));
    assert!(!text.contains("連線異常"));
    clock.note_failure(bnauth_core::ErrorCode::retry_connection().to_string());
    let text = tooltip(&clock, false).text;
    assert_eq!(text, "所有驗證器共用校時。\nE11：連線異常，請重試");
    assert!(clock.is_synced());
}

#[cfg(windows)]
#[test]
fn disk_worker_validation_reload_and_shutdown_preserve_then_release_the_primary_guard() {
    use bnauth_app::Phase;
    use std::fs::{self, OpenOptions};

    struct Fixture(PathBuf);
    impl Drop for Fixture {
        fn drop(&mut self) {
            // Only these two paths, created by this test, are removed.
            let _ = fs::remove_file(self.0.join("BNAuth.bnauth"));
            let _ = fs::remove_dir(&self.0);
        }
    }
    fn start(path: PathBuf) -> Application {
        Application::start(Box::new(move || {
            let (repo, snapshot) = bnauth_adapters::storage::Repository::open(path.clone())?;
            Ok((Box::new(DiskRepository(repo)), snapshot))
        }))
    }
    fn wait(app: &mut Application) {
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            app.poll();
            if app.phase != Phase::Loading && !app.saving && !app.cleaning {
                assert!(app.error.is_none(), "{:?}", app.error);
                return;
            }
            assert!(Instant::now() < deadline, "disk worker did not finish");
            std::thread::sleep(Duration::from_millis(1));
        }
    }
    fn assert_write_locked(path: &std::path::Path) {
        let error = OpenOptions::new().write(true).open(path).unwrap_err();
        assert_eq!(error.raw_os_error(), Some(32));
    }

    let dir = Fixture(
        std::env::temp_dir().join(format!("BNAuth-worker-lock-test-{}", uuid::Uuid::new_v4())),
    );
    fs::create_dir(&dir.0).unwrap();
    let path = dir.0.join("BNAuth.bnauth");
    let mut app = start(path.clone());
    wait(&mut app);
    assert_eq!(app.phase, Phase::Missing);
    let empty = VaultSnapshot::new_empty();
    let candidate = bnauth_core::prepare_import(
        &empty,
        "人工驗證器",
        "",
        empty.groups[0].id,
        bnauth_core::parse_manual("3132333435363738393031323334353637383930", "", "").unwrap(),
    )
    .unwrap()
    .candidate(&empty)
    .unwrap();
    app.commit(candidate, false).unwrap();
    wait(&mut app);
    let first = app.current.as_deref().unwrap().clone();
    assert_write_locked(&path);

    // The worker must release the old repository before opening its replacement.
    app.reload().unwrap();
    wait(&mut app);
    assert_eq!(app.phase, Phase::Ready);
    assert_eq!(app.current.as_deref(), Some(&first));
    assert_write_locked(&path);
    assert_eq!(
        bnauth_adapters::native::decode(&fs::read(&path).unwrap()).unwrap(),
        first
    );

    // Submit malformed text at the public application boundary: repository
    // validation must report it asynchronously before any disk write.
    let bytes = fs::read(&path).unwrap();
    let mut invalid = first.clone();
    invalid.authenticators[0].name = "名稱\0含隱藏字元".into();
    app.commit(invalid, false).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut failed = false;
    loop {
        for event in app.poll() {
            assert!(!matches!(event, bnauth_app::Event::Committed { .. }));
            failed |= event == bnauth_app::Event::Failed;
        }
        if !app.saving {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "validation worker did not finish"
        );
        std::thread::yield_now();
    }
    assert!(failed);
    assert!(app.error.as_deref().unwrap().contains("U+0000"));
    assert!(app.has_pending());
    assert_eq!(app.current.as_deref(), Some(&first));
    assert_eq!(fs::read(&path).unwrap(), bytes);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
    assert_write_locked(&path);
    for _ in 0..3 {
        assert!(app.poll().is_empty());
        assert!(!app.saving);
        assert!(app.has_pending());
    }
    app.discard();
    assert!(!app.has_pending());
    assert!(app.error.is_none());

    let mut last = first.clone();
    last.authenticators[0].description = "背景保存完成".into();
    app.commit(last, false).unwrap();
    drop(app);
    // Drop joins the worker: the queued save is durable and its guard is released.
    let writable = OpenOptions::new().write(true).open(&path).unwrap();
    drop(writable);
    let mut edited = bnauth_adapters::native::decode(&fs::read(&path).unwrap()).unwrap();
    assert_eq!(edited.revision, first.revision + 1);
    assert_eq!(edited.authenticators[0].description, "背景保存完成");
    edited.authenticators[0].description = "關閉後外部修改".into();
    fs::write(&path, &*bnauth_adapters::native::encode(&edited).unwrap()).unwrap();
    let mut reopened = start(path.clone());
    wait(&mut reopened);
    assert_eq!(reopened.current.as_deref(), Some(&edited));
    assert_write_locked(&path);
    drop(reopened);
    assert_eq!(fs::read_dir(&dir.0).unwrap().count(), 1);
}
