//! `Filter` resource shape from the Todoist Sync API (T-203, EPIC-03).
//!
//! Data only: no persistence, no business rules (EPIC-04 owns storage).
//! Unknown fields are accepted silently (no `deny_unknown_fields`) so
//! additive API changes deserialise without a code change, matching the
//! convention set by `common.rs` (T-202).

use serde::{Deserialize, Serialize};

use super::common::Id;
use super::flexible_bool;

/// A Todoist filter, as returned by the Sync API's `filters` resource.
///
/// Filters have no `priority` field in the Sync API, so [`super::Priority`]
/// is intentionally not used here.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Filter {
    /// The filter's unique id.
    pub id: Id,
    /// The filter's display name.
    pub name: String,
    /// The filter's query string (Todoist filter syntax).
    #[serde(default)]
    pub query: String,
    /// The filter's colour, as a Todoist colour name (e.g. `"berry_red"`).
    #[serde(default)]
    pub color: String,
    /// Position among other filters (used for manual ordering).
    #[serde(default)]
    pub item_order: i64,
    /// Whether the filter is marked as a favorite.
    #[serde(default)]
    pub is_favorite: bool,
    /// Whether the filter has been deleted.
    ///
    /// Tolerates both JSON booleans and the Sync API's legacy `0`/`1`
    /// integer encoding (T-203).
    #[serde(default, deserialize_with = "flexible_bool")]
    pub is_deleted: bool,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-203 `Filter` resource shape. All ids, names and
    //! queries are fake fixture data; no real credentials or personal data.
    use super::*;
    use serde_json::json;

    /// Compile-time proof that `Filter` derives the four traits the task
    /// requires (`Debug, Clone, Serialize, Deserialize`).
    fn assert_required_traits<
        T: std::fmt::Debug + Clone + Serialize + serde::de::DeserializeOwned,
    >() {
    }

    #[test]
    fn filter_derives_debug_clone_serialize_deserialize() {
        assert_required_traits::<Filter>();

        let filter: Filter =
            serde_json::from_value(json!({ "id": "fil-1", "name": "Overdue" })).unwrap();
        let cloned = filter.clone();
        assert_eq!(cloned.name, "Overdue");
        assert!(format!("{filter:?}").contains("Overdue"));
    }

    #[test]
    fn filter_deserializes_name_query_order_colour_and_deleted_fields() {
        let filter: Filter = serde_json::from_value(json!({
            "id": "fil-1",
            "name": "Overdue",
            "query": "overdue & 7 days",
            "color": "berry_red",
            "item_order": 2,
            "is_favorite": true,
            "is_deleted": true
        }))
        .unwrap();

        assert_eq!(filter.id.as_str(), "fil-1");
        assert_eq!(filter.name, "Overdue");
        assert_eq!(filter.query, "overdue & 7 days");
        assert_eq!(filter.color, "berry_red");
        assert_eq!(filter.item_order, 2);
        assert!(filter.is_favorite);
        assert!(filter.is_deleted);
    }

    #[test]
    fn filter_applies_defaults_when_optional_fields_are_missing() {
        let filter: Filter =
            serde_json::from_value(json!({ "id": "fil-2", "name": "No query" })).unwrap();

        assert_eq!(filter.query, "");
        assert_eq!(filter.color, "");
        assert_eq!(filter.item_order, 0);
        assert!(!filter.is_favorite);
        assert!(!filter.is_deleted);
    }

    #[test]
    fn filter_accepts_unknown_fields() {
        let filter: Filter = serde_json::from_value(json!({
            "id": "fil-3",
            "name": "Work only",
            "x": null
        }))
        .unwrap();

        assert_eq!(filter.name, "Work only");
    }

    #[test]
    fn filter_is_deleted_accepts_json_booleans() {
        for (raw, expected) in [(json!(true), true), (json!(false), false)] {
            let filter: Filter = serde_json::from_value(json!({
                "id": "fil-4",
                "name": "Legacy",
                "is_deleted": raw
            }))
            .unwrap();
            assert_eq!(filter.is_deleted, expected, "fixture value {raw}");
        }
    }

    #[test]
    fn filter_is_deleted_accepts_legacy_integers() {
        for (raw, expected) in [(json!(1), true), (json!(0), false)] {
            let filter: Filter = serde_json::from_value(json!({
                "id": "fil-5",
                "name": "Legacy",
                "is_deleted": raw
            }))
            .unwrap();
            assert_eq!(filter.is_deleted, expected, "fixture value {raw}");
        }
    }

    #[test]
    fn filter_round_trips_through_json() {
        let original = json!({
            "id": "fil-6",
            "name": "Round trip",
            "query": "@errand",
            "color": "charcoal",
            "item_order": 0,
            "is_favorite": false,
            "is_deleted": false
        });

        let filter: Filter = serde_json::from_value(original.clone()).unwrap();
        assert_eq!(serde_json::to_value(&filter).unwrap(), original);
    }
}
