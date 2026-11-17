//! Independently implemented platform adapters for BNAuth.
pub mod native;
pub mod storage;

pub use storage::{CommitOutcome, Repository};

pub mod export_file;
pub mod time_sync;
pub mod winauth2;
