//! Todoist Sync API response/request model types (T-202, EPIC-03).
//!
//! This task lands the scalar shapes every resource embeds: the `Id`
//! newtype, the `due`/`deadline` objects and the `priority` enum. Domain
//! structs (project, section, item, ...) arrive in later EPIC-03 tasks and
//! build on top of these.
//!
//! Note: this module is not yet wired into [`crate::api`] — `src/api/mod.rs`
//! is out of scope for T-202 (see task notes); a later task adds
//! `pub mod model;` there.

// No consumers yet: domain structs land in later EPIC-03 tasks (T-203,
// T-204) and reference these types. `pub` alone does not silence
// `dead_code` in a binary crate — same pattern as `error.rs` (T-003) and
// `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod common;

pub use common::{Deadline, Due, Id, Priority, PriorityOutOfRange};
