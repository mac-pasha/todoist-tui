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
pub mod item;
pub mod note;

pub use common::{Deadline, Due, Id, Priority, PriorityOutOfRange};
pub use item::Item;
pub use note::{FileAttachment, Note};

use serde::{Deserialize, Deserializer};

/// Deserializes a "flexible" boolean field that the Sync API sometimes
/// encodes as a JSON boolean and sometimes as a `0`/`1` integer (seen on
/// `checked`/`is_deleted`-style flags across resources).
///
/// Any other integer value is rejected rather than silently coerced.
pub(crate) fn flexible_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: Deserializer<'de>,
{
    #[derive(Deserialize)]
    #[serde(untagged)]
    enum BoolOrInt {
        Bool(bool),
        Int(i64),
    }

    match BoolOrInt::deserialize(deserializer)? {
        BoolOrInt::Bool(value) => Ok(value),
        BoolOrInt::Int(0) => Ok(false),
        BoolOrInt::Int(1) => Ok(true),
        BoolOrInt::Int(other) => Err(serde::de::Error::custom(format!(
            "expected a boolean or 0/1 integer, got integer {other}"
        ))),
    }
}
