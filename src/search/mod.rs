//! Fuzzy matching for task/filter search (T-902, EPIC-10).
//!
//! This task only lands the pure matching core (`fuzzy::match_score`); the
//! cache/index that will call it in bulk over a task list, and the UI
//! highlighting that will consume the matched ranges, arrive in later
//! EPIC-10/EPIC-04/EPIC-07 tasks. Keeping this module dependency-free (no
//! cache, no UI, no new crate) lets it land ahead of those.

// No consumers yet: later EPIC-10 tasks (and EPIC-04/EPIC-07 UI code) call
// into this module. `pub` alone does not silence `dead_code` in a binary
// crate, so allow it at module scope — same pattern as `src/api/mod.rs`
// (T-201) and `src/auth/mod.rs` (T-101).
#![allow(dead_code, unused_imports)]

pub mod fuzzy;

pub use fuzzy::{match_score, MatchScore};
