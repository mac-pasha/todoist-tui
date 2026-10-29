//! `Label` resource shape from the Todoist Sync API (T-203, EPIC-03).
//!
//! Data only: no persistence, no business rules (EPIC-04 owns storage).
//! Unknown fields are accepted silently (no `deny_unknown_fields`) so
//! additive API changes deserialise without a code change, matching the
//! convention set by `common.rs` (T-202).

use serde::{Deserialize, Serialize};

use super::common::Id;
use super::flexible_bool;

/// A Todoist label, as returned by the Sync API's `labels` resource.
///
/// Labels have no `priority` field in the Sync API, so [`super::Priority`]
/// is intentionally not used here.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Label {
    /// The label's unique id.
    pub id: Id,
    /// The label's display name.
    pub name: String,
    /// The label's colour, as a Todoist colour name (e.g. `"berry_red"`).
    #[serde(default)]
    pub color: String,
    /// Position among other labels (used for manual ordering).
    #[serde(default)]
    pub item_order: i64,
    /// Whether the label is marked as a favorite.
    #[serde(default)]
    pub is_favorite: bool,
    /// Whether the label has been deleted.
    ///
    /// Tolerates both JSON booleans and the Sync API's legacy `0`/`1`
    /// integer encoding (T-203).
    #[serde(default, deserialize_with = "flexible_bool")]
    pub is_deleted: bool,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-203 `Label` resource shape. All ids, names and
    //! colours are fake fixture data; no real credentials or personal data.
    use super::*;
    use serde_json::json;

    /// Compile-time proof that `Label` derives the four traits the task
    /// requires (`Debug, Clone, Serialize, Deserialize`).
    fn assert_required_traits<
        T: std::fmt::Debug + Clone + Serialize + serde::de::DeserializeOwned,
    >() {
    }

    #[test]
    fn label_derives_debug_clone_serialize_deserialize() {
        assert_required_traits::<Label>();

        let label: Label =
            serde_json::from_value(json!({ "id": "lab-1", "name": "errand" })).unwrap();
        let cloned = label.clone();
        assert_eq!(cloned.name, "errand");
        assert!(format!("{label:?}").contains("errand"));
    }

    #[test]
    fn label_deserializes_name_order_colour_and_deleted_fields() {
        let label: Label = serde_json::from_value(json!({
            "id": "lab-1",
            "name": "errand",
            "color": "berry_red",
            "item_order": 4,
            "is_favorite": true,
            "is_deleted": true
        }))
        .unwrap();

        assert_eq!(label.id.as_str(), "lab-1");
        assert_eq!(label.name, "errand");
        assert_eq!(label.color, "berry_red");
        assert_eq!(label.item_order, 4);
        assert!(label.is_favorite);
        assert!(label.is_deleted);
    }

    #[test]
    fn label_applies_defaults_when_optional_fields_are_missing() {
        let label: Label =
            serde_json::from_value(json!({ "id": "lab-2", "name": "home" })).unwrap();

        assert_eq!(label.color, "");
        assert_eq!(label.item_order, 0);
        assert!(!label.is_favorite);
        assert!(!label.is_deleted);
    }

    #[test]
    fn label_accepts_unknown_fields() {
        let label: Label = serde_json::from_value(json!({
            "id": "lab-3",
            "name": "work",
            "x": null
        }))
        .unwrap();

        assert_eq!(label.name, "work");
    }

    #[test]
    fn label_is_deleted_accepts_json_booleans() {
        for (raw, expected) in [(json!(true), true), (json!(false), false)] {
            let label: Label = serde_json::from_value(json!({
                "id": "lab-4",
                "name": "Legacy",
                "is_deleted": raw
            }))
            .unwrap();
            assert_eq!(label.is_deleted, expected, "fixture value {raw}");
        }
    }

    #[test]
    fn label_is_deleted_accepts_legacy_integers() {
        for (raw, expected) in [(json!(1), true), (json!(0), false)] {
            let label: Label = serde_json::from_value(json!({
                "id": "lab-5",
                "name": "Legacy",
                "is_deleted": raw
            }))
            .unwrap();
            assert_eq!(label.is_deleted, expected, "fixture value {raw}");
        }
    }

    #[test]
    fn label_round_trips_through_json() {
        let original = json!({
            "id": "lab-6",
            "name": "Round trip",
            "color": "charcoal",
            "item_order": 0,
            "is_favorite": false,
            "is_deleted": false
        });

        let label: Label = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&label).unwrap(), original);
    }
}
