use super::*;
use crate::runtime::SYNCING_NOTICE;
use bnauth_core::clock::{ClockObservation, RuntimeClock};

impl BnAuthApp {
    pub(super) fn reload_with_clock_cancel(&mut self) -> Result<(), String> {
        self.runtime.app.reload()?;
        self.observe_shared_clock(ClockObservation::now());
        self.cancel_clock_work();
        Ok(())
    }

    pub(super) fn observe_shared_clock(&mut self, observation: ClockObservation) {
        let runtime = &mut self.runtime;
        runtime.frame_time = runtime.clock.observe(observation);
    }

    /// An explicit reload abandons clock work tied to the previous open vault.
    /// Call after queuing the reload, so a rejected reload preserves pending work.
    pub(super) fn cancel_clock_work(&mut self) {
        if self.runtime.sync_manual {
            self.notifications.clear_status_if(SYNCING_NOTICE);
        }
        let runtime = &mut self.runtime;
        runtime.clear_pending_clock_save();
        runtime.sync_rx = None;
        runtime.sync_manual = false;
        runtime.pending_clock_merge = false;
        runtime.clock_notices = crate::runtime::ClockStatusNotices::default();
    }

    /// Check the running clock before returning to the saved calibration.
    pub(super) fn load_shared_clock(&mut self) {
        let observation = ClockObservation::now();
        self.observe_shared_clock(observation);
        self.rebase_shared_clock(observation, false);
    }

    /// Reuse the reading that was already checked against the previous clock.
    pub(super) fn rebase_shared_clock(
        &mut self,
        observation: ClockObservation,
        preserve_manual_clock: bool,
    ) {
        let runtime = &mut self.runtime;
        if preserve_manual_clock
            && runtime.pending_clock_manual
            && runtime.pending_clock_record.is_some()
        {
            // The restore ACK already consumed any sample included in that save.
            // A later sample is still live and can be saved against the new vault.
            return;
        }
        if preserve_manual_clock && runtime.sync_manual {
            runtime.clear_pending_clock_save();
        } else {
            self.cancel_clock_work();
        }
        let runtime = &mut self.runtime;
        let saved = runtime
            .app
            .current
            .as_ref()
            .and_then(|snapshot| snapshot.service_time_sync.battlenet.as_ref());
        match RuntimeClock::from_saved_observation(saved, observation) {
            Ok(mut clock) => {
                runtime.frame_time = clock.observe(observation);
                runtime.clock = clock;
            }
            Err(error) => {
                runtime.clock = RuntimeClock::new();
                runtime.clock.note_failure(error);
                runtime.frame_time = runtime.clock.now_ms();
            }
        }
    }

    /// A calibration waits for acknowledged data; it never writes an old draft.
    pub(super) fn save_shared_clock_if_ready(&mut self) {
        let runtime = &mut self.runtime;
        if runtime.app.phase != bnauth_app::Phase::Ready
            || runtime.app.saving
            || runtime.app.cleaning
            || runtime.app.has_pending()
            || runtime.app.warning.is_some()
            || !runtime.app.repository_state.allows_writes()
            || runtime.file_job_in_progress()
            || if runtime.close_requested {
                runtime.recovery.in_progress()
            } else {
                self.dialog.is_some()
            }
        {
            return;
        }
        let (Some(record), Some(current)) = (
            runtime.pending_clock_record.as_ref(),
            runtime.app.current.as_deref(),
        ) else {
            return;
        };
        if current.service_time_sync.battlenet.as_ref() == Some(record) {
            runtime.clear_pending_clock_save();
            return;
        }
        let candidate = current.with_battlenet_clock(Some(record.clone()));
        match runtime.app.commit_edit(false, || candidate) {
            Ok(()) => {
                runtime.clock_save_inflight = Some(record.clone());
                runtime.clock_save_manual = runtime.pending_clock_manual;
            }
            Err(error) => {
                runtime.app.error = Some(error.clone());
                if runtime.pending_clock_manual {
                    self.warn(error);
                }
            }
        }
    }

    pub(super) fn discard_unsaved_changes(&mut self) {
        let clock_write =
            !self.runtime.pending_clock_merge && self.runtime.clock_save_inflight.is_some();
        if !clock_write {
            self.observe_shared_clock(ClockObservation::now());
        }
        self.runtime.app.discard();
        if self.runtime.app.has_pending() {
            return;
        }
        self.runtime.pending_import = None;
        self.runtime.pending_groups = None;
        self.runtime.pending_full_restore = false;
        if std::mem::take(&mut self.runtime.pending_clock_merge) {
            self.runtime.clock_save_inflight = None;
            self.runtime.clock_save_manual = false;
        }
        if let Some(edit) = self.runtime.pending_list_edit.take() {
            self.rollback_list_edit(edit);
        }
        if clock_write {
            self.load_shared_clock();
        }
    }
}
