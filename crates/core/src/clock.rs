//! A shared Battle.net clock whose saved offset can be restored across launches.
//!
//! All OTPs and the progress bar should use one `now_ms` value per UI frame.

use crate::{ErrorCode, TimeSyncRecord};
pub use crate::{TIME_SOURCE, now_ms as system_now_ms};
use std::time::{Duration, Instant};

// Applies when rebasing a saved offset onto the current local clock.
const SAVED_CLOCK_RANGE_MS: std::ops::Range<i64> = 946_684_800_000..4_133_980_800_000;
pub(super) const MAX_RTT_MS: u64 = 4_000;
const CONTINUITY_TOLERANCE_MS: i128 = 1_000;
const CONFIRM_OFFSET_MS: i128 = 5_000;
const SAMPLE_AGREEMENT_MS: i128 = 1_000;

/// Preserve the reason for rejection so callers can coalesce only clock-change warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleValidationError {
    MonotonicDiscontinuity,
    ExcessiveRtt,
    TimestampOverflow,
    WallClockDiscontinuity,
}

impl std::fmt::Display for SampleValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let error = match self {
            Self::MonotonicDiscontinuity | Self::WallClockDiscontinuity => ErrorCode::time_deviation(),
            Self::ExcessiveRtt | Self::TimestampOverflow => ErrorCode::retry_connection(),
        };
        std::fmt::Display::fmt(&error, formatter)
    }
}

impl std::error::Error for SampleValidationError {}

/// A current-process anchor for a fresh measurement or a reloaded saved offset.
/// The `Instant` is reconstructed on reload and is not serializable.
#[derive(Clone, Debug)]
pub struct SyncSample {
    pub record: TimeSyncRecord,
    received_at: Instant,
    server_at_receive_ms: i64,
    local_at_receive_ms: i64,
}

/// A currently valid process-local reference, copied before beginning a network round.
#[derive(Clone, Copy, Debug)]
pub struct TimeReference {
    instant: Instant,
    utc_ms: i64,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ClockStatus {
    Local,
    Synced,
    Discontinuous,
}

/// One wall-clock/monotonic reading shared by continuity checks and rebasing.
/// Keeping this value avoids hiding a wall-clock step between those operations.
#[derive(Clone, Copy, Debug)]
pub struct ClockObservation {
    wall_ms: i64,
    instant: Instant,
}

impl ClockObservation {
    pub fn now() -> Self {
        Self::at(system_now_ms(), Instant::now())
    }

    /// Supply one reading from a caller-owned clock source, including simulations.
    pub fn at(wall_ms: i64, instant: Instant) -> Self {
        Self { wall_ms, instant }
    }
}

#[derive(Clone, Debug)]
pub struct RuntimeClock {
    anchor: Option<SyncSample>,
    last_tick: ClockObservation,
    last_error: Option<String>,
    discontinuous: bool,
    jump_notice_pending: bool,
}

impl Default for RuntimeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeClock {
    /// Start on local UTC when no saved calibration has been loaded.
    pub fn new() -> Self {
        Self::at(system_now_ms(), Instant::now())
    }

    fn at(wall_ms: i64, instant: Instant) -> Self {
        Self {
            anchor: None,
            last_tick: ClockObservation { wall_ms, instant },
            last_error: None,
            discontinuous: false,
            jump_notice_pending: false,
        }
    }

    /// Apply the shared saved offset to today's local wall clock. The original
    /// sample timestamp is historical metadata, never a frozen current time.
    pub fn from_saved(record: Option<&TimeSyncRecord>) -> Result<Self, String> {
        Self::from_saved_observation(record, ClockObservation::now())
    }

    /// Rebase at the same reading that was used to inspect the previous clock.
    pub fn from_saved_observation(
        record: Option<&TimeSyncRecord>,
        observation: ClockObservation,
    ) -> Result<Self, String> {
        Self::from_saved_at(record, observation.wall_ms, observation.instant)
    }

    fn from_saved_at(
        record: Option<&TimeSyncRecord>,
        wall_ms: i64,
        instant: Instant,
    ) -> Result<Self, String> {
        let mut clock = Self::at(wall_ms, instant);
        if let Some(record) = record {
            record
                .validate()
                .map_err(|_| ErrorCode::saved_offset_failure().to_string())?;
            let corrected_ms = saved_time_ms(record, wall_ms)
                .ok_or_else(|| ErrorCode::saved_offset_failure().to_string())?;
            clock.anchor = Some(SyncSample {
                record: record.clone(),
                received_at: instant,
                server_at_receive_ms: corrected_ms,
                local_at_receive_ms: wall_ms,
            });
        }
        Ok(clock)
    }

    pub fn now_ms(&mut self) -> i64 {
        self.observe(ClockObservation::now())
    }

    pub fn observe(&mut self, observation: ClockObservation) -> i64 {
        self.now_at(observation.wall_ms, observation.instant)
    }

    fn now_at(&mut self, wall_ms: i64, instant: Instant) -> i64 {
        let tick_continuous = continuous(
            self.last_tick.wall_ms,
            self.last_tick.instant,
            wall_ms,
            instant,
        );
        self.last_tick = ClockObservation { wall_ms, instant };
        let Some(anchor) = &mut self.anchor else {
            if !tick_continuous {
                self.discontinuous = true;
                self.jump_notice_pending = true;
            }
            return wall_ms;
        };
        // Also compare against the complete anchored interval, so small repeated
        // wall-clock steps cannot accumulate unnoticed between individual frames.
        if tick_continuous
            && continuous(
                anchor.local_at_receive_ms,
                anchor.received_at,
                wall_ms,
                instant,
            )
            && let Some(value) = advance(anchor.server_at_receive_ms, anchor.received_at, instant)
        {
            return value;
        }
        // Apply the same offset to the new Windows time. Only this process-local
        // reference moves; the measured calibration is retained as recorded.
        self.discontinuous = true;
        self.jump_notice_pending = true;
        anchor.received_at = instant;
        anchor.local_at_receive_ms = wall_ms;
        anchor.server_at_receive_ms = wall_ms.saturating_add(anchor.record.offset_ms);
        anchor.server_at_receive_ms
    }

    /// Consume the notification for a newly observed clock jump. The tooltip
    /// continues to recommend resynchronizing until a fresh sample is applied.
    pub fn take_clock_jump(&mut self) -> bool {
        std::mem::take(&mut self.jump_notice_pending)
    }

    /// Replace the current shared calibration with a validated fresh sample.
    pub fn apply(&mut self, sample: SyncSample) -> Result<(), String> {
        self.apply_at(sample, system_now_ms(), Instant::now())
    }

    fn apply_at(
        &mut self,
        sample: SyncSample,
        wall_ms: i64,
        instant: Instant,
    ) -> Result<(), String> {
        self.now_at(wall_ms, instant);
        if !continuous(
            sample.local_at_receive_ms,
            sample.received_at,
            wall_ms,
            instant,
        ) || advance(sample.server_at_receive_ms, sample.received_at, instant).is_none()
        {
            let error = ErrorCode::time_deviation().to_string();
            self.last_error = Some(error.clone());
            return Err(error);
        }
        self.anchor = Some(sample);
        self.last_error = None;
        self.discontinuous = false;
        self.jump_notice_pending = false;
        Ok(())
    }

    /// A failed refresh retains the active calibration.
    pub fn note_failure(&mut self, message: String) {
        self.now_ms();
        self.last_error = Some(message);
    }

    pub fn reference(&mut self) -> Option<TimeReference> {
        let utc_ms = self.now_ms();
        self.anchor.as_ref().map(|_| TimeReference {
            instant: self.last_tick.instant,
            utc_ms,
        })
    }

    pub fn is_synced(&self) -> bool {
        self.anchor.is_some()
    }

    /// Return the measured offset used by the shared clock and exports.
    pub fn offset_ms(&self) -> Option<i64> {
        self.last_success().map(|record| record.offset_ms)
    }

    pub fn status(&self) -> ClockStatus {
        if self.discontinuous {
            ClockStatus::Discontinuous
        } else if self.is_synced() {
            ClockStatus::Synced
        } else {
            ClockStatus::Local
        }
    }

    pub fn status_text(&self) -> String {
        if self.discontinuous {
            let deviation = ErrorCode::time_deviation().to_string();
            return match self.last_error() {
                Some(error) if error != deviation => {
                    format!("{deviation}\n{error}")
                }
                _ => deviation,
            };
        }
        if let Some(error) = self.last_error() {
            return error.to_owned();
        }
        match &self.anchor {
            Some(sample) => format!(
                "上次校時：{} · RTT {} ms",
                format_utc(sample.record.server_time_ms),
                sample.record.rtt_ms,
            ),
            None => "尚未校時".to_owned(),
        }
    }

    pub fn last_success(&self) -> Option<&TimeSyncRecord> {
        self.anchor.as_ref().map(|sample| &sample.record)
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
}

/// Correct the current local time using a calibration read from disk.
pub fn saved_time_ms(record: &TimeSyncRecord, wall_ms: i64) -> Option<i64> {
    wall_ms
        .checked_add(record.offset_ms)
        .filter(|value| SAVED_CLOCK_RANGE_MS.contains(value))
}

/// Validate one measured request before considering it for a synchronization round.
pub fn validate_sample(
    server_time_ms: i64,
    sent_wall_ms: i64,
    received_wall_ms: i64,
    sent_at: Instant,
    received_at: Instant,
) -> Result<SyncSample, SampleValidationError> {
    let rtt = received_at
        .checked_duration_since(sent_at)
        .ok_or(SampleValidationError::MonotonicDiscontinuity)?;
    if rtt > Duration::from_millis(MAX_RTT_MS) {
        return Err(SampleValidationError::ExcessiveRtt);
    }
    if !continuous(sent_wall_ms, sent_at, received_wall_ms, received_at) {
        return Err(SampleValidationError::WallClockDiscontinuity);
    }
    let rtt_ms = rtt.as_millis() as u64;
    let half_rtt = (rtt_ms / 2) as i64;
    let midpoint_wall_ms = sent_wall_ms
        .checked_add(half_rtt)
        .ok_or(SampleValidationError::TimestampOverflow)?;
    let server_at_receive_ms = server_time_ms
        .checked_add(half_rtt)
        .ok_or(SampleValidationError::TimestampOverflow)?;
    let offset_ms = server_time_ms
        .checked_sub(midpoint_wall_ms)
        .ok_or(SampleValidationError::TimestampOverflow)?;
    Ok(SyncSample {
        record: TimeSyncRecord {
            source: TIME_SOURCE.to_owned(),
            server_time_ms,
            local_wall_ms_at_sample: midpoint_wall_ms,
            offset_ms,
            rtt_ms,
        },
        received_at,
        server_at_receive_ms,
        local_at_receive_ms: received_wall_ms,
    })
}

/// Pick the smallest RTT among independently qualified samples. A correction over
/// five seconds needs a second sample that agrees within one second at one Instant.
pub fn select_sample(
    samples: &[SyncSample],
    reference: Option<TimeReference>,
) -> Result<SyncSample, String> {
    let mut best: Option<&SyncSample> = None;
    for (index, sample) in samples.iter().enumerate() {
        let baseline = reference
            .and_then(|r| advance(r.utc_ms, r.instant, sample.received_at))
            .unwrap_or(sample.local_at_receive_ms);
        let small_correction =
            (sample.server_at_receive_ms as i128 - baseline as i128).abs() <= CONFIRM_OFFSET_MS;
        let confirmed = small_correction
            || samples.iter().enumerate().any(|(other_index, other)| {
                if index == other_index {
                    return false;
                }
                project(
                    sample.server_at_receive_ms,
                    sample.received_at,
                    other.received_at,
                )
                .is_some_and(|at_other| {
                    (at_other as i128 - other.server_at_receive_ms as i128).abs()
                        <= SAMPLE_AGREEMENT_MS
                })
            });
        if confirmed && best.is_none_or(|previous| sample.record.rtt_ms < previous.record.rtt_ms) {
            best = Some(sample);
        }
    }
    best.cloned().ok_or_else(|| {
        let error = if samples.is_empty() {
            ErrorCode::retry_connection()
        } else {
            ErrorCode::time_deviation()
        };
        error.to_string()
    })
}

fn continuous(old_wall: i64, old_instant: Instant, wall: i64, instant: Instant) -> bool {
    instant
        .checked_duration_since(old_instant)
        .is_some_and(|elapsed| {
            // Duration's u64 seconds plus fractional milliseconds fit in i128.
            let elapsed_ms = elapsed.as_millis() as i128;
            ((wall as i128 - old_wall as i128) - elapsed_ms).abs() <= CONTINUITY_TOLERANCE_MS
        })
}

fn advance(value: i64, from: Instant, to: Instant) -> Option<i64> {
    let elapsed = i64::try_from(to.checked_duration_since(from)?.as_millis()).ok()?;
    value.checked_add(elapsed)
}

fn project(value: i64, from: Instant, to: Instant) -> Option<i64> {
    if to >= from {
        advance(value, from, to)
    } else {
        value.checked_sub(i64::try_from(from.duration_since(to).as_millis()).ok()?)
    }
}

fn format_utc(milliseconds: i64) -> String {
    let seconds = milliseconds.div_euclid(1_000);
    let days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    // Every 400 Gregorian years contain exactly 146,097 days.
    let mut year = 1970 + days.div_euclid(146_097) * 400;
    let mut days = days.rem_euclid(146_097);
    loop {
        let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
        let year_days = if leap { 366 } else { 365 };
        if days < year_days {
            break;
        }
        days -= year_days;
        year += 1;
    }
    let leap = year % 4 == 0 && (year % 100 != 0 || year % 400 == 0);
    let month_days = [
        31,
        if leap { 29 } else { 28 },
        31,
        30,
        31,
        30,
        31,
        31,
        30,
        31,
        30,
        31,
    ];
    let mut month = 1;
    for length in month_days {
        if days < length {
            break;
        }
        days -= length;
        month += 1;
    }
    format!(
        "{year:04}-{month:02}-{:02} {:02}:{:02}:{:02} UTC",
        days + 1,
        time / 3_600,
        time / 60 % 60,
        time % 60
    )
}

#[cfg(test)]
mod tests {
    use super::*;
    const BASE: i64 = 1_700_000_000_000;

    fn sample(base: Instant, start_ms: u64, rtt: u64, offset: i64) -> SyncSample {
        validate_sample(
            BASE + start_ms as i64 + rtt as i64 / 2 + offset,
            BASE + start_ms as i64,
            BASE + start_ms as i64 + rtt as i64,
            base + Duration::from_millis(start_ms),
            base + Duration::from_millis(start_ms + rtt),
        )
        .unwrap()
    }

    #[test]
    fn a_clock_without_saved_calibration_uses_local_time() {
        let base = Instant::now();
        let mut clock = RuntimeClock::at(BASE, base);
        assert_eq!(
            clock.now_at(BASE + 29_999, base + Duration::from_millis(29_999)),
            BASE + 29_999
        );
        assert_eq!(
            clock.now_at(BASE + 30_000, base + Duration::from_secs(30)),
            BASE + 30_000
        );
        assert_eq!(clock.status(), ClockStatus::Local);
        assert!(!clock.is_synced());
        assert!(clock.last_success().is_none());
    }

    #[test]
    fn saved_calibration_uses_current_wall_time_plus_offset_and_preserves_sample_metadata() {
        let base = Instant::now();
        let record = sample(base, 0, 120, 90_050).record;
        let original = record.clone();
        let today = BASE + 86_400_000 * 800;
        let mut clock = RuntimeClock::from_saved_at(Some(&record), today, base).unwrap();
        assert_eq!(clock.now_at(today, base), today + 90_050);
        assert_eq!(
            clock.now_at(today + 30_000, base + Duration::from_secs(30)),
            today + 120_050
        );
        assert_eq!(clock.offset_ms(), Some(90_050));
        assert!(clock.is_synced());
        assert_eq!(
            clock.status_text(),
            "上次校時：2023-11-14 22:14:50 UTC · RTT 120 ms"
        );
        assert_eq!(clock.last_success(), Some(&original));
        assert_eq!(record, original);
    }

    #[test]
    fn serialized_shared_offsets_survive_reopen_and_are_the_exact_xml_input_including_zero() {
        let base = Instant::now();
        for offset in [-90_050, -1, 0, 1, 90_050] {
            let mut fresh = RuntimeClock::at(BASE, base);
            fresh
                .apply_at(sample(base, 0, 0, offset), BASE, base)
                .unwrap();
            let encoded = serde_json::to_vec(fresh.last_success().unwrap()).unwrap();
            let saved: TimeSyncRecord = serde_json::from_slice(&encoded).unwrap();
            let today = BASE + 86_400_000 * 300;
            let mut reopened = RuntimeClock::from_saved_at(Some(&saved), today, base).unwrap();
            // Every record's code and the export caller share this one clock;
            // the XML writer alone maps a measured zero to its +1 ms sentinel.
            for elapsed in [0_u64, 1, 29_999, 30_000, 60_000] {
                let instant = base + Duration::from_millis(elapsed);
                let wall = today + elapsed as i64;
                assert_eq!(reopened.now_at(wall, instant), wall + offset);
                assert_eq!(reopened.offset_ms(), Some(offset));
            }
            assert_eq!(reopened.last_success(), fresh.last_success());
        }
        let no_saved = RuntimeClock::from_saved_at(None, BASE, base).unwrap();
        assert_eq!(no_saved.offset_ms(), None);
        assert!(!no_saved.is_synced());
        // Also cover the public wrappers with clocks from the current process.
        let now = system_now_ms();
        let record = TimeSyncRecord {
            source: TIME_SOURCE.into(),
            server_time_ms: now,
            local_wall_ms_at_sample: now,
            offset_ms: -1_234,
            rtt_ms: 10,
        };
        assert_eq!(
            RuntimeClock::from_saved(Some(&record)).unwrap().offset_ms(),
            Some(-1_234)
        );
    }

    #[test]
    fn failed_refresh_keeps_reloaded_offset_but_a_new_sample_replaces_it() {
        let base = Instant::now();
        let saved = sample(base, 0, 0, 500).record;
        let mut clock = RuntimeClock::from_saved_at(Some(&saved), BASE, base).unwrap();
        clock.last_error = Some(ErrorCode::retry_connection().to_string());
        assert_eq!(clock.offset_ms(), Some(500));
        assert_eq!(clock.status(), ClockStatus::Synced);
        assert_eq!(clock.status_text(), "E11：連線異常，請重試");
        assert_eq!(clock.last_success(), Some(&saved));
        clock
            .apply_at(
                sample(base, 1_000, 0, -300),
                BASE + 1_000,
                base + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(clock.offset_ms(), Some(-300));
        assert_eq!(clock.status(), ClockStatus::Synced);
        assert!(clock.last_error().is_none());
        assert_eq!(clock.last_success().unwrap().offset_ms, -300);
    }

    #[test]
    fn windows_clock_correction_preserves_the_offset_for_codes_exports_and_reopen() {
        let base = Instant::now();
        let fast_wall = BASE + 3_600_000;
        let record = validate_sample(BASE, fast_wall, fast_wall, base, base)
            .unwrap()
            .record;
        let mut clock = RuntimeClock::from_saved_at(Some(&record), fast_wall, base).unwrap();
        assert_eq!(clock.now_at(fast_wall, base), BASE);

        // Windows corrects the previously fast wall clock while BNAuth runs.
        let corrected_wall = BASE + 1_000;
        let later = base + Duration::from_secs(1);
        let expected = corrected_wall + record.offset_ms;
        assert_eq!(clock.now_at(corrected_wall, later), expected);
        assert_eq!(clock.status(), ClockStatus::Discontinuous);
        assert_eq!(clock.status_text(), "E13：時間偏差過大，請重新校時");
        assert_eq!(clock.last_success(), Some(&record));
        assert_eq!(clock.offset_ms(), Some(record.offset_ms));
        assert_eq!(
            clock.now_at(corrected_wall + 30_000, later + Duration::from_secs(30)),
            expected + 30_000
        );
        let mut reopened =
            RuntimeClock::from_saved_at(Some(&record), corrected_wall, later).unwrap();
        assert_eq!(reopened.now_at(corrected_wall, later), expected);
        assert_eq!(reopened.offset_ms(), Some(record.offset_ms));
    }

    #[test]
    fn stale_inflight_sample_keeps_the_previous_offset_until_a_fresh_sample_arrives() {
        let base = Instant::now();
        let old_sample = sample(base, 0, 0, 300);
        let old_record = old_sample.record.clone();
        let mut clock = RuntimeClock::from_saved_at(Some(&old_record), BASE, base).unwrap();
        let jumped_wall = BASE + 5_000;
        let later = base + Duration::from_secs(1);
        clock.now_at(jumped_wall, later);
        assert!(clock.apply_at(old_sample, jumped_wall, later).is_err());
        assert_eq!(clock.offset_ms(), Some(300));
        assert_eq!(clock.last_success(), Some(&old_record));
        assert_eq!(clock.status_text(), "E13：時間偏差過大，請重新校時");

        let current_sample =
            validate_sample(BASE + 1_000, jumped_wall, jumped_wall, later, later).unwrap();
        clock.apply_at(current_sample, jumped_wall, later).unwrap();
        assert_eq!(clock.offset_ms(), Some(-4_000));
        assert_eq!(clock.status(), ClockStatus::Synced);
    }

    #[test]
    fn fresh_apply_resolves_a_clock_jump_first_detected_inside_apply() {
        let base = Instant::now();
        let saved = sample(base, 0, 0, 300).record;
        let mut clock = RuntimeClock::from_saved_at(Some(&saved), BASE, base).unwrap();
        let jumped_wall = BASE + 5_000;
        let later = base + Duration::from_secs(1);
        let fresh = validate_sample(BASE + 1_000, jumped_wall, jumped_wall, later, later).unwrap();
        let fresh_record = fresh.record.clone();
        // No previous UI frame has observed the changed wall clock yet.
        clock.apply_at(fresh, jumped_wall, later).unwrap();
        assert_eq!(clock.status(), ClockStatus::Synced);
        assert_eq!(clock.offset_ms(), Some(-4_000));
        assert_eq!(clock.last_success(), Some(&fresh_record));
    }

    #[test]
    fn invalid_saved_calibration_and_unsafe_offset_arithmetic_are_rejected() {
        let base = Instant::now();
        let record = sample(base, 0, 0, 300).record;
        let mut invalid = record.clone();
        invalid.source = "https://unexpected.invalid/time".into();
        assert!(RuntimeClock::from_saved_at(Some(&invalid), BASE, base).is_err());
        invalid = record.clone();
        invalid.rtt_ms = 4_001;
        assert!(RuntimeClock::from_saved_at(Some(&invalid), BASE, base).is_err());
        for offset in [i64::MIN, i64::MAX] {
            invalid = record.clone();
            invalid.offset_ms = offset;
            assert!(RuntimeClock::from_saved_at(Some(&invalid), BASE, base).is_err());
        }
        for wall in [i64::MIN, i64::MAX] {
            assert!(RuntimeClock::from_saved_at(Some(&record), wall, base).is_err());
        }
        assert_eq!(record.offset_ms, 300);
    }

    #[test]
    fn midpoint_and_delayed_ui_delivery_keep_the_original_instant() {
        let base = Instant::now();
        let s = sample(base, 0, 120, 250);
        assert_eq!(s.record.local_wall_ms_at_sample, BASE + 60);
        assert_eq!(s.record.offset_ms, 250);
        let mut clock = RuntimeClock::at(BASE, base);
        clock
            .apply_at(s, BASE + 500, base + Duration::from_millis(500))
            .unwrap();
        assert_eq!(
            clock.now_at(BASE + 1_000, base + Duration::from_secs(1)),
            BASE + 1_250
        );
        assert_eq!(clock.status(), ClockStatus::Synced);
    }

    #[test]
    fn smallest_rtt_wins_among_qualified_samples() {
        let base = Instant::now();
        let samples = [
            sample(base, 0, 180, 100),
            sample(base, 200, 20, 120),
            sample(base, 230, 80, 150),
        ];
        let selected = select_sample(&samples, None).unwrap();
        assert_eq!(selected.record.rtt_ms, 20);
        assert_eq!(selected.record.offset_ms, 120);
    }

    #[test]
    fn a_large_offset_needs_independent_agreement_at_the_same_instant() {
        let base = Instant::now();
        let first = sample(base, 0, 40, 90_000);
        assert_eq!(
            select_sample(std::slice::from_ref(&first), None).unwrap_err(),
            "E13：時間偏差過大，請重新校時"
        );
        let second = sample(base, 10_000, 20, 90_050);
        let selected = select_sample(&[first, second], None).unwrap();
        assert_eq!(selected.record.offset_ms, 90_050);
        assert_eq!(selected.record.rtt_ms, 20);
    }

    #[test]
    fn a_low_rtt_outlier_does_not_overrule_two_consistent_samples() {
        let base = Instant::now();
        let samples = [
            sample(base, 0, 10, 50_000),
            sample(base, 100, 80, 90_000),
            sample(base, 200, 60, 90_020),
        ];
        let selected = select_sample(&samples, None).unwrap();
        assert_eq!(selected.record.offset_ms, 90_020);
        assert!(select_sample(&samples[..2], None).is_err());
    }

    #[test]
    fn a_valid_existing_anchor_is_used_instead_of_bad_local_time() {
        let base = Instant::now();
        let reference = TimeReference {
            instant: base,
            utc_ms: BASE + 80_000,
        };
        let selected = select_sample(&[sample(base, 0, 20, 80_100)], Some(reference)).unwrap();
        assert_eq!(selected.record.offset_ms, 80_100);
        assert!(select_sample(&[sample(base, 0, 20, 0)], Some(reference)).is_err());
    }

    #[test]
    fn samples_preserve_midpoint_math_at_rtt_limits_regardless_of_year() {
        let base = Instant::now();
        for server_time in [946_684_799_999, 4_133_980_800_000] {
            for rtt_ms in [0, 1, 3_999, 4_000] {
                let received_at = base + Duration::from_millis(rtt_ms);
                let sample =
                    validate_sample(server_time, BASE, BASE + rtt_ms as i64, base, received_at)
                        .unwrap();
                sample.record.validate().unwrap();
                let midpoint = BASE + (rtt_ms / 2) as i64;
                assert_eq!(sample.record.rtt_ms, rtt_ms);
                assert_eq!(sample.record.local_wall_ms_at_sample, midpoint);
                assert_eq!(sample.record.offset_ms, server_time - midpoint);
                assert_eq!(
                    sample.server_at_receive_ms,
                    server_time + (rtt_ms / 2) as i64
                );
                assert_eq!(sample.received_at, received_at);
            }
        }
        // The RTT limit applies to the measured duration, before truncating ms.
        assert_eq!(
            validate_sample(
                BASE,
                BASE,
                BASE + 4_000,
                base,
                base + Duration::from_secs(4) + Duration::from_nanos(1),
            )
            .unwrap_err()
            .to_string(),
            "E11：連線異常，請重試",
        );
    }

    #[test]
    fn rejects_clock_steps_and_reports_rtt_failure_first() {
        let base = Instant::now();
        assert_eq!(
            validate_sample(
                BASE,
                BASE,
                BASE + 1_002,
                base,
                base + Duration::from_millis(1)
            )
            .unwrap_err(),
            SampleValidationError::WallClockDiscontinuity,
        );
        assert_eq!(
            validate_sample(BASE, BASE, BASE, base + Duration::from_secs(1), base).unwrap_err(),
            SampleValidationError::MonotonicDiscontinuity,
        );

        // A simultaneous clock change must not mask an independent RTT failure.
        assert_eq!(
            validate_sample(
                BASE,
                BASE,
                BASE + 7_000,
                base,
                base + Duration::from_millis(4_001),
            )
            .unwrap_err(),
            SampleValidationError::ExcessiveRtt,
        );
    }

    #[test]
    fn clock_jump_warns_and_moves_the_running_clock_with_the_same_offset() {
        let base = Instant::now();
        let mut clock = RuntimeClock::at(BASE, base);
        clock.apply_at(sample(base, 0, 0, 300), BASE, base).unwrap();
        assert_eq!(
            clock.now_at(BASE + 2_001, base + Duration::from_secs(1)),
            BASE + 2_301
        );
        assert_eq!(clock.status(), ClockStatus::Discontinuous);
        assert!(clock.is_synced());
        assert_eq!(clock.offset_ms(), Some(300));
        assert!(clock.take_clock_jump());
        assert_eq!(
            clock.now_at(BASE + 12_001, base + Duration::from_secs(11)),
            BASE + 12_301
        );
        assert!(!clock.take_clock_jump());
        assert_eq!(
            clock.now_at(BASE + 60_000, base + Duration::from_secs(12)),
            BASE + 60_300
        );
        assert!(clock.take_clock_jump());
        assert_eq!(clock.status_text(), "E13：時間偏差過大，請重新校時");
    }

    #[test]
    fn accumulated_wall_drift_also_warns_and_keeps_the_offset() {
        let base = Instant::now();
        let mut clock = RuntimeClock::at(BASE, base);
        clock.apply_at(sample(base, 0, 0, 300), BASE, base).unwrap();
        assert_eq!(
            clock.now_at(BASE + 1_600, base + Duration::from_secs(1)),
            BASE + 1_300
        );
        assert_eq!(
            clock.now_at(BASE + 3_200, base + Duration::from_secs(2)),
            BASE + 3_500
        );
        assert_eq!(clock.status(), ClockStatus::Discontinuous);
    }

    #[test]
    fn stale_sample_after_a_wall_jump_cannot_reenable_sync() {
        let base = Instant::now();
        let mut clock = RuntimeClock::at(BASE, base);
        assert!(
            clock
                .apply_at(
                    sample(base, 0, 0, 0),
                    BASE + 5_000,
                    base + Duration::from_secs(1)
                )
                .is_err()
        );
        assert!(!clock.is_synced());
        assert_eq!(clock.status(), ClockStatus::Discontinuous);
        assert_eq!(clock.last_error(), Some("E13：時間偏差過大，請重新校時"));
        assert_eq!(clock.status_text(), "E13：時間偏差過大，請重新校時");
    }

    #[test]
    fn status_dates_use_utc_and_include_leap_day() {
        assert_eq!(format_utc(-1), "1969-12-31 23:59:59 UTC");
        assert_eq!(format_utc(951_782_400_000), "2000-02-29 00:00:00 UTC");
        assert_eq!(format_utc(BASE), "2023-11-14 22:13:20 UTC");
    }
}
