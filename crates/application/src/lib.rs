//! Serialized background commits. The UI sees only acknowledged snapshots.
use bnauth_core::VaultSnapshot;
use std::path::PathBuf;
use std::sync::mpsc::{self, Receiver, Sender};
use std::thread::{self, JoinHandle};

pub struct CommitOutcome {
    pub snapshot: VaultSnapshot,
    pub warning: Option<String>,
}
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct RepositoryState {
    pub cleanup_required: bool,
    pub blocked: bool,
}
impl RepositoryState {
    pub fn needs_attention(self) -> bool {
        self.cleanup_required || self.blocked
    }
}
pub trait Repository {
    /// Validate the complete candidate before writing; failure must retain the
    /// last acknowledged snapshot and report any unresolved transaction state.
    fn commit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String>;
    /// Restore a complete candidate, preserving a verified pre-restore copy
    /// before replacing an existing primary. Unsupported repositories refuse.
    fn restore(&mut self, _candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        Err("目前保存介面不支援完整還原。".into())
    }
    fn last_restore_backup(&self) -> Option<PathBuf> {
        None
    }
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
    Save(Box<VaultSnapshot>, bool, SaveMode),
    Cleanup,
    Stop,
}
enum Response {
    Opened(Result<(Option<VaultSnapshot>, RepositoryState), String>),
    Saved(
        Result<CommitOutcome, String>,
        bool,
        RepositoryState,
        Option<PathBuf>,
    ),
    Cleaned(Result<(), String>, RepositoryState),
}
#[derive(Clone, Copy)]
enum SaveMode {
    Commit,
    Restore,
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
    Committed { new_secret: bool },
    Cleaned,
    Failed,
}

pub struct Application {
    tx: Sender<Command>,
    rx: Receiver<Response>,
    worker: Option<JoinHandle<()>>,
    pub current: Option<VaultSnapshot>,
    pub phase: Phase,
    pub saving: bool,
    pub cleaning: bool,
    pub error: Option<String>,
    pub warning: Option<String>,
    pub repository_state: RepositoryState,
    pub restore_backup: Option<PathBuf>,
    pending: Option<(VaultSnapshot, bool, SaveMode)>,
}
impl Application {
    pub fn start(factory: Factory) -> Self {
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
                    Command::Save(candidate, new_secret, mode) => {
                        let repo = repository.as_mut().expect("Save follows a successful Open");
                        let result = match mode {
                            SaveMode::Commit => repo.commit(&candidate),
                            SaveMode::Restore => repo.restore(&candidate),
                        };
                        let backup = if matches!(mode, SaveMode::Restore) && result.is_ok() {
                            repo.last_restore_backup()
                        } else {
                            None
                        };
                        Response::Saved(result, new_secret, repo.state(), backup)
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
            restore_backup: None,
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
    pub fn commit(&mut self, candidate: VaultSnapshot, new_secret: bool) -> Result<(), String> {
        self.queue(candidate, new_secret, SaveMode::Commit)
    }
    pub fn restore(&mut self, candidate: VaultSnapshot, new_secret: bool) -> Result<(), String> {
        self.queue(candidate, new_secret, SaveMode::Restore)
    }
    fn queue(
        &mut self,
        candidate: VaultSnapshot,
        new_secret: bool,
        mode: SaveMode,
    ) -> Result<(), String> {
        if self.saving || self.cleaning || self.phase == Phase::Loading || self.pending.is_some() {
            return Err("前一次保存或暫存檔移除尚未完成。".into());
        }
        self.require_repository()?;
        self.require_writable()?;
        self.restore_backup = None;
        self.pending = Some((candidate, new_secret, mode));
        self.retry()
    }
    pub fn retry(&mut self) -> Result<(), String> {
        if self.saving || self.cleaning {
            return Err("正在保存資料或移除暫存檔，請稍候。".into());
        }
        self.require_repository()?;
        self.require_writable()?;
        let (snapshot, new_secret, mode) = self.pending.as_ref().ok_or("沒有需要保存的變更。")?;
        self.send(Command::Save(
            Box::new(snapshot.clone()),
            *new_secret,
            *mode,
        ))?;
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
    pub fn discard(&mut self) {
        if !self.saving && !self.cleaning {
            self.pending = None;
            if !self.repository_state.needs_attention() {
                self.error = None;
            }
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
                "仍有暫存檔尚未移除。請先重試移除暫存檔，或保留現有檔案，關閉程式後再處理。".into(),
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
            Err("上次保存留下的暫存檔尚未移除。請先重試移除暫存檔；移除時不會重新保存資料。".into())
        } else {
            Ok(())
        }
    }
    fn send(&self, command: Command) -> Result<(), String> {
        self.tx
            .send(command)
            .map_err(|_| "無法提交資料保存要求，請重新開啟程式。".to_owned())
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
                    self.current = snapshot;
                    self.error = None;
                    self.warning = None;
                    self.repository_state = state;
                    self.restore_backup = None;
                    events.push(Event::Loaded);
                }
                Response::Opened(Err(error)) => {
                    self.phase = Phase::Error;
                    self.error = Some(error);
                    // A failed reopen cannot resolve a previously blocked transaction.
                    // Only a successful open replaces that state with a fresh repository.
                    events.push(Event::Failed);
                }
                Response::Saved(Ok(result), new_secret, state, backup) => {
                    self.current = Some(result.snapshot);
                    self.warning = result.warning;
                    self.pending = None;
                    self.saving = false;
                    self.error = None;
                    self.phase = Phase::Ready;
                    self.repository_state = state;
                    self.restore_backup = backup;
                    events.push(Event::Committed { new_secret });
                }
                Response::Saved(Err(error), _, state, _) => {
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
        cleanup_warning: bool,
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
            behavior.cleanup_required |= behavior.cleanup_warning;
            Ok(CommitOutcome {
                snapshot,
                warning: behavior.cleanup_warning.then(|| "cleanup pending".into()),
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
        fn state(&self) -> RepositoryState {
            let behavior = self.behavior.lock().unwrap();
            RepositoryState {
                cleanup_required: behavior.cleanup_required,
                blocked: behavior.blocked,
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
    fn failed_commit_preserves_current_and_retry_uses_same_candidate() {
        let original = VaultSnapshot::new_empty(1_800_000_000_000);
        let behavior = Arc::new(Mutex::new(Behavior {
            fail_commit: true,
            ..Behavior::default()
        }));
        let mut app = start_memory(&original, &behavior);
        drain(&mut app);
        let mut candidate = original.clone();
        candidate.updated_at_ms += 1;
        app.commit(candidate.clone(), true).unwrap();
        assert_eq!(app.current, Some(original.clone()));
        assert_eq!(drain(&mut app), [Event::Failed]);
        assert!(app.has_pending());
        assert_eq!(app.current, Some(original));
        assert!(app.commit(candidate.clone(), false).is_err());
        behavior.lock().unwrap().fail_commit = false;
        app.retry().unwrap();
        assert_eq!(drain(&mut app), [Event::Committed { new_secret: true }]);
        candidate.revision += 1;
        assert_eq!(app.current, Some(candidate));
        assert!(!app.has_pending());
        assert_eq!(behavior.lock().unwrap().commits, 2);
    }
    #[test]
    fn open_failure_never_becomes_an_empty_writable_vault() {
        let mut app = Application::start(Box::new(|| Err("damaged file".into())));
        drain(&mut app);
        assert_eq!(app.phase, Phase::Error);
        assert!(app.current.is_none());
        assert!(
            app.commit(VaultSnapshot::new_empty(1_800_000_000_000), false)
                .is_err()
        );
        assert!(app.retry_cleanup().is_err());
    }
    #[test]
    fn cleanup_after_commit_does_not_save_or_increment_revision() {
        let original = VaultSnapshot::new_empty(1_800_000_000_000);
        let behavior = Arc::new(Mutex::new(Behavior {
            cleanup_warning: true,
            fail_cleanup: true,
            ..Behavior::default()
        }));
        let mut app = start_memory(&original, &behavior);
        drain(&mut app);
        app.commit(original, false).unwrap();
        drain(&mut app);
        let committed = app.current.clone();
        assert_eq!(committed.as_ref().unwrap().revision, 1);
        assert!(app.warning.is_some());
        assert!(!app.has_pending());
        app.retry_cleanup().unwrap();
        assert!(app.cleaning && !app.saving);
        assert!(app.retry_cleanup().is_err());
        assert!(app.reload().is_err());
        assert_eq!(drain(&mut app), [Event::Failed]);
        assert_eq!(app.current, committed);
        assert!(app.warning.is_some() && app.error.is_some());
        behavior.lock().unwrap().fail_cleanup = false;
        app.retry_cleanup().unwrap();
        assert_eq!(drain(&mut app), [Event::Cleaned]);
        assert_eq!(app.current, committed);
        assert!(app.warning.is_none() && app.error.is_none());
        assert!(!app.has_pending());
        let behavior = behavior.lock().unwrap();
        assert_eq!(behavior.commits, 1);
        assert_eq!(behavior.cleanups, 2);
    }
    #[test]
    fn cleanup_after_failed_save_keeps_candidate_until_explicit_save_retry() {
        let original = VaultSnapshot::new_empty(1_800_000_000_000);
        let behavior = Arc::new(Mutex::new(Behavior {
            fail_commit: true,
            leave_cleanup_on_failed_commit: true,
            ..Behavior::default()
        }));
        let mut app = start_memory(&original, &behavior);
        drain(&mut app);
        let mut candidate = original.clone();
        candidate.updated_at_ms += 1;
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
        assert_eq!(app.current, Some(original));
        assert!(app.has_pending());
        assert_eq!(app.pending.as_ref().unwrap().0, candidate);
        assert_eq!(behavior.lock().unwrap().commits, 1);
        assert_eq!(behavior.lock().unwrap().cleanups, 1);
        behavior.lock().unwrap().fail_commit = false;
        app.retry().unwrap();
        assert_eq!(drain(&mut app), [Event::Committed { new_secret: true }]);
        candidate.revision += 1;
        assert_eq!(app.current, Some(candidate));
        assert!(!app.has_pending());
        assert_eq!(behavior.lock().unwrap().commits, 2);
    }
    #[test]
    fn discard_keeps_failed_commit_cleanup_state_until_cleanup_succeeds() {
        let original = VaultSnapshot::new_empty(1_800_000_000_000);
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
        let error = app.error.clone();
        app.discard();
        assert!(!app.has_pending());
        assert_eq!(app.error, error);
        assert!(app.repository_state.needs_attention());
        assert!(
            app.warning.is_none(),
            "this was never a committed operation"
        );
        assert!(
            app.reload().is_err(),
            "reload must not abandon file ownership"
        );
        assert!(app.commit(original.clone(), false).is_err());
        assert_eq!(behavior.lock().unwrap().commits, 1);

        app.retry_cleanup().unwrap();
        assert_eq!(drain(&mut app), [Event::Failed]);
        assert!(app.repository_state.cleanup_required);
        app.discard();
        assert!(app.error.is_some());
        behavior.lock().unwrap().fail_cleanup = false;
        app.retry_cleanup().unwrap();
        assert_eq!(drain(&mut app), [Event::Cleaned]);
        assert!(!app.repository_state.needs_attention());
        assert!(app.error.is_none() && app.warning.is_none());
        assert_eq!(app.current, Some(original));
        assert_eq!(behavior.lock().unwrap().commits, 1);
        assert_eq!(behavior.lock().unwrap().cleanups, 2);
    }
    #[test]
    fn blocked_failure_survives_discard_and_failed_reload_until_successful_reload() {
        let original = VaultSnapshot::new_empty(1_800_000_000_000);
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
        assert!(app.error.is_some());
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
        assert_eq!(app.current.as_ref(), Some(&original));
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
        assert_eq!(app.current, Some(original));
        assert_eq!(behavior.lock().unwrap().commits, 1);
        assert_eq!(behavior.lock().unwrap().cleanups, 0);
    }
}

#[cfg(test)]
mod application_commit_tests;

#[cfg(test)]
mod application_restore_tests;
