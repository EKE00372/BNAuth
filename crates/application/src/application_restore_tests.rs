use super::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

#[derive(Default)]
struct Calls {
    commits: usize,
    restores: Vec<VaultSnapshot>,
}
struct RestoreRepo(Arc<Mutex<Calls>>);
impl Repository for RestoreRepo {
    fn commit(&mut self, _: &VaultSnapshot) -> Result<CommitOutcome, String> {
        self.0.lock().unwrap().commits += 1;
        Err("restore intent was lost".into())
    }
    fn restore(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        let mut calls = self.0.lock().unwrap();
        calls.restores.push(candidate.clone());
        if calls.restores.len() == 1 {
            return Err("injected restore failure".into());
        }
        let mut snapshot = candidate.clone();
        snapshot.revision += 1;
        Ok(CommitOutcome {
            snapshot,
            warning: None,
        })
    }
    fn last_restore_backup(&self) -> Option<PathBuf> {
        Some(PathBuf::from("BNAuth-before-restore-test.bnauth"))
    }
    fn retry_cleanup(&mut self) -> Result<(), String> {
        Ok(())
    }
}
fn settle(app: &mut Application) -> Vec<Event> {
    let deadline = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    loop {
        events.extend(app.poll());
        if app.phase != Phase::Loading && !app.saving {
            return events;
        }
        assert!(Instant::now() < deadline);
        std::thread::yield_now();
    }
}
#[test]
fn restore_retry_preserves_operation_and_only_acknowledges_the_candidate_and_copy_after_success() {
    let original = VaultSnapshot::new_empty(1_800_000_000_000);
    let calls = Arc::new(Mutex::new(Calls::default()));
    let worker_calls = calls.clone();
    let opened = original.clone();
    let mut app = Application::start(Box::new(move || {
        Ok((
            Box::new(RestoreRepo(worker_calls.clone())),
            Some(opened.clone()),
        ))
    }));
    settle(&mut app);
    let mut candidate = original.clone();
    candidate.updated_at_ms += 1;
    app.restore(candidate.clone(), true).unwrap();
    assert_eq!(app.current.as_ref(), Some(&original));
    assert_eq!(settle(&mut app), [Event::Failed]);
    assert_eq!(app.current.as_ref(), Some(&original));
    assert!(app.restore_backup.is_none() && app.has_pending());
    app.retry().unwrap();
    assert_eq!(settle(&mut app), [Event::Committed { new_secret: true }]);
    assert_eq!(
        app.restore_backup,
        Some(PathBuf::from("BNAuth-before-restore-test.bnauth"))
    );
    assert!(app.warning.is_none() && !app.has_pending());
    candidate.revision += 1;
    assert_eq!(app.current, Some(candidate.clone()));
    assert!(app.retry().is_err());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.commits, 0);
    candidate.revision -= 1;
    assert_eq!(calls.restores, [candidate.clone(), candidate]);
}

#[test]
fn repositories_without_explicit_restore_support_refuse_to_replace_data() {
    struct PlainRepo;
    impl Repository for PlainRepo {
        fn commit(&mut self, _: &VaultSnapshot) -> Result<CommitOutcome, String> {
            panic!("must not fall back to commit")
        }
        fn retry_cleanup(&mut self) -> Result<(), String> {
            Ok(())
        }
    }
    let mut repo = PlainRepo;
    assert!(
        repo.restore(&VaultSnapshot::new_empty(1))
            .err()
            .unwrap()
            .contains("不支援")
    );
}
