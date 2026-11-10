//! Local SQLite store (T-301, EPIC-04).
//!
//! T-301 lands the error vocabulary (`DbError` in `src/error.rs`) and this
//! module scaffold; T-302 (this module's `open` submodule) adds connection
//! opening with locked-down file permissions and baseline pragmas. T-303
//! adds the schema DDL and migration runner (`migrations`). T-304 adds
//! get/set accessors for the `meta` table (`meta`) — the sync cursor and
//! sync timestamps. Task persistence (row (de)serialisation) arrives in
//! later EPIC-04 tasks so the rest of the epic can land in parallel.

// No consumers of the module root yet beyond `open`/`migrations`: later
// EPIC-04 tasks add row (de)serialisation here. `pub` alone does not
// silence `dead_code` in a binary crate, so allow it at module scope —
// same pattern as `error.rs` (T-003) and `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod apply;
pub mod meta;
pub mod migrations;
pub mod open;
pub mod queries;
pub mod resources;
pub mod wipe;

pub use apply::{apply_sync, ApplyStats, ResourceCounts};
