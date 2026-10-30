//! Local SQLite store (T-301, EPIC-04).
//!
//! This task only lands the error vocabulary (`DbError` in `src/error.rs`)
//! and this module scaffold; connection handling, schema migrations and
//! task persistence arrive in later EPIC-04 tasks so the rest of the epic
//! can land in parallel.

// No consumers yet: later EPIC-04 tasks add the connection pool, migration
// runner and row (de)serialisation here. `pub` alone does not silence
// `dead_code` in a binary crate, so allow it at module scope — same pattern
// as `error.rs` (T-003) and `src/api/mod.rs` (T-201).
#![allow(dead_code)]
