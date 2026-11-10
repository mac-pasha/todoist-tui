//! Project / section / label structure mutations (T-802, EPIC-09).
//!
//! T-802 lands the pure command builders ([`command`]): each one validates
//! its input and returns a [`crate::queue::QueuedCommand`] ready for the
//! write queue. No cache writes, no enqueue, no UI; those arrive in later
//! EPIC-09 tasks, so items here have no production consumer yet — hence the
//! module-scope `#![allow(dead_code, unused_imports)]`, same pattern as
//! `src/queue/mod.rs` and `src/api/mod.rs`.
#![allow(dead_code, unused_imports)]

pub mod cascade;
pub mod command;
pub mod label;
pub mod project;
pub mod section;
