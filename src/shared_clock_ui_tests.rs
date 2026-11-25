use super::*;
use bnauth_core::clock::{ClockObservation, RuntimeClock};
use bnauth_core::{TimeSyncRecord, VaultSnapshot, clock::SyncSample};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};

struct Control {
    released: AtomicBool,
    failures: AtomicUsize,
    candidates: Mutex<Vec<VaultSnapshot>>,
}
struct ReleaseOnDrop(Arc<Control>);
impl Drop for ReleaseOnDrop {
    fn drop(&mut self) {
        self.0.released.store(true, Ordering::Release);
    }
}
struct ClockRepository(Arc<Control>);
impl bnauth_app::Repository for ClockRepository {
    fn commit(&mut self, candidate: &VaultSnapshot) -> Result<bnauth_app::CommitOutcome, String> {
        self.0.candidates.lock().unwrap().push(candidate.clone());
        let deadline = Instant::now() + Duration::from_secs(5);
        while !self.0.released.load(Ordering::Acquire) {
            assert!(Instant::now() < deadline, "test clock save gate timed out");
            std::thread::yield_now();
        }
        // This repository is called by one serialized application worker.
        if self.0.failures.load(Ordering::SeqCst) > 0 {
            self.0.failures.fetch_sub(1, Ordering::SeqCst);
            return Err("人工校時保存失敗".into());
        }
        candidate.validate()?;
        let mut snapshot = candidate.clone();
        snapshot.revision += 1;
        Ok(bnauth_app::CommitOutcome {
            snapshot,
            warning: None,
        })
    }
    fn retry_cleanup(&mut self) -> Result<(), String> {
        Ok(())
    }
}
fn sample(offset: i64) -> SyncSample {
    let instant = Instant::now();
    let wall = bnauth_core::now_ms();
    bnauth_core::clock::validate_sample(wall + offset, wall, wall, instant, instant).unwrap()
}
fn ready(released: bool, failures: usize) -> (GroupUi, ReleaseOnDrop) {
    let mut ui = GroupUi::new();
    let vault = VaultSnapshot::new_empty();
    ui.use_memory_vault(vault.clone());
    ui.app.dialog = None;
    let control = Arc::new(Control {
        released: AtomicBool::new(released),
        failures: AtomicUsize::new(failures),
        candidates: Mutex::new(Vec::new()),
    });
    let worker = control.clone();
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(ClockRepository(worker.clone())),
            Some(vault.clone()),
        ))
    }));
    wait_runtime(&mut ui);
    (ui, ReleaseOnDrop(control))
}
fn receive(ui: &mut GroupUi, sample: SyncSample) {
    receive_with_origin(ui, sample, true);
}

fn receive_with_origin(ui: &mut GroupUi, sample: SyncSample, manual: bool) {
    let (tx, rx) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(rx);
    ui.app.runtime.sync_manual = manual;
    tx.send(Ok(sample)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
}

#[test]
fn manual_calibration_joins_an_active_automatic_request_and_saves_once() {
    let (mut ui, control) = ready(false, 0);
    let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
    let result = sample(-1234);
    let record = result.record.clone();
    let (sender, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    ui.app.runtime.sync_manual = false;
    ui.app.begin_time_sync();
    assert!(ui.app.runtime.sync_manual);
    assert_eq!(ui.app.runtime.sync_requests, 0);
    sender.send(Ok(result)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(ui.app.runtime.app.saving);
    assert_eq!(ui.app.runtime.clock_save_inflight.as_ref(), Some(&record));
    assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
    assert!(ui.app.notifications.visible(Instant::now()).is_none());

    control.0.released.store(true, Ordering::Release);
    wait_runtime(&mut ui);
    let runtime = &ui.app.runtime;
    let current = runtime.app.current.as_deref().unwrap();
    assert_eq!(current.revision, original.revision + 1);
    assert_eq!(current.service_time_sync.battlenet.as_ref(), Some(&record));
    assert_eq!(runtime.clock.last_success(), Some(&record));
    assert!(runtime.pending_clock_record.is_none() && runtime.clock_save_inflight.is_none());
    let candidates = control.0.candidates.lock().unwrap();
    assert_eq!(candidates.len(), 1);
    assert_eq!(
        candidates[0].service_time_sync.battlenet.as_ref(),
        Some(&record)
    );
    assert_eq!(current.groups, original.groups);
    assert_eq!(ui.app.notifications.pending_warning_count(), 0);
    assert_eq!(
        ui.app.notifications.visible(Instant::now()),
        Some("校時成功")
    );
}

#[test]
fn a_failed_clock_save_keeps_the_sample_and_retry_saves_it_without_another_network_round() {
    for (clock_jump, new_result) in [(false, false), (true, false), (true, true)] {
        let (mut ui, control) = ready(true, 1);
        let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let fresh = sample(2345);
        let first = fresh.record.clone();
        receive(&mut ui, fresh);
        wait_runtime(&mut ui);
        assert!(ui.app.runtime.app.has_pending());
        assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
        assert_eq!(ui.app.runtime.clock_save_inflight.as_ref(), Some(&first));

        if clock_jump {
            let reading = ClockObservation::at(bnauth_core::now_ms() + 3_600_000, Instant::now());
            ui.app.poll_runtime_with_clock(&ui.ctx, || reading);
            assert_eq!(ui.app.runtime.pending_clock_record.as_ref(), Some(&first));
        }
        let expected = if new_result {
            let fresh = sample(900);
            let record = fresh.record.clone();
            receive(&mut ui, fresh);
            record
        } else {
            first.clone()
        };

        ui.view = TestView::Saving;
        ui.frame(Vec::new());
        ui.click(ui.point("重試"));
        wait_runtime(&mut ui);
        let runtime = &ui.app.runtime;
        let current = runtime.app.current.as_deref().unwrap();
        assert_eq!(current.revision, original.revision + 1);
        assert_eq!(
            current.service_time_sync.battlenet.as_ref(),
            Some(&expected)
        );
        assert!(runtime.pending_clock_record.is_none() && runtime.clock_save_inflight.is_none());
        assert!(!runtime.app.has_pending() && runtime.sync_rx.is_none());
        assert_eq!(runtime.sync_requests, 0);
        let candidates = control.0.candidates.lock().unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(
            candidates[0].service_time_sync.battlenet.as_ref(),
            Some(&first)
        );
        assert_eq!(
            candidates[1].service_time_sync.battlenet.as_ref(),
            Some(&expected)
        );
    }
}

#[test]
fn discarding_a_failed_clock_write_restores_the_saved_clock_and_does_not_requeue_it() {
    let (mut ui, control, original, old) = with_saved_clock(true);
    let token = ui.app.model.change_token();
    control.0.failures.store(1, Ordering::SeqCst);
    receive(&mut ui, sample(800));
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(800));
    ui.view = TestView::Saving;
    ui.frame(Vec::new());
    let time = IMPORT_TEST_TIME;
    let code = ui.app.otp_cache.code(&ui.app.model.entries[0].record, time);
    let calculations = ui.app.otp_cache.calculations;
    ui.click(ui.point("放棄"));
    ui.app.poll_runtime(&ui.ctx);
    assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
    assert_eq!(ui.app.model.change_token(), token);
    assert_eq!(ui.app.runtime.clock.last_success(), Some(&old));
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(old.offset_ms));
    assert!(
        ui.app.runtime.pending_clock_record.is_none()
            && ui.app.runtime.clock_save_inflight.is_none()
    );
    assert!(!ui.app.runtime.app.has_pending() && !ui.app.runtime.app.saving);
    assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
    let entry = &ui.app.model.entries[0].record;
    assert_eq!(ui.app.otp_cache.code(entry, time), code);
    assert_eq!(ui.app.otp_cache.calculations, calculations);
    let next_step = time + 30_000;
    assert_eq!(
        ui.app.otp_cache.code(entry, next_step),
        bnauth_core::totp(&entry.secret, next_step).unwrap()
    );
    assert_eq!(ui.app.otp_cache.calculations, calculations + 1);
}

#[test]
fn discarding_failed_user_data_keeps_the_independent_successful_calibration_for_saving() {
    let (mut ui, control) = ready(false, 1);
    let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
    let mut data = original.clone();
    data.groups.push(bnauth_core::Group {
        id: uuid::Uuid::new_v4(),
        name: "將捨棄的分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    ui.app.runtime.app.commit(data, false).unwrap();
    let fresh = sample(123);
    let record = fresh.record.clone();
    receive(&mut ui, fresh);
    control.0.released.store(true, Ordering::Release);
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending() && ui.app.runtime.clock_save_inflight.is_none());
    ui.app.discard_unsaved_changes();
    wait_runtime(&mut ui);
    let current = ui.app.runtime.app.current.as_deref().unwrap();
    assert_eq!(current.groups, original.groups);
    assert_eq!(current.authenticators, original.authenticators);
    assert_eq!(current.service_time_sync.battlenet.as_ref(), Some(&record));
    assert_eq!(current.revision, original.revision + 1);
    assert_eq!(control.0.candidates.lock().unwrap().len(), 2);
}

#[test]
fn retry_or_discard_after_a_manual_import_and_clock_save_fails() {
    for (discard, clock_jump) in [(false, false), (true, false), (false, true)] {
        let (mut ui, control) = ready(true, 1);
        let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
        ui.open(Dialog::Import(Box::new(import_draft())));
        ui.click(ui.point("預覽匯入"));

        let fresh = sample(150);
        let record = fresh.record.clone();
        receive(&mut ui, fresh);
        assert!(control.0.candidates.lock().unwrap().is_empty());

        ui.click(ui.point("確認"));
        wait_runtime(&mut ui);
        assert!(ui.app.runtime.app.has_pending());
        assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
        assert!(ui.app.model.entries.is_empty());

        if clock_jump {
            let reading = ClockObservation::at(bnauth_core::now_ms() + 3_600_000, Instant::now());
            ui.app.poll_runtime_with_clock(&ui.ctx, || reading);
            assert_eq!(ui.app.runtime.clock.last_success(), Some(&record));
        }
        ui.view = TestView::Saving;
        ui.frame(Vec::new());
        let button = if discard { "放棄" } else { "重試" };
        ui.click(ui.point(button));
        wait_runtime(&mut ui);

        let expected_count = usize::from(!discard);
        let current = ui.app.runtime.app.current.as_deref().unwrap();
        assert_eq!(current.revision, original.revision + 1);
        assert_eq!(current.authenticators.len(), expected_count);
        assert_eq!(ui.app.model.entries.len(), expected_count);
        assert_eq!(current.service_time_sync.battlenet.as_ref(), Some(&record));
        assert_eq!(ui.app.runtime.sync_requests, 0);
        let candidates = control.0.candidates.lock().unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[0].authenticators.len(), 1);
        assert_eq!(
            candidates[0].service_time_sync.battlenet.as_ref(),
            Some(&record)
        );
        assert_eq!(candidates[1].authenticators.len(), expected_count);
        assert_eq!(
            candidates[1].service_time_sync.battlenet.as_ref(),
            Some(&record)
        );
    }
}

#[test]
fn retry_saves_a_recovered_authenticator_with_a_late_clock_result() {
    let (mut ui, control) = ready(true, 1);
    let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
    let (reply, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    ui.app.runtime.sync_manual = true;

    ui.open(Dialog::Import(Box::new(import_draft())));
    ui.click(ui.point("預覽匯入"));
    assert!(control.0.candidates.lock().unwrap().is_empty());
    ui.click(ui.point("確認"));
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));

    let fresh = sample(150);
    let record = fresh.record.clone();
    reply.send(Ok(fresh)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert_eq!(ui.app.runtime.pending_clock_record.as_ref(), Some(&record));
    assert_eq!(control.0.candidates.lock().unwrap().len(), 1);

    ui.view = TestView::Saving;
    ui.frame(Vec::new());
    ui.click(ui.point("重試"));
    wait_runtime(&mut ui);

    let current = ui.app.runtime.app.current.as_deref().unwrap();
    let candidates = control.0.candidates.lock().unwrap();
    assert_eq!(current.revision, original.revision + 1);
    assert_eq!(candidates.len(), 2);
    assert!(candidates[0].service_time_sync.battlenet.is_none());
    assert_eq!(
        candidates[1].service_time_sync.battlenet.as_ref(),
        Some(&record)
    );
    assert_eq!(current.service_time_sync.battlenet.as_ref(), Some(&record));
    assert_eq!(current.authenticators.len(), 1);
    assert_eq!(current.authenticators, candidates[0].authenticators);
    assert_eq!(ui.app.model.entries.len(), 1);
    assert!(ui.app.runtime.pending_clock_record.is_none());
    assert_eq!(ui.app.runtime.sync_requests, 0);
}

#[test]
fn a_failed_explicit_reload_cancels_clock_work_and_late_replies_cannot_block_closing() {
    use super::lifecycle_ui_tests::{click_visible, interactive_frame};

    struct UnverifiedClockRepository {
        inner: ClockRepository,
        blocked: bool,
    }

    impl bnauth_app::Repository for UnverifiedClockRepository {
        fn state(&self) -> bnauth_app::RepositoryState {
            bnauth_app::RepositoryState {
                blocked: self.blocked,
                ..Default::default()
            }
        }

        fn commit(&mut self, candidate: &VaultSnapshot) -> Result<bnauth_app::CommitOutcome, String> {
            let result = self.inner.commit(candidate);
            self.blocked = result.is_err();
            result
        }

        fn retry_cleanup(&mut self) -> Result<(), String> {
            self.inner.retry_cleanup()
        }
    }

    for reply_before_reload in [false, true] {
        let (mut ui, control) = ready(true, 1);
        let original = ui.app.runtime.app.current.as_deref().unwrap().clone();
        let opens = Arc::new(AtomicUsize::new(0));
        let worker_opens = opens.clone();
        let worker = control.0.clone();
        let worker_vault = original.clone();
        ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
            if worker_opens.fetch_add(1, Ordering::SeqCst) > 0 {
                return Err("人工重新讀取失敗".into());
            }
            Ok((
                Box::new(UnverifiedClockRepository {
                    inner: ClockRepository(worker.clone()),
                    blocked: false,
                }),
                Some(worker_vault.clone()),
            ))
        }));
        wait_runtime(&mut ui);

        // A manual calibration is still running when an independent user edit
        // fails to save. Its reply may arrive before or after explicit reload.
        let (sender, receiver) = std::sync::mpsc::channel();
        ui.app.runtime.sync_rx = Some(receiver);
        ui.app.runtime.sync_manual = true;
        let mut candidate = original.clone();
        candidate.groups.push(bnauth_core::Group {
            id: uuid::Uuid::new_v4(),
            name: "尚未保存的使用者分組".into(),
            sort_order: 1,
            is_ungrouped: false,
        });
        ui.app.runtime.app.commit(candidate, false).unwrap();
        wait_runtime(&mut ui);
        assert!(ui.app.runtime.app.has_pending());
        assert!(ui.app.runtime.clock_save_inflight.is_none());
        if reply_before_reload {
            sender.send(Ok(sample(700))).unwrap();
            ui.app.poll_runtime(&ui.ctx);
            assert!(ui.app.runtime.pending_clock_record.is_some());
        }
        interactive_frame(&mut ui, Vec::new());
        let discard = ui.point("放棄");
        click_visible(&mut ui, discard);
        assert!(!ui.app.runtime.app.has_pending());
        assert_eq!(opens.load(Ordering::SeqCst), 1);
        assert!(!ui.points("根據名稱查找").is_empty());
        let reload = ui.point("重新讀取");
        click_visible(&mut ui, reload);
        wait_runtime(&mut ui);
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Error);
        assert_eq!(
            ui.app.runtime.app.error.as_deref(),
            Some("人工重新讀取失敗")
        );
        assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
        assert!(!ui.app.runtime.app.has_pending());
        assert!(ui.app.runtime.app.repository_state.blocked);
        assert!(ui.app.runtime.sync_rx.is_none());
        assert!(ui.app.runtime.pending_clock_record.is_none());
        assert!(ui.app.runtime.clock_save_inflight.is_none());
        assert!(
            !ui.app.runtime.sync_manual
                && !ui.app.runtime.pending_clock_manual
                && !ui.app.runtime.clock_save_manual
        );
        assert!(
            sender.send(Ok(sample(900))).is_err(),
            "a late pre-reload result must have no receiver in the failed session"
        );
        ui.app.runtime.close_requested = true;
        ui.app.poll_runtime(&ui.ctx);
        assert!(!ui.app.runtime.close_blocked());
        assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
    }
}

fn with_saved_clock(released: bool) -> (GroupUi, ReleaseOnDrop, VaultSnapshot, TimeSyncRecord) {
    let (mut ui, control) = ready(released, 0);
    let old_record = sample(-350).record;
    let mut saved = ui.app.runtime.app.current.as_deref().unwrap().clone();
    saved = bnauth_core::prepare_import(
        &saved,
        "原有驗證器",
        "",
        saved.groups[0].id,
        bnauth_core::parse_manual("3132333435363738393031323334353637383930", "", "").unwrap(),
    )
    .unwrap()
    .candidate(&saved)
    .unwrap();
    saved.service_time_sync.battlenet = Some(old_record.clone());
    saved.groups.push(bnauth_core::Group {
        id: uuid::Uuid::new_v4(),
        name: "原有分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let worker = control.0.clone();
    let worker_vault = saved.clone();
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(ClockRepository(worker.clone())),
            Some(worker_vault.clone()),
        ))
    }));
    wait_runtime(&mut ui);
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(old_record.offset_ms));
    (ui, control, saved, old_record)
}

#[test]
fn clock_ack_keeps_the_projection_and_cache_while_rebasing_later_edits() {
    let (mut ui, control, original, _) = with_saved_clock(true);
    let entry_id = ui.app.model.entries[0].id;
    let token = ui.app.model.change_token();
    let time = IMPORT_TEST_TIME;
    let code = ui.app.otp_cache.code(&ui.app.model.entries[0].record, time);
    let calculations = ui.app.otp_cache.calculations;

    let fresh = sample(350);
    let record = fresh.record.clone();
    receive(&mut ui, fresh);
    wait_runtime(&mut ui);
    let saved = ui.app.runtime.app.current.as_deref().unwrap();
    assert_eq!(saved.revision, original.revision + 1);
    assert_eq!(saved.service_time_sync.battlenet.as_ref(), Some(&record));
    assert!(std::ptr::eq(&*saved.groups, &*original.groups));
    assert!(std::ptr::eq(
        &*saved.authenticators,
        &*original.authenticators
    ));
    assert_eq!(&ui.app.model.snapshot(), saved);
    assert_eq!(ui.app.model.change_token(), token);
    assert_eq!(
        ui.app.otp_cache.code(&ui.app.model.entries[0].record, time),
        code
    );
    assert_eq!(ui.app.otp_cache.calculations, calculations);

    ui.app.edit(&ui.ctx, Some(entry_id));
    let latest = sample(800);
    let latest_record = latest.record.clone();
    receive(&mut ui, latest);
    assert!(!ui.app.runtime.app.saving);
    assert_eq!(control.0.candidates.lock().unwrap().len(), 1);

    let Some(Dialog::Edit { name, .. }) = ui.app.dialog.as_mut() else {
        unreachable!()
    };
    *name = "修改名稱".into();
    for _ in 0..3 {
        ui.frame(Vec::new());
    }
    ui.click(ui.point("套用"));
    assert_eq!(ui.app.model.change_token(), token);
    wait_runtime(&mut ui);
    let candidates = control.0.candidates.lock().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(candidates[1].revision, original.revision + 1);
    assert_eq!(
        candidates[1].service_time_sync.battlenet.as_ref(),
        Some(&latest_record)
    );
    let saved = ui.app.runtime.app.current.as_deref().unwrap();
    assert_eq!(saved.revision, original.revision + 2);
    assert_eq!(
        saved.service_time_sync.battlenet.as_ref(),
        Some(&latest_record)
    );
    assert!(ui.app.runtime.pending_clock_record.is_none());
    assert_eq!(ui.app.runtime.sync_requests, 0);
    let saved_entry = &saved.authenticators[0];
    assert_eq!(saved_entry.name, "修改名稱");
    let live_entry = &ui.app.model.entries[0].record;
    assert_eq!(live_entry, saved_entry);
    assert_eq!(ui.app.otp_cache.code(live_entry, time), code);
    assert_eq!(&ui.app.model.snapshot(), saved);
    assert_eq!(ui.app.model.change_token(), token + 1);
    assert_eq!(ui.app.otp_cache.calculations, calculations);
}

#[test]
fn a_system_time_change_keeps_the_saved_offset_and_does_not_start_a_save_or_sync() {
    let (mut ui, control, original, saved) = with_saved_clock(true);
    let now = Instant::now();
    let wall = bnauth_core::now_ms();
    for (elapsed, adjustment, expect_warning) in [
        (0, 60_000, true),
        (30_000, 60_000, false),
        (30_001, 60_000, false),
        (31_000, 0, true),
        (61_000, 0, false),
    ] {
        let instant = now + Duration::from_millis(elapsed as u64);
        let changed_wall = wall + elapsed + adjustment;
        let reading = ClockObservation::at(changed_wall, instant);
        ui.app.poll_runtime_with_clock(&ui.ctx, || reading);
        assert_eq!(ui.app.runtime.frame_time, changed_wall + saved.offset_ms);
        assert_eq!(ui.app.runtime.clock.last_success(), Some(&saved));
        assert_eq!(
            ui.app.notifications.visible(instant),
            expect_warning.then_some("E13：時間偏差過大，請重新校時")
        );
    }
    assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
    assert!(ui.app.runtime.pending_clock_record.is_none());
    assert_eq!(ui.app.runtime.sync_requests, 0);
    assert!(control.0.candidates.lock().unwrap().is_empty());
    assert!(!ui.app.runtime.close_blocked());
}

#[test]
fn full_restore_confirmation_replaces_only_invalid_source_calibration() {
    for (invalid_backup, pending_calibration, reply_after_restore) in [
        (false, false, None),
        (true, false, None),
        (true, true, None),
        (true, false, Some(false)),
        (true, false, Some(true)),
    ] {
        let (mut ui, control, original, saved) = with_saved_clock(reply_after_restore.is_none());
        full_restore_ui_tests::keep_restore_offline(&mut ui);
        let mut source = full_restore_ui_tests::source();
        let server_time = bnauth_core::now_ms();
        let old_wall = if invalid_backup {
            // This backup was calibrated on a PC whose calendar was 100 years ahead.
            server_time + 100 * 365 * 86_400_000_i64
        } else {
            server_time - 250
        };
        let at = Instant::now();
        source.service_time_sync.battlenet = Some(
            bnauth_core::clock::validate_sample(server_time, old_wall, old_wall, at, at)
                .unwrap()
                .record,
        );
        let original_source = source.clone();
        let mut expected = bnauth_core::prepare_full_restore(Some(&original), &source).unwrap();
        full_restore_ui_tests::load_preview(&mut ui, &source);
        ui.click(ui.point("我已知曉目前資料將被全部取代。"));
        if invalid_backup {
            let replacement = if pending_calibration {
                let fresh = sample(750);
                let record = fresh.record.clone();
                receive(&mut ui, fresh);
                assert_eq!(ui.app.runtime.pending_clock_record.as_ref(), Some(&record));
                record
            } else {
                saved
            };
            expected.candidate.service_time_sync.battlenet = Some(replacement);
        }
        let manual_reply = reply_after_restore.map(|after_restore| {
            let (sender, receiver) = std::sync::mpsc::channel();
            ui.app.runtime.sync_rx = Some(receiver);
            ui.app.runtime.sync_manual = true;
            (sender, after_restore)
        });
        let now = Instant::now();
        let assert_restore_notice = |ui: &mut GroupUi| {
            let notice = ui.app.notifications.visible(now).unwrap();
            assert_eq!(notice, "已還原完整資料庫");
        };
        ui.click(ui.point("還原"));
        assert!(ui.app.dialog.is_none());
        assert_eq!(ui.app.runtime.app.current.as_deref(), Some(&original));
        let late_record = manual_reply.map(|(sender, after_restore)| {
            if after_restore {
                control.0.released.store(true, Ordering::Release);
                wait_runtime(&mut ui);
                assert!(!ui.app.runtime.pending_full_restore);
                assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
                assert_restore_notice(&mut ui);
            }

            let fresh = sample(900);
            let record = fresh.record.clone();
            sender
                .send(Ok(fresh))
                .expect("restore keeps the manual receiver");
            ui.app.poll_runtime(&ui.ctx);
            if !after_restore {
                assert!(ui.app.runtime.app.saving && ui.app.runtime.pending_full_restore);
                assert_eq!(ui.app.runtime.pending_clock_record.as_ref(), Some(&record));
                control.0.released.store(true, Ordering::Release);
            }
            record
        });
        wait_runtime(&mut ui);
        assert_restore_notice(&mut ui);

        let restored = ui.app.runtime.app.current.as_deref().unwrap();
        let clock_save = u64::from(late_record.is_some());
        let expected = expected.candidate;
        assert_eq!(restored.revision, original.revision + 1 + clock_save);
        assert_eq!(restored.groups, source.groups);
        assert_eq!(restored.authenticators, source.authenticators);
        let expected_clock = late_record
            .as_ref()
            .or(expected.service_time_sync.battlenet.as_ref());
        assert_eq!(
            restored.service_time_sync.battlenet.as_ref(),
            expected_clock
        );
        assert!(!ui.app.runtime.close_blocked());
        assert_eq!(source, original_source);
        assert_eq!(
            ui.app.runtime.clock.offset_ms(),
            expected_clock.map(|record| record.offset_ms)
        );
        assert_eq!(ui.app.runtime.sync_requests, 0);
        for _ in 0..3 {
            ui.app.poll_runtime(&ui.ctx);
        }
        let candidates = control.0.candidates.lock().unwrap();
        if let Some(record) = late_record {
            let mut clock_candidate = expected.clone();
            clock_candidate.revision += 1;
            clock_candidate.service_time_sync.battlenet = Some(record);
            assert_eq!(candidates.as_slice(), &[expected, clock_candidate]);
        } else {
            assert_eq!(candidates.as_slice(), &[expected]);
        }
    }
}

#[test]
fn reload_and_full_restore_apply_their_saved_offset_after_a_system_time_change() {
    for restore in [false, true] {
        let (mut ui, control, original, saved) = with_saved_clock(true);
        let mut source = original.clone();
        let replacement = sample(925).record;
        source.service_time_sync.battlenet = Some(replacement.clone());
        source.groups[1].name = "來源組名與順序須完整保留".into();
        if restore {
            let preview = bnauth_core::prepare_full_restore(Some(&original), &source).unwrap();
            ui.app
                .runtime
                .app
                .restore_from_preview(|| preview.candidate)
                .unwrap();
            ui.app.runtime.pending_full_restore = true;
        } else {
            let loaded = Arc::new(Mutex::new(original.clone()));
            let worker_loaded = loaded.clone();
            let worker_control = control.0.clone();
            ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
                Ok((
                    Box::new(ClockRepository(worker_control.clone())),
                    Some(worker_loaded.lock().unwrap().clone()),
                ))
            }));
            wait_runtime(&mut ui);
            *loaded.lock().unwrap() = source.clone();
            ui.app.reload_with_clock_cancel().unwrap();
        }

        let instant = Instant::now();
        let corrected_wall = bnauth_core::now_ms();
        ui.app.runtime.clock = RuntimeClock::from_saved_observation(
            Some(&saved),
            ClockObservation::at(corrected_wall + 3_600_000, instant),
        )
        .unwrap();
        let reading =
            ClockObservation::at(corrected_wall + 1_000, instant + Duration::from_secs(1));
        let cache_clears = ui.app.otp_cache.clears;
        let deadline = Instant::now() + Duration::from_secs(5);
        loop {
            ui.app.poll_runtime_with_clock(&ui.ctx, || reading);
            if ui.app.runtime.app.phase == bnauth_app::Phase::Ready && !ui.app.runtime.app.saving {
                break;
            }
            assert!(Instant::now() < deadline, "ACK did not arrive");
            std::thread::yield_now();
        }
        assert_eq!(ui.app.otp_cache.clears, cache_clears + 1);
        let runtime = &ui.app.runtime;
        assert_eq!(
            runtime.app.current.as_deref().unwrap().groups,
            source.groups
        );
        assert_eq!(
            runtime.app.current.as_deref().unwrap().authenticators,
            source.authenticators
        );
        assert_eq!(runtime.clock.last_success(), Some(&replacement));
        assert_eq!(
            runtime.frame_time,
            corrected_wall + 1_000 + replacement.offset_ms
        );
        assert_eq!(
            control.0.candidates.lock().unwrap().len(),
            usize::from(restore)
        );
        assert_eq!(runtime.sync_requests, 0);
    }
}
