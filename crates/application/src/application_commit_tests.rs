use super::*;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

struct ScriptedRepo {
    attempts: Arc<Mutex<Vec<VaultSnapshot>>>,
}
impl Repository for ScriptedRepo {
    fn retry_cleanup(&mut self) -> Result<(), String> {
        Ok(())
    }
    fn commit(&mut self, candidate: &VaultSnapshot) -> Result<CommitOutcome, String> {
        let mut attempts = self.attempts.lock().unwrap();
        attempts.push(candidate.clone());
        if attempts.len() == 1 {
            return Err("injected precommit failure".into());
        }
        let mut snapshot = candidate.clone();
        snapshot.revision += 1;
        Ok(CommitOutcome {
            snapshot,
            warning: Some("committed; cleanup pending".into()),
        })
    }
}
fn settle(app: &mut Application) -> Vec<Event> {
    let until = Instant::now() + Duration::from_secs(5);
    let mut events = Vec::new();
    loop {
        events.extend(app.poll());
        if app.phase != Phase::Loading && !app.saving {
            return events;
        }
        assert!(Instant::now() < until, "worker must finish");
        std::thread::yield_now();
    }
}

#[test]
fn retry_preserves_secret_candidate_and_committed_warning_is_not_another_retry() {
    let original = VaultSnapshot::new_empty(1_700_000_000_000);
    let attempts = Arc::new(Mutex::new(Vec::new()));
    let seen = attempts.clone();
    let opened = original.clone();
    let mut app = Application::start(Box::new(move || {
        Ok((
            Box::new(ScriptedRepo {
                attempts: seen.clone(),
            }),
            Some(opened.clone()),
        ))
    }));
    settle(&mut app);
    let material =
        bnauth_core::parse_manual(&"12".repeat(20), "US123456789012", "AbCd123456").unwrap();
    let preview = bnauth_core::prepare_import(
        &original,
        "人工驗證器",
        "保留",
        original.groups[0].id,
        material,
        1_700_000_000_010,
    )
    .unwrap();
    let candidate = preview.candidate;
    app.commit(candidate.clone(), true).unwrap();
    assert!(
        settle(&mut app)
            .iter()
            .any(|event| matches!(event, Event::Failed))
    );
    assert_eq!(app.current, Some(original));
    assert!(app.has_pending());
    app.retry().unwrap();
    let events = settle(&mut app);
    assert_eq!(
        events
            .iter()
            .filter(|event| matches!(event, Event::Committed { new_secret: true }))
            .count(),
        1
    );
    let mut expected = candidate.clone();
    expected.revision += 1;
    assert_eq!(app.current, Some(expected));
    assert_eq!(app.warning.as_deref(), Some("committed; cleanup pending"));
    assert!(!app.has_pending());
    assert!(app.retry().is_err());
    assert_eq!(
        *attempts.lock().unwrap(),
        vec![candidate.clone(), candidate]
    );
}
