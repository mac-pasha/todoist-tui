//! Fixture-backed deserialisation tests for the EPIC-03 Sync API response
//! envelope (T-208).
//!
//! `tests/fixtures/sync_full.json` is a hand-written, obviously-fake
//! `POST /api/v1/sync` full-sync reply. These tests decode it into the real
//! `SyncResponse` / model types and assert, field by field, that nothing the
//! epic's acceptance list cares about is lost (`is_deleted`, `checked`,
//! `due`, `deadline`, `priority`, `labels`, `parent_id`, `section_id`,
//! `child_order`), that keys the model doesn't know about are tolerated at
//! three different nesting levels, and that re-serialising the decoded
//! response changes no asserted value.
//!
//! No network access: the fixture is compiled into the test binary with
//! `include_str!`, so the suite behaves identically offline.
//!
//! ## Why the `#[path]` includes below
//!
//! `todoist-tui` is a binary-only crate (a single `[[bin]]`, no `[lib]`), so
//! an integration test cannot `use todoist_tui::api::SyncResponse`. The two
//! files this suite needs are therefore compiled into this test binary via
//! `#[path]`, unmodified, so the assertions run against the real types:
//!
//! * `src/api/sync.rs` — `SyncResponse`, `SyncRequest`, `CommandStatus`,
//!   `CommandError`, `RESOURCE_TYPES`.
//! * `src/api/model/mod.rs` — `Item`, `Project`, `Section`, `Label`, `Note`,
//!   `Filter`, `User`, `Due`, `Deadline`, `Priority`, `Id`.
//!
//! Nothing is re-declared here: a hand-copied struct would keep passing after
//! a source change and defeat the point of a no-field-loss test.
//!
//! The include is deliberately narrow — `src/api/mod.rs` itself is *not*
//! included, only the two modules above. That keeps this suite independent of
//! the HTTP client that lands in `src/api/client.rs` (T-207): a later
//! `pub mod client;` in `src/api/mod.rs` will pull in `crate::auth` and
//! `crate::log`, which this fixture-only binary has no business linking.
//!
//! Two consequences worth knowing:
//!
//! * The `#[cfg(test)]` modules that live *inside* the two included files also
//!   compile and run in this binary, so their unit tests execute a second
//!   time. Harmless — they are pure, no IO — and cheaper than duplicating
//!   types. No source file is edited by this task.
//! * Those unit-test modules refer to their own types as `crate::api::...`, so
//!   the small `mod api` re-export shim below keeps those paths resolvable.
//!
//! ## Fixture provenance
//!
//! The JSON carries a `_fixture` header (JSON has no comments) that names the
//! API version the shape was taken from and the date it was captured; the
//! epic flags fixture drift as a risk, so
//! `fixture_declares_its_api_version_and_shape_date` asserts that header is
//! present and well-formed. All data is fake: ids start with `6X4Vfake`, the
//! only email is `user@example.invalid`, and the sync token starts with
//! `fake`.

#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../src/api/model/mod.rs"]
pub mod model;

#[path = "../src/api/sync.rs"]
pub mod sync;

/// Path shim for the unit-test modules embedded in the two files above: they
/// refer to their own types as `crate::api::...`, so re-exporting the included
/// modules under `api` keeps those paths resolvable without editing `src/`.
/// The binary under test does not use this module; only `#[cfg(test)]` code
/// inside the included files does.
mod api {
    pub(crate) use crate::model;
    pub(crate) use crate::sync;
    pub(crate) use crate::sync::{
        CommandError, CommandStatus, SyncRequest, SyncResponse, RESOURCE_TYPES,
    };
}

use serde_json::Value;

use crate::model::{Deadline, Due, Id, Item, Note, Priority, Project, Section};
use crate::sync::{CommandStatus, SyncResponse, RESOURCE_TYPES};

/// The committed full-sync fixture, compiled in so the tests need no file IO
/// and no network.
fn fixture_text() -> &'static str {
    include_str!("fixtures/sync_full.json")
}

/// The fixture as untyped JSON: used for "does the fixture actually contain
/// the awkward shapes this suite claims to cover" checks and for the
/// unknown-key assertions.
fn raw_fixture() -> Value {
    serde_json::from_str(fixture_text()).unwrap_or_else(|err| panic!("fixture is not JSON: {err}"))
}

/// The fixture decoded into the real envelope type — the thing under test.
fn decoded_fixture() -> SyncResponse {
    serde_json::from_str(fixture_text())
        .unwrap_or_else(|err| panic!("fixture must decode into SyncResponse: {err}"))
}

/// Looks up an item by id, failing loudly if the fixture lost it.
fn item<'a>(response: &'a SyncResponse, id: &str) -> &'a Item {
    response
        .items
        .iter()
        .find(|item| item.id.as_str() == id)
        .unwrap_or_else(|| panic!("fixture is missing item `{id}`"))
}

// --- acceptance 1: the fixture really is a realistic full-sync reply --------

#[test]
fn fixture_covers_every_documented_resource_and_envelope_field() {
    let raw = raw_fixture();

    // At least the resource counts the task asks for.
    assert!(
        raw["projects"].as_array().unwrap().len() >= 2,
        "need >= 2 projects"
    );
    assert!(
        raw["items"].as_array().unwrap().len() >= 3,
        "need >= 3 items"
    );
    assert!(
        !raw["sections"].as_array().unwrap().is_empty(),
        "need a section"
    );
    assert!(
        !raw["labels"].as_array().unwrap().is_empty(),
        "need a label"
    );
    assert!(!raw["notes"].as_array().unwrap().is_empty(), "need a note");
    assert!(
        !raw["filters"].as_array().unwrap().is_empty(),
        "need a filter"
    );

    // The envelope fields: `full_sync: true` plus the four map/date fields.
    assert_eq!(raw["full_sync"], Value::Bool(true));
    for field in [
        "full_sync_date_utc",
        "sync_token",
        "temp_id_mapping",
        "sync_status",
        "day_orders",
        "user",
    ] {
        assert!(!raw[field].is_null(), "fixture must carry `{field}`");
    }

    // Every resource type a full sync requests is present in the response, so
    // the fixture exercises the whole `RESOURCE_TYPES` list rather than a
    // convenient subset.
    for resource in RESOURCE_TYPES {
        assert!(
            raw.get(resource).is_some(),
            "fixture lacks resource `{resource}`"
        );
    }

    // The four item shapes the acceptance list calls out by name.
    let items = raw["items"].as_array().unwrap();
    let recurring = items
        .iter()
        .any(|i| i["due"]["is_recurring"] == Value::Bool(true));
    let deadlined = items.iter().any(|i| i["deadline"].is_object());
    let completed = items.iter().any(|i| i["checked"] == Value::Bool(true));
    let deleted = items.iter().any(|i| i["is_deleted"] == Value::Bool(true));
    assert!(recurring, "need an item with a recurring due");
    assert!(deadlined, "need an item with a deadline");
    assert!(completed, "need a completed/checked item");
    assert!(deleted, "need an is_deleted item");
}

#[test]
fn fixture_declares_its_api_version_and_shape_date() {
    let raw = raw_fixture();
    let header = raw["_fixture"]
        .as_object()
        .expect("fixture needs a `_fixture` header (JSON has no comments)");

    assert_eq!(
        header["api"], "POST /api/v1/sync",
        "header must name the API endpoint"
    );

    // `YYYY-MM-DD`, so a reader can tell how stale the shape is.
    let captured = header["shape_captured"].as_str().unwrap_or_default();
    assert_eq!(
        captured.len(),
        10,
        "`shape_captured` must be a YYYY-MM-DD date, got `{captured}`"
    );
    let digits_and_dashes = captured.chars().all(|c| c.is_ascii_digit() || c == '-');
    assert!(
        digits_and_dashes,
        "`shape_captured` must be a YYYY-MM-DD date, got `{captured}`"
    );
}

// --- acceptance 2: fake data only -----------------------------------------

/// Longest run of consecutive ASCII digits in `text` — used to prove the
/// fixture holds no phone-number-shaped values.
fn longest_digit_run(text: &str) -> usize {
    let mut best = 0;
    let mut run = 0;
    for c in text.chars() {
        if c.is_ascii_digit() {
            run += 1;
            best = best.max(run);
        } else {
            run = 0;
        }
    }
    best
}

/// Every id-looking string the fixture carries: resource ids, the foreign
/// keys between resources, temp-id mapping values and `day_orders` keys.
fn fixture_ids(raw: &Value) -> Vec<String> {
    let mut ids = Vec::new();
    for resource in [
        "projects", "items", "sections", "labels", "notes", "filters",
    ] {
        for object in raw[resource].as_array().unwrap() {
            ids.push(object["id"].as_str().unwrap().to_owned());
            for key in [
                "project_id",
                "section_id",
                "parent_id",
                "item_id",
                "user_id",
                "responsible_uid",
            ] {
                if let Some(value) = object.get(key).and_then(Value::as_str) {
                    ids.push(value.to_owned());
                }
            }
        }
    }
    ids.push(raw["user"]["id"].as_str().unwrap().to_owned());
    let mapping = raw["temp_id_mapping"].as_object().unwrap();
    ids.extend(
        mapping
            .values()
            .map(|value| value.as_str().unwrap().to_owned()),
    );
    ids.extend(raw["day_orders"].as_object().unwrap().keys().cloned());
    ids
}

#[test]
fn fixture_contains_only_obviously_fake_data() {
    let raw = raw_fixture();
    let text = fixture_text();

    // Ids: sync-API-shaped but unmistakably synthetic.
    let ids = fixture_ids(&raw);
    assert!(
        ids.len() >= 20,
        "expected a realistic spread of ids, got {}",
        ids.len()
    );
    for id in &ids {
        assert!(
            id.starts_with("6X4Vfake"),
            "id `{id}` is not obviously synthetic (must start with `6X4Vfake`)"
        );
    }

    // Exactly one email-shaped value, and it is in the reserved `.invalid` TLD.
    assert_eq!(raw["user"]["email"], "user@example.invalid");
    let without_email = text.replace("user@example.invalid", "");
    assert!(
        !without_email.contains('@'),
        "fixture holds an email/`@`handle other than user@example.invalid"
    );

    // No phone-number-shaped digit runs, no real hosts, no credential markers.
    assert!(
        longest_digit_run(text) < 10,
        "fixture holds a phone-number-shaped digit run"
    );
    for marker in [
        "Bearer ",
        "api.todoist.com",
        "todoist.com",
        "sk_live",
        "ghp_",
        "-----BEGIN",
    ] {
        assert!(
            !text.contains(marker),
            "fixture must not contain `{marker}`"
        );
    }

    // The sync token is a fake, not something lifted from a real account.
    let token = raw["sync_token"].as_str().unwrap();
    assert!(
        token.starts_with("fake"),
        "sync_token `{token}` must be obviously fake"
    );
    assert!(
        token.len() >= 16,
        "a realistic sync token is longer than `{token}`"
    );
}

// --- acceptance 3: no field loss, asserted against the real types ---------

#[test]
fn envelope_fields_decode_without_loss() {
    let response = decoded_fixture();

    assert_eq!(response.sync_token, "fakeSyncTokenForT208Fixture0001");
    assert!(response.full_sync, "full_sync must decode to true");
    assert_eq!(
        response.full_sync_date_utc.as_deref(),
        Some("2024-05-01T09:00:00Z")
    );

    assert_eq!(
        response.temp_id_mapping.get("temp-fake-0001"),
        Some(&Id::new("6X4VfakeItm0003"))
    );
    assert_eq!(
        response.day_orders.get("6X4VfakePrj0001"),
        Some(&Value::from(1))
    );
    assert_eq!(
        response.day_orders.get("6X4VfakePrj0002"),
        Some(&Value::from(2))
    );

    // Both `sync_status` wire shapes decode: the `"ok"` string and the
    // per-command error object.
    match response.sync_status.get("cmd-fake-uuid-0001") {
        Some(CommandStatus::Ok(status)) => assert_eq!(status, "ok"),
        other => panic!("expected CommandStatus::Ok, got {other:?}"),
    }
    match response.sync_status.get("cmd-fake-uuid-0002") {
        Some(CommandStatus::Error(error)) => {
            assert_eq!(error.error_code, Some(21));
            assert!(error
                .error
                .as_deref()
                .unwrap_or_default()
                .contains("Fake command failure"));
        }
        other => panic!("expected CommandStatus::Error, got {other:?}"),
    }
}

#[test]
fn projects_and_section_decode_without_field_loss() {
    let response = decoded_fixture();
    assert_eq!(response.projects.len(), 2);
    assert_eq!(response.sections.len(), 1);

    let home: &Project = &response.projects[0];
    assert_eq!(home.id, Id::new("6X4VfakePrj0001"));
    assert_eq!(home.name, "Home");
    assert_eq!(home.parent_id, None);
    assert_eq!(home.child_order, 1);
    assert_eq!(home.color, "berry_red");
    assert!(home.is_favorite);
    assert!(!home.is_archived);
    assert!(!home.is_deleted);
    assert_eq!(home.view_style, "list");

    let work: &Project = &response.projects[1];
    assert_eq!(work.id, Id::new("6X4VfakePrj0002"));
    assert_eq!(work.name, "Work");
    assert_eq!(work.parent_id, Some(Id::new("6X4VfakePrj0001")));
    assert_eq!(work.child_order, 2);
    assert_eq!(work.view_style, "board");

    let section: &Section = &response.sections[0];
    assert_eq!(section.id, Id::new("6X4VfakeSec0001"));
    assert_eq!(section.name, "Errands");
    assert_eq!(section.project_id, Id::new("6X4VfakePrj0001"));
    assert_eq!(section.section_order, 1);
    assert!(!section.is_deleted);
    assert!(!section.is_archived);
}

#[test]
fn recurring_item_keeps_due_priority_labels_section_and_order() {
    let response = decoded_fixture();
    let milk = item(&response, "6X4VfakeItm0001");

    assert_eq!(milk.content, "Buy milk");
    assert_eq!(milk.description, "Two litres, semi-skimmed");
    assert_eq!(milk.project_id, Id::new("6X4VfakePrj0001"));
    // section_id, child_order and priority are all acceptance-listed fields.
    assert_eq!(milk.section_id, Some(Id::new("6X4VfakeSec0001")));
    assert_eq!(milk.parent_id, None);
    assert_eq!(milk.child_order, 1);
    assert_eq!(
        milk.priority,
        Priority::new(4).unwrap(),
        "priority 4 is the highest"
    );
    assert_eq!(milk.labels, vec!["errand".to_owned(), "home".to_owned()]);
    assert!(!milk.checked);
    assert!(!milk.is_deleted);
    assert_eq!(milk.added_at, "2024-04-28T08:15:00Z");
    assert_eq!(milk.completed_at, None);
    assert_eq!(milk.user_id, Id::new("6X4VfakeUsr0001"));
    assert_eq!(milk.deadline, None);

    // The whole recurring `due` object, field by field.
    let due: &Due = milk.due.as_ref().expect("recurring item must keep its due");
    assert_eq!(due.date, "2024-05-06");
    assert_eq!(due.string.as_deref(), Some("every Monday"));
    assert_eq!(due.lang.as_deref(), Some("en"));
    assert!(due.is_recurring, "is_recurring must survive as true");
    assert_eq!(due.timezone, None);
}

#[test]
fn deadline_item_keeps_deadline_parent_and_responsible_user() {
    let response = decoded_fixture();
    let report = item(&response, "6X4VfakeItm0002");

    assert_eq!(report.content, "Draft the quarterly report");
    assert_eq!(
        report.parent_id,
        Some(Id::new("6X4VfakeItm0001")),
        "parent_id must survive"
    );
    assert_eq!(report.section_id, None);
    assert_eq!(report.child_order, 2);
    assert_eq!(report.priority, Priority::new(2).unwrap());
    assert_eq!(report.labels, Vec::<String>::new());
    assert!(!report.checked);
    assert!(!report.is_deleted);
    assert_eq!(report.responsible_uid, Some(Id::new("6X4VfakeUsr0002")));

    let due: &Due = report
        .due
        .as_ref()
        .expect("item must keep its non-recurring due");
    assert_eq!(due.date, "2024-05-20T17:00:00Z");
    assert!(!due.is_recurring);
    assert_eq!(due.timezone.as_deref(), Some("Europe/Lisbon"));

    let deadline: &Deadline = report
        .deadline
        .as_ref()
        .expect("item must keep its deadline");
    assert_eq!(deadline.date, "2024-06-15");
    assert_eq!(deadline.lang.as_deref(), Some("en"));
}

#[test]
fn checked_and_deleted_flags_decode_without_loss() {
    let response = decoded_fixture();

    let call = item(&response, "6X4VfakeItm0003");
    assert!(
        call.checked,
        "checked must decode to true for a completed item"
    );
    assert!(!call.is_deleted);
    assert_eq!(call.completed_at.as_deref(), Some("2024-05-01T08:30:00Z"));
    assert_eq!(call.priority, Priority::new(1).unwrap());
    assert_eq!(call.due, None);
    assert_eq!(call.deadline, None);

    let scratch = item(&response, "6X4VfakeItm0004");
    assert!(
        scratch.is_deleted,
        "is_deleted must decode to true for a deleted item"
    );
    assert!(!scratch.checked);
    assert_eq!(scratch.completed_at, None);
    assert_eq!(scratch.priority, Priority::new(3).unwrap());

    // The two flags are independent, one item each.
    assert_eq!(response.items.iter().filter(|i| i.checked).count(), 1);
    assert_eq!(response.items.iter().filter(|i| i.is_deleted).count(), 1);
}

#[test]
fn labels_filter_note_and_user_decode_without_field_loss() {
    let response = decoded_fixture();

    assert_eq!(response.labels.len(), 2);
    assert_eq!(response.labels[0].name, "errand");
    assert_eq!(response.labels[0].color, "blue");
    assert_eq!(response.labels[0].item_order, 1);
    assert!(response.labels[1].is_favorite);
    assert!(response.labels.iter().all(|label| !label.is_deleted));

    assert_eq!(response.filters.len(), 1);
    let filter = &response.filters[0];
    assert_eq!(filter.name, "Overdue fakes");
    assert_eq!(filter.query, "overdue & #Home");
    assert_eq!(filter.color, "berry_red");
    assert_eq!(filter.item_order, 1);
    assert!(filter.is_favorite);

    assert_eq!(response.notes.len(), 1);
    let note: &Note = &response.notes[0];
    assert_eq!(note.item_id, Some(Id::new("6X4VfakeItm0001")));
    assert_eq!(note.project_id, None);
    assert_eq!(note.posted_at, "2024-04-29T07:00:00Z");
    assert!(!note.is_deleted);
    let attachment = note
        .file_attachment
        .as_ref()
        .expect("note must keep its attachment");
    assert_eq!(attachment.file_name.as_deref(), Some("fake-receipt.pdf"));
    assert_eq!(attachment.file_size, Some(12345));
    assert_eq!(attachment.resource_type.as_deref(), Some("file"));

    let user = response
        .user
        .as_ref()
        .expect("full sync must carry the user object");
    assert_eq!(user.id, Id::new("6X4VfakeUsr0001"));
    assert_eq!(user.full_name, "Ada Example");
    assert_eq!(user.email, "user@example.invalid");
    assert_eq!(user.color, "#ff8581");
    assert_eq!(user.tz_info.timezone, "Europe/Lisbon");
    assert_eq!(user.tz_info.hours, 1);
    assert_eq!(user.tz_info.minutes, 0);
    assert_eq!(user.tz_info.gmt_string, "GMT +01:00");
    assert_eq!(user.tz_info.is_dst, 1);
}

// --- acceptance 4: unknown keys at three nesting levels -------------------

#[test]
fn unknown_keys_at_top_level_and_nested_are_tolerated() {
    let raw = raw_fixture();

    // First prove the fixture actually carries unknown keys, otherwise this
    // test could pass trivially after an "unknown keys removed" edit.
    for key in ["_fixture", "last_write_ts", "live_notifications", "stats"] {
        assert!(
            raw.get(key).is_some(),
            "top-level unknown key `{key}` is missing from the fixture"
        );
    }
    let first_item = &raw["items"][0];
    for key in ["duration", "note_count", "v2_id"] {
        assert!(
            first_item.get(key).is_some(),
            "unknown item key `{key}` is missing"
        );
    }
    for key in ["tz_offset", "next_occurrence_date"] {
        assert!(
            first_item["due"].get(key).is_some(),
            "unknown due key `{key}` is missing"
        );
    }
    assert!(
        raw["user"].get("premium_status").is_some(),
        "unknown user key is missing"
    );

    // Decoding still succeeds, and the known fields around the unknown keys
    // keep their values.
    let response = decoded_fixture();
    let milk = item(&response, "6X4VfakeItm0001");
    assert_eq!(milk.priority, Priority::new(4).unwrap());
    assert_eq!(milk.child_order, 1);
    assert_eq!(milk.due.as_ref().unwrap().date, "2024-05-06");
    assert!(milk.due.as_ref().unwrap().is_recurring);
    assert_eq!(response.sync_token, "fakeSyncTokenForT208Fixture0001");
    assert_eq!(response.projects.len(), 2);
}

// --- acceptance 5: round-trip re-serialisation ---------------------------

/// Every field that must survive decode -> encode unchanged. Only keys the
/// model knows about are listed: unknown keys are dropped on re-serialisation
/// by design (they are tolerated on input, not echoed back), which is why
/// `duration`, `note_count`, `v2_id`, `tz_offset`, `next_occurrence_date`,
/// `inbox_project`, `reactions`, `file_url_expires_at`, `premium_status` and
/// friends are deliberately absent here.
const ROUND_TRIP_POINTERS: &[&str] = &[
    "/sync_token",
    "/full_sync",
    "/full_sync_date_utc",
    "/temp_id_mapping/temp-fake-0001",
    "/day_orders/6X4VfakePrj0001",
    "/day_orders/6X4VfakePrj0002",
    "/sync_status/cmd-fake-uuid-0001",
    "/sync_status/cmd-fake-uuid-0002/error_code",
    "/sync_status/cmd-fake-uuid-0002/error",
    "/projects/0/id",
    "/projects/0/name",
    "/projects/0/parent_id",
    "/projects/0/child_order",
    "/projects/0/color",
    "/projects/0/is_favorite",
    "/projects/0/is_archived",
    "/projects/0/is_deleted",
    "/projects/0/view_style",
    "/projects/1/name",
    "/projects/1/parent_id",
    "/projects/1/child_order",
    "/projects/1/view_style",
    "/sections/0/id",
    "/sections/0/name",
    "/sections/0/project_id",
    "/sections/0/section_order",
    "/sections/0/is_deleted",
    "/sections/0/is_archived",
    "/items/0/id",
    "/items/0/content",
    "/items/0/description",
    "/items/0/project_id",
    "/items/0/section_id",
    "/items/0/parent_id",
    "/items/0/child_order",
    "/items/0/priority",
    "/items/0/labels",
    "/items/0/due/date",
    "/items/0/due/string",
    "/items/0/due/lang",
    "/items/0/due/is_recurring",
    "/items/0/due/timezone",
    "/items/0/deadline",
    "/items/0/checked",
    "/items/0/is_deleted",
    "/items/0/added_at",
    "/items/0/completed_at",
    "/items/0/user_id",
    "/items/0/responsible_uid",
    "/items/1/parent_id",
    "/items/1/child_order",
    "/items/1/priority",
    "/items/1/labels",
    "/items/1/due/date",
    "/items/1/due/is_recurring",
    "/items/1/due/timezone",
    "/items/1/deadline/date",
    "/items/1/deadline/lang",
    "/items/1/responsible_uid",
    "/items/2/child_order",
    "/items/2/priority",
    "/items/2/labels",
    "/items/2/checked",
    "/items/2/is_deleted",
    "/items/2/completed_at",
    "/items/3/child_order",
    "/items/3/priority",
    "/items/3/labels",
    "/items/3/checked",
    "/items/3/is_deleted",
    "/labels/0/id",
    "/labels/0/name",
    "/labels/0/color",
    "/labels/0/item_order",
    "/labels/0/is_favorite",
    "/labels/0/is_deleted",
    "/labels/1/name",
    "/labels/1/is_favorite",
    "/notes/0/id",
    "/notes/0/item_id",
    "/notes/0/project_id",
    "/notes/0/content",
    "/notes/0/posted_at",
    "/notes/0/is_deleted",
    "/notes/0/file_attachment/file_name",
    "/notes/0/file_attachment/file_size",
    "/notes/0/file_attachment/file_type",
    "/notes/0/file_attachment/file_url",
    "/notes/0/file_attachment/resource_type",
    "/filters/0/id",
    "/filters/0/name",
    "/filters/0/query",
    "/filters/0/color",
    "/filters/0/item_order",
    "/filters/0/is_favorite",
    "/filters/0/is_deleted",
    "/user/id",
    "/user/full_name",
    "/user/email",
    "/user/color",
    "/user/tz_info/timezone",
    "/user/tz_info/hours",
    "/user/tz_info/minutes",
    "/user/tz_info/gmt_string",
    "/user/tz_info/is_dst",
];

#[test]
fn round_trip_re_serialisation_changes_no_asserted_field() {
    let raw = raw_fixture();
    let round = serde_json::to_value(decoded_fixture()).expect("re-serialise SyncResponse");

    for pointer in ROUND_TRIP_POINTERS {
        let expected = raw
            .pointer(pointer)
            .unwrap_or_else(|| panic!("fixture is missing JSON pointer `{pointer}`"));
        let actual = round
            .pointer(pointer)
            .unwrap_or_else(|| panic!("round trip dropped `{pointer}`"));
        assert_eq!(
            actual, expected,
            "`{pointer}` changed value through the round trip"
        );
    }

    // Encoding is stable: re-parsing the re-serialised response and encoding
    // it again produces exactly the same JSON.
    let reparsed: SyncResponse =
        serde_json::from_value(round.clone()).expect("round-tripped JSON must re-decode");
    let twice = serde_json::to_value(reparsed).expect("re-serialise twice");
    assert_eq!(twice, round, "round trip is not stable");

    // And the typed view agrees with the raw fixture for every acceptance-
    // listed item field, so the pointer comparison above cannot pass merely
    // because both sides drifted together.
    assert_typed_items_match_raw_fixture(&decoded_fixture());
    assert_eq!(
        raw.pointer("/items/0/id"),
        Some(&Value::from("6X4VfakeItm0001"))
    );
}

/// Cross-checks the typed `Item` view of the acceptance fields
/// (`child_order`, `priority`, `labels`, `parent_id`, `section_id`, `checked`,
/// `is_deleted`, `due`, `deadline`) against the same fields in the raw
/// fixture.
///
/// Only the keys the model actually declares are compared inside `due` /
/// `deadline`: the fixture's extra `due.tz_offset` /
/// `due.next_occurrence_date` keys are unknown on purpose and are not echoed
/// back, so comparing the whole nested object would contradict the
/// unknown-field-tolerance requirement above.
fn assert_typed_items_match_raw_fixture(response: &SyncResponse) {
    let raw = raw_fixture();
    for (index, item) in response.items.iter().enumerate() {
        assert_item_matches_raw(index, item, &raw["items"][index]);
    }
}

/// The per-item half of [`assert_typed_items_match_raw_fixture`]: the eight
/// acceptance-listed item fields plus the nested `due` / `deadline` objects.
fn assert_item_matches_raw(index: usize, item: &Item, raw_item: &Value) {
    assert_eq!(
        Value::from(item.child_order),
        raw_item["child_order"],
        "items[{index}].child_order"
    );
    assert_eq!(
        Value::from(item.priority.get()),
        raw_item["priority"],
        "items[{index}].priority"
    );
    assert_eq!(
        Value::from(item.checked),
        raw_item["checked"],
        "items[{index}].checked"
    );
    assert_eq!(
        Value::from(item.is_deleted),
        raw_item["is_deleted"],
        "items[{index}].is_deleted"
    );
    assert_eq!(
        serde_json::to_value(&item.labels).unwrap(),
        raw_item["labels"],
        "items[{index}].labels"
    );
    assert_eq!(
        serde_json::to_value(&item.section_id).unwrap(),
        raw_item["section_id"],
        "items[{index}].section_id"
    );
    assert_eq!(
        serde_json::to_value(&item.parent_id).unwrap(),
        raw_item["parent_id"],
        "items[{index}].parent_id"
    );

    assert_due_matches(index, item.due.as_ref(), &raw_item["due"]);
    assert_deadline_matches(index, item.deadline.as_ref(), &raw_item["deadline"]);
}

/// Asserts the decoded [`Due`] equals the fixture's `due` object — for the
/// fields `Due` actually declares only.
fn assert_due_matches(index: usize, due: Option<&Due>, raw_due: &Value) {
    match (due, raw_due) {
        (None, Value::Null) => {}
        (Some(due), Value::Object(raw_due)) => {
            assert_eq!(
                Value::from(due.date.as_str()),
                raw_due["date"],
                "items[{index}].due.date"
            );
            assert_eq!(
                Value::from(due.string.as_deref()),
                raw_due["string"],
                "items[{index}].due.string"
            );
            assert_eq!(
                Value::from(due.lang.as_deref()),
                raw_due["lang"],
                "items[{index}].due.lang"
            );
            assert_eq!(
                Value::from(due.is_recurring),
                raw_due["is_recurring"],
                "items[{index}].due.is_recurring"
            );
            assert_eq!(
                Value::from(due.timezone.as_deref()),
                raw_due["timezone"],
                "items[{index}].due.timezone"
            );
        }
        (decoded, raw_due) => {
            panic!("items[{index}].due decoded as {decoded:?} but fixture has {raw_due}")
        }
    }
}

/// Asserts the decoded [`Deadline`] equals the fixture's `deadline` object.
fn assert_deadline_matches(index: usize, deadline: Option<&Deadline>, raw_deadline: &Value) {
    match (deadline, raw_deadline) {
        (None, Value::Null) => {}
        (Some(deadline), Value::Object(raw_deadline)) => {
            assert_eq!(
                Value::from(deadline.date.as_str()),
                raw_deadline["date"],
                "items[{index}].deadline.date"
            );
            assert_eq!(
                Value::from(deadline.lang.as_deref()),
                raw_deadline["lang"],
                "items[{index}].deadline.lang"
            );
        }
        (decoded, raw_deadline) => {
            panic!("items[{index}].deadline decoded as {decoded:?} but fixture has {raw_deadline}")
        }
    }
}
