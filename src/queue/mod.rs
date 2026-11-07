//! Offline command queue (T-402, EPIC-05).
//!
//! T-402 landed the error vocabulary (`QueueError` in `src/error.rs`) and
//! this module scaffold; T-403 adds the [`command`] submodule, modelling a
//! single queued command and its Sync API JSON projection. Persistence and
//! the replay loop are not wired up yet, so items here have no production
//! consumer until later EPIC-05 tasks land — hence the module-scope
//! `#![allow(dead_code, unused_imports)]`, same pattern as `error.rs`
//! (T-003) and `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod command;

pub use command::{CommandState, QueuedCommand};
