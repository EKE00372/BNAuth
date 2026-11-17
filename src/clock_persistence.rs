use super::*;
use crate::runtime::ClockStorageContext;
use bnauth_core::clock::{ClockObservation, RuntimeClock};

impl BnAuthApp {
    pub(super) fn reload_with_clock_cancel(&mut self) -> Result<(), String> {
        self.runtime.app.reload()?;
        let context = self.runtime.clock_storage_context();
        self.observe_shared_clock(ClockObservation::now(), &context);
        self.cancel_clock_work();
        Ok(())
    }

    pub(super) fn observe_shared_clock(
        &mut self,
        observation: ClockObservation,
        context: &ClockStorageContext,
    ) {
        let runtime = &mut self.runtime;
        runtime.frame_time = runtime.clock.observe(observation);
        if runtime.clock.take_invalidation() {
            runtime.note_clock_invalidation_from(runtime.clock.is_synced(), context);
        }
    }

    /// An explicit reload abandons clock work tied to the previous open vault.
    /// Call after queuing the reload, so a rejected reload preserves pending work.
    pub(super) fn cancel_clock_work(&mut self) {
        let runtime = &mut self.runtime;
        runtime.pending_clock_record = None;
        runtime.clock_save_inflight = None;
        runtime.pending_clock_invalidation = false;
        runtime.clock_invalidation_inflight = false;
        runtime.sync_rx = None;
        runtime.sync_manual = false;
        runtime.pending_clock_manual = false;
        runtime.clock_save_manual = false;
        runtime.clock_notices = crate::runtime::ClockStatusNotices::default();
    }

    /// Opening or restoring a complete working vault selects its shared clock.
    pub(super) fn load_shared_clock(&mut self) {
        let context = self.runtime.clock_storage_context();
        self.load_shared_clock_observed(ClockObservation::now(), &context);
    }

    pub(super) fn load_shared_clock_observed(
        &mut self,
        observation: ClockObservation,
        context: &ClockStorageContext,
    ) {
        self.observe_shared_clock(observation, context);
        self.cancel_clock_work();
        let runtime = &mut self.runtime;
        let saved = runtime
            .app
            .current
            .as_ref()
            .and_then(|snapshot| snapshot.service_time_sync.battlenet.as_ref());
        let known_invalid =
            saved.is_some_and(|record| runtime.invalidated_clock_records.contains(record));
        let record = if known_invalid { None } else { saved };
        runtime.pending_clock_invalidation = known_invalid;
        match RuntimeClock::from_saved_observation(record, observation) {
            Ok(clock) => runtime.clock = clock,
            Err(error) => {
                runtime.clock = RuntimeClock::new();
                runtime
                    .clock
                    .note_failure(format!("無法套用已保存校時：{error}"));
            }
        }
        self.otp_cache.clear();
    }

    /// A calibration waits for acknowledged data; it never writes an old draft.
    pub(super) fn save_shared_clock_if_ready(&mut self) {
        let runtime = &mut self.runtime;
        if runtime.allow_close_with_residue
            || runtime.app.phase != bnauth_app::Phase::Ready
            || runtime.app.saving
            || runtime.app.cleaning
            || runtime.app.has_pending()
            || runtime.app.warning.is_some()
            || runtime.app.repository_state.needs_attention()
            || runtime.export_rx.is_some()
            || runtime.group_import_rx.is_some()
            || runtime.full_restore_rx.is_some()
            || (self.dialog.is_some() && !runtime.close_requested)
        {
            return;
        }
        if !runtime.pending_clock_invalidation && runtime.pending_clock_record.is_none() {
            return;
        }
        let Some(current) = &runtime.app.current else {
            return;
        };
        let invalidation = runtime.pending_clock_invalidation;
        let record = if invalidation {
            None
        } else {
            runtime.pending_clock_record.clone()
        };
        if current.service_time_sync.battlenet == record {
            let manual = runtime.pending_clock_manual;
            runtime.pending_clock_record = None;
            runtime.clock_save_inflight = None;
            runtime.pending_clock_invalidation = false;
            runtime.clock_invalidation_inflight = false;
            runtime.pending_clock_manual = false;
            runtime.clock_save_manual = false;
            if manual {
                self.notify("校時成功並已保存");
            }
            return;
        }
        let mut candidate = current.clone();
        candidate.service_time_sync.battlenet = record.clone();
        match runtime.app.commit(candidate, false) {
            Ok(()) => {
                runtime.clock_save_inflight = record;
                runtime.clock_invalidation_inflight = invalidation;
                runtime.clock_save_manual = runtime.pending_clock_manual;
            }
            Err(error) => {
                runtime.app.error = Some(error.clone());
                if runtime.pending_clock_manual {
                    self.warn(format!("校時結果尚未保存：{error}"));
                }
            }
        }
    }

    pub(super) fn discard_unsaved_changes(&mut self) {
        let observation = ClockObservation::now();
        let context = self.runtime.clock_storage_context();
        self.observe_shared_clock(observation, &context);
        let clock_write = self.runtime.clock_save_inflight.is_some();
        let invalidation_write = self.runtime.clock_invalidation_inflight;
        self.runtime.app.discard();
        if !self.runtime.app.has_pending() {
            if let Some(snapshot) = &self.runtime.app.current {
                self.model.apply_snapshot(snapshot);
                self.otp_cache.clear();
            }
            if clock_write {
                // Discard does not replace current or the runtime in-flight
                // marker; the standalone loader can capture that context again.
                self.load_shared_clock();
            }
            if invalidation_write {
                self.runtime.pending_clock_invalidation = false;
                self.runtime.clock_invalidation_inflight = false;
                self.warn("校時失效狀態未保存；下次開啟請先重新校時。".to_owned());
            }
        }
    }
}
