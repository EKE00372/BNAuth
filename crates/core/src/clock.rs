//! A process-local Battle.net clock. Persisted measurement history is never an anchor.
//!
//! All OTPs and the progress bar should use one `now_ms` value per UI frame.

use crate::TimeSyncRecord;
pub use crate::{TIME_SOURCE, now_ms as system_now_ms};
use std::time::{Duration, Instant};

pub const MIN_SERVER_TIME_MS: i64 = 946_684_800_000; // 2000-01-01 UTC
pub const MAX_SERVER_TIME_MS: i64 = 4_133_980_800_000; // 2101-01-01 UTC, exclusive
const MAX_RTT: Duration = Duration::from_secs(2);
const CONTINUITY_TOLERANCE_MS: i128 = 1_000;
const CONFIRM_OFFSET_MS: i128 = 5_000;
const SAMPLE_AGREEMENT_MS: i128 = 1_000;

/// Preserve the reason for rejection so callers can coalesce only clock-change warnings.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SampleValidationError {
    MonotonicDiscontinuity,
    ExcessiveRtt,
    ServerTimeOutOfRange,
    WallClockDiscontinuity,
}

impl std::fmt::Display for SampleValidationError {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str(match self {
            Self::MonotonicDiscontinuity => "校時樣本的單調時間不連續。",
            Self::ExcessiveRtt => "校時樣本往返超過 2 秒，未採用。",
            Self::ServerTimeOutOfRange => "校時回覆不在 2000～2100 年的有效範圍。",
            Self::WallClockDiscontinuity => "校時採樣期間本機時間不連續，未採用。",
        })
    }
}

impl std::error::Error for SampleValidationError {}

/// Only fresh measurements have this process's `Instant`; this is not serializable.
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
    SyncedAfterFailure,
    Discontinuous,
}

impl ClockStatus {
    pub fn label(self) -> &'static str {
        match self {
            Self::Local => "本機時間；尚未校時",
            Self::Synced => "已校時",
            Self::SyncedAfterFailure => "校時失敗；沿用本次有效校時",
            Self::Discontinuous => "時間不連續；請手動校時",
        }
    }
}

#[derive(Clone, Debug)]
struct Observation {
    wall_ms: i64,
    instant: Instant,
}

#[derive(Clone, Debug)]
pub struct RuntimeClock {
    anchor: Option<SyncSample>,
    last_tick: Observation,
    last_success: Option<TimeSyncRecord>,
    last_error: Option<String>,
    discontinuous: bool,
}

impl Default for RuntimeClock {
    fn default() -> Self {
        Self::new()
    }
}

impl RuntimeClock {
    /// A new process always starts on local UTC, irrespective of saved history.
    pub fn new() -> Self {
        Self::at(system_now_ms(), Instant::now())
    }

    fn at(wall_ms: i64, instant: Instant) -> Self {
        Self {
            anchor: None,
            last_tick: Observation { wall_ms, instant },
            last_success: None,
            last_error: None,
            discontinuous: false,
        }
    }

    pub fn now_ms(&mut self) -> i64 {
        self.now_at(system_now_ms(), Instant::now())
    }

    fn now_at(&mut self, wall_ms: i64, instant: Instant) -> i64 {
        let tick_continuous = continuous(
            self.last_tick.wall_ms,
            self.last_tick.instant,
            wall_ms,
            instant,
        );
        // Also compare against the complete anchored interval, so small repeated
        // wall-clock steps cannot accumulate unnoticed between individual frames.
        let anchor_continuous = self.anchor.as_ref().is_none_or(|anchor| {
            continuous(
                anchor.local_at_receive_ms,
                anchor.received_at,
                wall_ms,
                instant,
            )
        });
        if !tick_continuous || !anchor_continuous {
            self.anchor = None;
            self.discontinuous = true;
        }
        self.last_tick = Observation { wall_ms, instant };
        if let Some(anchor) = &self.anchor {
            if let Some(value) = advance(anchor.server_at_receive_ms, anchor.received_at, instant) {
                return value;
            }
            self.anchor = None;
            self.discontinuous = true;
        }
        wall_ms
    }

    /// Only a validated fresh sample can establish an anchor. Disk history cannot.
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
            let error = "校時回覆到達後時間不連續，未套用；請手動校時。".to_owned();
            self.last_error = Some(error.clone());
            return Err(error);
        }
        self.last_success = Some(sample.record.clone());
        self.anchor = Some(sample);
        self.last_error = None;
        self.discontinuous = false;
        Ok(())
    }

    /// A failed refresh never discards a still-continuous successful anchor.
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

    pub fn status(&self) -> ClockStatus {
        match (
            self.anchor.is_some(),
            self.last_error.is_some(),
            self.discontinuous,
        ) {
            (true, false, _) => ClockStatus::Synced,
            (true, true, _) => ClockStatus::SyncedAfterFailure,
            (false, _, true) => ClockStatus::Discontinuous,
            _ => ClockStatus::Local,
        }
    }

    pub fn status_text(&self) -> String {
        match (self.status(), self.last_success.as_ref()) {
            (ClockStatus::Synced, Some(record)) => format!(
                "已校時 · {} · RTT {} ms",
                format_utc(record.server_time_ms),
                record.rtt_ms,
            ),
            (ClockStatus::SyncedAfterFailure, Some(record)) => format!(
                "校時失敗；沿用 {} 的有效校時，請手動重試。",
                format_utc(record.server_time_ms),
            ),
            (ClockStatus::Discontinuous, _) => "時間不連續，使用本機時間；請手動校時。".to_owned(),
            (ClockStatus::Local, _) if self.last_error.is_some() => {
                "校時失敗，使用本機時間；請手動重試。".to_owned()
            }
            _ => self.status().label().to_owned(),
        }
    }

    pub fn last_success(&self) -> Option<&TimeSyncRecord> {
        self.last_success.as_ref()
    }

    pub fn last_error(&self) -> Option<&str> {
        self.last_error.as_deref()
    }
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
    if rtt > MAX_RTT {
        return Err(SampleValidationError::ExcessiveRtt);
    }
    if !(MIN_SERVER_TIME_MS..MAX_SERVER_TIME_MS).contains(&server_time_ms) {
        return Err(SampleValidationError::ServerTimeOutOfRange);
    }
    if !continuous(sent_wall_ms, sent_at, received_wall_ms, received_at) {
        return Err(SampleValidationError::WallClockDiscontinuity);
    }
    // The checks above bound RTT to 2000 ms and server time to 2000–2100.
    // Production wall times come from the Windows clock, well within i64 milliseconds.
    let rtt_ms = rtt.as_millis() as u64;
    let half_rtt = (rtt_ms / 2) as i64;
    let midpoint_wall_ms = sent_wall_ms + half_rtt;
    let server_at_receive_ms = server_time_ms + half_rtt;
    let offset_ms = server_time_ms - midpoint_wall_ms;
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
        if samples.is_empty() {
            "沒有符合品質要求的校時樣本；請手動重試。".to_owned()
        } else {
            "校時偏差超過 5 秒，但沒有第二個一致樣本；未套用，請手動重試。".to_owned()
        }
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
    let mut days = seconds.div_euclid(86_400);
    let time = seconds.rem_euclid(86_400);
    let mut year = 1970;
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
    fn a_new_clock_uses_local_time_not_stored_history() {
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
        assert!(select_sample(std::slice::from_ref(&first), None).is_err());
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
    fn bounded_samples_preserve_midpoint_math_at_rtt_and_date_limits() {
        let base = Instant::now();
        for server_time in [MIN_SERVER_TIME_MS, MAX_SERVER_TIME_MS - 1] {
            for rtt_ms in [0, 1, 1_999, 2_000] {
                let received_at = base + Duration::from_millis(rtt_ms);
                let sample =
                    validate_sample(server_time, BASE, BASE + rtt_ms as i64, base, received_at)
                        .unwrap();
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
                BASE + 2_000,
                base,
                base + Duration::from_secs(2) + Duration::from_nanos(1),
            )
            .unwrap_err(),
            SampleValidationError::ExcessiveRtt,
        );
    }

    #[test]
    fn rejects_long_rtt_bad_date_and_clock_steps() {
        let base = Instant::now();
        assert_eq!(
            validate_sample(
                BASE,
                BASE,
                BASE + 2001,
                base,
                base + Duration::from_millis(2001)
            )
            .unwrap_err(),
            SampleValidationError::ExcessiveRtt,
        );
        assert!(
            validate_sample(
                BASE,
                BASE,
                BASE + 2_000,
                base,
                base + Duration::from_millis(2_000)
            )
            .is_ok()
        );
        for invalid_time in [MIN_SERVER_TIME_MS - 1, MAX_SERVER_TIME_MS] {
            assert_eq!(
                validate_sample(invalid_time, BASE, BASE, base, base).unwrap_err(),
                SampleValidationError::ServerTimeOutOfRange,
            );
        }
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

        // A simultaneous wall-clock change must not turn an independent RTT or
        // server-date failure into a suppressible clock-change notification.
        assert_eq!(
            validate_sample(
                BASE,
                BASE,
                BASE + 7_000,
                base,
                base + Duration::from_millis(2_001),
            )
            .unwrap_err(),
            SampleValidationError::ExcessiveRtt,
        );
        assert_eq!(
            validate_sample(
                MIN_SERVER_TIME_MS - 1,
                BASE,
                BASE + 5_020,
                base,
                base + Duration::from_millis(20),
            )
            .unwrap_err(),
            SampleValidationError::ServerTimeOutOfRange,
        );
    }

    #[test]
    fn failed_refresh_keeps_a_continuous_anchor_and_success_history() {
        let base = Instant::now();
        let mut clock = RuntimeClock::at(BASE, base);
        clock.apply_at(sample(base, 0, 0, 500), BASE, base).unwrap();
        // Inject failure after one simulated second; the public wrapper simply
        // observes the real clocks first, then records this same error state.
        clock.now_at(BASE + 1_000, base + Duration::from_secs(1));
        clock.last_error = Some("離線".to_owned());
        assert_eq!(
            clock.now_at(BASE + 2_000, base + Duration::from_secs(2)),
            BASE + 2_500
        );
        assert_eq!(clock.status(), ClockStatus::SyncedAfterFailure);
        assert_eq!(clock.last_success().unwrap().offset_ms, 500);
    }

    #[test]
    fn clock_jump_invalidates_anchor_without_a_network_action() {
        let base = Instant::now();
        let mut clock = RuntimeClock::at(BASE, base);
        clock.apply_at(sample(base, 0, 0, 300), BASE, base).unwrap();
        assert_eq!(
            clock.now_at(BASE + 2_001, base + Duration::from_secs(1)),
            BASE + 2_001
        );
        assert_eq!(clock.status(), ClockStatus::Discontinuous);
        assert!(!clock.is_synced());
        assert!(clock.last_success().is_some());
    }

    #[test]
    fn accumulated_wall_drift_also_invalidates_anchor() {
        let base = Instant::now();
        let mut clock = RuntimeClock::at(BASE, base);
        clock.apply_at(sample(base, 0, 0, 300), BASE, base).unwrap();
        assert_eq!(
            clock.now_at(BASE + 1_600, base + Duration::from_secs(1)),
            BASE + 1_300
        );
        assert_eq!(
            clock.now_at(BASE + 3_200, base + Duration::from_secs(2)),
            BASE + 3_200
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
        assert_eq!(
            clock.last_error(),
            Some("校時回覆到達後時間不連續，未套用；請手動校時。")
        );
    }

    #[test]
    fn status_dates_use_utc_and_include_leap_day() {
        assert_eq!(format_utc(951_782_400_000), "2000-02-29 00:00:00 UTC");
        assert_eq!(format_utc(BASE), "2023-11-14 22:13:20 UTC");
    }
}
