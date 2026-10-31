//! Local SQLite store (T-301, EPIC-04).
//!
//! T-301 lands the error vocabulary (`DbError` in `src/error.rs`) and this
//! module scaffold; T-302 (this module's `open` submodule) adds connection
//! opening with locked-down file permissions and baseline pragmas. Schema
//! migrations and task persistence arrive in later EPIC-04 tasks so the
//! rest of the epic can land in parallel.

// No consumers of the module root yet beyond `open`: later EPIC-04 tasks
// add the migration runner and row (de)serialisation here. `pub` alone
// does not silence `dead_code` in a binary crate, so allow it at module
// scope — same pattern as `error.rs` (T-003) and `src/api/mod.rs` (T-201).
#![allow(dead_code)]

pub mod open;
