use super::*;
use serde_json::{Value, json};
use std::io::Cursor;

const SEED: &str = "00112233445566778899AABBCCDDEEFF01234567";
const CONNECTION_NOTICE: &str = "E11：連線異常，請重試";

#[test]
fn restore_request_uses_only_the_three_fields_with_a_dashed_serial() {
    let mut serial = "US123456789012".to_owned();
    bnauth_core::submit_serial_input(&mut serial, true).unwrap();
    let bytes = request_body("owner@example.test", &serial, "aB12cd34").unwrap();
    let request: Value = serde_json::from_slice(&bytes).unwrap();
    assert_eq!(
        request,
        json!({
            "accountIdentifier": "owner@example.test",
            "serial": "US-1234-5678-9012",
            "restoreCode": "aB12cd34",
        })
    );
}

#[test]
fn restore_reply_returns_valid_seed_with_or_without_account_setup_flag() {
    let input = format!(
        "{{\"deviceSecret\":\"{SEED}\",\"requireHealup\":true,\"timeMs\":123,\"future\":\"escaped \\\" {{[\"}}"
    );
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    let mut body = ureq::Body::builder().data(input);
    let secret = read_reply(&mut body, deadline, &|| false).unwrap();
    assert_eq!(secret, Secret::from_hex(SEED).unwrap());

    let input = format!("{}{{\"deviceSecret\":\"{SEED}\"}}", " ".repeat(2048));
    let mut body = ureq::Body::builder().reader(Cursor::new(input));
    let secret = read_reply(&mut body, deadline, &|| false).unwrap();
    assert_eq!(secret, Secret::from_hex(SEED).unwrap());
}

#[test]
fn restore_reply_rejects_missing_or_invalid_seed_without_echoing_the_body() {
    let setup = parse_reply(br#"{"requireHealup":true,"deviceSecret":null,"url":"ignored"}"#);
    assert_eq!(setup.err().as_deref(), Some(ACCOUNT_SETUP));

    let missing = parse_reply(br#"{"requireHealup":false,"deviceSecret":""}"#);
    assert_eq!(missing.err().as_deref(), Some(CONNECTION_NOTICE));

    for input in [
        br#"{"deviceSecret":"1234"}"#.as_slice(),
        br#"{"deviceSecret":"GG112233445566778899AABBCCDDEEFF01234567"}"#,
    ] {
        assert_eq!(parse_reply(input).err().as_deref(), Some(CONNECTION_NOTICE));
    }

    for input in [
        br#"{"deviceSecret":"0123","deviceSecret":"4567"}"#.as_slice(),
        br#"{"deviceSecret":1234}"#,
        br#"{"requireHealup":"true"}"#,
        br#"{"deviceSecret":"unterminated"#,
    ] {
        assert_eq!(parse_reply(input).err().as_deref(), Some(CONNECTION_NOTICE));
    }
}

#[test]
fn restore_reply_caps_size_and_depth_without_parsing_unneeded_fields() {
    let too_large = vec![b' '; MAX_BODY + 1];
    let mut body = ureq::Body::builder().reader(Cursor::new(too_large));
    let deadline = Instant::now() + std::time::Duration::from_secs(30);
    assert_eq!(
        read_reply(&mut body, deadline, &|| false).err().as_deref(),
        Some(CONNECTION_NOTICE)
    );

    let input = format!(
        "{{\"deviceSecret\":\"{SEED}\",\"future\":{}0{}}}",
        "[".repeat(MAX_JSON_DEPTH),
        "]".repeat(MAX_JSON_DEPTH),
    );
    assert_eq!(
        parse_reply(input.as_bytes()).err().as_deref(),
        Some(CONNECTION_NOTICE)
    );
}

#[test]
fn restore_failure_displays_specific_http_status_without_service_body() {
    for status in [307, 404, 503] {
        assert_eq!(
            ErrorCode::http(status).to_string(),
            format!("E12：HTTP 錯誤，服務回傳 HTTP {status}。")
        );
    }
    assert_eq!(ErrorCode::http(429).to_string(), "E14：請求次數過多，請稍後重試");
}
