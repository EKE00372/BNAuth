//! In-memory runtime used by UI tests; all records follow the production snapshot format.
use std::time::{Duration, Instant};

struct DialogMemoryRepository;

impl bnauth_app::Repository for DialogMemoryRepository {
    fn restore(
        &mut self,
        candidate: &bnauth_core::VaultSnapshot,
    ) -> Result<bnauth_app::CommitOutcome, String> {
        self.commit(candidate)
    }

    fn commit(
        &mut self,
        candidate: &bnauth_core::VaultSnapshot,
    ) -> Result<bnauth_app::CommitOutcome, String> {
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

pub fn drain_memory_app(app: &mut bnauth_app::Application) {
    let deadline = Instant::now() + Duration::from_secs(5);
    loop {
        app.poll();
        if app.phase != bnauth_app::Phase::Loading && !app.saving {
            assert!(app.error.is_none(), "{:?}", app.error);
            return;
        }
        assert!(
            Instant::now() < deadline,
            "memory repository did not finish"
        );
        std::thread::yield_now();
    }
}

pub fn runtime(vault: bnauth_core::VaultSnapshot, frame_time: i64) -> crate::runtime::RuntimeState {
    vault.validate().unwrap();
    let mut app = bnauth_app::Application::start(Box::new(move || {
        Ok((Box::new(DialogMemoryRepository), Some(vault.clone())))
    }));
    drain_memory_app(&mut app);
    crate::runtime::RuntimeState {
        app,
        path: Default::default(),
        clock: bnauth_core::clock::RuntimeClock::new(),
        frame_time,
        sync_rx: None,
        sync_manual: false,
        export_rx: None,
        group_import_rx: None,
        pending_import_group: None,
        full_restore_rx: None,
        pending_full_restore: false,
        pending_clock_record: None,
        clock_save_inflight: None,
        pending_clock_invalidation: false,
        clock_invalidation_inflight: false,
        invalidated_clock_records: Vec::new(),
        pending_clock_manual: false,
        clock_save_manual: false,
        close_requested: false,
        allow_close_with_residue: false,
        clock_notices: Default::default(),
    }
}
