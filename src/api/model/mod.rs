//! Todoist Sync API response/request model types (T-202/T-203, EPIC-03).
//!
//! `common` lands the scalar shapes every resource embeds: the `Id`
//! newtype, the `due`/`deadline` objects and the `priority` enum (T-202).
//! `project`, `section`, `label`, `filter` and `user` (T-203) build on top
//! of those for the five Sync API resources that don't have their own
//! dedicated task elsewhere in the epic. Remaining domain structs (item,
//! ...) arrive in later EPIC-03 tasks.
//!
//! Note: this module is not yet wired into [`crate::api`] — `src/api/mod.rs`
//! is out of scope for T-202 (see task notes); a later task adds
//! `pub mod model;` there.

// No consumers yet: later EPIC-03 tasks (e.g. item, sync client) reference
// these types. `pub` alone does not silence `dead_code` in a binary crate
// — same pattern as `error.rs` (T-003) and `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod common;
pub mod filter;
pub mod label;
pub mod project;
pub mod section;
pub mod user;

pub use common::{Deadline, Due, Id, Priority, PriorityOutOfRange};
pub use filter::Filter;
pub use label::Label;
pub use project::Project;
pub use section::Section;
pub use user::{TzInfo, User};

use serde::{Deserialize, Deserializer};

/// Deserialises a boolean from either a JSON boolean or the Sync API's
/// legacy `0`/`1` integer encoding (T-203).
///
/// Several Sync API resources (notably `is_deleted`) have historically sent
/// `0`/`1` instead of `false`/`true`; this helper accepts both so the
/// domain structs in [`project`], [`section`], [`label`], [`filter`] and
/// [`user`] don't choke on either form. Any integer other than `0`/`1` is
/// treated as truthy (non-zero), matching the loose C-style convention the
/// legacy encoding follows.
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
        BoolOrInt::Int(value) => Ok(value != 0),
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-203 module wiring: the five resource modules are
    //! declared and their types re-exported from `api::model`, and the shared
    //! `flexible_bool` deserialiser behaves for both encodings. All data is
    //! fake fixture data; no real credentials or personal data.
    use super::*;
    use serde::Deserialize;
    use serde_json::json;

    // --- module declaration / re-exports --------------------------------

    #[test]
    fn mod_declares_and_reexports_the_five_resource_modules() {
        // Compile-time proof: each module path resolves...
        let _: crate::api::model::project::Project;
        let _: crate::api::model::section::Section;
        let _: crate::api::model::label::Label;
        let _: crate::api::model::filter::Filter;
        let _: crate::api::model::user::User;

        // ...and each type is reachable through the `mod.rs` re-export under
        // the exact same underlying type (not a look-alike).
        assert_eq!(
            std::any::type_name::<Project>(),
            std::any::type_name::<project::Project>()
        );
        assert_eq!(
            std::any::type_name::<Section>(),
            std::any::type_name::<section::Section>()
        );
        assert_eq!(
            std::any::type_name::<Label>(),
            std::any::type_name::<label::Label>()
        );
        assert_eq!(
            std::any::type_name::<Filter>(),
            std::any::type_name::<filter::Filter>()
        );
        assert_eq!(
            std::any::type_name::<User>(),
            std::any::type_name::<user::User>()
        );
        assert_eq!(
            std::any::type_name::<TzInfo>(),
            std::any::type_name::<user::TzInfo>()
        );
    }

    #[test]
    fn mod_source_declares_the_modules_and_re_exports_as_public() {
        // A private `mod project;` would still compile the list above (same
        // crate), so the declarations/re-exports must be checked in source.
        let source = include_str!("mod.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("mod.rs has a production half");

        for module in ["common", "project", "section", "label", "filter", "user"] {
            assert!(
                production.contains(&format!("pub mod {module};")),
                "mod.rs must declare `pub mod {module};`"
            );
        }
        for reexport in [
            "pub use project::Project;",
            "pub use section::Section;",
            "pub use label::Label;",
            "pub use filter::Filter;",
            "pub use user::{TzInfo, User};",
        ] {
            assert!(
                production.contains(reexport),
                "mod.rs must re-export `{reexport}`"
            );
        }
    }

    // --- shared flexible_bool deserialiser ------------------------------

    /// Minimal probe struct so `flexible_bool` can be tested directly.
    #[derive(Debug, Deserialize)]
    struct BoolProbe {
        #[serde(default, deserialize_with = "super::flexible_bool")]
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
            assert!(result.is_err(), "fixture value {bad} must not deserialise");
        }
    }
}
