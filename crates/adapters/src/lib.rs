//! Independently implemented platform adapters for BNAuth.
pub mod native;
pub mod storage;

pub use storage::{CommitOutcome, Repository};

pub mod email_recovery;
pub mod export_file;
pub mod time_sync;
pub mod winauth2;

use bnauth_core::ErrorCode;

fn operation_io_error(error: &std::io::Error, fallback: ErrorCode) -> ErrorCode {
    #[cfg(windows)]
    match error.raw_os_error() {
        Some(5 | 19) => return ErrorCode::PermissionDenied,
        Some(32 | 33) => return ErrorCode::FileInUse,
        Some(39 | 112) => return ErrorCode::DiskFull,
        _ => {}
    }
    match error.kind() {
        std::io::ErrorKind::PermissionDenied => ErrorCode::PermissionDenied,
        std::io::ErrorKind::StorageFull => ErrorCode::DiskFull,
        _ => fallback,
    }
}

fn temp_cleanup_error(error: &std::io::Error) -> ErrorCode {
    operation_io_error(error, ErrorCode::FileInUse)
}

/// Format an I/O error for display without relying on Windows' localized code suffix.
pub fn io_error_detail(error: &std::io::Error) -> String {
    #[cfg(windows)]
    if let Some(code) = error.raw_os_error() {
        return format!("Windows error {}", code as u32);
    }
    error.to_string()
}

/// Compare the whole open file, including its length, without retaining another copy.
fn file_matches_bytes(file: &mut std::fs::File, mut expected: &[u8]) -> std::io::Result<bool> {
    use std::io::{ErrorKind, Read, Seek};

    file.rewind()?;
    let mut buffer = zeroize::Zeroizing::new([0_u8; 64 * 1024]);
    loop {
        let count = match file.read(&mut buffer[..]) {
            Ok(0) => return Ok(expected.is_empty()),
            Ok(count) => count,
            Err(error) if error.kind() == ErrorKind::Interrupted => continue,
            Err(error) => return Err(error),
        };
        let Some(rest) = expected.strip_prefix(&buffer[..count]) else {
            return Ok(false);
        };
        expected = rest;
    }
}
