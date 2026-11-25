//! Serialized background commits. The UI sees only acknowledged snapshots.
use bnauth_core::{ErrorCode, VaultSnapshot};
use std::sync::{
    Arc,
    mpsc::{self, Receiver, Sender},
};
use std::thread::{self, JoinHandle};

pub struct CommitOutcome {
    pub snapshot: VaultSnapshot,
    pub warning: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepositoryState {
    pub cleanup_required: bool,
    pub blocked: bool,
    pub read_only: bool,
}
impl RepositoryState {
    pub fn needs_attention(self) -> bool {
        self.cleanup_required || self.blocked
    }

    pub fn allows_writes(self) -> bool {
        !self.read_only && !self.needs_attention()
    }
}
pub trait Repository {
    /// Validate the complete candidate before writing; failure must retain the
    /// last acknowledged snapshot and report any unresolved transaction state.
    fn commit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String>;
    /// Edits to an acknowledged vault have already checked their changed rows,
    /// ordering and capacity. The worker still checks the revision and live clock.
    fn commit_edit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.commit(candidate)
    }
    /// The backup's groups and records were validated before displaying the preview.
    fn restore_from_preview(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.commit(candidate)
    }
    /// Leave ordinary save residue for startup cleanup without touching the files now.
    fn defer_cleanup(&mut self) {}
    /// Retry residue removal only. This must never commit or advance a revision.
    fn retry_cleanup(&mut self) -> Result<(), String>;
    /// Report retained files or an uncertain transaction even when an operation fails.
    fn state(&self) -> RepositoryState {
        RepositoryState::default()
    }
}
pub type OpenResult = Result<(Box<dyn Repository>, Option<VaultSnapshot>), String>;
pub type Factory = Box<dyn Fn() -> OpenResult + Send>;

enum Command {
    Open,
    Save(Arc<VaultSnapshot>, bool, SaveMode),
    DeferCleanup,
    Cleanup,
    Stop,
}
enum Response {
    Opened(Result<(Option<VaultSnapshot>, RepositoryState), String>),
    Saved(Result<CommitOutcome, String>, bool, RepositoryState),
    Cleaned(Result<(), String>, RepositoryState),
}
#[derive(Clone, Copy)]
enum SaveMode {
    Commit,
    Edit,
    RestorePreview,
}
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Phase {
    Loading,
    Missing,
    Ready,
    Error,
}
#[derive(Debug, PartialEq, Eq)]
pub enum Event {
    Loaded,
    Committed { calibrate_if_needed: bool },
    Cleaned,
    Failed,
}

pub struct Application {
    tx: Sender<Command>,
    rx: Receiver<Response>,
    worker: Option<JoinHandle<()>>,
    pub current: Option<Arc<VaultSnapshot>>,
    pub phase: Phase,
    pub saving: bool,
    pub cleaning: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub repository_state: RepositoryState,
    /// The worker borrows this snapshot through a shared handle for each save attempt.
    pending: Option<(Arc<VaultSnapshot>, bool, SaveMode)>,
}
impl Application {
    pub fn start(factory: Factory) -> Self {
        Self::start_with_notify(factory, || {})
    }

    pub fn start_with_notify(factory: Factory, notify: impl Fn() + Send + 'static) -> Self {
        let (tx, requests) = mpsc::channel();
        let (results, rx) = mpsc::channel();
        let worker = thread::spawn(move || {
            let mut repository: Option<Box<dyn Repository>> = None;
            while let Ok(command) = requests.recv() {
                let response = match command {
                    Command::Open => {
                        repository = None;
                        Response::Opened(match factory() {
                            Ok((repo, snapshot)) => {
                                let state = repo.state();
                                repository = Some(repo);
                                Ok((snapshot, state))
                            }
                            Err(error) => Err(error),
                        })
                    }
                    Command::Save(candidate, calibrate_if_needed, mode) => {
                        let repo = repository.as_mut().expect("Save follows a successful Open");
                        let result = match mode {
                            SaveMode::Commit => repo.commit(&candidate),
                            SaveMode::Edit => repo.commit_edit(&candidate),
                            SaveMode::RestorePreview => repo.restore_from_preview(&candidate),
                        };
                        Response::Saved(result, calibrate_if_needed, repo.state())
                    }
                    Command::DeferCleanup => {
                        let repo = repository.as_mut().expect("Discard follows a failed save");
                        repo.defer_cleanup();
                        continue;
                    }
                    Command::Cleanup => {
                        let repo = repository
                            .as_mut()
                            .expect("Cleanup follows a successful Open");
                        let result = repo.retry_cleanup();
                        Response::Cleaned(result, repo.state())
                    }
                    Command::Stop => break,
                };
                if results.send(response).is_err() {
                    break;
                }
                notify();
            }
            // Release the native path mutex on the thread that acquired it.
            drop(repository);
        });
        let mut app = Self {
            tx,
            rx,
            worker: Some(worker),
            current: None,
            phase: Phase::Loading,
            saving: false,
            cleaning: false,
            error: None,
            warning: None,
            repository_state: RepositoryState::default(),
            pending: None,
        };
        if let Err(error) = app.send(Command::Open) {
            app.phase = Phase::Error;
            app.error = Some(error);
        }
        app
    }
    /// Queue a candidate. Validation and storage run on the repository worker;
    /// their result is acknowledged by `poll`, with failures retained for retry.
    pub fn commit(
        &mut self,
        candidate: VaultSnapshot,
        calibrate_if_needed: bool,
    ) -> Result<(), String> {
        self.commit_prepared(calibrate_if_needed, || candidate)
    }
    /// Take ownership of prepared data only after the save can be queued.
    pub fn commit_prepared(
        &mut self,
        calibrate_if_needed: bool,
        take_candidate: impl FnOnce() -> VaultSnapshot,
    ) -> Result<(), String> {
        self.queue(take_candidate, calibrate_if_needed, SaveMode::Commit)
    }
    /// Queue a locally checked edit derived from the acknowledged full vault.
    /// Keep this origin on retry, when only the pending clock can be replaced.
    pub fn commit_edit(
        &mut self,
        calibrate_if_needed: bool,
        take_candidate: impl FnOnce() -> VaultSnapshot,
    ) -> Result<(), String> {
        self.queue(take_candidate, calibrate_if_needed, SaveMode::Edit)
    }
    /// Queue the candidate from a checked restore preview. Only the clock can change on retry.
    pub fn restore_from_preview(
        &mut self,
        take_candidate: impl FnOnce() -> VaultSnapshot,
    ) -> Result<(), String> {
        self.queue(take_candidate, false, SaveMode::RestorePreview)
    }
    fn queue(
        &mut self,
        take_candidate: impl FnOnce() -> VaultSnapshot,
        calibrate_if_needed: bool,
        mode: SaveMode,
    ) -> Result<(), String> {
        if self.saving || self.cleaning || self.phase == Phase::Loading || self.pending.is_some() {
            return Err("前一次保存或暫存檔移除尚未完成。".into());
        }
        self.require_repository()?;
        self.require_writable()?;
        if self.repository_state.read_only {
            return Err(ErrorCode::PermissionDenied.notice().to_string());
        }
        self.pending = Some((Arc::new(take_candidate()), calibrate_if_needed, mode));
        self.retry()
    }
    pub fn retry(&mut self) -> Result<(), String> {
        self.retry_pending(|_| {})
    }

    pub fn pending_time_sync(&self) -> Option<&bnauth_core::TimeSyncRecord> {
        self.pending
            .as_ref()
            .and_then(|(snapshot, _, _)| snapshot.service_time_sync.battlenet.as_ref())
    }

    /// Refresh the calibration in a pending save before retrying.
    pub fn retry_with_clock(
        &mut self,
        record: Option<bnauth_core::TimeSyncRecord>,
    ) -> Result<(), String> {
        self.retry_pending(|snapshot| Arc::make_mut(snapshot).service_time_sync.battlenet = record)
    }

    fn retry_pending(
        &mut self,
        update: impl FnOnce(&mut Arc<VaultSnapshot>),
    ) -> Result<(), String> {
        if self.saving || self.cleaning {
            return Err("正在保存資料或移除暫存檔，請稍候。".into());
        }
        self.require_repository()?;
        // A retained candidate can retry after the file's read-only attribute
        // changes. Let the repository check the live attribute on each attempt.
        self.require_writable()?;
        let (snapshot, calibrate_if_needed, mode) =
            self.pending.as_mut().ok_or("沒有需要保存的變更。")?;
        update(snapshot);
        let command = Command::Save(Arc::clone(snapshot), *calibrate_if_needed, *mode);
        self.send(command)?;
        self.error = None;
        self.saving = true;
        Ok(())
    }
    pub fn retry_cleanup(&mut self) -> Result<(), String> {
        if self.saving || self.cleaning {
            return Err("正在保存資料或移除暫存檔，請稍候。".into());
        }
        self.require_repository()?;
        if self.repository_state.blocked {
            return Err("無法確認資料檔的目前狀態，已停止自動移除暫存檔。請保留現有檔案，關閉程式後再處理。".into());
        }
        // A failed save may also leave temporary files. Preserve its candidate and
        // let the caller separately retry the save after cleanup succeeds.
        self.send(Command::Cleanup)?;
        self.error = None;
        self.cleaning = true;
        Ok(())
    }
    pub fn has_pending(&self) -> bool {
        self.pending.is_some()
    }

    pub fn shows_save_status(&self) -> bool {
        self.saving
            || self.cleaning
            || self.has_pending()
            || (self.warning.is_some() && !self.repository_state.blocked)
    }

    pub fn discard(&mut self) {
        if self.saving || self.cleaning {
            return;
        }
        let pending = self.pending.take();
        if pending.is_some() || !self.repository_state.blocked {
            self.error = None;
        }
        if pending.is_some()
            && self.repository_state.cleanup_required
            && !self.repository_state.blocked
        {
            let _ = self.tx.send(Command::DeferCleanup);
            self.repository_state.cleanup_required = false;
        }
    }
    pub fn reload(&mut self) -> Result<(), String> {
        if self.saving || self.cleaning || self.has_pending() {
            return Err("請先處理尚未保存的變更或尚未完成的暫存檔移除，再重新載入。".into());
        }
        if self.phase == Phase::Loading {
            return Err("仍在載入中。".into());
        }
        if self.repository_state.cleanup_required {
            return Err(
                "仍有暫存檔尚未移除。請先清理暫存檔，或保留現有檔案，關閉程式後再處理。".into(),
            );
        }
        self.send(Command::Open)?;
        self.phase = Phase::Loading;
        self.error = None;
        Ok(())
    }
    fn require_repository(&self) -> Result<(), String> {
        match self.phase {
            Phase::Loading => Err("資料檔仍在載入中。".into()),
            Phase::Error => Err("請先重新載入資料檔。".into()),
            Phase::Missing | Phase::Ready => Ok(()),
        }
    }
    fn require_writable(&self) -> Result<(), String> {
        if self.repository_state.blocked {
            Err("無法確認資料檔的目前狀態，已停止保存。請保留現有檔案，重新載入或關閉程式後再處理。".into())
        } else if self.repository_state.cleanup_required {
            Err("上次保存留下的暫存檔尚未移除。請先清理暫存檔；移除時不會重新保存資料。".into())
        } else {
            Ok(())
        }
    }
    fn send(&self, command: Command) -> Result<(), String> {
        self.tx.send(command).map_err(|error| {
            let code = match error.0 {
                Command::Open => ErrorCode::ReadFailed,
                Command::Cleanup => ErrorCode::DeleteFailed,
                _ => ErrorCode::WriteFailed,
            };
            code.notice().to_string()
        })
    }
    pub fn poll(&mut self) -> Vec<Event> {
        let mut events = Vec::new();
        while let Ok(response) = self.rx.try_recv() {
            match response {
                Response::Opened(Ok((snapshot, state))) => {
                    self.phase = if snapshot.is_some() {
                        Phase::Ready
                    } else {
                        Phase::Missing
                    };
                    self.current = snapshot.map(Arc::new);
                    self.error = None;
                    self.warning = None;
                    self.repository_state = state;
                    events.push(Event::Loaded);
                }
                Response::Opened(Err(error)) => {
                    self.phase = Phase::Error;
                    self.error = Some(error);
                    // A failed reopen cannot resolve a previously blocked transaction.
                    // Only a successful open replaces that state with a fresh repository.
                    events.push(Event::Failed);
                }
                Response::Saved(Ok(result), calibrate_if_needed, state) => {
                    self.current = Some(Arc::new(result.snapshot));
                    self.warning = result.warning;
                    self.pending = None;
                    self.saving = false;
                    self.error = None;
                    self.phase = Phase::Ready;
                    self.repository_state = state;
                    events.push(Event::Committed {
                        calibrate_if_needed,
                    });
                }
                Response::Saved(Err(error), _, state) => {
                    self.saving = false;
                    self.error = Some(error);
                    self.repository_state = state;
                    events.push(Event::Failed);
                }
                Response::Cleaned(Ok(()), state) => {
                    self.cleaning = false;
                    self.warning = None;
                    self.error = None;
                    self.repository_state = state;
                    events.push(Event::Cleaned);
                }
                Response::Cleaned(Err(error), state) => {
                    self.cleaning = false;
                    self.error = Some(error);
                    self.repository_state = state;
                    events.push(Event::Failed);
                }
            }
        }
        events
    }
}
impl Drop for Application {
    fn drop(&mut self) {
        let _ = self.tx.send(Command::Stop);
        if let Some(worker) = self.worker.take() {
            let _ = worker.join();
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::{Arc, Mutex};
    use std::time::{Duration, Instant};
    #[derive(Default)]
    struct Behavior {
        fail_open: bool,
        fail_commit: bool,
        fail_cleanup: bool,
        leave_cleanup_on_failed_commit: bool,
        block_on_failed_commit: bool,
        cleanup_required: bool,
        blocked: bool,
        commits: usize,
        cleanups: usize,
    }
    struct MemoryRepo {
        behavior: Arc<Mutex<Behavior>>,
    }
    impl Repository for MemoryRepo {
        fn commit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
            let mut behavior = self.behavior.lock().unwrap();
            behavior.commits += 1;
            if behavior.fail_commit {
                behavior.cleanup_required |= behavior.leave_cleanup_on_failed_commit;
                behavior.blocked |= behavior.block_on_failed_commit;
                return Err("injected disk failure".into());
            }
            let mut snapshot = candidate.clone();
            snapshot.revision += 1;
            Ok(CommitOutcome {
                snapshot,
                warning: None,
            })
        }
        fn retry_cleanup(&mut self) -> Result<(), String> {
            let mut behavior = self.behavior.lock().unwrap();
            behavior.cleanups += 1;
            if behavior.fail_cleanup {
                return Err("injected cleanup failure".into());
            }
            behavior.cleanup_required = false;
            Ok(())
        }
        fn defer_cleanup(&mut self) {
            self.behavior.lock().unwrap().cleanup_required = false;
        }
        fn state(&self) -> RepositoryState {
            let behavior = self.behavior.lock().unwrap();
            RepositoryState {
                cleanup_required: behavior.cleanup_required,
                blocked: behavior.blocked,
                read_only: false,
            }
        }
    }
    fn start_memory(original: &VaultSnapshot, behavior: &Arc<Mutex<Behavior>>) -> Application {
        let behavior = behavior.clone();
        let opened = original.clone();
        Application::start(Box::new(move || {
            if behavior.lock().unwrap().fail_open {
                return Err("injected open failure".into());
            }
            Ok((
                Box::new(MemoryRepo {
                    behavior: behavior.clone(),
                }),
                Some(opened.clone()),
            ))
        }))
    }
    fn drain(app: &mut Application) -> Vec<Event> {
        let deadline = Instant::now() + Duration::from_secs(5);
        let mut events = Vec::new();
        loop {
            events.extend(app.poll());
            if app.phase != Phase::Loading && !app.saving && !app.cleaning {
                return events;
            }
            assert!(Instant::now() < deadline);
            thread::yield_now();
        }
    }
    #[test]
    fn open_failure_never_becomes_an_empty_writable_vault() {
        let mut app = Application::start(Box::new(|| Err("damaged file".into())));
        drain(&mut app);
        assert_eq!(app.phase, Phase::Error);
        assert!(app.current.is_none());
        assert!(app.commit(VaultSnapshot::new_empty(), false).is_err());
        assert!(app.retry_cleanup().is_err());
    }
    #[test]
    fn cleanup_after_failed_save_keeps_candidate_until_explicit_save_retry() {
        let original = VaultSnapshot::new_empty();
        let behavior = Arc::new(Mutex::new(Behavior {
            fail_commit: true,
            leave_cleanup_on_failed_commit: true,
            ..Behavior::default()
        }));
        let mut app = start_memory(&original, &behavior);
        drain(&mut app);
        let mut candidate = original.clone();
        candidate.groups.push(bnauth_core::Group {
            id: bnauth_core::Uuid::new_v4(),
            name: "Test group".into(),
            sort_order: 1,
            is_ungrouped: false,
        });
        app.commit(candidate.clone(), true).unwrap();
        drain(&mut app);
        assert!(app.repository_state.cleanup_required);
        app.retry_cleanup().unwrap();
        app.discard();
        assert!(app.has_pending());
        assert!(app.retry().is_err());
        assert!(app.commit(original.clone(), false).is_err());
        assert_eq!(drain(&mut app), [Event::Cleaned]);
        assert!(!app.repository_state.cleanup_required);
        assert_eq!(app.current.as_deref(), Some(&original));
        assert!(app.has_pending());
        assert_eq!(app.pending.as_ref().unwrap().0.as_ref(), &candidate);
        assert_eq!(behavior.lock().unwrap().commits, 1);
        assert_eq!(behavior.lock().unwrap().cleanups, 1);
        behavior.lock().unwrap().fail_commit = false;
        app.retry().unwrap();
        assert_eq!(
            drain(&mut app),
            [Event::Committed {
                calibrate_if_needed: true
            }]
        );
        candidate.revision += 1;
        assert_eq!(app.current.as_deref(), Some(&candidate));
        assert!(!app.has_pending());
        assert_eq!(behavior.lock().unwrap().commits, 2);
    }
    #[test]
    fn discard_defers_failed_commit_cleanup_and_allows_another_save() {
        let original = VaultSnapshot::new_empty();
        let behavior = Arc::new(Mutex::new(Behavior {
            fail_commit: true,
            leave_cleanup_on_failed_commit: true,
            fail_cleanup: true,
            ..Behavior::default()
        }));
        let mut app = start_memory(&original, &behavior);
        drain(&mut app);
        app.commit(original.clone(), false).unwrap();
        assert_eq!(drain(&mut app), [Event::Failed]);
        assert!(app.repository_state.cleanup_required);
        app.discard();
        assert!(!app.has_pending());
        assert!(!app.repository_state.needs_attention());
        assert!(app.error.is_none() && app.warning.is_none());
        assert_eq!(app.current.as_deref(), Some(&original));
        assert_eq!(behavior.lock().unwrap().commits, 1);

        behavior.lock().unwrap().fail_commit = false;
        app.commit(original.clone(), false).unwrap();
        assert_eq!(
            drain(&mut app),
            [Event::Committed {
                calibrate_if_needed: false
            }]
        );
        let behavior = behavior.lock().unwrap();
        assert!(!behavior.cleanup_required);
        assert_eq!(behavior.commits, 2);
        assert_eq!(behavior.cleanups, 0);
    }
    #[test]
    fn blocked_failure_survives_discard_and_failed_reload_until_successful_reload() {
        let original = VaultSnapshot::new_empty();
        let behavior = Arc::new(Mutex::new(Behavior {
            fail_commit: true,
            block_on_failed_commit: true,
            ..Behavior::default()
        }));
        let mut app = start_memory(&original, &behavior);
        drain(&mut app);
        app.commit(original.clone(), false).unwrap();
        assert_eq!(drain(&mut app), [Event::Failed]);
        app.discard();
        assert!(!app.has_pending());
        assert!(app.error.is_none());
        assert!(app.repository_state.blocked);
        assert!(!app.repository_state.cleanup_required);
        assert!(app.retry_cleanup().is_err());
        assert!(app.commit(original.clone(), false).is_err());
        behavior.lock().unwrap().fail_open = true;
        app.reload().unwrap();
        assert_eq!(drain(&mut app), [Event::Failed]);
        assert_eq!(app.phase, Phase::Error);
        assert!(app.repository_state.blocked);
        assert!(!app.repository_state.cleanup_required);
        assert_eq!(app.error.as_deref(), Some("injected open failure"));
        app.discard();
        assert!(app.repository_state.needs_attention());
        assert!(app.error.is_some());
        assert_eq!(app.current.as_deref(), Some(&original));
        {
            let mut behavior = behavior.lock().unwrap();
            // Simulate a new repository opened after the external state was repaired.
            behavior.fail_open = false;
            behavior.blocked = false;
            behavior.fail_commit = false;
        }
        app.reload().unwrap();
        assert_eq!(drain(&mut app), [Event::Loaded]);
        assert_eq!(app.phase, Phase::Ready);
        assert!(!app.repository_state.needs_attention());
        assert!(app.error.is_none());
        assert_eq!(app.current.as_deref(), Some(&original));
        assert_eq!(behavior.lock().unwrap().commits, 1);
        assert_eq!(behavior.lock().unwrap().cleanups, 0);
    }
}

#[cfg(test)]
mod application_restore_tests;
