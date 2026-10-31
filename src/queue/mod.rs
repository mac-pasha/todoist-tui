//! Offline command queue (T-402, EPIC-05).
//!
//! This task only lands the error vocabulary (`QueueError` in
//! `src/error.rs`) and this module scaffold; persistence, replay and
//! conflict handling for queued Todoist Sync API commands arrive in later
//! EPIC-05 tasks so the rest of the epic can land in parallel.

// No submodules yet: later EPIC-05 tasks add queue persistence, command
// (de)serialisation and the replay loop here. Left empty-bodied for now,
// so there is nothing to mark `dead_code` — later tasks add the
// `#![allow(dead_code)]` module-scope attribute if/when they introduce
// unused-until-wired items, same pattern as `error.rs` (T-003) and
// `src/api/mod.rs` (T-201).
