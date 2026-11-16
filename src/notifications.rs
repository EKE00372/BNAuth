use std::collections::VecDeque;
use std::time::{Duration, Instant};

const STATUS_DURATION: Duration = Duration::from_secs(3);
const WARNING_DURATION: Duration = Duration::from_secs(5);
const MAX_WARNINGS: usize = 32;

struct Message {
    text: String,
    remaining: Duration,
    visible_since: Option<Instant>,
}

impl Message {
    fn new(text: String, duration: Duration) -> Self {
        Self {
            text,
            remaining: duration,
            visible_since: None,
        }
    }

    fn pause(&mut self, now: Instant) {
        if let Some(since) = self.visible_since.take() {
            self.remaining = self
                .remaining
                .saturating_sub(now.saturating_duration_since(since));
        }
    }
}

/// Status updates may replace each other; warnings retain their own display time.
/// Call `visible` only when painting, and `pause` when hiding the notification UI.
#[derive(Default)]
pub struct Notifications {
    status: Option<Message>,
    warnings: VecDeque<Message>,
}

impl Notifications {
    pub fn status(&mut self, text: impl Into<String>) {
        // A save finishing behind a warning must not appear later as a stale toast.
        if self.warnings.is_empty() {
            self.status = Some(Message::new(text.into(), STATUS_DURATION));
        }
    }

    pub fn warning(&mut self, text: impl Into<String>) {
        self.clear_status();
        let text = text.into();
        if self.warnings.iter().any(|message| message.text == text) {
            return;
        }
        if self.warnings.len() == MAX_WARNINGS {
            // Keep the current warning and evict the oldest waiting one.
            self.warnings.remove(1);
        }
        self.warnings
            .push_back(Message::new(text, WARNING_DURATION));
    }

    pub fn clear_status(&mut self) {
        self.status = None;
    }

    /// Return the text to paint, starting its timer only on this call. Advancing
    /// to the next warning gives it a full display interval of its own.
    pub fn visible(&mut self, now: Instant) -> Option<&str> {
        self.pause(now);
        let message = self.warnings.front_mut().or(self.status.as_mut())?;
        message.visible_since = Some(now);
        Some(&message.text)
    }

    /// Freeze the remaining display time before a hidden/minimized UI stops
    /// painting. Repeated calls while already paused do not consume time.
    pub fn pause(&mut self, now: Instant) {
        if let Some(message) = self.warnings.front_mut() {
            message.pause(now);
            if message.remaining.is_zero() {
                self.warnings.pop_front();
            }
        } else if let Some(message) = &mut self.status {
            message.pause(now);
            if message.remaining.is_zero() {
                self.status = None;
            }
        }
    }

    #[cfg(test)]
    pub fn pending_warning_count(&self) -> usize {
        self.warnings.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn status_starts_when_painted_and_replacement_receives_its_own_interval() {
        let now = Instant::now();
        let mut notifications = Notifications::default();
        notifications.status("Saving");
        let first_paint = now + Duration::from_secs(60);
        assert_eq!(notifications.visible(first_paint), Some("Saving"));
        assert_eq!(
            notifications.visible(first_paint + Duration::from_secs(2)),
            Some("Saving")
        );
        notifications.status("Saved");
        let next_paint = first_paint + Duration::from_secs(20);
        assert_eq!(notifications.visible(next_paint), Some("Saved"));
        assert_eq!(
            notifications.visible(next_paint + Duration::from_millis(2999)),
            Some("Saved")
        );
        assert_eq!(
            notifications.visible(next_paint + Duration::from_secs(3)),
            None
        );
    }

    #[test]
    fn warning_preempts_status_and_cannot_be_replaced_or_cleared_by_saving() {
        let now = Instant::now();
        let mut notifications = Notifications::default();
        notifications.status("Saving");
        assert_eq!(notifications.visible(now), Some("Saving"));
        notifications.warning("Time changed; using local time");
        assert_eq!(
            notifications.visible(now),
            Some("Time changed; using local time")
        );
        notifications.status("Saved");
        notifications.clear_status();
        assert_eq!(
            notifications.visible(now + Duration::from_millis(4999)),
            Some("Time changed; using local time")
        );
        assert_eq!(notifications.visible(now + Duration::from_secs(5)), None);
        assert_eq!(notifications.pending_warning_count(), 0);
    }

    #[test]
    fn queued_warnings_each_receive_five_seconds_from_their_first_paint() {
        let now = Instant::now();
        let mut notifications = Notifications::default();
        notifications.warning("First");
        notifications.warning("Second");
        notifications.pause(now + Duration::from_secs(60));
        let first_paint = now + Duration::from_secs(120);
        assert_eq!(notifications.visible(first_paint), Some("First"));
        let second_paint = first_paint + Duration::from_secs(20);
        assert_eq!(notifications.visible(second_paint), Some("Second"));
        assert_eq!(
            notifications.visible(second_paint + Duration::from_millis(4999)),
            Some("Second")
        );
        assert_eq!(
            notifications.visible(second_paint + Duration::from_secs(5)),
            None
        );
    }

    #[test]
    fn hiding_pauses_active_warning_without_starting_the_next_one() {
        let now = Instant::now();
        let mut notifications = Notifications::default();
        notifications.warning("First");
        notifications.warning("Second");
        assert_eq!(notifications.visible(now), Some("First"));
        notifications.pause(now + Duration::from_secs(2));
        notifications.pause(now + Duration::from_secs(60));
        let resumed = now + Duration::from_secs(120);
        assert_eq!(notifications.visible(resumed), Some("First"));
        assert_eq!(
            notifications.visible(resumed + Duration::from_millis(2999)),
            Some("First")
        );
        assert_eq!(
            notifications.visible(resumed + Duration::from_secs(3)),
            Some("Second")
        );
        assert_eq!(
            notifications.visible(resumed + Duration::from_secs(8)),
            None
        );
    }

    #[test]
    fn hiding_also_pauses_status_and_clearing_only_removes_status() {
        let now = Instant::now();
        let mut notifications = Notifications::default();
        notifications.status("Saved");
        assert_eq!(notifications.visible(now), Some("Saved"));
        notifications.pause(now + Duration::from_secs(1));
        let resumed = now + Duration::from_secs(60);
        assert_eq!(notifications.visible(resumed), Some("Saved"));
        assert_eq!(
            notifications.visible(resumed + Duration::from_millis(1999)),
            Some("Saved")
        );
        notifications.clear_status();
        assert_eq!(
            notifications.visible(resumed + Duration::from_secs(2)),
            None
        );
    }

    #[test]
    fn duplicate_warnings_do_not_extend_time_and_can_recur_after_completion() {
        let now = Instant::now();
        let mut notifications = Notifications::default();
        notifications.warning("First");
        notifications.warning("Second");
        notifications.warning("First");
        notifications.warning("Second");
        assert_eq!(notifications.pending_warning_count(), 2);
        assert_eq!(notifications.visible(now), Some("First"));
        assert_eq!(
            notifications.visible(now + Duration::from_secs(4)),
            Some("First")
        );
        notifications.warning("First");
        assert_eq!(
            notifications.visible(now + Duration::from_secs(5)),
            Some("Second")
        );
        notifications.warning("First");
        assert_eq!(notifications.pending_warning_count(), 2);
        assert_eq!(
            notifications.visible(now + Duration::from_secs(10)),
            Some("First")
        );
        assert_eq!(notifications.visible(now + Duration::from_secs(15)), None);
    }

    #[test]
    fn a_full_queue_keeps_active_warning_and_drops_only_the_oldest_waiting_one() {
        let now = Instant::now();
        let mut notifications = Notifications::default();
        notifications.warning("Active");
        assert_eq!(notifications.visible(now), Some("Active"));
        for index in 0..MAX_WARNINGS {
            notifications.warning(format!("Waiting {index}"));
        }
        assert_eq!(notifications.pending_warning_count(), MAX_WARNINGS);
        assert_eq!(notifications.visible(now), Some("Active"));
        for index in 1..MAX_WARNINGS {
            let expected = format!("Waiting {index}");
            assert_eq!(
                notifications.visible(now + WARNING_DURATION * index as u32),
                Some(expected.as_str())
            );
        }
        assert_eq!(
            notifications.visible(now + WARNING_DURATION * MAX_WARNINGS as u32),
            None
        );
    }
}
