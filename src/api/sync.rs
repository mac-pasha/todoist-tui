//! Sync API request/response envelope types (T-205, EPIC-03).
//!
//! [`SyncRequest`] is the body sent to `POST /sync`; [`SyncResponse`] is
//! the shape of its reply. Both are data-only (no HTTP, no retry logic —
//! that's T-206/later tasks); this module just owns the wire format so
//! the client can serialise/deserialise without guessing at field names.
//!
//! The eight resource types requested by a full sync are a hard
//! acceptance item for this epic, so they live in a single
//! [`RESOURCE_TYPES`] const that both [`SyncRequest::full`] and this
//! module's unit test read from, keeping them from drifting apart.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{Filter, Id, Item, Label, Note, Project, Section, User};

/// The eight Sync API resource types requested by a full sync, in the
/// exact order the Sync API docs list them.
pub const RESOURCE_TYPES: [&str; 8] = [
    "projects",
    "items",
    "sections",
    "labels",
    "notes",
    "filters",
    "user",
    "day_orders",
];

/// Body of a `POST /sync` request.
///
/// `commands` is omitted from the serialised body entirely when empty
/// (`skip_serializing_if`), since the Sync API treats a present-but-empty
/// `commands` array and an absent one identically but an absent field
/// keeps request bodies smaller and request logs cleaner.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SyncRequest {
    /// `"*"` for a full sync, or a token returned by a previous sync
    /// response for an incremental sync.
    pub sync_token: String,

    /// Which resource types to fetch. Empty for a pure command-submission
    /// request (no incremental resource pull).
    #[serde(default)]
    pub resource_types: Vec<String>,

    /// Write commands to submit alongside (or instead of) a resource
    /// pull. Omitted from the wire body entirely when empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub commands: Vec<Value>,
}

impl SyncRequest {
    /// Builds a full-sync request: `sync_token` is `"*"` and
    /// `resource_types` is exactly [`RESOURCE_TYPES`], no commands.
    pub fn full() -> Self {
        Self {
            sync_token: "*".to_string(),
            resource_types: RESOURCE_TYPES.iter().map(ToString::to_string).collect(),
            commands: Vec::new(),
        }
    }

    /// Builds an incremental-sync request from a caller-supplied sync
    /// token (as returned by a previous [`SyncResponse::sync_token`]),
    /// requesting the same eight resource types, no commands.
    pub fn incremental(sync_token: impl Into<String>) -> Self {
        Self {
            sync_token: sync_token.into(),
            resource_types: RESOURCE_TYPES.iter().map(ToString::to_string).collect(),
            commands: Vec::new(),
        }
    }
}

/// Per-command outcome reported in [`SyncResponse::sync_status`] when a
/// submitted command fails.
///
/// Kept tolerant (unknown fields ignored, nothing `deny_unknown_fields`)
/// since the exact error payload shape varies by command/error type.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
pub struct CommandError {
    /// Machine-readable error code, e.g. `"PROJECT_NOT_FOUND"`.
    #[serde(default)]
    pub error_code: Option<i64>,

    /// Human-readable error message.
    #[serde(default)]
    pub error: Option<String>,

    /// Any additional fields the Sync API attaches to the error, kept
    /// verbatim rather than dropped.
    #[serde(flatten)]
    pub extra: HashMap<String, Value>,
}

/// Outcome of a single submitted command, keyed by `command.uuid` under
/// [`SyncResponse::sync_status`].
///
/// The Sync API reports success as the literal string `"ok"` and failure
/// as a JSON object describing the error; both forms must deserialise
/// without error.
#[derive(Debug, Clone, PartialEq, Deserialize, Serialize)]
#[serde(untagged)]
pub enum CommandStatus {
    /// The command succeeded (`"ok"` on the wire).
    Ok(String),
    /// The command failed; the object describes why.
    Error(CommandError),
}

/// Body of a `POST /sync` response.
///
/// Every resource array defaults to empty so a response that omits a
/// resource (e.g. because it wasn't requested, or nothing changed since
/// `sync_token`) deserialises without error. `user` is optional for the
/// same reason: it's absent unless `"user"` was requested.
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct SyncResponse {
    /// Token to pass as `sync_token` on the next incremental sync.
    pub sync_token: String,

    /// `true` if this response represents a full sync (either because a
    /// full sync was requested, or the server decided the client's
    /// previous `sync_token` was stale).
    #[serde(default)]
    pub full_sync: bool,

    /// Maps client-generated temporary ids (used in submitted commands)
    /// to the permanent ids the server assigned.
    #[serde(default)]
    pub temp_id_mapping: HashMap<String, Id>,

    /// Per-command outcomes, keyed by the command's `uuid`. Absent when
    /// no commands were submitted.
    #[serde(default)]
    pub sync_status: HashMap<String, CommandStatus>,

    /// UTC timestamp of the last full sync, present only on full-sync
    /// responses.
    #[serde(default)]
    pub full_sync_date_utc: Option<String>,

    #[serde(default)]
    pub projects: Vec<Project>,

    #[serde(default)]
    pub items: Vec<Item>,

    #[serde(default)]
    pub sections: Vec<Section>,

    #[serde(default)]
    pub labels: Vec<Label>,

    #[serde(default)]
    pub notes: Vec<Note>,

    #[serde(default)]
    pub filters: Vec<Filter>,

    #[serde(default)]
    pub user: Option<User>,

    /// Map of project id -> ordering value. The exact value shape isn't
    /// pinned down by the Sync API docs, so this stays a tolerant
    /// `serde_json::Map`-backed value rather than a guessed struct.
    #[serde(default)]
    pub day_orders: serde_json::Map<String, Value>,
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-205 sync request/response envelopes.
    //!
    //! All tokens, ids and names here are fake fixture data: no real API
    //! token, user id, email address or personal data appears anywhere in
    //! this module.
    //!
    //! The first block of tests was written alongside the implementation
    //! (acceptance-critical assertions); everything below the
    //! `--- hardening` marker is the independent unit-test pass: mutation
    //! style checks, negative cases, round-trips and defaults.
    use super::*;
    use serde_json::json;

    #[test]
    fn full_sync_token_is_star() {
        assert_eq!(SyncRequest::full().sync_token, "*");
    }

    #[test]
    fn full_resource_types_match_const_exactly() {
        let value = serde_json::to_value(SyncRequest::full()).expect("serialise SyncRequest");
        let resource_types = value
            .get("resource_types")
            .expect("resource_types present")
            .as_array()
            .expect("resource_types is an array");

        let expected: Vec<Value> = RESOURCE_TYPES
            .iter()
            .map(|s| Value::String((*s).to_string()))
            .collect();

        assert_eq!(resource_types, &expected);
        assert_eq!(resource_types.len(), 8);
    }

    #[test]
    fn full_resource_types_are_exactly_eight_known_entries() {
        let value = serde_json::to_value(SyncRequest::full()).expect("serialise SyncRequest");
        let resource_types = value
            .get("resource_types")
            .expect("resource_types present")
            .as_array()
            .expect("resource_types is an array");

        let got: Vec<&str> = resource_types
            .iter()
            .map(|v| v.as_str().expect("entry is a string"))
            .collect();

        assert_eq!(
            got,
            vec![
                "projects",
                "items",
                "sections",
                "labels",
                "notes",
                "filters",
                "user",
                "day_orders",
            ]
        );
    }

    #[test]
    fn commands_omitted_from_body_when_empty() {
        let value = serde_json::to_value(SyncRequest::full()).expect("serialise SyncRequest");
        assert!(
            value.get("commands").is_none(),
            "commands must be omitted when empty, got {value:?}"
        );
    }

    #[test]
    fn commands_present_when_non_empty() {
        let mut request = SyncRequest::full();
        request
            .commands
            .push(serde_json::json!({"type": "item_add"}));
        let value = serde_json::to_value(&request).expect("serialise SyncRequest");
        assert!(value.get("commands").is_some());
    }

    #[test]
    fn incremental_uses_caller_supplied_token() {
        let request = SyncRequest::incremental("abc123");
        assert_eq!(request.sync_token, "abc123");
        assert_eq!(request.resource_types.len(), 8);
        assert_eq!(request.commands.len(), 0);
    }

    #[test]
    fn sync_response_defaults_every_array_to_empty_and_user_is_optional() {
        let response: SyncResponse =
            serde_json::from_str(r#"{"sync_token": "abc", "full_sync": true}"#)
                .expect("deserialise minimal SyncResponse");

        assert_eq!(response.sync_token, "abc");
        assert!(response.full_sync);
        assert_eq!(response.projects.len(), 0);
        assert_eq!(response.items.len(), 0);
        assert_eq!(response.sections.len(), 0);
        assert_eq!(response.labels.len(), 0);
        assert_eq!(response.notes.len(), 0);
        assert_eq!(response.filters.len(), 0);
        assert!(response.user.is_none());
        assert_eq!(response.day_orders.len(), 0);
        assert_eq!(response.sync_status.len(), 0);
        assert_eq!(response.temp_id_mapping.len(), 0);
        assert!(response.full_sync_date_utc.is_none());
    }

    #[test]
    fn sync_status_deserialises_ok_string_form() {
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "abc",
                "sync_status": {"11eb4-1234": "ok"}
            }"#,
        )
        .expect("deserialise SyncResponse with ok sync_status");

        match response.sync_status.get("11eb4-1234") {
            Some(CommandStatus::Ok(s)) => assert_eq!(s, "ok"),
            other => panic!("expected CommandStatus::Ok, got {other:?}"),
        }
    }

    #[test]
    fn sync_status_deserialises_error_object_form() {
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "abc",
                "sync_status": {
                    "11eb4-1234": {
                        "error_code": 404,
                        "error": "Project not found",
                        "http_code": 404
                    }
                }
            }"#,
        )
        .expect("deserialise SyncResponse with error sync_status");

        match response.sync_status.get("11eb4-1234") {
            Some(CommandStatus::Error(err)) => {
                assert_eq!(err.error_code, Some(404));
                assert_eq!(err.error.as_deref(), Some("Project not found"));
            }
            other => panic!("expected CommandStatus::Error, got {other:?}"),
        }
    }

    #[test]
    fn unknown_top_level_fields_are_ignored() {
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "abc",
                "some_future_field": {"nested": true},
                "another_one": [1, 2, 3]
            }"#,
        )
        .expect("unknown top-level fields must not fail deserialisation");

        assert_eq!(response.sync_token, "abc");
    }

    // --- hardening: shared helpers ---------------------------------------

    /// Fake, obviously-not-real sync token (never a real API token).
    const FAKE_TOKEN: &str = "fake-sync-token-0001";

    /// Compile-time proof that the envelope types derive the traits the
    /// rest of the client relies on (`Debug`, `Clone`, `Serialize`,
    /// `Deserialize`); `SyncResponse` must additionally be `Default`
    /// (built field-by-field in later tasks).
    fn assert_envelope_traits<
        T: std::fmt::Debug + Clone + Serialize + serde::de::DeserializeOwned,
    >() {
    }

    /// The `resource_types` array of a serialised request, as `&str`s.
    fn serialised_resource_types(request: &SyncRequest) -> Vec<String> {
        let value = serde_json::to_value(request).expect("serialise SyncRequest");
        value
            .get("resource_types")
            .expect("resource_types present")
            .as_array()
            .expect("resource_types is an array")
            .iter()
            .map(|v| v.as_str().expect("entry is a string").to_owned())
            .collect()
    }

    /// Top-level keys of a serialised request, sorted so the assertion does
    /// not depend on field declaration order.
    fn sorted_request_keys(request: &SyncRequest) -> Vec<String> {
        let value = serde_json::to_value(request).expect("serialise SyncRequest");
        let mut keys: Vec<String> = value
            .as_object()
            .expect("request serialises to a JSON object")
            .keys()
            .cloned()
            .collect();
        keys.sort();
        keys
    }

    // --- hardening: request serialisation --------------------------------

    #[test]
    fn full_request_serialises_exactly_sync_token_and_resource_types() {
        // With no commands the body is exactly two keys: no `commands`,
        // no stray debug/helper fields leaking onto the wire.
        assert_eq!(
            sorted_request_keys(&SyncRequest::full()),
            vec!["resource_types", "sync_token"]
        );
    }

    #[test]
    fn full_request_resource_types_have_no_duplicates_or_empty_entries() {
        let types = serialised_resource_types(&SyncRequest::full());

        let mut unique = types.clone();
        unique.sort();
        unique.dedup();
        assert_eq!(
            unique.len(),
            types.len(),
            "duplicate resource type in {types:?}"
        );

        for entry in &types {
            assert!(!entry.is_empty(), "empty resource type in {types:?}");
            assert!(
                !entry.contains(' '),
                "resource type {entry:?} must not contain spaces"
            );
        }
    }

    #[test]
    fn resource_type_const_is_lowercase_and_matches_documented_order() {
        // Guards the const itself (not just `full()`): every entry is a
        // lowercase ASCII resource name, and the order is the documented
        // one, so reordering the const is a deliberate, visible change.
        for entry in RESOURCE_TYPES {
            assert_ne!(entry, "");
            assert_eq!(entry, entry.to_lowercase(), "{entry:?} must be lowercase");
            assert!(
                entry.chars().all(|c| c.is_ascii_lowercase() || c == '_'),
                "{entry:?} must be snake_case ASCII"
            );
        }

        assert_eq!(
            RESOURCE_TYPES,
            [
                "projects",
                "items",
                "sections",
                "labels",
                "notes",
                "filters",
                "user",
                "day_orders",
            ],
            "RESOURCE_TYPES order/content must match the acceptance list exactly"
        );
    }

    #[test]
    fn full_and_incremental_request_the_identical_resource_type_list() {
        assert_eq!(
            serialised_resource_types(&SyncRequest::full()),
            serialised_resource_types(&SyncRequest::incremental(FAKE_TOKEN))
        );
    }

    #[test]
    fn full_request_round_trips_through_json_string() {
        let request = SyncRequest::full();
        let wire = serde_json::to_string(&request).expect("serialise SyncRequest");
        let back: SyncRequest = serde_json::from_str(&wire).expect("deserialise SyncRequest");

        assert_eq!(back, request);
        assert_eq!(back.sync_token, "*");
        assert_eq!(back.resource_types.len(), 8);
    }

    #[test]
    fn full_is_idempotent_and_equal_on_repeated_calls() {
        let first = SyncRequest::full();
        let second = SyncRequest::full();
        assert_eq!(first, second);

        // `full()` must not have leaked state into the const (e.g. by
        // mutating a shared buffer): the const is still pristine.
        assert_eq!(RESOURCE_TYPES[0], "projects");
        assert_eq!(RESOURCE_TYPES[7], "day_orders");
    }

    #[test]
    fn commands_key_disappears_again_after_clearing() {
        let mut request = SyncRequest::full();
        request
            .commands
            .push(json!({"type": "item_add", "uuid": "cmd-1"}));
        assert_eq!(
            sorted_request_keys(&request),
            vec!["commands", "resource_types", "sync_token"]
        );

        request.commands.clear();
        assert_eq!(
            sorted_request_keys(&request),
            vec!["resource_types", "sync_token"]
        );
    }

    #[test]
    fn commands_round_trip_preserves_order_and_payloads() {
        let mut request = SyncRequest::full();
        request
            .commands
            .push(json!({"type": "item_add", "uuid": "cmd-1", "temp_id": "tmp-1"}));
        request
            .commands
            .push(json!({"type": "item_update", "uuid": "cmd-2", "args": {"id": "item-1"}}));
        request
            .commands
            .push(json!({"type": "item_delete", "uuid": "cmd-3"}));

        let wire = serde_json::to_string(&request).expect("serialise SyncRequest");
        let back: SyncRequest = serde_json::from_str(&wire).expect("deserialise SyncRequest");

        assert_eq!(back.commands.len(), 3);
        assert_eq!(back.commands[0]["uuid"], json!("cmd-1"));
        assert_eq!(back.commands[1]["uuid"], json!("cmd-2"));
        assert_eq!(back.commands[2]["uuid"], json!("cmd-3"));
        assert_eq!(back.commands[1]["args"]["id"], json!("item-1"));
    }

    #[test]
    fn request_deserialises_without_commands_or_resource_types() {
        // A command-only submission: both optional collections are absent.
        let request: SyncRequest = serde_json::from_str(r#"{"sync_token":"fake-sync-token-0001"}"#)
            .expect("deserialise command-only SyncRequest");

        assert_eq!(request.sync_token, FAKE_TOKEN);
        assert_eq!(request.resource_types, Vec::<String>::new());
        assert_eq!(request.commands, Vec::<Value>::new());
    }

    #[test]
    fn request_requires_sync_token() {
        let err = serde_json::from_str::<SyncRequest>(r#"{"resource_types":["projects"]}"#)
            .expect_err("a request without sync_token must be rejected");
        assert!(
            err.to_string().contains("sync_token"),
            "error should name the missing field, got: {err}"
        );
    }

    #[test]
    fn request_rejects_null_sync_token() {
        // `sync_token` is a non-optional `String`, so an explicit null must
        // be a decode error (serde reports it as a type mismatch; the
        // important part is that it is refused, not defaulted to `"*"`).
        let err = serde_json::from_str::<SyncRequest>(r#"{"sync_token":null}"#)
            .expect_err("sync_token is mandatory and cannot be null");
        assert!(
            err.to_string().contains("invalid type: null"),
            "expected a null type error, got: {err}"
        );
    }

    #[test]
    fn incremental_accepts_owned_and_borrowed_tokens() {
        let owned = SyncRequest::incremental(String::from(FAKE_TOKEN));
        let borrowed = SyncRequest::incremental(FAKE_TOKEN);

        assert_eq!(owned, borrowed);
        assert_eq!(owned.sync_token, FAKE_TOKEN);
    }

    #[test]
    fn incremental_keeps_token_verbatim_without_star_injection() {
        // The caller-supplied token is used as-is: no `"*"` substitution,
        // no trimming, even for a token that already means full sync.
        assert_eq!(SyncRequest::incremental("*").sync_token, "*");
        assert_eq!(SyncRequest::incremental("").sync_token, "");
        assert_eq!(
            SyncRequest::incremental("  spaced-token  ").sync_token,
            "  spaced-token  "
        );
    }

    #[test]
    fn incremental_request_has_no_commands_and_round_trips() {
        let request = SyncRequest::incremental(FAKE_TOKEN);
        assert_eq!(request.commands, Vec::<Value>::new());
        assert_eq!(
            sorted_request_keys(&request),
            vec!["resource_types", "sync_token"]
        );

        let wire = serde_json::to_string(&request).expect("serialise SyncRequest");
        let back: SyncRequest = serde_json::from_str(&wire).expect("deserialise SyncRequest");
        assert_eq!(back, request);
    }

    // --- hardening: response deserialisation -----------------------------

    /// A response carrying every documented field, used by the populated
    /// decode + round-trip tests. Fake ids/names/emails only.
    const POPULATED_RESPONSE: &str = r#"
        {
            "sync_token": "fake-sync-token-0002",
            "full_sync": true,
            "temp_id_mapping": {"tmp-1": "item-1"},
            "sync_status": {
                "cmd-1": "ok",
                "cmd-2": {"error_code": 32, "error": "item does not exist"}
            },
            "full_sync_date_utc": "2024-05-01T10:00:00Z",
            "projects": [{"id": "proj-1", "name": "Inbox"}],
            "items": [{
                "id": "item-1",
                "content": "Buy milk",
                "project_id": "proj-1",
                "child_order": 1,
                "priority": 2,
                "added_at": "2024-05-01T10:00:00Z",
                "user_id": "user-1"
            }],
            "sections": [{"id": "sec-1", "name": "Today", "project_id": "proj-1"}],
            "labels": [{"id": "label-1", "name": "errand"}],
            "notes": [{
                "id": "note-1",
                "item_id": "item-1",
                "content": "from the corner shop",
                "posted_at": "2024-05-01T10:00:00Z"
            }],
            "filters": [{"id": "filter-1", "name": "Today's errands"}],
            "user": {"id": "user-1", "full_name": "Ada Example", "email": "ada@example.test"},
            "day_orders": {"proj-1": 1}
        }
    "#;

    #[test]
    fn sync_response_deserialises_every_documented_field() {
        let response: SyncResponse =
            serde_json::from_str(POPULATED_RESPONSE).expect("deserialise populated SyncResponse");

        assert_eq!(response.sync_token, "fake-sync-token-0002");
        assert!(response.full_sync);
        assert_eq!(
            response.full_sync_date_utc.as_deref(),
            Some("2024-05-01T10:00:00Z")
        );

        assert_eq!(
            response.temp_id_mapping.get("tmp-1"),
            Some(&Id::from("item-1"))
        );
        assert_eq!(response.sync_status.len(), 2);

        assert_eq!(response.projects.len(), 1);
        assert_eq!(response.projects[0].name, "Inbox");
        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].content, "Buy milk");
        assert_eq!(response.sections.len(), 1);
        assert_eq!(response.sections[0].name, "Today");
        assert_eq!(response.labels.len(), 1);
        assert_eq!(response.labels[0].name, "errand");
        assert_eq!(response.notes.len(), 1);
        assert_eq!(response.notes[0].content, "from the corner shop");
        assert_eq!(response.filters.len(), 1);
        assert_eq!(response.filters[0].name, "Today's errands");

        let user = response.user.expect("user must decode when present");
        assert_eq!(user.full_name, "Ada Example");
        assert_eq!(response.day_orders.get("proj-1"), Some(&json!(1)));
    }

    #[test]
    fn resource_arrays_use_their_own_domain_types() {
        // Type-level pin per array: assigning the field to a local of the
        // expected type fails to compile if any array is wired to the
        // wrong resource struct (e.g. `sections: Vec<Project>`).
        let response: SyncResponse =
            serde_json::from_str(POPULATED_RESPONSE).expect("decode populated response");

        let projects: &Vec<Project> = &response.projects;
        let items: &Vec<Item> = &response.items;
        let sections: &Vec<Section> = &response.sections;
        let labels: &Vec<Label> = &response.labels;
        let notes: &Vec<Note> = &response.notes;
        let filters: &Vec<Filter> = &response.filters;
        let user: &Option<User> = &response.user;
        let day_orders: &serde_json::Map<String, Value> = &response.day_orders;

        assert_eq!(projects.len(), 1);
        assert_eq!(items.len(), 1);
        assert_eq!(sections.len(), 1);
        assert_eq!(labels.len(), 1);
        assert_eq!(notes.len(), 1);
        assert_eq!(filters.len(), 1);
        assert!(user.is_some());
        assert_eq!(day_orders.len(), 1);

        // Field accesses unique to each resource type, proving the arrays
        // really decoded into distinct structs rather than one shared
        // shape: `view_style` only exists on `Project`, `priority` on
        // `Item`, `section_order` on `Section`, `item_order` on
        // `Label`/`Filter`, `posted_at` on `Note`, `tz_info` on `User`.
        assert_eq!(projects[0].view_style, "");
        assert_eq!(items[0].priority.get(), 2);
        assert_eq!(sections[0].section_order, 0);
        assert_eq!(labels[0].item_order, 0);
        assert_eq!(notes[0].posted_at, "2024-05-01T10:00:00Z");
        assert_eq!(filters[0].item_order, 0);
        assert_ne!(user.as_ref().expect("user present").full_name, "");
    }

    #[test]
    fn sync_response_serialises_all_eight_resource_keys() {
        // Cross-check between the request and the envelope: every resource
        // type the client requests is a field the envelope can carry.
        let value = serde_json::to_value(SyncResponse::default()).expect("serialise SyncResponse");
        let object = value.as_object().expect("response serialises to an object");

        for resource_type in RESOURCE_TYPES {
            assert!(
                object.contains_key(resource_type),
                "SyncResponse has no `{resource_type}` field for a requested resource"
            );
        }
        assert_eq!(
            object.len(),
            13,
            "unexpected envelope field set: {object:?}"
        );
    }

    #[test]
    fn sync_response_explicit_empty_collections_match_defaults() {
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "fake-sync-token-0003",
                "projects": [], "items": [], "sections": [], "labels": [],
                "notes": [], "filters": [], "temp_id_mapping": {},
                "sync_status": {}, "day_orders": {}
            }"#,
        )
        .expect("explicit empty collections must decode");

        assert!(response.projects.is_empty(), "projects should be empty");
        assert!(response.items.is_empty(), "items should be empty");
        assert!(response.sections.is_empty(), "sections should be empty");
        assert!(response.labels.is_empty(), "labels should be empty");
        assert!(response.notes.is_empty(), "notes should be empty");
        assert!(response.filters.is_empty(), "filters should be empty");
        assert!(response.temp_id_mapping.is_empty(), "temp_id_mapping empty");
        assert!(response.sync_status.is_empty(), "sync_status empty");
        assert!(response.day_orders.is_empty(), "day_orders empty");
        assert!(response.user.is_none(), "user should be absent");
    }

    #[test]
    fn sync_response_full_sync_defaults_false_and_date_is_none() {
        let response: SyncResponse = serde_json::from_str(r#"{"sync_token":"fake-token"}"#)
            .expect("minimal response must decode");

        assert!(!response.full_sync, "full_sync must default to false");
        assert!(
            response.full_sync_date_utc.is_none(),
            "no date on incremental"
        );
    }

    #[test]
    fn sync_response_user_null_is_treated_as_absent() {
        // The Sync API omits `user` unless requested; an explicit null is
        // tolerated because the field is an `Option`.
        let response: SyncResponse = serde_json::from_str(r#"{"sync_token":"t","user":null}"#)
            .expect("null user must decode");
        assert!(response.user.is_none());
    }

    #[test]
    fn sync_response_requires_sync_token() {
        // Without a new token the client cannot sync incrementally again,
        // so a body that omits it is a protocol error, not a default.
        let err = serde_json::from_str::<SyncResponse>(r#"{"full_sync":true}"#)
            .expect_err("response without sync_token must be rejected");
        assert!(
            err.to_string().contains("sync_token"),
            "error should name sync_token, got: {err}"
        );
    }

    #[test]
    fn unknown_nested_and_top_level_fields_are_ignored() {
        // Additive API changes must not break decoding: unknown keys at the
        // envelope level and inside resource objects are both accepted.
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "fake-token",
                "brand_new_envelope_field": {"nested": [1, 2, 3]},
                "projects": [{"id": "proj-1", "name": "Inbox", "shiny_new_field": true}],
                "user": {"id": "user-1", "full_name": "Ada Example", "new_flag": "x"}
            }"#,
        )
        .expect("unknown fields must not fail deserialisation");

        assert_eq!(response.projects.len(), 1);
        assert_eq!(response.projects[0].name, "Inbox");
        assert!(response.user.is_some());
    }

    #[test]
    fn day_orders_tolerates_scalar_and_object_values() {
        // The value shape is not pinned down by the API docs, so the map
        // must keep whatever the server sends instead of guessing.
        let response: SyncResponse = serde_json::from_str(
            r#"{"sync_token":"t","day_orders":{"proj-1":1,"proj-2":{"order":2}}}"#,
        )
        .expect("heterogeneous day_orders must decode");

        assert_eq!(response.day_orders.get("proj-1"), Some(&json!(1)));
        assert_eq!(
            response.day_orders.get("proj-2"),
            Some(&json!({"order": 2}))
        );
    }

    #[test]
    fn sync_response_round_trips_through_json_value() {
        let response: SyncResponse =
            serde_json::from_str(POPULATED_RESPONSE).expect("deserialise populated SyncResponse");
        let value = serde_json::to_value(&response).expect("serialise SyncResponse");
        let reparsed: SyncResponse =
            serde_json::from_value(value.clone()).expect("re-deserialise SyncResponse");
        let value2 = serde_json::to_value(&reparsed).expect("re-serialise SyncResponse");

        assert_eq!(value, value2, "SyncResponse must be JSON-stable");
        assert_eq!(reparsed.sync_token, response.sync_token);
        assert_eq!(reparsed.projects.len(), 1);
        assert_eq!(reparsed.sync_status.len(), 2);
        assert_eq!(reparsed.temp_id_mapping.len(), 1);
    }

    #[test]
    fn sync_status_any_other_string_decodes_as_ok_variant() {
        // Documented leniency: the `Ok` variant is a bare `String`, so
        // non-`"ok"` strings decode as `Ok(<value>)` rather than failing.
        // Pinned here so a future switch to a strict `Ok` marker is a
        // deliberate, test-visible change (see bug notes in the task
        // report: callers must compare against `"ok"`, not just match on
        // the variant).
        let response: SyncResponse =
            serde_json::from_str(r#"{"sync_token":"t","sync_status":{"cmd-1":"weird-value"}}"#)
                .expect("any string form must decode");
        assert_eq!(
            response.sync_status.get("cmd-1"),
            Some(&CommandStatus::Ok("weird-value".to_string()))
        );
    }

    #[test]
    fn sync_status_null_value_is_rejected() {
        // `null` inside the status map matches neither variant, so it must
        // surface as an error instead of silently becoming "success".
        assert!(
            serde_json::from_str::<SyncResponse>(
                r#"{"sync_token":"t","sync_status":{"cmd-1":null}}"#
            )
            .is_err(),
            "null sync_status entry must be rejected"
        );
    }

    #[test]
    fn null_resource_arrays_are_rejected_rather_than_silently_emptied() {
        // `#[serde(default)]` only covers *absent* keys; an explicit null
        // is a type error. Pinned so the behaviour is intentional and
        // visible if the API ever sends nulls (see bug notes).
        for field in [
            "projects",
            "items",
            "sections",
            "labels",
            "notes",
            "filters",
            "temp_id_mapping",
            "sync_status",
            "day_orders",
        ] {
            let body = format!(r#"{{"sync_token":"t","{field}":null}}"#);
            assert!(
                serde_json::from_str::<SyncResponse>(&body).is_err(),
                "explicit null for `{field}` should be a decode error, body: {body}"
            );
        }
    }

    #[test]
    fn response_arrays_are_independent_of_one_another() {
        // A response that only carries `items` must not populate the other
        // resource arrays (guards against a copy-paste default mixup).
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "t",
                "items": [{
                    "id": "item-1",
                    "content": "Buy milk",
                    "project_id": "proj-1",
                    "child_order": 1,
                    "priority": 1,
                    "added_at": "2024-05-01T10:00:00Z",
                    "user_id": "user-1"
                }]
            }"#,
        )
        .expect("items-only response must decode");

        assert_eq!(response.items.len(), 1);
        assert_eq!(response.items[0].id, Id::from("item-1"));
        assert!(response.projects.is_empty(), "projects must stay empty");
        assert!(response.sections.is_empty(), "sections must stay empty");
        assert!(response.labels.is_empty(), "labels must stay empty");
        assert!(response.notes.is_empty(), "notes must stay empty");
        assert!(response.filters.is_empty(), "filters must stay empty");
        assert!(response.user.is_none(), "user must stay absent");
        assert!(response.day_orders.is_empty(), "day_orders must stay empty");
    }

    #[test]
    fn temp_id_mapping_maps_temp_ids_to_real_ids() {
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "t",
                "temp_id_mapping": {"tmp-item-1": "item-1", "tmp-project-1": "proj-1"}
            }"#,
        )
        .expect("temp_id_mapping must decode");

        assert_eq!(response.temp_id_mapping.len(), 2);
        assert_eq!(
            response.temp_id_mapping.get("tmp-item-1"),
            Some(&Id::from("item-1"))
        );
        assert_eq!(
            response.temp_id_mapping.get("tmp-project-1"),
            Some(&Id::from("proj-1"))
        );
        assert!(!response.temp_id_mapping.contains_key("tmp-item-2"));
    }

    #[test]
    fn full_request_wire_shape_is_pinned_exactly() {
        // Lock the full-sync body: a single JSON object of exactly two
        // keys, in a stable shape, so accidental field additions or
        // renames show up as a test failure rather than on the wire.
        let wire = serde_json::to_string(&SyncRequest::full()).expect("serialise SyncRequest");
        assert_eq!(
            wire,
            concat!(
                r#"{"sync_token":"*","resource_types":["projects","items","sections","#,
                r#""labels","notes","filters","user","day_orders"]}"#
            )
        );
    }

    // --- hardening: sync_status / CommandStatus ---------------------------

    #[test]
    fn sync_status_mixes_ok_and_error_entries_in_one_map() {
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "t",
                "sync_status": {
                    "cmd-ok": "ok",
                    "cmd-bad": {"error_code": 32, "error": "item does not exist"}
                }
            }"#,
        )
        .expect("mixed sync_status must decode");

        assert_eq!(
            response.sync_status.get("cmd-ok"),
            Some(&CommandStatus::Ok("ok".to_string()))
        );
        match response.sync_status.get("cmd-bad") {
            Some(CommandStatus::Error(err)) => {
                assert_eq!(err.error_code, Some(32));
                assert_eq!(err.error.as_deref(), Some("item does not exist"));
                assert!(err.extra.is_empty(), "no extra fields expected");
            }
            other => panic!("expected CommandStatus::Error, got {other:?}"),
        }
    }

    #[test]
    fn sync_status_error_object_preserves_extra_fields_verbatim() {
        let response: SyncResponse = serde_json::from_str(
            r#"{
                "sync_token": "t",
                "sync_status": {
                    "cmd-bad": {
                        "error_code": 15,
                        "error": "invalid argument",
                        "error_extra": {"argument": "due_string"},
                        "error_tag": "ARGUMENT_INVALID"
                    }
                }
            }"#,
        )
        .expect("error object with extra fields must decode");

        match response.sync_status.get("cmd-bad") {
            Some(CommandStatus::Error(err)) => {
                assert_eq!(err.error_code, Some(15));
                assert_eq!(err.extra.get("error_tag"), Some(&json!("ARGUMENT_INVALID")));
                assert_eq!(
                    err.extra.get("error_extra"),
                    Some(&json!({"argument": "due_string"}))
                );
            }
            other => panic!("expected CommandStatus::Error, got {other:?}"),
        }
    }

    #[test]
    fn sync_status_error_object_without_known_fields_still_decodes() {
        // Tolerance check: an empty/unknown-only error payload must not
        // abort the whole sync response.
        let response: SyncResponse =
            serde_json::from_str(r#"{"sync_token":"t","sync_status":{"cmd-1":{}}}"#)
                .expect("empty error object must decode");

        match response.sync_status.get("cmd-1") {
            Some(CommandStatus::Error(err)) => {
                assert!(err.error_code.is_none(), "error_code should be None");
                assert!(err.error.is_none(), "error should be None");
                assert!(err.extra.is_empty(), "extra should be empty");
            }
            other => panic!("expected CommandStatus::Error, got {other:?}"),
        }
    }

    #[test]
    fn sync_status_absent_versus_empty_object_are_equivalent() {
        let absent: SyncResponse =
            serde_json::from_str(r#"{"sync_token":"t"}"#).expect("absent sync_status must decode");
        let empty: SyncResponse = serde_json::from_str(r#"{"sync_token":"t","sync_status":{}}"#)
            .expect("empty sync_status must decode");

        assert!(absent.sync_status.is_empty());
        assert!(empty.sync_status.is_empty());
    }

    #[test]
    fn sync_status_rejects_values_that_are_neither_string_nor_object() {
        // Negative case: the untagged enum only accepts the two documented
        // forms, so a number/boolean/array must be a decode error rather
        // than a silently-wrong `Ok(<something>)`.
        for body in [
            r#"{"sync_token":"t","sync_status":{"cmd-1":1}}"#,
            r#"{"sync_token":"t","sync_status":{"cmd-1":true}}"#,
            r#"{"sync_token":"t","sync_status":{"cmd-1":["ok"]}}"#,
        ] {
            assert!(
                serde_json::from_str::<SyncResponse>(body).is_err(),
                "expected a decode error for sync_status value in {body}"
            );
        }
    }

    #[test]
    fn command_status_ok_serialises_as_bare_string_and_round_trips() {
        let ok = CommandStatus::Ok("ok".to_string());
        let wire = serde_json::to_string(&ok).expect("serialise CommandStatus::Ok");
        assert_eq!(
            wire, "\"ok\"",
            "success must go back on the wire as a bare string"
        );
        let back: CommandStatus = serde_json::from_str(&wire).expect("deserialise CommandStatus");
        assert_eq!(back, ok);
    }

    #[test]
    fn command_error_round_trips_and_keeps_extra_fields() {
        let error = CommandError {
            error_code: Some(32),
            error: Some("item does not exist".to_string()),
            extra: HashMap::from([("error_tag".to_string(), json!("NOT_FOUND"))]),
        };
        let status = CommandStatus::Error(error.clone());
        let wire = serde_json::to_string(&status).expect("serialise CommandStatus::Error");
        let back: CommandStatus = serde_json::from_str(&wire).expect("deserialise CommandStatus");
        assert_eq!(back, status);
    }

    // --- hardening: derives and `api::mod` wiring -------------------------

    #[test]
    fn envelope_types_derive_required_traits() {
        assert_envelope_traits::<SyncRequest>();
        assert_envelope_traits::<SyncResponse>();
        assert_envelope_traits::<CommandError>();
        assert_envelope_traits::<CommandStatus>();

        // `SyncResponse` must be constructible empty for later tasks that
        // build it incrementally from parts.
        let empty = SyncResponse::default();
        assert_eq!(empty.sync_token, "");
        assert!(!empty.full_sync);
        assert!(
            matches!(empty.projects.as_slice(), []),
            "projects must default empty"
        );

        // `Clone` really copies: mutating the clone leaves the original alone.
        let mut request = SyncRequest::full();
        request.commands.push(json!({"type": "item_add"}));
        let mut cloned = request.clone();
        cloned.commands.clear();
        assert_eq!(request.commands.len(), 1);
        assert_eq!(cloned.commands, Vec::<Value>::new());
    }

    #[test]
    fn api_module_reexports_the_envelope_types() {
        // Acceptance item 6: `src/api/mod.rs` declares `pub mod sync;`,
        // `pub mod model;` and re-exports the envelope types. Importing
        // them via `crate::api::` only compiles if all of that holds.
        use crate::api::model::Id as ReexportedId;
        use crate::api::{
            sync as sync_module, CommandError as ReexportedCommandError,
            CommandStatus as ReexportedCommandStatus, SyncRequest as ReexportedSyncRequest,
            SyncResponse as ReexportedSyncResponse, RESOURCE_TYPES as REexportedResourceTypes,
        };

        let request = ReexportedSyncRequest::full();
        assert_eq!(request.sync_token, "*");
        assert_eq!(REexportedResourceTypes.len(), 8);
        assert_eq!(sync_module::RESOURCE_TYPES, REexportedResourceTypes);

        let response = ReexportedSyncResponse::default();
        assert!(response.user.is_none());

        let status = ReexportedCommandStatus::Error(ReexportedCommandError {
            error_code: None,
            error: None,
            extra: HashMap::new(),
        });
        assert!(matches!(status, ReexportedCommandStatus::Error(_)));
        assert_eq!(ReexportedId::from("proj-1").as_str(), "proj-1");
    }
}
