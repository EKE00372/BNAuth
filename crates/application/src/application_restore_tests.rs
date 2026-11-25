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
    fn restore_from_preview(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
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
fn restore_retry_preserves_operation_and_only_acknowledges_the_candidate_after_success() {
    let original = VaultSnapshot::new_empty();
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
    candidate.groups.push(bnauth_core::Group {
        id: bnauth_core::Uuid::new_v4(),
        name: "Test group".into(),
        sort_order: 1,
        is_ungrouped: false,
    });
    app.restore_from_preview(|| candidate.clone()).unwrap();
    assert_eq!(app.current.as_deref(), Some(&original));
    assert_eq!(settle(&mut app), [Event::Failed]);
    assert_eq!(app.current.as_deref(), Some(&original));
    assert!(app.has_pending());
    app.retry().unwrap();
    assert_eq!(
        settle(&mut app),
        [Event::Committed {
            calibrate_if_needed: false
        }]
    );
    assert!(app.warning.is_none() && !app.has_pending());
    candidate.revision += 1;
    assert_eq!(app.current.as_deref(), Some(&candidate));
    assert!(app.retry().is_err());
    let calls = calls.lock().unwrap();
    assert_eq!(calls.commits, 0);
    candidate.revision -= 1;
    assert_eq!(calls.restores, [candidate.clone(), candidate]);
}
