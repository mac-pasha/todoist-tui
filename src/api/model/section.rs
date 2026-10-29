//! `Section` resource shape from the Todoist Sync API (T-203, EPIC-03).
//!
//! Data only: no persistence, no business rules (EPIC-04 owns storage).
//! Unknown fields are accepted silently (no `deny_unknown_fields`) so
//! additive API changes deserialise without a code change, matching the
//! convention set by `common.rs` (T-202).

use serde::{Deserialize, Serialize};

use super::common::Id;
use super::flexible_bool;

/// A Todoist section, as returned by the Sync API's `sections` resource.
// `section_order` is the field name the acceptance criteria and the Sync
// API itself use; it's not a stylistic choice clippy's lint should flag.
#[allow(clippy::struct_field_names)]
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Section {
    /// The section's unique id.
    pub id: Id,
    /// The section's display name.
    pub name: String,
    /// The id of the project this section belongs to.
    pub project_id: Id,
    /// Position among sibling sections (used for manual ordering).
    #[serde(default)]
    pub section_order: i64,
    /// Whether the section has been deleted.
    ///
    /// Tolerates both JSON booleans and the Sync API's legacy `0`/`1`
    /// integer encoding (T-203).
    #[serde(default, deserialize_with = "flexible_bool")]
    pub is_deleted: bool,
    /// Whether the section has been archived.
    #[serde(default)]
    pub is_archived: bool,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-203 `Section` resource shape. All ids and names
    //! are fake fixture data; no real credentials or personal data.
    use super::*;
    use serde_json::json;

    /// Compile-time proof that `Section` derives the four traits the task
    /// requires (`Debug, Clone, Serialize, Deserialize`).
    fn assert_required_traits<
        T: std::fmt::Debug + Clone + Serialize + serde::de::DeserializeOwned,
    >() {
    }

    #[test]
    fn section_derives_debug_clone_serialize_deserialize() {
        assert_required_traits::<Section>();

        let section: Section = serde_json::from_value(json!({
            "id": "sec-1",
            "name": "Today",
            "project_id": "proj-1"
        }))
        .unwrap();
        let cloned = section.clone();
        assert_eq!(cloned.name, "Today");
        assert!(format!("{section:?}").contains("Today"));
    }

    #[test]
    fn section_deserializes_every_acceptance_field() {
        let section: Section = serde_json::from_value(json!({
            "id": "sec-1",
            "name": "Today",
            "project_id": "proj-1",
            "section_order": 2,
            "is_deleted": true,
            "is_archived": true
        }))
        .unwrap();

        assert_eq!(section.id.as_str(), "sec-1");
        assert_eq!(section.name, "Today");
        assert_eq!(section.project_id, Id::new("proj-1"));
        assert_eq!(section.section_order, 2);
        assert!(section.is_deleted);
        assert!(section.is_archived);
    }

    #[test]
    fn section_applies_defaults_when_optional_fields_are_missing() {
        // `id`, `name` and `project_id` are mandatory; `section_order`,
        // `is_deleted` and `is_archived` all carry `#[serde(default)]`.
        let section: Section = serde_json::from_value(json!({
            "id": "sec-2",
            "name": "Someday",
            "project_id": "proj-2"
        }))
        .unwrap();

        assert_eq!(section.section_order, 0);
        assert!(!section.is_deleted);
        assert!(!section.is_archived);
    }

    #[test]
    fn section_accepts_unknown_fields() {
        let section: Section = serde_json::from_value(json!({
            "id": "sec-3",
            "name": "Later",
            "project_id": "proj-3",
            "x": null
        }))
        .unwrap();

        assert_eq!(section.name, "Later");
    }

    #[test]
    fn section_is_deleted_accepts_json_booleans() {
        for (raw, expected) in [(json!(true), true), (json!(false), false)] {
            let section: Section = serde_json::from_value(json!({
                "id": "sec-4",
                "name": "Legacy",
                "project_id": "proj-4",
                "is_deleted": raw
            }))
            .unwrap();
            assert_eq!(section.is_deleted, expected, "fixture value {raw}");
        }
    }

    #[test]
    fn section_is_deleted_accepts_legacy_integers() {
        for (raw, expected) in [(json!(1), true), (json!(0), false)] {
            let section: Section = serde_json::from_value(json!({
                "id": "sec-5",
                "name": "Legacy",
                "project_id": "proj-5",
                "is_deleted": raw
            }))
            .unwrap();
            assert_eq!(section.is_deleted, expected, "fixture value {raw}");
        }
    }

    #[test]
    fn section_round_trips_through_json() {
        let original = json!({
            "id": "sec-6",
            "name": "Round trip",
            "project_id": "proj-6",
            "section_order": 1,
            "is_deleted": false,
            "is_archived": true
        });

        let section: Section = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&section).unwrap(), original);
    }

    #[test]
    fn section_rejects_a_missing_project_id() {
        // `project_id` is not optional in the acceptance criteria, so a
        // section without one must fail rather than silently default.
        let result: Result<Section, _> = serde_json::from_value(json!({
            "id": "sec-7",
            "name": "Orphan"
        }));

        assert!(result.is_err(), "missing `project_id` must not deserialise");
    }
}
