//! Todoist Sync API response/request model types (T-202/T-203/T-204, EPIC-03).
//!
//! `common` lands the scalar shapes every resource embeds: the `Id`
//! newtype, the `due`/`deadline` objects and the `priority` enum (T-202).
//! `project`, `section`, `label`, `filter` and `user` (T-203) and `item`
//! and `note` (T-204) build on top of those for the Sync API resources
//! that don't have their own dedicated task elsewhere in the epic.
//!
//! Note: this module is not yet wired into [`crate::api`] — `src/api/mod.rs`
//! is out of scope for T-202 (see task notes); a later task adds
//! `pub mod model;` there.

// No consumers yet: later EPIC-03 tasks (e.g. sync client) reference
// these types. `pub` alone does not silence `dead_code` in a binary crate
// — same pattern as `error.rs` (T-003) and `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod common;
pub mod filter;
pub mod item;
pub mod label;
pub mod note;
pub mod project;
pub mod section;
pub mod user;

pub use common::{Deadline, Due, Id, Priority, PriorityOutOfRange};
pub use filter::Filter;
pub use item::Item;
pub use label::Label;
pub use note::{FileAttachment, Note};
pub use project::Project;
pub use section::Section;
pub use user::{TzInfo, User};

use serde::{Deserialize, Deserializer};

/// Deserialises a boolean from either a JSON boolean or the Sync API's
/// legacy `0`/`1` integer encoding (T-203/T-204).
///
/// Several Sync API resources (notably `is_deleted`/`checked`-style flags)
/// have historically sent `0`/`1` instead of `false`/`true`; this helper
/// accepts both so the domain structs in [`project`], [`section`],
/// [`label`], [`filter`], [`user`], [`item`] and [`note`] don't choke on
/// either form. Any integer other than `0`/`1` is rejected rather than
/// silently coerced, surfacing unexpected API data instead of guessing at
/// its meaning.
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

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[derive(Deserialize)]
    struct BoolProbe {
        #[serde(default, deserialize_with = "flexible_bool")]
        value: bool,
    }

    #[test]
    fn flexible_bool_accepts_json_booleans() {
        assert!(
            serde_json::from_value::<BoolProbe>(json!({ "value": true }))
                .unwrap()
                .value
        );
        assert!(
            !serde_json::from_value::<BoolProbe>(json!({ "value": false }))
                .unwrap()
                .value
        );
    }

    #[test]
    fn flexible_bool_accepts_legacy_integer_encoding() {
        assert!(
            serde_json::from_value::<BoolProbe>(json!({ "value": 1 }))
                .unwrap()
                .value
        );
        assert!(
            !serde_json::from_value::<BoolProbe>(json!({ "value": 0 }))
                .unwrap()
                .value
        );
    }

    #[test]
    fn flexible_bool_falls_back_to_the_field_default_when_absent() {
        assert!(
            !serde_json::from_value::<BoolProbe>(json!({}))
                .unwrap()
                .value
        );
    }

    #[test]
    fn flexible_bool_rejects_values_that_are_neither_bool_nor_integer() {
        // Strings, arrays and objects are never valid in the field's place;
        // only booleans and integers are. (`null` is a separate, documented
        // limitation reported to the coder: a present-but-null key errors out
        // instead of falling back to the `#[serde(default)]` value, because
        // serde only applies the default when the key is absent.)
        for bad in [json!("1"), json!([1]), json!({}), json!({ "nested": true })] {
            let result: Result<BoolProbe, _> = serde_json::from_value(json!({ "value": bad }));
            assert!(result.is_err(), "expected error for {bad}");
        }
    }

    #[test]
    fn flexible_bool_rejects_out_of_range_integers() {
        for bad in [json!(2), json!(-1), json!(42)] {
            let result: Result<BoolProbe, _> = serde_json::from_value(json!({ "value": bad }));
            assert!(result.is_err(), "expected error for integer {bad}");
        }
    }
}
