//! A shared Battle.net clock whose saved offset can be restored across launches.
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
    last_success: Option<TimeSyncRecord>,
    last_error: Option<String>,
    discontinuous: bool,
    from_saved: bool,
    invalidation_pending: bool,
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
            last_success: None,
            last_error: None,
            discontinuous: false,
            from_saved: false,
            invalidation_pending: false,
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
            record.validate()?;
            let corrected_ms = wall_ms
                .checked_add(record.offset_ms)
                .filter(|value| (MIN_SERVER_TIME_MS..MAX_SERVER_TIME_MS).contains(value))
                .ok_or("保存的校時偏移套用後超出有效時間範圍，請手動校時。")?;
            clock.anchor = Some(SyncSample {
                record: record.clone(),
                received_at: instant,
                server_at_receive_ms: corrected_ms,
                local_at_receive_ms: wall_ms,
            });
            clock.last_success = Some(record.clone());
            clock.from_saved = true;
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
            self.invalidate_anchor();
        }
        self.last_tick = ClockObservation { wall_ms, instant };
        if let Some(anchor) = &self.anchor {
            if let Some(value) = advance(anchor.server_at_receive_ms, anchor.received_at, instant) {
                return value;
            }
            self.invalidate_anchor();
        }
        wall_ms
    }

    fn invalidate_anchor(&mut self) {
        if self.anchor.take().is_some() {
            self.invalidation_pending = true;
        }
        self.discontinuous = true;
    }

    /// Consume the request to remove an invalid calibration from active storage.
    /// Retaining `last_success` is only for history; it must not reactivate on
    /// restart. A newer valid sample may already be active when this is consumed;
    /// callers must invalidate old saved values without discarding that sample.
    pub fn take_invalidation(&mut self) -> bool {
        std::mem::take(&mut self.invalidation_pending)
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
            let error = "校時回覆到達後時間不連續，未套用；請手動校時。".to_owned();
            self.last_error = Some(error.clone());
            return Err(error);
        }
        self.last_success = Some(sample.record.clone());
        self.anchor = Some(sample);
        self.last_error = None;
        self.discontinuous = false;
        self.from_saved = false;
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

    /// Return the exact shared measured offset used for persistence and XML.
    /// Observing the clocks first prevents exporting an invalidated calibration.
    pub fn offset_ms(&mut self) -> Option<i64> {
        self.offset_at(system_now_ms(), Instant::now())
    }

    fn offset_at(&mut self, wall_ms: i64, instant: Instant) -> Option<i64> {
        self.now_at(wall_ms, instant);
        self.anchor.as_ref().map(|anchor| anchor.record.offset_ms)
    }

    pub fn is_from_saved(&self) -> bool {
        self.from_saved && self.anchor.is_some()
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
                "校時失敗；沿用{} {} 的有效校時，請手動重試。",
                if self.from_saved { "已保存的" } else { "" },
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
        assert_eq!(
            clock.offset_at(today + 30_000, base + Duration::from_secs(30)),
            Some(90_050)
        );
        assert!(clock.is_synced() && clock.is_from_saved());
        assert!(clock.status_text().starts_with("已校時 ·"));
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
                assert_eq!(reopened.offset_at(wall, instant), Some(offset));
            }
            assert_eq!(reopened.last_success(), fresh.last_success());
        }
        let mut no_saved = RuntimeClock::from_saved_at(None, BASE, base).unwrap();
        assert_eq!(no_saved.offset_at(BASE, base), None);
        assert!(!no_saved.is_synced() && !no_saved.is_from_saved());
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
        clock.last_error = Some("離線".into());
        assert_eq!(clock.offset_at(BASE, base), Some(500));
        assert_eq!(clock.status(), ClockStatus::SyncedAfterFailure);
        assert!(clock.status_text().contains("已保存的"));
        assert_eq!(clock.last_success(), Some(&saved));
        clock
            .apply_at(
                sample(base, 1_000, 0, -300),
                BASE + 1_000,
                base + Duration::from_secs(1),
            )
            .unwrap();
        assert_eq!(
            clock.offset_at(BASE + 1_000, base + Duration::from_secs(1)),
            Some(-300)
        );
        assert!(!clock.is_from_saved());
        assert_eq!(clock.status(), ClockStatus::Synced);
        assert!(clock.last_error().is_none());
        assert_eq!(clock.last_success().unwrap().offset_ms, -300);
    }

    #[test]
    fn reloaded_clock_jump_invalidates_export_offset_but_retains_saved_record() {
        let base = Instant::now();
        let record = sample(base, 0, 0, 300).record;
        let mut clock = RuntimeClock::from_saved_at(Some(&record), BASE, base).unwrap();
        assert_eq!(
            clock.offset_at(BASE + 2_001, base + Duration::from_secs(1)),
            None
        );
        assert_eq!(clock.status(), ClockStatus::Discontinuous);
        assert!(!clock.is_from_saved());
        assert_eq!(clock.last_success(), Some(&record));
        assert_eq!(
            clock.now_at(BASE + 3_001, base + Duration::from_secs(2)),
            BASE + 3_001
        );
    }

    #[test]
    fn known_invalid_saved_offset_is_removed_once_and_cannot_reactivate_on_reopen() {
        let base = Instant::now();
        let fast_wall = BASE + 3_600_000;
        let record = validate_sample(BASE, fast_wall, fast_wall, base, base)
            .unwrap()
            .record;
        let mut persisted = Some(record.clone());
        let mut clock = RuntimeClock::from_saved_at(persisted.as_ref(), fast_wall, base).unwrap();
        assert_eq!(clock.now_at(fast_wall, base), BASE);
        assert!(
            !clock.take_invalidation(),
            "loading a valid saved result is not an invalidation"
        );

        // Windows corrects the previously fast wall clock while BNAuth runs.
        let corrected_wall = BASE + 1_000;
        let later = base + Duration::from_secs(1);
        assert_eq!(clock.now_at(corrected_wall, later), corrected_wall);
        assert_eq!(clock.status(), ClockStatus::Discontinuous);
        assert_eq!(
            clock.last_success(),
            Some(&record),
            "history stays available"
        );
        if clock.take_invalidation() {
            persisted = None;
        } else {
            panic!("a dropped valid anchor must request persisted invalidation");
        }
        assert!(
            !clock.take_invalidation(),
            "the persistence request is one-shot"
        );
        assert_eq!(
            clock.offset_at(corrected_wall + 1_000, later + Duration::from_secs(1)),
            None
        );
        assert!(
            !clock.take_invalidation(),
            "local fallback must not repeatedly request writes"
        );

        let encoded = serde_json::to_vec(&persisted).unwrap();
        let reopened_record: Option<TimeSyncRecord> = serde_json::from_slice(&encoded).unwrap();
        let mut reopened =
            RuntimeClock::from_saved_at(reopened_record.as_ref(), corrected_wall, later).unwrap();
        assert_eq!(reopened.now_at(corrected_wall, later), corrected_wall);
        assert_eq!(reopened.offset_at(corrected_wall, later), None);
        assert_eq!(reopened.status(), ClockStatus::Local);
        assert!(!reopened.take_invalidation());
    }

    #[test]
    fn observing_and_rebasing_at_one_reading_cannot_hide_an_unobserved_or_intermediate_step() {
        let instant = Instant::now();
        let fast_wall = BASE + 3_600_000;
        let saved = validate_sample(BASE, fast_wall, fast_wall, instant, instant)
            .unwrap()
            .record;
        let mut old = RuntimeClock::from_saved_observation(
            Some(&saved),
            ClockObservation::at(fast_wall, instant),
        )
        .unwrap();
        let corrected = ClockObservation::at(BASE + 1_000, instant + Duration::from_secs(1));
        assert!(
            !old.take_invalidation(),
            "a step is not signalled until observed"
        );
        assert_eq!(old.observe(corrected), BASE + 1_000);
        assert!(old.take_invalidation());

        // A genuinely distinct source remains usable at the same reading. A
        // second step after that reading must still be visible after rebasing.
        let source = validate_sample(
            BASE + 1_250,
            BASE + 1_000,
            BASE + 1_000,
            instant + Duration::from_secs(1),
            instant + Duration::from_secs(1),
        )
        .unwrap()
        .record;
        let mut rebased = RuntimeClock::from_saved_observation(Some(&source), corrected).unwrap();
        assert_eq!(rebased.observe(corrected), BASE + 1_250);
        assert!(rebased.is_synced() && !rebased.take_invalidation());
        let second_step = ClockObservation::at(BASE + 10_000, instant + Duration::from_secs(2));
        assert_eq!(rebased.observe(second_step), BASE + 10_000);
        assert!(rebased.take_invalidation());
        assert!(!rebased.is_synced());
    }

    #[test]
    fn stale_inflight_sample_cannot_restore_invalid_offset_but_a_new_sample_supersedes_it() {
        let base = Instant::now();
        let old_sample = sample(base, 0, 0, 300);
        let old_record = old_sample.record.clone();
        let mut clock = RuntimeClock::from_saved_at(Some(&old_record), BASE, base).unwrap();
        let jumped_wall = BASE + 5_000;
        let later = base + Duration::from_secs(1);
        clock.now_at(jumped_wall, later);
        assert!(clock.apply_at(old_sample, jumped_wall, later).is_err());
        assert_eq!(clock.offset_at(jumped_wall, later), None);
        assert_eq!(clock.last_success(), Some(&old_record));
        assert!(
            clock.clone().take_invalidation(),
            "a rejected stale result cannot cancel the pending invalidation"
        );

        let current_sample =
            validate_sample(BASE + 1_000, jumped_wall, jumped_wall, later, later).unwrap();
        clock.apply_at(current_sample, jumped_wall, later).unwrap();
        assert_eq!(clock.offset_at(jumped_wall, later), Some(-4_000));
        assert_eq!(clock.status(), ClockStatus::Synced);
        assert!(
            clock.take_invalidation(),
            "a newer valid sample must not hide old persisted invalidation"
        );
        assert_eq!(clock.offset_at(jumped_wall, later), Some(-4_000));
        assert_eq!(clock.last_success().unwrap().offset_ms, -4_000);
    }

    #[test]
    fn fresh_apply_reports_a_clock_jump_first_detected_inside_apply_without_losing_fresh_result() {
        let base = Instant::now();
        let saved = sample(base, 0, 0, 300).record;
        let mut clock = RuntimeClock::from_saved_at(Some(&saved), BASE, base).unwrap();
        let jumped_wall = BASE + 5_000;
        let later = base + Duration::from_secs(1);
        let fresh = validate_sample(BASE + 1_000, jumped_wall, jumped_wall, later, later).unwrap();
        let fresh_record = fresh.record.clone();
        // No previous UI frame has observed the changed wall clock yet.
        clock.apply_at(fresh, jumped_wall, later).unwrap();
        assert!(
            clock.take_invalidation(),
            "the old saved offset still needs invalidation"
        );
        assert!(!clock.take_invalidation());
        assert_eq!(clock.status(), ClockStatus::Synced);
        assert_eq!(clock.offset_at(jumped_wall, later), Some(-4_000));
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
        invalid.server_time_ms = MIN_SERVER_TIME_MS - 1;
        assert!(RuntimeClock::from_saved_at(Some(&invalid), BASE, base).is_err());
        invalid = record.clone();
        invalid.rtt_ms = 2_001;
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
