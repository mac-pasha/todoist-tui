//! Todoist REST API client (T-201, EPIC-03).
//!
//! This task only lands the error vocabulary (`ApiError` in `src/error.rs`)
//! and this module scaffold; HTTP wiring (request building, retries, task
//! fetch/sync) arrives in later EPIC-03 tasks so the rest of the epic can
//! land in parallel.

// No consumers yet: later EPIC-03 tasks add the HTTP client, request
// builders and response types here. `pub` alone does not silence
// `dead_code` in a binary crate, so allow it at module scope — same pattern
// as `error.rs` (T-003) and `src/auth/mod.rs` (T-101).
#![allow(dead_code)]

pub mod model;
