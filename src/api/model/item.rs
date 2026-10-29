//! The Todoist Sync API `item` resource (T-204, EPIC-03).
//!
//! `Item` is the hottest struct in the app — it is kept `Clone` and cheap
//! (no owned client handles, no interior mutability) so it can be freely
//! copied around the TUI's render/update loop.
//!
//! As with the scalar types in [`super::common`], no struct here sets
//! `deny_unknown_fields`: unknown keys on the wire must not break
//! deserialization.

use serde::{Deserialize, Serialize};

use super::common::{Deadline, Due, Id, Priority};
use super::flexible_bool;

/// A single Todoist task ("item" in Sync API terminology).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Item {
    pub id: Id,

    pub content: String,

    /// Markdown-ish free-text body. Empty string when the item has no
    /// description, never absent from the wire, but kept `#[serde(default)]`
    /// defensively.
    #[serde(default)]
    pub description: String,

    pub project_id: Id,

    #[serde(default)]
    pub section_id: Option<Id>,

    #[serde(default)]
    pub parent_id: Option<Id>,

    pub child_order: i64,

    pub priority: Priority,

    /// Label names attached to the item. Absent on the wire for items with
    /// no labels, so this defaults to an empty `Vec` rather than failing.
    #[serde(default)]
    pub labels: Vec<String>,

    #[serde(default)]
    pub due: Option<Due>,

    #[serde(default)]
    pub deadline: Option<Deadline>,

    #[serde(deserialize_with = "flexible_bool", default)]
    pub checked: bool,

    #[serde(rename = "is_deleted", deserialize_with = "flexible_bool", default)]
    pub is_deleted: bool,

    pub added_at: String,

    #[serde(default)]
    pub completed_at: Option<String>,

    pub user_id: Id,

    #[serde(default)]
    pub responsible_uid: Option<Id>,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-204 `Item` domain struct.
    //!
    //! All data here is fake fixture data: no real ids, tokens or personal
    //! information. Every test deserialises from `serde_json::Value` so the
    //! assertions read like the wire format the Sync API actually sends.
    use super::*;
    use serde_json::{json, Value};

    /// The smallest JSON body the Sync API can send for an item: only the
    /// fields `Item` declares as mandatory. Everything else is absent so the
    /// `#[serde(default)]` paths get exercised.
    fn minimal_item_json() -> Value {
        json!({
            "id": "6X7rM8997g3RQmvh",
            "content": "Buy milk",
            "project_id": "6X7rM8997g3RQmvh",
            "child_order": 1,
            "priority": 2,
            "added_at": "2024-05-01T10:00:00Z",
            "user_id": "6X7rM8997g3RQmvh"
        })
    }

    /// Merges `fields` over [`minimal_item_json`] and parses the result.
    fn item_from_json(fields: Value) -> Result<Item, serde_json::Error> {
        let mut base = minimal_item_json();
        let object = base.as_object_mut().unwrap();
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                object.insert(key, value);
            }
        }
        serde_json::from_value(base)
    }

    /// A fully-populated item: every field from the T-204 acceptance list,
    /// a complete recurring `due` object (all five `Due` fields) and a
    /// `deadline` object.
    fn full_item_json() -> Value {
        json!({
            "id": "6X7rM8997g3RQmvh",
            "content": "Buy milk",
            "description": "2 litres, semi-skimmed",
            "project_id": "6X7rM8997g3RPrj1",
            "section_id": "6X7rM8997g3RSec1",
            "parent_id": "6X7rM8997g3RPar1",
            "child_order": 42,
            "priority": 4,
            "labels": ["errand", "home"],
            "due": {
                "date": "2024-05-01T10:00:00Z",
                "string": "every day",
                "lang": "en",
                "is_recurring": true,
                "timezone": "Europe/Berlin"
            },
            "deadline": { "date": "2024-05-03", "lang": "en" },
            "checked": true,
            "is_deleted": false,
            "added_at": "2024-05-01T10:00:00Z",
            "completed_at": "2024-05-02T09:30:00Z",
            "user_id": "6X7rM8997g3RUsr1",
            "responsible_uid": "6X7rM8997g3RUsr2"
        })
    }

    // --- Full payload / no field loss ----------------------------------

    #[test]
    fn item_deserialises_every_listed_field() {
        let item = serde_json::from_value::<Item>(full_item_json()).unwrap();

        assert_eq!(item.id.as_str(), "6X7rM8997g3RQmvh");
        assert_eq!(item.content, "Buy milk");
        assert_eq!(item.description, "2 litres, semi-skimmed");
        assert_eq!(item.project_id.as_str(), "6X7rM8997g3RPrj1");
        assert_eq!(
            item.section_id.as_ref().map(Id::as_str),
            Some("6X7rM8997g3RSec1")
        );
        assert_eq!(
            item.parent_id.as_ref().map(Id::as_str),
            Some("6X7rM8997g3RPar1")
        );
        assert_eq!(item.child_order, 42);
        assert_eq!(item.priority.get(), 4);
        assert_eq!(item.labels, vec!["errand".to_owned(), "home".to_owned()]);
        assert!(item.checked);
        assert!(!item.is_deleted);
        assert_eq!(item.added_at, "2024-05-01T10:00:00Z");
        assert_eq!(item.completed_at.as_deref(), Some("2024-05-02T09:30:00Z"));
        assert_eq!(item.user_id.as_str(), "6X7rM8997g3RUsr1");
        assert_eq!(
            item.responsible_uid.as_ref().map(Id::as_str),
            Some("6X7rM8997g3RUsr2")
        );
    }

    #[test]
    fn item_keeps_every_field_of_a_recurring_due_object() {
        let item = serde_json::from_value::<Item>(full_item_json()).unwrap();

        let due = item
            .due
            .as_ref()
            .expect("full payload carries a due object");
        assert_eq!(due.date, "2024-05-01T10:00:00Z");
        assert_eq!(due.string.as_deref(), Some("every day"));
        assert_eq!(due.lang.as_deref(), Some("en"));
        assert!(due.is_recurring);
        assert_eq!(due.timezone.as_deref(), Some("Europe/Berlin"));

        let deadline = item
            .deadline
            .as_ref()
            .expect("full payload carries a deadline");
        assert_eq!(deadline.date, "2024-05-03");
        assert_eq!(deadline.lang.as_deref(), Some("en"));
    }

    #[test]
    fn item_full_payload_round_trips_without_field_loss() {
        let original = full_item_json();
        let item = serde_json::from_value::<Item>(original.clone()).unwrap();

        let reparsed = serde_json::to_value(&item).unwrap();
        assert_eq!(reparsed, original);
    }

    #[test]
    fn item_with_null_due_deserialises_and_round_trips() {
        // Wire shape of a task with no date: explicit JSON nulls for every
        // optional field, none of which may be dropped or rejected.
        let original = json!({
            "id": "6X7rM8997g3RQmvh",
            "content": "Buy milk",
            "description": "",
            "project_id": "6X7rM8997g3RPrj1",
            "section_id": null,
            "parent_id": null,
            "child_order": 3,
            "priority": 1,
            "labels": [],
            "due": null,
            "deadline": null,
            "checked": false,
            "is_deleted": false,
            "added_at": "2024-05-01T10:00:00Z",
            "completed_at": null,
            "user_id": "6X7rM8997g3RUsr1",
            "responsible_uid": null
        });
        let item = serde_json::from_value::<Item>(original.clone()).unwrap();

        assert_eq!(item.due, None);
        assert_eq!(item.deadline, None);
        assert_eq!(item.section_id, None);
        assert_eq!(item.parent_id, None);
        assert_eq!(item.completed_at, None);
        assert_eq!(item.responsible_uid, None);
        // Nothing else may be lost on the way in or out.
        assert_eq!(item.content, "Buy milk");
        assert_eq!(item.child_order, 3);
        assert_eq!(item.priority.get(), 1);
        assert_eq!(item.labels, Vec::<String>::new());
        assert_eq!(serde_json::to_value(&item).unwrap(), original);
    }

    // --- Defaults ------------------------------------------------------

    #[test]
    fn item_labels_default_to_empty_when_key_is_absent() {
        let item = item_from_json(json!({})).unwrap();

        assert_eq!(item.labels, Vec::<String>::new());
        assert_eq!(item.labels.len(), 0);
        assert_eq!(serde_json::to_value(&item).unwrap()["labels"], json!([]));
    }

    #[test]
    fn item_optional_fields_default_when_absent() {
        let item = item_from_json(json!({})).unwrap();

        assert_eq!(item.description, "");
        assert_eq!(item.section_id, None);
        assert_eq!(item.parent_id, None);
        assert_eq!(item.due, None);
        assert_eq!(item.deadline, None);
        assert!(!item.checked);
        assert!(!item.is_deleted);
        assert_eq!(item.completed_at, None);
        assert_eq!(item.responsible_uid, None);
    }

    // --- Flexible booleans ---------------------------------------------

    #[test]
    fn item_checked_accepts_bool_and_integer_forms() {
        for (raw, expected) in [
            (json!(true), true),
            (json!(false), false),
            (json!(1), true),
            (json!(0), false),
        ] {
            let label = raw.to_string();
            let item = item_from_json(json!({ "checked": raw })).unwrap();
            assert_eq!(item.checked, expected, "checked = {label}");
        }
    }

    #[test]
    fn item_is_deleted_accepts_bool_and_integer_forms() {
        for (raw, expected) in [
            (json!(true), true),
            (json!(false), false),
            (json!(1), true),
            (json!(0), false),
        ] {
            let label = raw.to_string();
            let item = item_from_json(json!({ "is_deleted": raw })).unwrap();
            assert_eq!(item.is_deleted, expected, "is_deleted = {label}");
        }
    }

    #[test]
    fn item_rejects_boolean_flags_that_are_neither_bool_nor_0_or_1() {
        for raw in [json!(2), json!(-1), json!(42)] {
            let label = raw.to_string();
            assert!(
                item_from_json(json!({ "checked": raw })).is_err(),
                "checked = {label} must be rejected"
            );
        }
        for raw in [json!(2), json!(-1), json!(42)] {
            let label = raw.to_string();
            assert!(
                item_from_json(json!({ "is_deleted": raw })).is_err(),
                "is_deleted = {label} must be rejected"
            );
        }
    }

    // --- Unknown-field tolerance ---------------------------------------

    #[test]
    fn item_ignores_unknown_fields() {
        let item = item_from_json(json!({
            "sync_extra_field": {"nested": [1, 2, 3]},
            "day_order": -1,
            "assigned_by_uid": null,
            "due": {
                "date": "2024-05-01",
                "is_recurring": false,
                "added_by_uid": "6X7rM8997g3RUsr9"
            },
            "deadline": { "date": "2024-05-03", "unexpected_key": "ignored" }
        }))
        .unwrap();

        // Known fields still land, unknown ones are simply dropped.
        assert_eq!(item.content, "Buy milk");
        assert_eq!(item.due.as_ref().unwrap().date, "2024-05-01");
        assert_eq!(item.deadline.as_ref().unwrap().date, "2024-05-03");
    }

    // --- Boundary / wiring ---------------------------------------------

    #[test]
    fn item_priority_boundaries_are_validated_through_the_item_struct() {
        // `Priority` itself is covered by common.rs; this asserts the
        // validation actually applies to an `Item` deserialised off the wire.
        for raw in [1, 2, 3, 4] {
            let item = item_from_json(json!({ "priority": raw })).unwrap();
            assert_eq!(item.priority.get(), raw);
        }
        for raw in [0, 5, 255] {
            let label = raw.to_string();
            assert!(
                item_from_json(json!({ "priority": raw })).is_err(),
                "priority = {label} must be rejected"
            );
        }
    }

    #[test]
    fn item_is_reexported_from_model_root_and_item_module_is_declared() {
        // Compile-time proof for acceptance item 6: `src/api/model/mod.rs`
        // declares `pub mod item;` and re-exports `Item`.
        let item = item_from_json(json!({})).unwrap();
        let _: crate::api::model::Item = item.clone();
        let _: crate::api::model::item::Item = item;
    }

    #[test]
    fn item_rejects_a_payload_missing_a_mandatory_field() {
        // Tolerant about *unknown* and *optional* keys, strict about the
        // required ones — a silent default here would corrupt the domain
        // model rather than merely ignore an additive API change.
        for mandatory in [
            "id",
            "content",
            "project_id",
            "child_order",
            "priority",
            "added_at",
            "user_id",
        ] {
            let mut payload = full_item_json();
            payload.as_object_mut().unwrap().remove(mandatory);
            assert!(
                serde_json::from_value::<Item>(payload).is_err(),
                "missing mandatory field `{mandatory}` must be a decode error"
            );
        }
    }
}
