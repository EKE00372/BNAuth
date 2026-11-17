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
    fn restore(&mut self, candidate: &VaultSnapshot) -> Result<bnauth_app::CommitOutcome, String> {
        self.commit(candidate)
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
    let vault = VaultSnapshot::new_empty(IMPORT_TEST_TIME);
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
fn a_new_automatic_sample_during_a_manual_calibration_save_keeps_the_manual_feedback() {
    let (mut ui, control) = ready(false, 0);
    let original = ui.app.runtime.app.current.clone().unwrap();
    let first = sample(-1234);
    let first_record = first.record.clone();
    // A manual click joins an automatic request already in flight. It must
    // share that request and still provide the manual completion feedback.
    let (sender, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    ui.app.runtime.sync_manual = false;
    ui.app.begin_time_sync();
    sender.send(Ok(first)).unwrap();
    ui.app.poll_runtime(&ui.ctx);
    assert!(ui.app.runtime.app.saving);
    assert_eq!(
        ui.app.runtime.clock_save_inflight.as_ref(),
        Some(&first_record)
    );
    let second = sample(500);
    let second_record = second.record.clone();
    receive_with_origin(&mut ui, second, false);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert_eq!(
        ui.app.runtime.pending_clock_record.as_ref(),
        Some(&second_record)
    );
    control.0.released.store(true, Ordering::Release);
    wait_runtime(&mut ui);
    let runtime = &ui.app.runtime;
    let current = runtime.app.current.as_ref().unwrap();
    assert_eq!(current.revision, original.revision + 2);
    assert_eq!(
        current.service_time_sync.battlenet.as_ref(),
        Some(&second_record)
    );
    assert_eq!(runtime.clock.last_success(), Some(&second_record));
    assert!(runtime.pending_clock_record.is_none() && runtime.clock_save_inflight.is_none());
    let candidates = control.0.candidates.lock().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0].service_time_sync.battlenet.as_ref(),
        Some(&first_record)
    );
    assert_eq!(
        candidates[1].service_time_sync.battlenet.as_ref(),
        Some(&second_record)
    );
    assert_eq!(candidates[1].revision, original.revision + 1);
    assert_eq!(current.groups, original.groups);
    drop(candidates);
    assert_eq!(ui.app.notifications.pending_warning_count(), 0);
    assert_eq!(
        ui.app.notifications.visible(Instant::now()),
        Some("校時成功並已保存")
    );
}

#[test]
fn a_failed_clock_save_keeps_the_sample_and_retry_saves_it_without_another_network_round() {
    let (mut ui, control) = ready(true, 1);
    let original = ui.app.runtime.app.current.clone().unwrap();
    let fresh = sample(2345);
    let record = fresh.record.clone();
    receive(&mut ui, fresh);
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert_eq!(ui.app.runtime.pending_clock_record.as_ref(), Some(&record));
    assert_eq!(ui.app.runtime.clock_save_inflight.as_ref(), Some(&record));
    assert!(ui.app.runtime.clock.is_synced() && ui.app.runtime.sync_rx.is_none());
    ui.app.runtime.app.retry().unwrap();
    wait_runtime(&mut ui);
    let runtime = &ui.app.runtime;
    assert_eq!(
        runtime
            .app
            .current
            .as_ref()
            .unwrap()
            .service_time_sync
            .battlenet
            .as_ref(),
        Some(&record)
    );
    assert!(runtime.pending_clock_record.is_none() && runtime.clock_save_inflight.is_none());
    assert!(!runtime.app.has_pending() && runtime.sync_rx.is_none());
    assert_eq!(control.0.candidates.lock().unwrap().len(), 2);
}

#[test]
fn discarding_a_failed_clock_write_restores_the_saved_clock_and_does_not_requeue_it() {
    let (mut ui, control) = ready(true, 1);
    let old = sample(-100).record;
    let mut original = ui.app.runtime.app.current.clone().unwrap();
    original.service_time_sync.battlenet = Some(old.clone());
    // Reopen a saved snapshot through the actual Loaded event.
    let worker = control.0.clone();
    let loaded = original.clone();
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(ClockRepository(worker.clone())),
            Some(loaded.clone()),
        ))
    }));
    wait_runtime(&mut ui);
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(-100));
    receive(&mut ui, sample(800));
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(800));
    ui.app.discard_unsaved_changes();
    ui.app.poll_runtime(&ui.ctx);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert_eq!(ui.app.runtime.clock.last_success(), Some(&old));
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(-100));
    assert!(
        ui.app.runtime.pending_clock_record.is_none()
            && ui.app.runtime.clock_save_inflight.is_none()
    );
    assert!(!ui.app.runtime.app.has_pending() && !ui.app.runtime.app.saving);
    assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
}

#[test]
fn discarding_failed_user_data_keeps_the_independent_successful_calibration_for_saving() {
    let (mut ui, control) = ready(false, 1);
    let original = ui.app.runtime.app.current.clone().unwrap();
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
    let current = ui.app.runtime.app.current.as_ref().unwrap();
    assert_eq!(current.groups, original.groups);
    assert_eq!(current.authenticators, original.authenticators);
    assert_eq!(current.service_time_sync.battlenet.as_ref(), Some(&record));
    assert_eq!(current.revision, original.revision + 1);
    assert_eq!(control.0.candidates.lock().unwrap().len(), 2);
}

#[test]
fn reloading_installs_the_saved_offset_and_clears_old_pending_clock_results() {
    let (mut ui, control) = ready(true, 0);
    let saved_record = sample(-321).record;
    let mut saved = ui.app.runtime.app.current.clone().unwrap();
    saved.service_time_sync.battlenet = Some(saved_record.clone());
    let worker = control.0.clone();
    ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
        Ok((
            Box::new(ClockRepository(worker.clone())),
            Some(saved.clone()),
        ))
    }));
    ui.app.runtime.pending_clock_record = Some(sample(999).record);
    ui.app.runtime.clock_save_inflight = None;
    let (_sender, receiver) = std::sync::mpsc::channel();
    ui.app.runtime.sync_rx = Some(receiver);
    wait_runtime(&mut ui);
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(-321));
    assert!(ui.app.runtime.clock.is_from_saved());
    assert_eq!(ui.app.runtime.clock.last_success(), Some(&saved_record));
    assert!(
        ui.app.runtime.pending_clock_record.is_none()
            && ui.app.runtime.clock_save_inflight.is_none()
    );
    assert!(ui.app.runtime.sync_rx.is_none());
    assert!(control.0.candidates.lock().unwrap().is_empty());
}

#[test]
fn full_restore_installs_source_saved_clock_and_drops_pre_restore_deferred_samples() {
    let (mut ui, control) = ready(true, 0);
    let record: TimeSyncRecord = sample(4321).record;
    let mut source = VaultSnapshot::new_empty(IMPORT_TEST_TIME);
    source.service_time_sync.battlenet = Some(record.clone());
    let preview = bnauth_core::prepare_full_restore(
        ui.app.runtime.app.current.as_ref(),
        &source,
        IMPORT_TEST_TIME + 1,
    )
    .unwrap();
    ui.app.runtime.pending_clock_record = Some(sample(-888).record);
    ui.app
        .runtime
        .app
        .restore(preview.candidate, false)
        .unwrap();
    ui.app.runtime.pending_full_restore = true;
    wait_runtime(&mut ui);
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(4321));
    assert!(ui.app.runtime.clock.is_from_saved());
    assert_eq!(
        ui.app
            .runtime
            .app
            .current
            .as_ref()
            .unwrap()
            .service_time_sync
            .battlenet
            .as_ref(),
        Some(&record)
    );
    assert!(
        ui.app.runtime.pending_clock_record.is_none()
            && ui.app.runtime.clock_save_inflight.is_none()
    );
    assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
}

#[test]
fn a_failed_explicit_reload_cancels_clock_work_and_late_replies_cannot_block_closing() {
    for (reply_before_reload, pending_invalidation) in
        [(false, false), (true, false), (false, true)]
    {
        let (mut ui, control) = ready(true, 1);
        let original = ui.app.runtime.app.current.clone().unwrap();
        let opens = Arc::new(AtomicUsize::new(0));
        let worker_opens = opens.clone();
        let worker = control.0.clone();
        let worker_vault = original.clone();
        ui.app.runtime.app = bnauth_app::Application::start(Box::new(move || {
            if worker_opens.fetch_add(1, Ordering::SeqCst) > 0 {
                return Err("人工重新讀取失敗".into());
            }
            Ok((
                Box::new(ClockRepository(worker.clone())),
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
        if pending_invalidation {
            ui.app.runtime.invalidated_clock_records = vec![sample(-500).record];
            ui.app.runtime.pending_clock_record = None;
            ui.app.runtime.pending_clock_invalidation = true;
            ui.app.runtime.clock = bnauth_core::clock::RuntimeClock::new();
            assert!(ui.app.runtime.close_blocked());
        }

        ui.view = TestView::Saving;
        ui.frame(Vec::new());
        ui.click(ui.point("放棄變更並重新讀取資料檔"));
        wait_runtime(&mut ui);
        assert_eq!(opens.load(Ordering::SeqCst), 2);
        assert_eq!(ui.app.runtime.app.phase, bnauth_app::Phase::Error);
        assert_eq!(
            ui.app.runtime.app.error.as_deref(),
            Some("人工重新讀取失敗")
        );
        assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
        assert!(!ui.app.runtime.app.has_pending());
        assert!(!ui.app.runtime.app.repository_state.needs_attention());
        assert!(ui.app.runtime.sync_rx.is_none());
        assert!(ui.app.runtime.pending_clock_record.is_none());
        assert!(ui.app.runtime.clock_save_inflight.is_none());
        assert!(!ui.app.runtime.pending_clock_invalidation);
        assert!(!ui.app.runtime.clock_invalidation_inflight);
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
    let mut saved = ui.app.runtime.app.current.clone().unwrap();
    saved.service_time_sync.battlenet = Some(old_record.clone());
    saved.groups.push(bnauth_core::Group {
        id: uuid::Uuid::new_v4(),
        name: "校時清理須保留的分組".into(),
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
fn an_invalid_saved_clock_is_atomically_cleared_and_cannot_reactivate_after_reload() {
    let (mut ui, control, original, old_record) = with_saved_clock(true);
    // Core clock tests exercise actual wall jumps. Simulate its consumed signal
    // and invalid-record marker here to verify the UI's persistence transaction.
    ui.app.runtime.invalidated_clock_records = vec![old_record.clone()];
    ui.app.runtime.pending_clock_invalidation = true;
    ui.app.runtime.clock = bnauth_core::clock::RuntimeClock::new();
    ui.app.load_shared_clock();
    assert!(ui.app.runtime.pending_clock_invalidation);
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
    ui.app.poll_runtime(&ui.ctx);
    assert!(ui.app.runtime.app.saving);
    assert!(ui.app.runtime.clock_invalidation_inflight);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    wait_runtime(&mut ui);

    let cleared = ui.app.runtime.app.current.as_ref().unwrap();
    assert!(cleared.service_time_sync.battlenet.is_none());
    assert_eq!(cleared.revision, original.revision + 1);
    assert_eq!(cleared.groups, original.groups);
    assert_eq!(cleared.authenticators, original.authenticators);
    assert!(ui.app.runtime.pending_clock_record.is_none());
    assert!(!ui.app.runtime.pending_clock_invalidation);
    assert!(!ui.app.runtime.clock_invalidation_inflight);
    assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
    assert!(
        control.0.candidates.lock().unwrap()[0]
            .service_time_sync
            .battlenet
            .is_none()
    );
    ui.app.load_shared_clock();
    ui.app.poll_runtime(&ui.ctx);
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
    assert!(!ui.app.runtime.clock.is_synced());
    assert!(!ui.app.runtime.pending_clock_invalidation);
    assert!(!ui.app.runtime.close_blocked());
    assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
}

#[test]
fn a_new_calibration_during_an_invalidation_write_survives_the_clear_acknowledgement() {
    let (mut ui, control, original, old_record) = with_saved_clock(false);
    ui.app.runtime.invalidated_clock_records = vec![old_record.clone()];
    ui.app.runtime.pending_clock_invalidation = true;
    ui.app.runtime.clock = bnauth_core::clock::RuntimeClock::new();
    ui.app.poll_runtime(&ui.ctx);
    assert!(ui.app.runtime.app.saving);
    assert!(ui.app.runtime.clock_invalidation_inflight);
    let fresh = sample(825);
    let fresh_record = fresh.record.clone();
    receive(&mut ui, fresh);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert_eq!(
        ui.app.runtime.pending_clock_record.as_ref(),
        Some(&fresh_record)
    );
    assert!(!ui.app.runtime.pending_clock_invalidation);
    assert!(
        ui.app
            .runtime
            .invalidated_clock_records
            .contains(&old_record)
    );
    assert!(ui.app.runtime.clock_invalidation_inflight);
    control.0.released.store(true, Ordering::Release);
    wait_runtime(&mut ui);

    let current = ui.app.runtime.app.current.as_ref().unwrap();
    assert_eq!(
        current.service_time_sync.battlenet.as_ref(),
        Some(&fresh_record)
    );
    assert_eq!(current.revision, original.revision + 2);
    assert_eq!(current.groups, original.groups);
    assert_eq!(current.authenticators, original.authenticators);
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(825));
    assert!(ui.app.runtime.pending_clock_record.is_none());
    assert!(!ui.app.runtime.pending_clock_invalidation);
    assert!(!ui.app.runtime.clock_invalidation_inflight);
    let candidates = control.0.candidates.lock().unwrap();
    assert_eq!(candidates.len(), 2);
    assert!(candidates[0].service_time_sync.battlenet.is_none());
    assert_eq!(
        candidates[1].service_time_sync.battlenet.as_ref(),
        Some(&fresh_record)
    );
    drop(candidates);
    ui.app.load_shared_clock();
    ui.app.poll_runtime(&ui.ctx);
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(825));
    assert!(ui.app.runtime.clock.is_synced());
    assert!(!ui.app.runtime.pending_clock_invalidation);
    assert_eq!(control.0.candidates.lock().unwrap().len(), 2);
}

fn invalidate_shared_clock(ui: &mut GroupUi) {
    // Core tests inject actual wall-clock jumps and verify take_invalidation.
    // Consume the same runtime transition without changing the machine clock.
    ui.app.runtime.note_clock_invalidation(false);
    ui.app.runtime.clock = bnauth_core::clock::RuntimeClock::new();
}

#[test]
fn a_failed_new_sample_cannot_forget_a_known_invalid_saved_clock_on_discard() {
    let (mut ui, control, original, saved) = with_saved_clock(true);
    ui.app.dialog = Some(Dialog::Info);
    invalidate_shared_clock(&mut ui);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    let fresh = sample(750);
    let fresh_record = fresh.record.clone();
    receive(&mut ui, fresh);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(750));
    assert!(
        !ui.app.runtime.app.saving,
        "the modal defers metadata persistence"
    );
    control.0.failures.store(1, Ordering::SeqCst);
    ui.app.dialog = None;
    ui.app.poll_runtime(&ui.ctx);
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    ui.view = TestView::Saving;
    ui.frame(Vec::new());
    ui.click(ui.point("放棄這次未保存變更"));
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
    assert!(ui.app.runtime.pending_clock_invalidation);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    ui.app.poll_runtime(&ui.ctx);
    wait_runtime(&mut ui);
    let current = ui.app.runtime.app.current.as_ref().unwrap();
    assert!(current.service_time_sync.battlenet.is_none());
    assert_eq!(current.revision, original.revision + 1);
    assert_eq!(current.groups, original.groups);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    let candidates = control.0.candidates.lock().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0].service_time_sync.battlenet.as_ref(),
        Some(&fresh_record)
    );
    assert!(candidates[1].service_time_sync.battlenet.is_none());
}

#[test]
fn invalidating_an_unsaved_sample_protects_both_it_and_the_saved_predecessor_after_discard() {
    let (mut ui, control, original, saved) = with_saved_clock(true);
    control.0.failures.store(1, Ordering::SeqCst);
    let fresh = sample(900);
    let fresh_record = fresh.record.clone();
    receive(&mut ui, fresh);
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending());
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    invalidate_shared_clock(&mut ui);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert!(
        ui.app
            .runtime
            .invalidated_clock_records
            .contains(&fresh_record)
    );
    ui.app.discard_unsaved_changes();
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
    assert!(ui.app.runtime.pending_clock_invalidation);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert!(
        ui.app
            .runtime
            .invalidated_clock_records
            .contains(&fresh_record)
    );
    ui.app.poll_runtime(&ui.ctx);
    wait_runtime(&mut ui);
    assert!(
        ui.app
            .runtime
            .app
            .current
            .as_ref()
            .unwrap()
            .service_time_sync
            .battlenet
            .is_none()
    );
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    ui.app.load_shared_clock();
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
}

#[test]
fn an_invalidated_inflight_sample_remains_invalid_after_its_storage_ack_and_reload() {
    let (mut ui, control, original, saved) = with_saved_clock(false);
    let fresh = sample(950);
    let fresh_record = fresh.record.clone();
    receive(&mut ui, fresh);
    assert!(ui.app.runtime.app.saving);
    invalidate_shared_clock(&mut ui);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert!(
        ui.app
            .runtime
            .invalidated_clock_records
            .contains(&fresh_record)
    );
    // Hold the automatic clear long enough to inspect the acknowledged but
    // already-invalid calibration and the subsequent shared-clock reload.
    ui.app.dialog = Some(Dialog::Info);
    control.0.released.store(true, Ordering::Release);
    wait_runtime(&mut ui);
    let current = ui.app.runtime.app.current.as_ref().unwrap();
    assert_eq!(
        current.service_time_sync.battlenet.as_ref(),
        Some(&fresh_record)
    );
    assert_eq!(current.revision, original.revision + 1);
    assert!(
        ui.app
            .runtime
            .invalidated_clock_records
            .contains(&fresh_record)
    );
    assert!(ui.app.runtime.pending_clock_invalidation);
    ui.app.load_shared_clock();
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
    assert!(ui.app.runtime.pending_clock_invalidation);
    ui.app.dialog = None;
    ui.app.poll_runtime(&ui.ctx);
    wait_runtime(&mut ui);
    assert!(
        ui.app
            .runtime
            .app
            .current
            .as_ref()
            .unwrap()
            .service_time_sync
            .battlenet
            .is_none()
    );
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
    let candidates = control.0.candidates.lock().unwrap();
    assert_eq!(candidates.len(), 2);
    assert_eq!(
        candidates[0].service_time_sync.battlenet.as_ref(),
        Some(&fresh_record)
    );
    assert!(candidates[1].service_time_sync.battlenet.is_none());
}

#[test]
fn restoring_a_full_backup_cannot_reactivate_a_calibration_already_known_invalid() {
    let (mut ui, control, _, saved) = with_saved_clock(true);
    invalidate_shared_clock(&mut ui);
    let mut source = VaultSnapshot::new_empty(IMPORT_TEST_TIME);
    source.service_time_sync.battlenet = Some(saved.clone());
    source.groups.push(bnauth_core::Group {
        id: uuid::Uuid::new_v4(),
        name: "來源完整分組".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    let preview = bnauth_core::prepare_full_restore(
        ui.app.runtime.app.current.as_ref(),
        &source,
        IMPORT_TEST_TIME + 1,
    )
    .unwrap();
    ui.app.dialog = Some(Dialog::Info);
    ui.app
        .runtime
        .app
        .restore(preview.candidate, false)
        .unwrap();
    ui.app.runtime.pending_full_restore = true;
    wait_runtime(&mut ui);
    assert_eq!(
        ui.app.runtime.app.current.as_ref().unwrap().groups,
        source.groups
    );
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
    assert!(ui.app.runtime.pending_clock_invalidation);
    ui.app.dialog = None;
    ui.app.poll_runtime(&ui.ctx);
    wait_runtime(&mut ui);
    assert!(
        ui.app
            .runtime
            .app
            .current
            .as_ref()
            .unwrap()
            .service_time_sync
            .battlenet
            .is_none()
    );
    assert_eq!(
        ui.app.runtime.app.current.as_ref().unwrap().groups,
        source.groups
    );
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert_eq!(control.0.candidates.lock().unwrap().len(), 2);
}

#[test]
fn discovering_old_invalidation_during_fresh_apply_preserves_fresh_candidate_but_protects_disk() {
    let (mut ui, control, original, saved) = with_saved_clock(true);
    ui.app.dialog = Some(Dialog::Info);
    let fresh = sample(1_100);
    let fresh_record = fresh.record.clone();
    receive(&mut ui, fresh);
    // Core's regression covers take_invalidation() becoming true inside apply.
    // The runtime must invalidate predecessors without marking fresh as invalid.
    ui.app.runtime.note_clock_invalidation(true);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert!(
        !ui.app
            .runtime
            .invalidated_clock_records
            .contains(&fresh_record)
    );
    assert_eq!(
        ui.app.runtime.pending_clock_record.as_ref(),
        Some(&fresh_record)
    );
    assert_eq!(ui.app.runtime.clock.offset_ms(), Some(1_100));
    assert!(!ui.app.runtime.pending_clock_invalidation);
    control.0.failures.store(1, Ordering::SeqCst);
    ui.app.dialog = None;
    ui.app.poll_runtime(&ui.ctx);
    wait_runtime(&mut ui);
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert!(ui.app.runtime.app.has_pending());
    ui.app.discard_unsaved_changes();
    assert_eq!(ui.app.runtime.clock.offset_ms(), None);
    assert!(ui.app.runtime.pending_clock_invalidation);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    ui.app.poll_runtime(&ui.ctx);
    wait_runtime(&mut ui);
    assert!(
        ui.app
            .runtime
            .app
            .current
            .as_ref()
            .unwrap()
            .service_time_sync
            .battlenet
            .is_none()
    );
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert_eq!(control.0.candidates.lock().unwrap().len(), 2);
}

#[test]
fn acknowledged_clear_or_new_calibration_does_not_let_old_backup_reactivate_invalid_result() {
    for save_new_sample in [false, true] {
        let (mut ui, control, _, invalid) = with_saved_clock(true);
        invalidate_shared_clock(&mut ui);
        if save_new_sample {
            ui.app.dialog = Some(Dialog::Info);
            receive(&mut ui, sample(1_250));
            ui.app.dialog = None;
        }
        ui.app.poll_runtime(&ui.ctx);
        wait_runtime(&mut ui);
        let acknowledged = ui.app.runtime.app.current.as_ref().unwrap();
        let acknowledged_revision = acknowledged.revision;
        if save_new_sample {
            assert_eq!(
                acknowledged
                    .service_time_sync
                    .battlenet
                    .as_ref()
                    .unwrap()
                    .offset_ms,
                1_250
            );
        } else {
            assert!(acknowledged.service_time_sync.battlenet.is_none());
        }
        assert!(ui.app.runtime.invalidated_clock_records.contains(&invalid));
        assert!(!ui.app.runtime.close_blocked());
        assert_eq!(control.0.candidates.lock().unwrap().len(), 1);

        let mut source = VaultSnapshot::new_empty(IMPORT_TEST_TIME);
        for (index, name) in [" 原備份分組 ", "備份空分組"].into_iter().enumerate() {
            source.groups.push(bnauth_core::Group {
                id: uuid::Uuid::new_v4(),
                name: name.into(),
                sort_order: index as u32 + 1,
                is_ungrouped: false,
            });
        }
        source = bnauth_core::prepare_import(
            &source,
            " 原文標題 ",
            " 原文備註\r\n第二行 ",
            source.groups[1].id,
            bnauth_core::parse_manual(
                "3132333435363738393031323334353637383930",
                "US-1234-5678-9012",
                "AbCd123456",
            )
            .unwrap(),
            IMPORT_TEST_TIME,
        )
        .unwrap()
        .candidate;
        source.authenticators[0].secret = None;
        source.authenticators[0]
            .legacy_metadata
            .push(bnauth_core::LegacyMetadata {
                source: "fixture".into(),
                source_version: Some("0.2.0".into()),
                field: "完整歷史".into(),
                original_value: " 原始歷史\r\n文字 ".into(),
                unit: None,
            });
        source.service_time_sync.battlenet = Some(invalid.clone());
        let original_source = source.clone();
        let preview = bnauth_core::prepare_full_restore(
            ui.app.runtime.app.current.as_ref(),
            &source,
            IMPORT_TEST_TIME + 1,
        )
        .unwrap();
        assert!(!preview.new_secret);
        let mut expected = preview.candidate;
        expected.service_time_sync.battlenet = None;
        full_restore_ui_tests::load_preview(&mut ui, &source);
        ui.click(ui.point("我確認以備份取代目前全部資料"));
        ui.click(ui.point("確認還原並保存"));
        assert!(ui.app.dialog.is_none());
        assert!(ui.app.runtime.pending_full_restore);
        wait_runtime(&mut ui);
        let restored = ui.app.runtime.app.current.as_ref().unwrap();
        assert_eq!(restored.groups, source.groups);
        assert_eq!(restored.authenticators, source.authenticators);
        assert!(restored.service_time_sync.battlenet.is_none());
        assert_eq!(restored.revision, acknowledged_revision + 1);
        assert_eq!(
            restored
                .provenance
                .last()
                .unwrap()
                .source_time_sync
                .as_ref(),
            Some(&invalid)
        );
        assert_eq!(ui.app.runtime.clock.offset_ms(), None);
        assert!(!ui.app.runtime.pending_clock_invalidation);
        assert!(ui.app.runtime.invalidated_clock_records.contains(&invalid));
        assert_eq!(source, original_source);
        let candidates = control.0.candidates.lock().unwrap();
        assert_eq!(candidates.len(), 2);
        assert_eq!(candidates[1], expected);
        drop(candidates);
        ui.app.load_shared_clock();
        for _ in 0..3 {
            ui.app.poll_runtime(&ui.ctx);
        }
        assert_eq!(ui.app.runtime.clock.offset_ms(), None);
        assert!(!ui.app.runtime.pending_clock_invalidation);
        assert!(!ui.app.runtime.clock_invalidation_inflight);
        assert!(!ui.app.runtime.close_blocked());
        assert_eq!(
            control.0.candidates.lock().unwrap().len(),
            2,
            "the old tombstone must not cause repeated metadata writes"
        );
        assert!(ui.app.runtime.invalidated_clock_records.contains(&invalid));
    }
}

#[test]
fn full_restore_confirmation_observes_the_old_clock_and_only_clears_a_matching_invalid_source() {
    for same_calibration in [false, true] {
        let (mut ui, control, original, saved) = with_saved_clock(true);
        let mut source = full_restore_ui_tests::source();
        let source_record = if same_calibration {
            saved.clone()
        } else {
            let mut fresh = saved.clone();
            fresh.server_time_ms += 1;
            fresh.local_wall_ms_at_sample += 1;
            assert_ne!(fresh, saved);
            assert_eq!(fresh.offset_ms, saved.offset_ms);
            fresh
        };
        source.service_time_sync.battlenet = Some(source_record.clone());
        let original_source = source.clone();
        let mut expected =
            bnauth_core::prepare_full_restore(Some(&original), &source, IMPORT_TEST_TIME + 1)
                .unwrap()
                .candidate;
        if same_calibration {
            expected.service_time_sync.battlenet = None;
        }
        full_restore_ui_tests::load_preview(&mut ui, &source);
        ui.click(ui.point("我確認以備份取代目前全部資料"));

        // No poll has observed this OS step. The actual confirmation handler
        // must inspect the old clock/storage context before selecting the source.
        ui.app.runtime.clock = RuntimeClock::from_saved_observation(
            Some(&saved),
            ClockObservation::at(bnauth_core::now_ms() + 3_600_000, Instant::now()),
        )
        .unwrap();
        assert!(ui.app.runtime.invalidated_clock_records.is_empty());
        assert!(!ui.app.runtime.clock.take_invalidation());
        ui.click(ui.point("確認還原並保存"));
        assert!(ui.app.dialog.is_none());
        assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
        assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
        wait_runtime(&mut ui);

        let restored = ui.app.runtime.app.current.as_ref().unwrap();
        assert_eq!(restored.revision, original.revision + 1);
        assert_eq!(restored.groups, source.groups);
        assert_eq!(restored.authenticators, source.authenticators);
        assert_eq!(restored.provenance, expected.provenance);
        assert_eq!(
            restored
                .provenance
                .last()
                .unwrap()
                .source_time_sync
                .as_ref(),
            Some(&source_record)
        );
        assert_eq!(restored.service_time_sync, expected.service_time_sync);
        assert!(!ui.app.runtime.pending_clock_invalidation);
        assert!(!ui.app.runtime.clock_invalidation_inflight);
        assert!(!ui.app.runtime.close_blocked());
        assert_eq!(source, original_source);
        let candidates = control.0.candidates.lock().unwrap();
        assert_eq!(candidates.as_slice(), &[expected]);
        drop(candidates);
        if same_calibration {
            assert_eq!(ui.app.runtime.clock.offset_ms(), None);
        } else {
            assert_eq!(
                ui.app.runtime.clock.offset_ms(),
                Some(source_record.offset_ms)
            );
            assert!(
                !ui.app
                    .runtime
                    .invalidated_clock_records
                    .contains(&source_record)
            );
        }
        for _ in 0..3 {
            ui.app.poll_runtime(&ui.ctx);
        }
        assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
    }
}

#[test]
fn loaded_and_full_restore_acknowledgements_observe_the_previous_clock_before_replacing_it() {
    for restore in [false, true] {
        for same_calibration in [false, true] {
            let (mut ui, control, original, saved) = with_saved_clock(true);
            let mut source = original.clone();
            let replacement = if same_calibration {
                saved.clone()
            } else {
                sample(925).record
            };
            source.service_time_sync.battlenet = Some(replacement.clone());
            source.groups[1].name = "來源組名與順序須完整保留".into();
            ui.app.dialog = Some(Dialog::Info); // Inspect the ACK before a deferred clear.
            if restore {
                let preview = bnauth_core::prepare_full_restore(
                    Some(&original),
                    &source,
                    IMPORT_TEST_TIME + 1,
                )
                .unwrap();
                ui.app
                    .runtime
                    .app
                    .restore(preview.candidate, false)
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

            // The OS step happens after the last observation and before the
            // asynchronous Loaded/restore ACK. This uses the actual core signal.
            let instant = Instant::now();
            let corrected_wall = bnauth_core::now_ms();
            ui.app.runtime.clock = RuntimeClock::from_saved_observation(
                Some(&saved),
                ClockObservation::at(corrected_wall + 3_600_000, instant),
            )
            .unwrap();
            assert!(!ui.app.runtime.clock.take_invalidation());
            let reading =
                ClockObservation::at(corrected_wall + 1_000, instant + Duration::from_secs(1));
            let deadline = Instant::now() + Duration::from_secs(5);
            loop {
                // Re-establish the same unobserved-step precondition before
                // every drain attempt. Whichever poll receives the real worker
                // ACK must observe this race, without scheduler sleeps/barriers.
                ui.app.runtime.clock = RuntimeClock::from_saved_observation(
                    Some(&saved),
                    ClockObservation::at(corrected_wall + 3_600_000, instant),
                )
                .unwrap();
                ui.app.runtime.invalidated_clock_records.clear();
                ui.app.runtime.pending_clock_invalidation = false;
                ui.app.poll_runtime_with_clock(&ui.ctx, || reading);
                if ui.app.runtime.app.phase == bnauth_app::Phase::Ready
                    && !ui.app.runtime.app.saving
                {
                    break;
                }
                assert!(Instant::now() < deadline, "ACK did not arrive");
                std::thread::yield_now();
            }
            let runtime = &ui.app.runtime;
            assert!(runtime.invalidated_clock_records.contains(&saved));
            assert_eq!(runtime.app.current.as_ref().unwrap().groups, source.groups);
            assert_eq!(
                runtime.app.current.as_ref().unwrap().authenticators,
                source.authenticators
            );
            assert_eq!(runtime.clock.is_synced(), !same_calibration);
            assert_eq!(runtime.pending_clock_invalidation, same_calibration);
            if same_calibration {
                assert_eq!(runtime.frame_time, corrected_wall + 1_000);
            } else {
                assert!(!runtime.invalidated_clock_records.contains(&replacement));
                assert_eq!(runtime.clock.last_success(), Some(&replacement));
                assert_eq!(
                    runtime.frame_time,
                    corrected_wall + 1_000 + replacement.offset_ms
                );
            }
        }
    }
}

#[test]
fn reload_rebases_at_the_old_context_observation_without_marking_the_new_source_invalid() {
    let (mut ui, _control, original, saved) = with_saved_clock(true);
    let instant = Instant::now();
    let wall = bnauth_core::now_ms();
    let start = ClockObservation::at(wall + 3_600_000, instant);
    let end = ClockObservation::at(wall + 1_000, instant + Duration::from_secs(1));
    ui.app.runtime.clock = RuntimeClock::from_saved_observation(Some(&saved), start).unwrap();
    let context = ui.app.runtime.clock_storage_context();
    let replacement = sample(625).record;
    let mut acknowledged = original;
    acknowledged.service_time_sync.battlenet = Some(replacement.clone());
    // Exercise the reload seam after an ACK changed current. The full worker
    // integration is covered above; this fixes the transition to one reading.
    ui.app.runtime.app.current = Some(acknowledged);
    ui.app.load_shared_clock_observed(end, &context);
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert!(
        !ui.app
            .runtime
            .invalidated_clock_records
            .contains(&replacement)
    );
    assert!(ui.app.runtime.clock.is_synced());
    assert!(!ui.app.runtime.pending_clock_invalidation);
    assert_eq!(ui.app.runtime.clock.observe(end), wall + 1_000 + 625);
    let later_step = ClockObservation::at(wall + 10_000, instant + Duration::from_secs(2));
    let context = ui.app.runtime.clock_storage_context();
    ui.app.observe_shared_clock(later_step, &context);
    assert!(
        ui.app
            .runtime
            .invalidated_clock_records
            .contains(&replacement)
    );
    assert!(!ui.app.runtime.clock.is_synced());
}

#[test]
fn discarding_a_clock_write_observes_a_step_before_loading_the_saved_predecessor() {
    let (mut ui, control, original, saved) = with_saved_clock(true);
    control.0.failures.store(1, Ordering::SeqCst);
    let fresh = sample(825);
    let inflight = fresh.record.clone();
    receive(&mut ui, fresh);
    wait_runtime(&mut ui);
    assert!(ui.app.runtime.app.has_pending());
    // Only this clock instance's baseline is advanced; no OS/global clock is
    // changed. The normal discard entry observes the real corrected time.
    ui.app.runtime.clock = RuntimeClock::from_saved_observation(
        Some(&inflight),
        ClockObservation::at(bnauth_core::now_ms() + 3_600_000, Instant::now()),
    )
    .unwrap();
    assert!(!ui.app.runtime.clock.take_invalidation());
    ui.app.discard_unsaved_changes();
    assert_eq!(ui.app.runtime.app.current.as_ref(), Some(&original));
    assert!(ui.app.runtime.invalidated_clock_records.contains(&saved));
    assert!(ui.app.runtime.invalidated_clock_records.contains(&inflight));
    assert!(!ui.app.runtime.clock.is_synced());
    assert!(ui.app.runtime.pending_clock_invalidation);
    assert!(!ui.app.runtime.app.has_pending());
    assert_eq!(control.0.candidates.lock().unwrap().len(), 1);
}
