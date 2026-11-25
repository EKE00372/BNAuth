//! One explicitly requested, bounded anonymous Battle.net time-sampling round.
//!
//! Call on a worker thread. This module has no scheduler, account credentials,
//! retry timer, file writes, or ownership of the GUI's common runtime clock.

use bnauth_core::clock::{
    SampleValidationError, SyncSample, TIME_SOURCE, TimeReference, select_sample, system_now_ms,
    validate_sample,
};
use bnauth_core::{ErrorCode, OperationError};
use serde::Deserialize;
use std::io::Read;
use std::time::{Duration, Instant};

const SAMPLE_LIMIT: usize = 3;
const SAMPLE_TIMEOUT: Duration = Duration::from_secs(5);
const ROUND_TIMEOUT: Duration = Duration::from_secs(15);
const MAX_BODY: usize = 16 * 1024;

#[derive(Debug)]
pub struct SyncFailure {
    message: String,
    clock_discontinuity: bool,
}

impl SyncFailure {
    pub fn is_clock_discontinuity(&self) -> bool {
        self.clock_discontinuity
    }

    pub fn into_message(self) -> String {
        self.message
    }
}

impl std::fmt::Display for SyncFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.message)
    }
}

impl From<String> for SyncFailure {
    fn from(message: String) -> Self {
        Self {
            message,
            clock_discontinuity: false,
        }
    }
}

impl From<OperationError> for SyncFailure {
    fn from(error: OperationError) -> Self {
        error.to_string().into()
    }
}

impl From<SampleValidationError> for SyncFailure {
    fn from(error: SampleValidationError) -> Self {
        Self {
            message: error.to_string(),
            clock_discontinuity: matches!(
                error,
                SampleValidationError::WallClockDiscontinuity
                    | SampleValidationError::MonotonicDiscontinuity
            ),
        }
    }
}

#[derive(Debug)]
struct SampleFailure {
    failure: SyncFailure,
    stop_round: bool,
}

impl SampleFailure {
    fn sample(error: OperationError) -> Self {
        Self {
            failure: error.into(),
            stop_round: false,
        }
    }
    fn validation(error: SampleValidationError) -> Self {
        Self {
            failure: error.into(),
            stop_round: false,
        }
    }
    fn stop(error: OperationError) -> Self {
        Self {
            failure: error.into(),
            stop_round: true,
        }
    }
}

#[derive(Deserialize)]
#[serde(deny_unknown_fields)]
struct TimeReply {
    #[serde(rename = "timeMs")]
    time_ms: i64,
}

/// Performs at most three samples within one fifteen-second round. The caller
/// coalesces concurrent manual/import triggers and applies a success in memory.
pub fn synchronize(reference: Option<TimeReference>) -> Result<SyncSample, SyncFailure> {
    let started = Instant::now();
    // Keep one connection pool for the complete round. Individual requests
    // retain their own remaining time budget.
    let agent = time_agent();
    collect_samples(
        reference,
        |timeout| request_sample(&agent, timeout),
        || started.elapsed(),
    )
}

fn collect_samples(
    reference: Option<TimeReference>,
    mut request: impl FnMut(Duration) -> Result<SyncSample, SampleFailure>,
    mut elapsed: impl FnMut() -> Duration,
) -> Result<SyncSample, SyncFailure> {
    let mut samples = Vec::with_capacity(SAMPLE_LIMIT);
    let mut last_failure = None;
    for _ in 0..SAMPLE_LIMIT {
        let remaining = ROUND_TIMEOUT.saturating_sub(elapsed());
        if remaining.is_zero() {
            last_failure = Some(ErrorCode::retry_connection().into());
            break;
        }
        match request(remaining.min(SAMPLE_TIMEOUT)) {
            Ok(sample) => {
                if elapsed() > ROUND_TIMEOUT {
                    last_failure = Some(ErrorCode::retry_connection().into());
                    break;
                }
                samples.push(sample);
            }
            Err(error) => {
                if error.stop_round {
                    return Err(error.failure);
                }
                last_failure = Some(error.failure);
            }
        }
    }
    match select_sample(&samples, reference) {
        Ok(sample) => Ok(sample),
        Err(error) if samples.is_empty() => Err(last_failure.unwrap_or_else(|| error.into())),
        Err(error) => Err(error.into()),
    }
}

fn time_agent() -> ureq::Agent {
    // Disable all redirects, optional decompression, environment proxies, cookies
    // and credentials. The only request target is this compile-time HTTPS URL.
    ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .proxy(None)
        .max_response_header_size(MAX_BODY)
        .build()
        .into()
}

fn request_sample(agent: &ureq::Agent, timeout: Duration) -> Result<SyncSample, SampleFailure> {
    let sent_wall_ms = system_now_ms();
    let sent_at = Instant::now();
    let mut response = agent
        .get(TIME_SOURCE)
        .header("Accept", "application/json")
        .header("Cache-Control", "no-cache")
        .header("User-Agent", "BNAuth/0.2")
        .config()
        .timeout_global(Some(timeout))
        .timeout_resolve(Some(timeout))
        .timeout_connect(Some(timeout))
        .build()
        .call()
        .map_err(|_| SampleFailure::sample(ErrorCode::retry_connection()))?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(status_failure(status));
    }
    if response
        .body()
        .content_length()
        .is_some_and(|length| length > MAX_BODY as u64)
    {
        return Err(SampleFailure::sample(ErrorCode::retry_connection()));
    }
    let mut bytes = Vec::new();
    response
        .body_mut()
        .as_reader()
        .take(MAX_BODY as u64 + 1)
        .read_to_end(&mut bytes)
        .map_err(|_| SampleFailure::sample(ErrorCode::retry_connection()))?;
    let received_at = Instant::now();
    let received_wall_ms = system_now_ms();
    if received_at.duration_since(sent_at) > timeout {
        return Err(SampleFailure::sample(ErrorCode::retry_connection()));
    }
    let server_time_ms = parse_reply(&bytes).map_err(SampleFailure::sample)?;
    validate_sample(
        server_time_ms,
        sent_wall_ms,
        received_wall_ms,
        sent_at,
        received_at,
    )
    .map_err(SampleFailure::validation)
}

fn status_failure(status: u16) -> SampleFailure {
    let error = ErrorCode::http(status);
    if (300..500).contains(&status) {
        SampleFailure::stop(error)
    } else {
        SampleFailure::sample(error)
    }
}

fn parse_reply(bytes: &[u8]) -> Result<i64, OperationError> {
    if bytes.len() > MAX_BODY {
        return Err(ErrorCode::retry_connection());
    }
    // This schema is flat and denies unknown fields, so nested objects/arrays are
    // rejected immediately, well before the generic API JSON depth limit of16.
    // serde's struct visitor also rejects duplicate timeMs keys and non-i64 data.
    serde_json::from_slice::<TimeReply>(bytes)
        .map(|reply| reply.time_ms)
        .map_err(|_| ErrorCode::retry_connection())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;
    const BASE: i64 = 1_700_000_000_000;

    fn sample(base: Instant, index: u64, offset: i64) -> SyncSample {
        let start = index * 100;
        validate_sample(
            BASE + start as i64 + 10 + offset,
            BASE + start as i64,
            BASE + start as i64 + 20,
            base + Duration::from_millis(start),
            base + Duration::from_millis(start + 20),
        )
        .unwrap()
    }

    fn clock_jump_failure(base: Instant, start_ms: u64) -> SampleFailure {
        let sent_at = base + Duration::from_millis(start_ms);
        let error = validate_sample(
            BASE + start_ms as i64 + 10,
            BASE + start_ms as i64,
            BASE + start_ms as i64 + 5_020,
            sent_at,
            sent_at + Duration::from_millis(20),
        )
        .unwrap_err();
        SampleFailure::validation(error)
    }

    #[test]
    fn time_ms_is_a_strict_integer_with_no_ignored_data() {
        assert_eq!(parse_reply(br#"{"timeMs":1700000000000}"#).unwrap(), BASE);
        for body in [
            br#"{}"#.as_slice(),
            br#"{"timeMs":"1700000000000"}"#,
            br#"{"timeMs":1.5}"#,
            br#"{"timeMs":null}"#,
            br#"{"timeMs":9223372036854775808}"#,
            br#"{"timeMs":1,"timeMs":2}"#,
            br#"{"timeMs":1700000000000,"extra":1}"#,
            br#"{"timeMs":{"nested":[[[1]]]}}"#,
            br#"{"timeMs":1700000000000} trailing"#,
        ] {
            assert_eq!(
                parse_reply(body).unwrap_err().to_string(),
                "E11：連線異常，請重試"
            );
        }
    }

    #[test]
    fn response_size_is_checked_before_parsing() {
        let mut body = br#"{"timeMs":1700000000000}"#.to_vec();
        body.resize(MAX_BODY, b' ');
        assert_eq!(parse_reply(&body).unwrap(), BASE);
        body.push(b' ');
        assert_eq!(
            parse_reply(&body).unwrap_err().to_string(),
            "E11：連線異常，請重試"
        );
    }

    #[test]
    fn two_timeouts_then_a_sampling_clock_jump_retain_the_failure_kind() {
        let base = Instant::now();
        let calls = Cell::new(0);
        let elapsed_ms = Cell::new(0);
        let error = collect_samples(
            None,
            |timeout| {
                assert_eq!(timeout, SAMPLE_TIMEOUT);
                let attempt = calls.get();
                calls.set(attempt + 1);
                if attempt < 2 {
                    elapsed_ms.set(elapsed_ms.get() + 5_000);
                    Err(SampleFailure::sample(ErrorCode::retry_connection()))
                } else {
                    elapsed_ms.set(elapsed_ms.get() + 20);
                    Err(clock_jump_failure(base, 10_000))
                }
            },
            || Duration::from_millis(elapsed_ms.get()),
        )
        .unwrap_err();
        assert_eq!(calls.get(), 3);
        assert_eq!(elapsed_ms.get(), 10_020);
        assert!(error.is_clock_discontinuity());
        assert_eq!(
            error.into_message(),
            "E13：時間偏差過大，請重新校時"
        );
    }

    #[test]
    fn an_independent_last_failure_does_not_inherit_an_earlier_clock_jump() {
        let base = Instant::now();
        let calls = Cell::new(0);
        let error = collect_samples(
            None,
            |_| {
                let attempt = calls.get();
                calls.set(attempt + 1);
                if attempt == 0 {
                    Err(clock_jump_failure(base, 0))
                } else {
                    Err(SampleFailure::sample(ErrorCode::retry_connection()))
                }
            },
            || Duration::ZERO,
        )
        .unwrap_err();
        assert_eq!(calls.get(), 3);
        assert!(!error.is_clock_discontinuity());
        assert_eq!(error.into_message(), "E11：連線異常，請重試");
    }

    #[test]
    fn remaining_round_budget_caps_a_sample_and_late_results_are_rejected() {
        let ticks = Cell::new(0);
        let base = Instant::now();
        let result = collect_samples(
            None,
            |timeout| {
                assert_eq!(timeout, Duration::from_millis(2_500));
                Ok(sample(base, 0, 0))
            },
            || {
                ticks.set(ticks.get() + 1);
                if ticks.get() == 1 {
                    Duration::from_millis(12_500)
                } else {
                    Duration::from_millis(15_001)
                }
            },
        );
        assert_eq!(
            result.unwrap_err().into_message(),
            "E11：連線異常，請重試"
        );
    }

    #[test]
    fn a_bad_sample_does_not_discard_a_confirmed_large_correction() {
        for last_sample_jumps in [false, true] {
            let base = Instant::now();
            let calls = Cell::new(0);
            let result = collect_samples(
                None,
                |_| {
                    let index = calls.get();
                    calls.set(index + 1);
                    if last_sample_jumps && index == 2 {
                        Err(clock_jump_failure(base, 200))
                    } else if !last_sample_jumps && index == 1 {
                        Err(SampleFailure::sample(ErrorCode::retry_connection()))
                    } else {
                        Ok(sample(base, index, 90_000))
                    }
                },
                || Duration::ZERO,
            )
            .unwrap();
            assert_eq!(result.record.offset_ms, 90_000);
            assert_eq!(calls.get(), 3);
        }
    }

    #[test]
    fn redirects_and_rate_limits_end_the_round_immediately() {
        for status in [301, 302, 307, 308, 401, 429] {
            let calls = Cell::new(0);
            let result = collect_samples(
                None,
                |_| {
                    calls.set(calls.get() + 1);
                    Err(status_failure(status))
                },
                || Duration::ZERO,
            );
            assert_eq!(calls.get(), 1);
            let message = result.unwrap_err().into_message();
            if status == 429 {
                assert_eq!(message, "E14：請求次數過多，請稍後重試");
            } else {
                assert_eq!(message, format!("E12：HTTP 錯誤，服務回傳 HTTP {status}。"));
            }
        }
    }
}
