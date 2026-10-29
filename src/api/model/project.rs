//! `Project` resource shape from the Todoist Sync API (T-203, EPIC-03).
//!
//! Data only: no persistence, no business rules (EPIC-04 owns storage).
//! Unknown fields are accepted silently (no `deny_unknown_fields`) so
//! additive API changes deserialise without a code change, matching the
//! convention set by `common.rs` (T-202).

use serde::{Deserialize, Serialize};

use super::common::Id;
use super::flexible_bool;

/// A Todoist project, as returned by the Sync API's `projects` resource.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Project {
    /// The project's unique id.
    pub id: Id,
    /// The project's display name.
    pub name: String,
    /// The id of the parent project, or `None` for a top-level project.
    #[serde(default)]
    pub parent_id: Option<Id>,
    /// Position among sibling projects (used for manual ordering).
    #[serde(default)]
    pub child_order: i64,
    /// The project's colour, as a Todoist colour name (e.g. `"berry_red"`).
    #[serde(default)]
    pub color: String,
    /// Whether the project is marked as a favorite.
    #[serde(default)]
    pub is_favorite: bool,
    /// Whether the project has been archived.
    #[serde(default)]
    pub is_archived: bool,
    /// Whether the project has been deleted.
    ///
    /// Tolerates both JSON booleans and the Sync API's legacy `0`/`1`
    /// integer encoding (T-203).
    #[serde(default, deserialize_with = "flexible_bool")]
    pub is_deleted: bool,
    /// The project's display style, e.g. `"list"` or `"board"`.
    #[serde(default)]
    pub view_style: String,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-203 `Project` resource shape. All ids, names
    //! and colours are fake fixture data; no real credentials or personal
    //! data.
    use super::*;
    use serde_json::json;

    /// Compile-time proof that `Project` derives the four traits the task
    /// requires (`Debug, Clone, Serialize, Deserialize`).
    fn assert_required_traits<
        T: std::fmt::Debug + Clone + Serialize + serde::de::DeserializeOwned,
    >() {
    }

    #[test]
    fn project_derives_debug_clone_serialize_deserialize() {
        assert_required_traits::<Project>();

        // `Clone` and `Debug` are exercised at runtime too, so a hand-written
        // opaque impl could not sneak past the bound above.
        let project: Project = serde_json::from_value(json!({
            "id": "proj-1",
            "name": "Groceries",
            "is_favorite": true,
        }))
        .unwrap();
        let cloned = project.clone();
        assert_eq!(cloned.name, "Groceries");
        assert!(cloned.is_favorite);
        assert!(format!("{project:?}").contains("Groceries"));
    }

    #[test]
    fn project_deserializes_every_acceptance_field() {
        let project: Project = serde_json::from_value(json!({
            "id": "proj-1",
            "name": "Groceries",
            "parent_id": "proj-0",
            "child_order": 3,
            "color": "berry_red",
            "is_favorite": true,
            "is_archived": true,
            "is_deleted": true,
            "view_style": "board"
        }))
        .unwrap();

        assert_eq!(project.id.as_str(), "proj-1");
        assert_eq!(project.name, "Groceries");
        assert_eq!(project.parent_id, Some(Id::new("proj-0")));
        assert_eq!(project.child_order, 3);
        assert_eq!(project.color, "berry_red");
        assert!(project.is_favorite);
        assert!(project.is_archived);
        assert!(project.is_deleted);
        assert_eq!(project.view_style, "board");
    }

    #[test]
    fn project_applies_defaults_when_optional_fields_are_missing() {
        // Only the two mandatory fields (`id`, `name`) present — every
        // `#[serde(default)]` field must fall back without erroring.
        let project: Project =
            serde_json::from_value(json!({ "id": "proj-2", "name": "Inbox" })).unwrap();

        assert_eq!(project.id.as_str(), "proj-2");
        assert_eq!(project.name, "Inbox");
        assert_eq!(project.parent_id, None);
        assert_eq!(project.child_order, 0);
        assert_eq!(project.color, "");
        assert!(!project.is_favorite);
        assert!(!project.is_archived);
        assert!(!project.is_deleted);
        assert_eq!(project.view_style, "");
    }

    #[test]
    fn project_accepts_unknown_fields() {
        // Additive Sync API changes (here: an unknown key whose value is
        // `null`) must not break deserialisation.
        let project: Project = serde_json::from_value(json!({
            "id": "proj-3",
            "name": "Work",
            "x": null
        }))
        .unwrap();

        assert_eq!(project.name, "Work");
    }

    #[test]
    fn project_is_deleted_accepts_json_booleans() {
        for (raw, expected) in [(json!(true), true), (json!(false), false)] {
            let project: Project = serde_json::from_value(json!({
                "id": "proj-4",
                "name": "Legacy",
                "is_deleted": raw
            }))
            .unwrap();
            assert_eq!(project.is_deleted, expected, "fixture value {raw}");
        }
    }

    #[test]
    fn project_is_deleted_accepts_legacy_integers() {
        for (raw, expected) in [(json!(1), true), (json!(0), false)] {
            let project: Project = serde_json::from_value(json!({
                "id": "proj-5",
                "name": "Legacy",
                "is_deleted": raw
            }))
            .unwrap();
            assert_eq!(project.is_deleted, expected, "fixture value {raw}");
        }
    }

    #[test]
    fn project_is_deleted_rejects_out_of_range_integers() {
        // `flexible_bool` only accepts the legacy `0`/`1` encoding (plus
        // real JSON booleans); any other integer is a decode error rather
        // than being silently coerced.
        let result: Result<Project, _> = serde_json::from_value(json!({
            "id": "proj-6",
            "name": "Legacy",
            "is_deleted": 7
        }));

        assert!(result.is_err());
    }

    #[test]
    fn project_reserialised_defaults_can_be_read_back() {
        // Serialising a struct built from a minimal payload emits every field
        // explicitly (including `"parent_id": null`); that denser form must be
        // readable again rather than tripping over the explicit null.
        let sparse: Project =
            serde_json::from_value(json!({ "id": "proj-9", "name": "Sparse" })).unwrap();
        let dense = serde_json::to_value(&sparse).unwrap();
        assert_eq!(dense["parent_id"], json!(null));

        let reparsed: Project = serde_json::from_value(dense).unwrap();
        assert_eq!(reparsed.parent_id, None);
        assert_eq!(reparsed.name, "Sparse");
    }

    #[test]
    fn project_round_trips_through_json() {
        let original = json!({
            "id": "proj-7",
            "name": "Round trip",
            "parent_id": "proj-6",
            "child_order": 1,
            "color": "charcoal",
            "is_favorite": false,
            "is_archived": false,
            "is_deleted": false,
            "view_style": "list"
        });

        let project: Project = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&project).unwrap(), original);
    }

    #[test]
    fn project_serialises_legacy_integer_is_deleted_as_a_boolean() {
        // Reading the legacy `0`/`1` encoding must not leak back out on the
        // wire: serialisation always emits a JSON boolean.
        let project: Project = serde_json::from_value(json!({
            "id": "proj-8",
            "name": "Legacy",
            "is_deleted": 1
        }))
        .unwrap();

        let serialised = serde_json::to_value(&project).unwrap();
        assert_eq!(serialised["is_deleted"], json!(true));
    }
}
