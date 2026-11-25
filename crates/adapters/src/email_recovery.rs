//! One user-requested restore of an existing Battle.net authenticator.

use bnauth_core::{ErrorCode, Secret};
use serde::{Deserialize, Serialize};
use std::io::Read;
use std::time::Instant;
use zeroize::Zeroizing;

const RESTORE_URL: &str =
    "https://authenticator-rest-api.bnet-identity.blizzard.net/v2/authenticator/device";
const MAX_BODY: usize = 1024 * 1024;
const MAX_HEADERS: usize = 16 * 1024;
const MAX_JSON_DEPTH: usize = 16;
const ACCOUNT_SETUP: &str = "Battle.net 要求完成帳號設定。請前往官方 App 處理後再恢復。";

#[derive(Serialize)]
#[serde(rename_all = "camelCase")]
struct RestoreRequest<'a> {
    account_identifier: &'a str,
    serial: &'a str,
    restore_code: &'a str,
}

#[derive(Deserialize)]
#[serde(rename_all = "camelCase")]
struct RestoreReply<'a> {
    #[serde(default)]
    require_healup: bool,
    #[serde(borrow)]
    device_secret: Option<&'a str>,
}

/// Connect once over HTTPS. Call this only on a worker after an explicit user
/// action with the credentials for the authenticator currently on their account.
pub fn recover_existing(
    email: &str,
    serial: &str,
    restore_code: &str,
    deadline: Instant,
    cancelled: impl Fn() -> bool,
) -> Result<Secret, String> {
    check_active(deadline, &cancelled)?;
    let request_body = request_body(email, serial, restore_code)?;
    let agent: ureq::Agent = ureq::Agent::config_builder()
        .https_only(true)
        .max_redirects(0)
        .http_status_as_error(false)
        .proxy(None)
        .max_idle_connections(0)
        .max_response_header_size(MAX_HEADERS)
        .build()
        .into();
    let timeout = deadline
        .checked_duration_since(Instant::now())
        .filter(|duration| !duration.is_zero())
        .ok_or_else(connection_failure)?;
    let mut response = agent
        .post(RESTORE_URL)
        .header("Accept", "application/json")
        .header("Content-Type", "application/json")
        .header("Cache-Control", "no-store")
        .config()
        .timeout_global(Some(timeout))
        .timeout_resolve(Some(timeout))
        .timeout_connect(Some(timeout))
        .build()
        .send(request_body.as_slice())
        .map_err(|_| connection_failure())?;
    drop(request_body);

    check_active(deadline, &cancelled)?;
    let status = response.status().as_u16();
    if status != 200 {
        return Err(ErrorCode::http(status).to_string());
    }
    read_reply(response.body_mut(), deadline, &cancelled)
}

fn check_active(deadline: Instant, cancelled: &impl Fn() -> bool) -> Result<(), String> {
    if cancelled() {
        Err("已取消恢復。".to_owned())
    } else if Instant::now() >= deadline {
        Err(connection_failure())
    } else {
        Ok(())
    }
}

fn read_reply(
    body: &mut ureq::Body,
    deadline: Instant,
    cancelled: &impl Fn() -> bool,
) -> Result<Secret, String> {
    let (capacity, read_limit) = match body.content_length() {
        Some(length) if length > MAX_BODY as u64 => return Err(connection_failure()),
        Some(length) => (length as usize, length as usize),
        None => (1024, MAX_BODY + 1),
    };
    let mut bytes = Zeroizing::new(Vec::with_capacity(capacity));
    let mut reader = body.as_reader();
    loop {
        check_active(deadline, cancelled)?;
        let remaining = (bytes.capacity().min(read_limit) - bytes.len()).min(8192);
        let read = reader
            .by_ref()
            .take(remaining as u64)
            .read_to_end(&mut bytes)
            .map_err(|_| connection_failure())?;
        if read < remaining || bytes.len() == read_limit {
            break;
        }
        if bytes.len() < bytes.capacity() {
            continue;
        }
        // A streamed reply has no declared size. Wipe its old buffer when growing it.
        let mut larger = Zeroizing::new(Vec::with_capacity((bytes.len() * 2 + 1).min(read_limit)));
        larger.extend_from_slice(&bytes);
        bytes = larger;
    }
    check_active(deadline, cancelled)?;
    parse_reply(&bytes)
}

fn request_body(
    email: &str,
    serial: &str,
    restore_code: &str,
) -> Result<Zeroizing<Vec<u8>>, String> {
    let request = RestoreRequest {
        account_identifier: email,
        serial,
        restore_code,
    };
    let mut body = Zeroizing::new(Vec::with_capacity(2048));
    serde_json::to_writer(&mut *body, &request).map_err(|_| connection_failure())?;
    Ok(body)
}

fn connection_failure() -> String {
    ErrorCode::retry_connection().to_string()
}

fn parse_reply(bytes: &[u8]) -> Result<Secret, String> {
    if bytes.len() > MAX_BODY || !json_depth_is_bounded(bytes) {
        return Err(connection_failure());
    }
    let reply: RestoreReply<'_> = serde_json::from_slice(bytes).map_err(|_| connection_failure())?;
    let encoded = reply.device_secret.filter(|text| !text.is_empty());
    let encoded = encoded.ok_or_else(|| {
        if reply.require_healup {
            ACCOUNT_SETUP.to_owned()
        } else {
            connection_failure()
        }
    })?;
    Secret::from_hex(encoded).map_err(|_| connection_failure())
}

// Enforce the depth cap even in ignored fields. Serde handles JSON syntax;
// this pass only counts nesting outside string literals.
fn json_depth_is_bounded(bytes: &[u8]) -> bool {
    let mut depth = 0usize;
    let mut quoted = false;
    let mut escaped = false;
    for &byte in bytes {
        if quoted {
            if escaped {
                escaped = false;
            } else if byte == b'\\' {
                escaped = true;
            } else if byte == b'"' {
                quoted = false;
            }
        } else {
            match byte {
                b'"' => quoted = true,
                b'{' | b'[' => {
                    depth += 1;
                    if depth > MAX_JSON_DEPTH {
                        return false;
                    }
                }
                b'}' | b']' => depth = depth.saturating_sub(1),
                _ => {}
            }
        }
    }
    true
}

#[cfg(test)]
#[path = "email_recovery_tests.rs"]
mod tests;
