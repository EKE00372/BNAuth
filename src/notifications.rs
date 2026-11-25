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

/// Transient updates may replace each other. Warnings and retained status keep
/// their display time even when another update arrives.
/// Call `visible_for` only when painting, and `pause` when hiding the notification UI.
#[derive(Default)]
pub struct Notifications {
    status: Option<Message>,
    retained_status: Option<Message>,
    warnings: VecDeque<Message>,
}

impl Notifications {
    pub fn status(&mut self, text: impl Into<String>) {
        // A save finishing behind a warning must not appear later as a stale toast.
        if self.warnings.is_empty() {
            self.status = Some(Message::new(text.into(), STATUS_DURATION));
        }
    }

    pub fn retained_status(&mut self, text: impl Into<String>) {
        self.clear_status();
        self.retained_status = Some(Message::new(text.into(), STATUS_DURATION));
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

    pub fn clear_status_if(&mut self, text: &str) {
        self.status.take_if(|message| message.text == text);
    }

    /// Return the text and the delay until it needs to be removed or replaced.
    pub fn visible_for(&mut self, now: Instant) -> Option<(&str, Duration)> {
        self.pause(now);
        let message = self
            .warnings
            .front_mut()
            .or(self.retained_status.as_mut())
            .or(self.status.as_mut())?;
        message.visible_since = Some(now);
        Some((&message.text, message.remaining))
    }

    #[cfg(test)]
    pub fn visible(&mut self, now: Instant) -> Option<&str> {
        self.visible_for(now).map(|(text, _)| text)
    }

    /// Freeze the remaining display time before a hidden/minimized UI stops
    /// painting. Repeated calls while already paused do not consume time.
    pub fn pause(&mut self, now: Instant) {
        if let Some(message) = self.warnings.front_mut() {
            message.pause(now);
            if message.remaining.is_zero() {
                self.warnings.pop_front();
            }
        }
        for status in [&mut self.retained_status, &mut self.status] {
            if let Some(message) = status {
                message.pause(now);
                if message.remaining.is_zero() {
                    *status = None;
                }
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

}
