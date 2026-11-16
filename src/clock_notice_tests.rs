use super::{ClockStatus, ClockStatusNotices};
use crate::notifications::Notifications;
use std::time::{Duration, Instant};

const REJECTED_SAMPLE: &str = "校時回覆到達後時間不連續，未套用；請手動校時。";
const CLOCK_JUMP: &str = "時間不連續，已改用本機時間；請手動校時。";

fn continuity_rejections() -> [String; 2] {
    let producer = bnauth_adapters::time_sync::SyncFailure::from(
        bnauth_core::clock::SampleValidationError::WallClockDiscontinuity,
    );
    assert!(producer.is_clock_discontinuity());
    [
        REJECTED_SAMPLE.to_owned(),
        format!("校時失敗：{}", producer.into_message()),
    ]
}

#[test]
fn a_rejected_sync_sample_reports_one_full_warning_without_replaying_the_same_jump() {
    for rejection in continuity_rejections() {
        for previous in [
            ClockStatus::Local,
            ClockStatus::Synced,
            ClockStatus::SyncedAfterFailure,
        ] {
            let mut notices = ClockStatusNotices::default();
            let mut notifications = Notifications::default();
            assert!(notices.observe(previous, None).is_none());
            notices.begin_sync();

            // Applying the queued sample detected a wall-clock change. Its detailed
            // rejection already explains this transition, but the observer must
            // remember it so the next normal frame cannot replay a generic warning.
            if let Some(message) =
                notices.observe(ClockStatus::Discontinuous, Some(rejection.clone()))
            {
                notifications.warning(message);
            }
            let first_paint = Instant::now();
            assert_eq!(notifications.pending_warning_count(), 1);
            assert_eq!(notifications.visible(first_paint), Some(rejection.as_str()));
            for frame in 1..=20 {
                if let Some(message) = notices.observe(ClockStatus::Discontinuous, None) {
                    notifications.warning(message);
                }
                assert_eq!(notifications.pending_warning_count(), 1);
                assert_eq!(
                    notifications.visible(first_paint + Duration::from_millis(frame * 100)),
                    Some(rejection.as_str()),
                );
            }
            assert_eq!(
                notifications.visible(first_paint + Duration::from_millis(4_999)),
                Some(rejection.as_str()),
            );
            assert!(
                notifications
                    .visible(first_paint + Duration::from_secs(5))
                    .is_none()
            );
            assert_eq!(notifications.pending_warning_count(), 0);
            for _ in 0..3 {
                assert!(notices.observe(ClockStatus::Discontinuous, None).is_none());
            }

            // A successful correction rearms a genuinely later wall-clock change.
            notices.begin_sync();
            assert!(notices.observe(ClockStatus::Synced, None).is_none());
            let message = notices
                .observe(ClockStatus::Discontinuous, None)
                .expect("a new jump after successful sync must still be reported");
            assert_eq!(message, CLOCK_JUMP);
            notifications.warning(message);
            let next_paint = first_paint + Duration::from_secs(10);
            assert_eq!(notifications.visible(next_paint), Some(CLOCK_JUMP));
            assert!(
                notifications
                    .visible(next_paint + Duration::from_secs(5))
                    .is_none()
            );
        }
    }
}

#[test]
fn independent_save_and_network_errors_do_not_hide_a_standalone_clock_jump() {
    const SAVE_WARNING: &str = "資料已保存，部分暫存檔尚未移除。";
    const NETWORK_FAILURE: &str = "校時失敗：校時連線逾時；請手動重試。";
    let mut notices = ClockStatusNotices::default();
    let mut notifications = Notifications::default();
    assert!(notices.observe(ClockStatus::Synced, None).is_none());
    notices.begin_sync();

    // These events can arrive in the same poll: a completed save, a failed
    // network refresh, and an independently detected wall-clock correction.
    notifications.warning(SAVE_WARNING);
    notifications.warning(NETWORK_FAILURE);
    let message = notices
        .observe(ClockStatus::Discontinuous, None)
        .expect("an unrelated error must not suppress a real clock jump");
    notifications.warning(message);
    assert_eq!(notifications.pending_warning_count(), 3);

    let first_paint = Instant::now();
    for (index, expected) in [SAVE_WARNING, NETWORK_FAILURE, CLOCK_JUMP]
        .into_iter()
        .enumerate()
    {
        assert_eq!(
            notifications.visible(first_paint + Duration::from_secs(index as u64 * 5)),
            Some(expected),
        );
    }
    assert!(
        notifications
            .visible(first_paint + Duration::from_secs(15))
            .is_none()
    );
    assert!(notices.observe(ClockStatus::Discontinuous, None).is_none());
}

#[test]
fn a_jump_reported_while_sync_is_pending_does_not_repeat_when_its_old_sample_arrives() {
    for rejection in continuity_rejections() {
        let mut notices = ClockStatusNotices::default();
        let mut notifications = Notifications::default();
        assert!(notices.observe(ClockStatus::Synced, None).is_none());
        notices.begin_sync();

        // A normal UI poll can detect the system-clock adjustment before the sync
        // worker finishes its round and sends the already-received older sample.
        let message = notices
            .observe(ClockStatus::Discontinuous, None)
            .expect("the independently detected jump must be reported immediately");
        notifications.warning(message);
        let first_paint = Instant::now();
        assert_eq!(notifications.visible(first_paint), Some(CLOCK_JUMP));

        for frame in 1..=30 {
            assert!(notices.observe(ClockStatus::Discontinuous, None).is_none());
            assert_eq!(
                notifications.visible(first_paint + Duration::from_millis(frame * 100)),
                Some(CLOCK_JUMP),
            );
        }
        assert_eq!(
            notifications.visible(first_paint + Duration::from_millis(4_999)),
            Some(CLOCK_JUMP),
        );
        assert!(
            notifications
                .visible(first_paint + Duration::from_secs(5))
                .is_none()
        );
        assert_eq!(notifications.pending_warning_count(), 0);
        // The worker may finish after the first warning has already expired. It is
        // still the result of the same in-flight round and must not add a replay.
        assert!(
            notices
                .observe(ClockStatus::Discontinuous, Some(rejection.clone()))
                .is_none()
        );

        // A newly established valid clock separates this episode from a later one.
        notices.begin_sync();
        assert!(notices.observe(ClockStatus::Synced, None).is_none());
        notices.begin_sync();
        assert_eq!(
            notices.observe(ClockStatus::Discontinuous, Some(rejection.clone())),
            Some(rejection.clone()),
        );
    }
}

#[test]
fn new_failed_sync_rounds_report_their_own_errors_even_before_any_success() {
    for rejection in continuity_rejections() {
        let mut notices = ClockStatusNotices::default();
        let mut notifications = Notifications::default();
        assert!(notices.observe(ClockStatus::Local, None).is_none());
        let first_paint = Instant::now();

        for attempt in 0..2 {
            // The user retries manually after the previous failed round and toast.
            // There has been no successful sync to reset Discontinuous in between.
            notices.begin_sync();
            let warning = notices
                .observe(ClockStatus::Discontinuous, Some(rejection.clone()))
                .expect("a new sync round must still report its own rejected sample");
            assert_eq!(warning, rejection.as_str());
            notifications.warning(warning);
            let shown_at = first_paint + Duration::from_secs(attempt * 10);
            assert_eq!(notifications.pending_warning_count(), 1);
            assert_eq!(notifications.visible(shown_at), Some(rejection.as_str()));
            assert_eq!(
                notifications.visible(shown_at + Duration::from_millis(4_999)),
                Some(rejection.as_str()),
            );
            assert!(
                notifications
                    .visible(shown_at + Duration::from_secs(5))
                    .is_none()
            );
            assert_eq!(notifications.pending_warning_count(), 0);
            assert!(notices.observe(ClockStatus::Discontinuous, None).is_none());
        }
    }
}
