//! The Todoist Sync API `note` resource (T-204, EPIC-03).
//!
//! Notes are comments attached to an item or a project. As with every
//! other model in this module, unknown JSON fields never cause a decode
//! error, and the nested `file_attachment` object is itself tolerant of
//! whatever additive shape the API sends.

use serde::{Deserialize, Serialize};

use super::common::Id;
use super::flexible_bool;

/// A comment ("note" in Sync API terminology) on an item or project.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Note {
    pub id: Id,

    #[serde(default)]
    pub item_id: Option<Id>,

    #[serde(default)]
    pub project_id: Option<Id>,

    pub content: String,

    pub posted_at: String,

    #[serde(rename = "is_deleted", deserialize_with = "flexible_bool", default)]
    pub is_deleted: bool,

    #[serde(default)]
    pub file_attachment: Option<FileAttachment>,
}

/// A file attached to a [`Note`].
///
/// Deliberately tolerant: every field beyond `file_name` is optional so
/// that additive API changes (new attachment metadata keys) deserialize
/// without a code change.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileAttachment {
    #[serde(default)]
    pub file_name: Option<String>,

    #[serde(default)]
    pub file_size: Option<i64>,

    #[serde(default)]
    pub file_type: Option<String>,

    #[serde(default)]
    pub file_url: Option<String>,

    #[serde(default)]
    pub resource_type: Option<String>,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-204 `Note` / `FileAttachment` domain structs.
    //!
    //! All data here is fake fixture data: no real ids, tokens, URLs or
    //! personal information.
    use super::*;
    use serde_json::{json, Value};

    /// The smallest JSON body the Sync API can send for a note.
    fn minimal_note_json() -> Value {
        json!({
            "id": "6X7rM8997g3RNot1",
            "content": "Remember to check the fridge",
            "posted_at": "2024-05-01T10:00:00Z"
        })
    }

    /// Merges `fields` over [`minimal_note_json`] and parses the result.
    fn note_from_json(fields: Value) -> Result<Note, serde_json::Error> {
        let mut base = minimal_note_json();
        let object = base.as_object_mut().unwrap();
        if let Value::Object(fields) = fields {
            for (key, value) in fields {
                object.insert(key, value);
            }
        }
        serde_json::from_value(base)
    }

    // --- Full payload --------------------------------------------------

    #[test]
    fn note_deserialises_every_listed_field_with_attachment() {
        let original = json!({
            "id": "6X7rM8997g3RNot1",
            "item_id": "6X7rM8997g3RItm1",
            "project_id": "6X7rM8997g3RPrj1",
            "content": "Remember to check the fridge",
            "posted_at": "2024-05-01T10:00:00Z",
            "is_deleted": false,
            "file_attachment": {
                "file_name": "shopping.txt",
                "file_size": 1024,
                "file_type": "text/plain",
                "file_url": "https://example.invalid/f/shopping.txt",
                "resource_type": "file"
            }
        });
        let note = serde_json::from_value::<Note>(original.clone()).unwrap();

        assert_eq!(note.id.as_str(), "6X7rM8997g3RNot1");
        assert_eq!(
            note.item_id.as_ref().map(Id::as_str),
            Some("6X7rM8997g3RItm1")
        );
        assert_eq!(
            note.project_id.as_ref().map(Id::as_str),
            Some("6X7rM8997g3RPrj1")
        );
        assert_eq!(note.content, "Remember to check the fridge");
        assert_eq!(note.posted_at, "2024-05-01T10:00:00Z");
        assert!(!note.is_deleted);

        let attachment = note
            .file_attachment
            .as_ref()
            .expect("attachment is present");
        assert_eq!(attachment.file_name.as_deref(), Some("shopping.txt"));
        assert_eq!(attachment.file_size, Some(1024));
        assert_eq!(attachment.file_type.as_deref(), Some("text/plain"));
        assert_eq!(
            attachment.file_url.as_deref(),
            Some("https://example.invalid/f/shopping.txt")
        );
        assert_eq!(attachment.resource_type.as_deref(), Some("file"));

        // No field loss in either direction.
        assert_eq!(serde_json::to_value(&note).unwrap(), original);
    }

    #[test]
    fn note_full_payload_round_trips_without_field_loss() {
        let original = json!({
            "id": "6X7rM8997g3RNot1",
            "item_id": null,
            "project_id": "6X7rM8997g3RPrj1",
            "content": "Remember to check the fridge",
            "posted_at": "2024-05-01T10:00:00Z",
            "is_deleted": true,
            "file_attachment": null
        });
        let note = serde_json::from_value::<Note>(original.clone()).unwrap();

        assert_eq!(note.item_id, None);
        assert!(note.is_deleted);
        assert_eq!(note.file_attachment, None);
        assert_eq!(serde_json::to_value(&note).unwrap(), original);
    }

    // --- Defaults ------------------------------------------------------

    #[test]
    fn note_optional_fields_default_when_absent() {
        let note = note_from_json(json!({})).unwrap();

        assert_eq!(note.item_id, None);
        assert_eq!(note.project_id, None);
        assert_eq!(note.file_attachment, None);
        assert!(!note.is_deleted);
    }

    // --- Optional file_attachment tolerance ----------------------------

    #[test]
    fn note_null_file_attachment_deserialises_to_none() {
        let note = note_from_json(json!({ "file_attachment": null })).unwrap();

        assert_eq!(note.file_attachment, None);
    }

    #[test]
    fn note_partial_file_attachment_fills_missing_fields_with_none() {
        // Additive/partial attachments must not fail to decode.
        let note = note_from_json(json!({
            "file_attachment": { "file_name": "shopping.txt" }
        }))
        .unwrap();

        let attachment = note.file_attachment.as_ref().unwrap();
        assert_eq!(attachment.file_name.as_deref(), Some("shopping.txt"));
        assert_eq!(attachment.file_size, None);
        assert_eq!(attachment.file_type, None);
        assert_eq!(attachment.file_url, None);
        assert_eq!(attachment.resource_type, None);
    }

    #[test]
    fn note_file_attachment_tolerates_unknown_keys() {
        let note = note_from_json(json!({
            "file_attachment": {
                "file_name": "shopping.txt",
                "file_size": 2048,
                "image_width": 640,
                "upload_state": "completed"
            }
        }))
        .unwrap();

        let attachment = note.file_attachment.as_ref().unwrap();
        assert_eq!(attachment.file_name.as_deref(), Some("shopping.txt"));
        assert_eq!(attachment.file_size, Some(2048));
    }

    // --- Flexible booleans ---------------------------------------------

    #[test]
    fn note_is_deleted_accepts_bool_and_integer_forms() {
        for (raw, expected) in [
            (json!(true), true),
            (json!(false), false),
            (json!(1), true),
            (json!(0), false),
        ] {
            let label = raw.to_string();
            let note = note_from_json(json!({ "is_deleted": raw })).unwrap();
            assert_eq!(note.is_deleted, expected, "is_deleted = {label}");
        }
    }

    #[test]
    fn note_rejects_is_deleted_values_that_are_neither_bool_nor_0_or_1() {
        for raw in [json!(2), json!(-1), json!(42)] {
            let label = raw.to_string();
            assert!(
                note_from_json(json!({ "is_deleted": raw })).is_err(),
                "is_deleted = {label} must be rejected"
            );
        }
    }

    // --- Unknown-field tolerance ---------------------------------------

    #[test]
    fn note_ignores_unknown_fields() {
        let note = note_from_json(json!({
            "reactions": {"total_count": 3},
            "attachment_count": 1,
            "posted_uid": "6X7rM8997g3RUsr1",
            "file_attachment": {
                "file_name": "shopping.txt",
                "upload_state": "completed"
            }
        }))
        .unwrap();

        assert_eq!(note.content, "Remember to check the fridge");
        assert_eq!(
            note.file_attachment.as_ref().unwrap().file_name.as_deref(),
            Some("shopping.txt")
        );
    }

    // --- mod.rs wiring -------------------------------------------------

    #[test]
    fn note_and_attachment_are_reexported_from_model_root() {
        // Compile-time proof that `src/api/model/mod.rs` re-exports both
        // types under the expected names (acceptance item 6).
        let _: crate::api::model::Note = note_from_json(json!({})).unwrap();
        let _: crate::api::model::FileAttachment = FileAttachment {
            file_name: None,
            file_size: None,
            file_type: None,
            file_url: None,
            resource_type: None,
        };
    }

    #[test]
    fn note_module_is_declared_in_model_root() {
        // Compile-time proof for acceptance item 6: `pub mod note;`.
        let note = note_from_json(json!({})).unwrap();
        let _: crate::api::model::note::Note = note;
        let _: crate::api::model::note::FileAttachment = FileAttachment {
            file_name: None,
            file_size: None,
            file_type: None,
            file_url: None,
            resource_type: None,
        };
    }

    #[test]
    fn note_rejects_a_payload_missing_a_mandatory_field() {
        for mandatory in ["id", "content", "posted_at"] {
            let mut payload = minimal_note_json();
            payload.as_object_mut().unwrap().remove(mandatory);
            assert!(
                serde_json::from_value::<Note>(payload).is_err(),
                "missing mandatory field `{mandatory}` must be a decode error"
            );
        }
    }
}
