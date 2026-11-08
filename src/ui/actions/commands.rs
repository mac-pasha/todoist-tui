//! Map a [`TaskAction`] to the Sync API write command(s) it enqueues
//! (T-702, EPIC-08).
//!
//! [`task_action_to_commands`] is pure: no HTTP, no DB, no terminal state —
//! it only decides *which* command(s) a user action produces and what their
//! `args` JSON looks like. The envelope is reused, not re-invented: every
//! command is built through [`QueuedCommand::new`] (T-403), which mints the
//! command `uuid` (`Uuid::new_v4`, exactly once per command) and projects
//! onto the wire via [`QueuedCommand::to_sync_command`].
//!
//! `temp_id` interpretation (T-702 acceptance): [`TaskAction`] and its
//! payloads (T-701) carry no "parent is not yet synced" flag, so the
//! "creating against a not-yet-synced parent" case is treated as *every*
//! creating action: `item_add` and `note_add` always carry a freshly
//! generated `temp_id` (a v4 uuid string, per the Sync API docs), because a
//! later command in the same batch may need to reference the resource
//! before the server assigns its real id. Non-create actions
//! (edit/complete/uncomplete/delete/move/reorder) always address an id the
//! server already knows, so they never produce a `temp_id`.
//!
//! `due` args shape: both `AddPayload::due` and `EditPayload::due` hold a
//! free-form quick-add date string (T-701). The current Sync API
//! (`{base}/api/v1/sync`, T-206) models dates as `due` objects, so the
//! string is serialised as `{"string": ...}` (`item_add`/`item_update`) and
//! a [`FieldEdit::Clear`] serialises as `"due": null`.

use serde_json::{json, Map, Value};
use uuid::Uuid;

use super::{
    AddCommentPayload, AddPayload, EditPayload, FieldEdit, MovePayload, ReorderPayload, TaskAction,
};
use crate::api::model::Id;
use crate::error::ActionError;
use crate::queue::QueuedCommand;

/// First `child_order` [`item_reorder`] assigns; subsequent entries are
/// strictly ascending with no gaps (1, 2, 3, ...).
const REORDER_CHILD_ORDER_BASE: usize = 1;

/// Turns a [`TaskAction`] into the Sync write command(s) to enqueue.
///
/// Each in-scope action maps to exactly one command, so the returned vector
/// always holds one element today; it stays a `Vec` because later EPIC-08
/// work (e.g. add-task-with-reminder) fans a single user action out into
/// several commands. The mapping is:
///
/// | `TaskAction`    | command `type`  | `temp_id` |
/// |-----------------|-----------------|-----------|
/// | `Add`           | `item_add`      | yes       |
/// | `Edit`          | `item_update`   | no        |
/// | `Complete`      | `item_complete` | no        |
/// | `Uncomplete`    | `item_uncomplete` | no      |
/// | `Delete`        | `item_delete`   | no        |
/// | `Move`          | `item_move`     | no        |
/// | `Reorder`       | `item_reorder`  | no        |
/// | `AddComment`    | `note_add`      | yes       |
///
/// # Errors
///
/// Returns [`ActionError::InvalidInput`] when the payload cannot produce a
/// well-formed command (currently only [`TaskAction::Move`] with zero or
/// more than one destination set); no command is built in that case.
pub fn task_action_to_commands(action: &TaskAction) -> Result<Vec<QueuedCommand>, ActionError> {
    let command = match action {
        TaskAction::Add(payload) => item_add(payload),
        TaskAction::Edit(payload) => item_update(payload),
        TaskAction::Complete(payload) => by_id("item_complete", &payload.id),
        TaskAction::Uncomplete(payload) => by_id("item_uncomplete", &payload.id),
        TaskAction::Delete(payload) => by_id("item_delete", &payload.id),
        TaskAction::Move(payload) => item_move(payload)?,
        TaskAction::Reorder(payload) => item_reorder(payload),
        TaskAction::AddComment(payload) => note_add(payload),
    };
    Ok(vec![command])
}

/// A fresh client-generated temporary id: the Sync API accepts any unique
/// string and resolves it via `temp_id_mapping`; a v4 uuid string keeps it
/// collision-free without extra bookkeeping.
fn fresh_temp_id() -> String {
    Uuid::new_v4().to_string()
}

/// `item_add` from an [`AddPayload`]: all fields serialise (an add always
/// carries the full item shape), optional ones only when present. Carries a
/// `temp_id` — see the module docs.
fn item_add(payload: &AddPayload) -> QueuedCommand {
    let mut args = Map::new();
    args.insert("content".into(), json!(payload.content));
    args.insert("description".into(), json!(payload.description));
    args.insert("project_id".into(), json!(payload.project));
    if let Some(section) = &payload.section {
        args.insert("section_id".into(), json!(section));
    }
    if let Some(parent) = &payload.parent {
        args.insert("parent_id".into(), json!(parent));
    }
    if let Some(due) = &payload.due {
        args.insert("due".into(), json!({ "string": due }));
    }
    args.insert("priority".into(), json!(payload.priority));
    args.insert("labels".into(), json!(payload.labels));
    QueuedCommand::new("item_add", Value::Object(args), Some(fresh_temp_id()))
}

/// `item_update` from an [`EditPayload`]: only the fields that actually
/// changed serialise. `None`/[`FieldEdit::Unchanged`] fields are skipped
/// entirely, [`FieldEdit::Set`] serialises the new value, and
/// [`FieldEdit::Clear`] serialises JSON `null` (the Sync API's way of
/// removing an already-nullable field). Never carries a `temp_id` — the
/// edited id already exists server-side.
fn item_update(payload: &EditPayload) -> QueuedCommand {
    let mut args = Map::new();
    args.insert("id".into(), json!(payload.id));
    if let Some(content) = &payload.content {
        args.insert("content".into(), json!(content));
    }
    if let Some(description) = &payload.description {
        args.insert("description".into(), json!(description));
    }
    if let Some(project) = &payload.project {
        args.insert("project_id".into(), json!(project));
    }
    match &payload.section {
        FieldEdit::Set(section) => {
            args.insert("section_id".into(), json!(section));
        }
        FieldEdit::Clear => {
            args.insert("section_id".into(), Value::Null);
        }
        FieldEdit::Unchanged => {}
    }
    match &payload.parent {
        FieldEdit::Set(parent) => {
            args.insert("parent_id".into(), json!(parent));
        }
        FieldEdit::Clear => {
            args.insert("parent_id".into(), Value::Null);
        }
        FieldEdit::Unchanged => {}
    }
    match &payload.due {
        FieldEdit::Set(due) => {
            args.insert("due".into(), json!({ "string": due }));
        }
        FieldEdit::Clear => {
            args.insert("due".into(), Value::Null);
        }
        FieldEdit::Unchanged => {}
    }
    if let Some(priority) = &payload.priority {
        args.insert("priority".into(), json!(priority));
    }
    if let Some(labels) = &payload.labels {
        args.insert("labels".into(), json!(labels));
    }
    QueuedCommand::new("item_update", Value::Object(args), None)
}

/// `item_complete`/`item_uncomplete`/`item_delete`: an id-only command.
fn by_id(kind: &'static str, id: &Id) -> QueuedCommand {
    QueuedCommand::new(kind, json!({ "id": id }), None)
}

/// `item_move` from a [`MovePayload`]: args are the task `id` plus exactly
/// one destination key — `project_id` xor `section_id` xor `parent_id`.
/// Zero or more than one destination set is a caller bug, not something to
/// guess a command for, so it returns [`ActionError::InvalidInput`] without
/// building anything. Never carries a `temp_id`.
// ponytail: no cross-field destination plausibility checks (e.g. moving to
// a section in another project); the server rejects those, and the
// T-703+ dispatch layer owns richer validation.
fn item_move(payload: &MovePayload) -> Result<QueuedCommand, ActionError> {
    let destinations = [
        ("project_id", payload.project.as_ref()),
        ("section_id", payload.section.as_ref()),
        ("parent_id", payload.parent.as_ref()),
    ];
    let present: Vec<(&str, &Id)> = destinations
        .into_iter()
        .filter_map(|(key, id)| id.map(|id| (key, id)))
        .collect();
    let (key, id) = match present.as_slice() {
        [(key, id)] => (*key, *id),
        _ => {
            return Err(ActionError::InvalidInput {
                field: "destination",
                reason: format!(
                    "item_move requires exactly one of project_id/section_id/parent_id, \
                     got {} set",
                    present.len()
                ),
            })
        }
    };
    let mut args = Map::new();
    args.insert("id".into(), json!(payload.id));
    args.insert(key.into(), json!(id));
    Ok(QueuedCommand::new("item_move", Value::Object(args), None))
}

/// `item_reorder` from a [`ReorderPayload`]: `{"items": [{"id": ...,
/// "child_order": ...}]}` with `child_order` assigned from
/// [`REORDER_CHILD_ORDER_BASE`] over `ordered_ids` order, so the sequence is
/// strictly ascending with no gaps (1, 2, 3, ...). Never carries a
/// `temp_id`.
// ponytail: a zero-id reorder serialises as an empty `items` array rather
// than erroring — the acceptance pins only the ascending/gapless sequence;
// reject it here if a caller ever needs that.
fn item_reorder(payload: &ReorderPayload) -> QueuedCommand {
    let items: Vec<Value> = payload
        .ordered_ids
        .iter()
        .enumerate()
        .map(|(index, id)| json!({ "id": id, "child_order": index + REORDER_CHILD_ORDER_BASE }))
        .collect();
    QueuedCommand::new("item_reorder", json!({ "items": items }), None)
}

/// `note_add` from an [`AddCommentPayload`]: `item_id` + `content`, plus a
/// `temp_id` — see the module docs.
fn note_add(payload: &AddCommentPayload) -> QueuedCommand {
    QueuedCommand::new(
        "note_add",
        json!({ "item_id": payload.item_id, "content": payload.content }),
        Some(fresh_temp_id()),
    )
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-702 `TaskAction` → Sync-command mapping.
    //!
    //! Written by the independent unit-test pass, not the implementation
    //! author: every T-702 acceptance item maps to at least one test below,
    //! plus edge/negative cases. All ids, contents and names are fake
    //! fixture data — no real Todoist ids or personal data appear.
    use super::*;
    use crate::api::model::Priority;
    use crate::ui::actions::{CompletePayload, DeletePayload, UncompletePayload};

    // ---------------------------------------------------------------
    // Fixture helpers.
    // ---------------------------------------------------------------

    fn priority_2() -> Priority {
        Priority::new(2).expect("2 is a valid priority")
    }

    fn add_payload_full() -> AddPayload {
        AddPayload {
            content: "fixture buy oat milk".to_string(),
            description: "fixture body text".to_string(),
            project: Id::from("fixture-project-1"),
            section: Some(Id::from("fixture-section-1")),
            parent: Some(Id::from("fixture-task-9")),
            due: Some("fixture-tomorrow".to_string()),
            priority: priority_2(),
            labels: vec!["fixture-label".to_string()],
        }
    }

    fn add_payload_minimal() -> AddPayload {
        AddPayload {
            content: "fixture bare task".to_string(),
            description: String::new(),
            project: Id::from("fixture-project-1"),
            section: None,
            parent: None,
            due: None,
            priority: priority_2(),
            labels: Vec::new(),
        }
    }

    /// An `EditPayload` that changes nothing: every optional field `None`,
    /// every [`FieldEdit`] `Unchanged`.
    fn edit_payload_unchanged() -> EditPayload {
        EditPayload {
            id: Id::from("fixture-task-1"),
            content: None,
            description: None,
            project: None,
            section: FieldEdit::Unchanged,
            parent: FieldEdit::Unchanged,
            due: FieldEdit::Unchanged,
            priority: None,
            labels: None,
        }
    }

    fn move_payload(project: Option<Id>, section: Option<Id>, parent: Option<Id>) -> MovePayload {
        MovePayload {
            id: Id::from("fixture-task-1"),
            project,
            section,
            parent,
        }
    }

    /// Every in-scope action, in the module-doc mapping-table order, with
    /// distinct fixture ids per variant.
    fn all_actions() -> Vec<(&'static str, TaskAction)> {
        vec![
            ("item_add", TaskAction::Add(add_payload_full())),
            (
                "item_update",
                TaskAction::Edit(EditPayload {
                    content: Some("fixture renamed".to_string()),
                    ..edit_payload_unchanged()
                }),
            ),
            (
                "item_complete",
                TaskAction::Complete(CompletePayload {
                    id: Id::from("fixture-task-1"),
                }),
            ),
            (
                "item_uncomplete",
                TaskAction::Uncomplete(UncompletePayload {
                    id: Id::from("fixture-task-1"),
                }),
            ),
            (
                "item_delete",
                TaskAction::Delete(DeletePayload {
                    id: Id::from("fixture-task-1"),
                }),
            ),
            (
                "item_move",
                TaskAction::Move(move_payload(
                    Some(Id::from("fixture-project-2")),
                    None,
                    None,
                )),
            ),
            (
                "item_reorder",
                TaskAction::Reorder(ReorderPayload {
                    project: Id::from("fixture-project-1"),
                    ordered_ids: vec![Id::from("fixture-task-2"), Id::from("fixture-task-1")],
                }),
            ),
            (
                "note_add",
                TaskAction::AddComment(AddCommentPayload {
                    item_id: Id::from("fixture-task-1"),
                    content: "fixture comment".to_string(),
                }),
            ),
        ]
    }

    // ---------------------------------------------------------------
    // Acceptance: action → command `type` mapping (all eight, exact).
    // ---------------------------------------------------------------

    #[test]
    fn every_action_maps_to_exactly_one_command_with_the_exact_wire_type() {
        for (expected_type, action) in all_actions() {
            let commands = task_action_to_commands(&action)
                .unwrap_or_else(|err| panic!("{expected_type} must map cleanly: {err}"));
            assert_eq!(
                commands.len(),
                1,
                "each in-scope action fans out to exactly one command"
            );
            let cmd = &commands[0];
            assert_eq!(
                cmd.kind, expected_type,
                "envelope kind must match the table"
            );
            // Wire projection carries the same type string.
            assert_eq!(
                cmd.to_sync_command()["type"],
                json!(expected_type),
                "wire `type` must match for {expected_type}"
            );
        }
    }

    // ---------------------------------------------------------------
    // Acceptance: fresh v4 uuid per command.
    // ---------------------------------------------------------------

    #[test]
    fn two_calls_for_the_same_action_get_distinct_v4_uuids() {
        let action = TaskAction::Add(add_payload_minimal());
        let first = task_action_to_commands(&action).expect("first add maps");
        let second = task_action_to_commands(&action).expect("second add maps");
        assert_ne!(
            first[0].uuid, second[0].uuid,
            "every call must mint its own command uuid"
        );
        assert_eq!(
            first[0].uuid.get_version_num(),
            4,
            "command uuid must be a v4 uuid"
        );
        assert_eq!(
            second[0].uuid.get_version_num(),
            4,
            "command uuid must be a v4 uuid"
        );
        // The uuid that reaches the wire is the construction-time one, as a
        // hyphenated string.
        assert_eq!(
            first[0].to_sync_command()["uuid"],
            json!(first[0].uuid.to_string())
        );
    }

    // ---------------------------------------------------------------
    // Acceptance: temp_id on creating actions only.
    // ---------------------------------------------------------------

    #[test]
    fn add_and_add_comment_carry_a_temp_id_other_actions_do_not() {
        for (wire_type, action) in all_actions() {
            let cmd = &task_action_to_commands(&action).expect("action maps")[0];
            let creates = matches!(wire_type, "item_add" | "note_add");
            let temp_id = cmd.temp_id.as_deref();
            if creates {
                let tid = temp_id.expect("creating actions must carry a temp_id");
                let parsed = Uuid::parse_str(tid).expect("temp_id must be a uuid string");
                assert_eq!(
                    parsed.get_version_num(),
                    4,
                    "temp_id must be a v4 uuid, not the command uuid"
                );
                assert_ne!(
                    tid,
                    cmd.uuid.to_string(),
                    "temp_id must be freshly minted, distinct from the command uuid"
                );
                assert_eq!(
                    cmd.to_sync_command()["temp_id"],
                    json!(tid),
                    "temp_id must ride top-level on the wire"
                );
            } else {
                assert!(
                    temp_id.is_none(),
                    "{wire_type} must not carry a temp_id, got {temp_id:?}"
                );
                assert!(
                    cmd.to_sync_command().get("temp_id").is_none(),
                    "{wire_type} wire command must omit the temp_id key entirely"
                );
            }
        }
    }

    #[test]
    fn two_add_calls_get_distinct_temp_ids() {
        let action = TaskAction::Add(add_payload_minimal());
        let first = task_action_to_commands(&action).expect("first add maps");
        let second = task_action_to_commands(&action).expect("second add maps");
        assert_ne!(
            first[0].temp_id, second[0].temp_id,
            "every creating command must get a fresh temp_id"
        );
    }

    // ---------------------------------------------------------------
    // item_add: args shape.
    // ---------------------------------------------------------------

    #[test]
    fn item_add_serialises_the_full_payload_including_optional_fields() {
        let payload = add_payload_full();
        let cmd =
            &task_action_to_commands(&TaskAction::Add(payload.clone())).expect("full add maps")[0];
        assert_eq!(
            cmd.args,
            json!({
                "content": "fixture buy oat milk",
                "description": "fixture body text",
                "project_id": "fixture-project-1",
                "section_id": "fixture-section-1",
                "parent_id": "fixture-task-9",
                "due": { "string": "fixture-tomorrow" },
                "priority": 2,
                "labels": ["fixture-label"],
            }),
            "every payload field must serialise; due as the {{string}} object"
        );
    }

    #[test]
    fn item_add_omits_absent_optional_fields_but_keeps_mandatory_ones() {
        let cmd = &task_action_to_commands(&TaskAction::Add(add_payload_minimal()))
            .expect("minimal add maps")[0];
        let args = cmd.args.as_object().expect("args is a JSON object");
        let keys: std::collections::BTreeSet<&str> = args.keys().map(String::as_str).collect();
        assert_eq!(
            keys,
            ["content", "description", "labels", "priority", "project_id"]
                .into_iter()
                .collect(),
            "absent section/parent/due must not serialise at all"
        );
    }

    // ---------------------------------------------------------------
    // Acceptance: item_update serialises only changed fields.
    // ---------------------------------------------------------------

    #[test]
    fn item_update_with_only_priority_changed_has_exactly_id_plus_priority() {
        // Reading of the acceptance item: "only `priority` set" means only
        // one changed field; `id` is the mandatory addressing key of
        // item_update, so the args map is `id` + exactly that one changed
        // key — nothing else leaks.
        let payload = EditPayload {
            priority: Some(priority_2()),
            ..edit_payload_unchanged()
        };
        let cmd = &task_action_to_commands(&TaskAction::Edit(payload)).expect("edit maps")[0];
        let args = cmd.args.as_object().expect("args is a JSON object");
        assert_eq!(
            args.len(),
            2,
            "exactly the id key plus one changed key: {args:?}"
        );
        let keys: Vec<&str> = args.keys().map(String::as_str).collect();
        assert!(
            keys.contains(&"id") && keys.contains(&"priority"),
            "keys must be exactly {{id, priority}}, got {keys:?}"
        );
        assert_eq!(args["id"], json!("fixture-task-1"));
        assert_eq!(args["priority"], json!(2));
        assert!(
            cmd.temp_id.is_none(),
            "item_update must never carry a temp_id"
        );
    }

    #[test]
    fn item_update_with_nothing_changed_serialises_only_the_id() {
        let cmd = &task_action_to_commands(&TaskAction::Edit(edit_payload_unchanged()))
            .expect("no-op edit maps")[0];
        assert_eq!(
            cmd.args,
            json!({ "id": "fixture-task-1" }),
            "no changed field may leak into the args"
        );
    }

    #[test]
    fn item_update_clear_serialises_null_unchanged_omits_and_set_writes_value() {
        let payload = EditPayload {
            section: FieldEdit::Clear,
            parent: FieldEdit::Unchanged,
            due: FieldEdit::Set("fixture-next-week".to_string()),
            project: Some(Id::from("fixture-project-3")),
            ..edit_payload_unchanged()
        };
        let cmd = &task_action_to_commands(&TaskAction::Edit(payload)).expect("edit maps")[0];
        let args = cmd.args.as_object().expect("args is a JSON object");
        assert_eq!(
            args["section_id"],
            Value::Null,
            "Clear must serialise an explicit JSON null"
        );
        assert!(
            !args.contains_key("parent_id"),
            "Unchanged must leave the key absent entirely, not null"
        );
        assert_eq!(
            args["due"],
            json!({ "string": "fixture-next-week" }),
            "Set on a FieldEdit serialises the new value in the due shape"
        );
        assert_eq!(args["project_id"], json!("fixture-project-3"));
        assert_eq!(
            args.len(),
            4,
            "id, project_id, section_id, due, nothing else: {args:?}"
        );
    }

    #[test]
    fn item_update_empty_string_description_is_a_value_not_a_clear() {
        // The T-701 vocabulary distinguishes `Some("")` ("cleared to
        // empty") from `None` ("untouched"); only `None` may be skipped.
        let payload = EditPayload {
            description: Some(String::new()),
            ..edit_payload_unchanged()
        };
        let cmd = &task_action_to_commands(&TaskAction::Edit(payload)).expect("edit maps")[0];
        assert_eq!(
            cmd.args["description"],
            json!(""),
            "Some(\"\") must serialise as an empty string, not be dropped"
        );
    }

    // ---------------------------------------------------------------
    // Acceptance: item_move — exactly one destination key, xor.
    // ---------------------------------------------------------------

    #[test]
    fn item_move_serialises_exactly_one_destination_key_per_destination_kind() {
        let cases = [
            (
                Some(Id::from("fixture-project-2")),
                None,
                None,
                "project_id",
            ),
            (
                None,
                Some(Id::from("fixture-section-2")),
                None,
                "section_id",
            ),
            (None, None, Some(Id::from("fixture-task-9")), "parent_id"),
        ];
        for (project, section, parent, expected_key) in cases {
            let cmd =
                &task_action_to_commands(&TaskAction::Move(move_payload(project, section, parent)))
                    .expect("single-destination move maps")[0];
            let args = cmd.args.as_object().expect("args is a JSON object");
            assert_eq!(
                args.len(),
                2,
                "exactly id plus one destination key: {args:?}"
            );
            assert!(args.contains_key("id"), "the moved item id must be present");
            let destination = args
                .get(expected_key)
                .unwrap_or_else(|| panic!("{expected_key} must be the single key: {args:?}"));
            assert!(
                !destination.is_null(),
                "{expected_key} must carry the destination id"
            );
            assert!(
                cmd.temp_id.is_none(),
                "item_move must never carry a temp_id"
            );
        }
    }

    #[test]
    fn item_move_with_zero_destinations_is_invalid_input_and_builds_nothing() {
        let result = task_action_to_commands(&TaskAction::Move(move_payload(None, None, None)));
        match result {
            Err(ActionError::InvalidInput { field, reason }) => {
                assert_eq!(field, "destination");
                assert!(
                    reason.contains('0'),
                    "error must report the number of destinations set: {reason}"
                );
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn item_move_with_two_destinations_is_invalid_input() {
        let result = task_action_to_commands(&TaskAction::Move(move_payload(
            Some(Id::from("fixture-project-2")),
            Some(Id::from("fixture-section-2")),
            None,
        )));
        match result {
            Err(ActionError::InvalidInput { field, reason }) => {
                assert_eq!(field, "destination");
                assert!(
                    reason.contains('2'),
                    "error must report the number of destinations set: {reason}"
                );
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    #[test]
    fn item_move_with_three_destinations_is_invalid_input() {
        let result = task_action_to_commands(&TaskAction::Move(move_payload(
            Some(Id::from("fixture-project-2")),
            Some(Id::from("fixture-section-2")),
            Some(Id::from("fixture-task-9")),
        )));
        match result {
            Err(ActionError::InvalidInput { field, reason }) => {
                assert_eq!(field, "destination");
                assert!(
                    reason.contains('3'),
                    "error must report the number of destinations set: {reason}"
                );
            }
            other => panic!("expected InvalidInput, got {other:?}"),
        }
    }

    // ---------------------------------------------------------------
    // Acceptance: item_reorder — ascending, gapless child_order.
    // ---------------------------------------------------------------

    #[test]
    fn item_reorder_on_five_items_assigns_child_order_one_through_five_in_order() {
        let ordered = [
            "fixture-task-e",
            "fixture-task-a",
            "fixture-task-c",
            "fixture-task-b",
            "fixture-task-d",
        ];
        let payload = ReorderPayload {
            project: Id::from("fixture-project-1"),
            ordered_ids: ordered.iter().map(|id| Id::from(*id)).collect(),
        };
        let cmd = &task_action_to_commands(&TaskAction::Reorder(payload))
            .expect("five-item reorder maps")[0];
        let args = cmd.args.as_object().expect("args is a JSON object");
        let items = args["items"].as_array().expect("items is an array");
        assert_eq!(
            args.len(),
            1,
            "item_reorder args carry only the items array: {args:?}"
        );
        assert_eq!(items.len(), 5, "one entry per reordered id");
        let mut child_orders = Vec::new();
        for (entry, expected_id) in items.iter().zip(ordered.iter()) {
            let obj = entry.as_object().expect("each entry is an object");
            assert_eq!(
                obj.len(),
                2,
                "each entry carries exactly id and child_order: {obj:?}"
            );
            assert_eq!(obj["id"], json!(expected_id), "ids must keep payload order");
            child_orders.push(
                obj["child_order"]
                    .as_u64()
                    .expect("child_order is a number"),
            );
        }
        assert_eq!(
            child_orders,
            vec![1, 2, 3, 4, 5],
            "child_order must be strictly ascending with no gaps from 1"
        );
        assert!(
            cmd.temp_id.is_none(),
            "item_reorder must never carry a temp_id"
        );
    }

    #[test]
    fn item_reorder_with_no_ids_yields_an_empty_items_array() {
        // ponytail edge (documented in the implementation): an empty
        // reorder serialises as `{"items": []}` rather than erroring.
        let payload = ReorderPayload {
            project: Id::from("fixture-project-1"),
            ordered_ids: Vec::new(),
        };
        let cmd =
            &task_action_to_commands(&TaskAction::Reorder(payload)).expect("empty reorder maps")[0];
        assert_eq!(cmd.args, json!({ "items": [] }));
    }

    // ---------------------------------------------------------------
    // id-only actions: item_complete / item_uncomplete / item_delete.
    // ---------------------------------------------------------------

    #[test]
    fn id_only_actions_serialise_exactly_the_id_arg() {
        let cases = [
            (
                TaskAction::Complete(CompletePayload {
                    id: Id::from("fixture-task-1"),
                }),
                "item_complete",
            ),
            (
                TaskAction::Uncomplete(UncompletePayload {
                    id: Id::from("fixture-task-1"),
                }),
                "item_uncomplete",
            ),
            (
                TaskAction::Delete(DeletePayload {
                    id: Id::from("fixture-task-1"),
                }),
                "item_delete",
            ),
        ];
        for (action, wire_type) in cases {
            let cmd = &task_action_to_commands(&action)
                .unwrap_or_else(|err| panic!("{wire_type} maps cleanly: {err}"))[0];
            assert_eq!(
                cmd.args,
                json!({ "id": "fixture-task-1" }),
                "{wire_type} carries exactly the id"
            );
        }
    }

    // ---------------------------------------------------------------
    // note_add: args shape.
    // ---------------------------------------------------------------

    #[test]
    fn note_add_serialises_item_id_and_content_and_carries_a_temp_id() {
        let payload = AddCommentPayload {
            item_id: Id::from("fixture-task-1"),
            content: "fixture comment body".to_string(),
        };
        let cmd =
            &task_action_to_commands(&TaskAction::AddComment(payload)).expect("note_add maps")[0];
        assert_eq!(
            cmd.args,
            json!({
                "item_id": "fixture-task-1",
                "content": "fixture comment body",
            })
        );
        assert!(
            cmd.temp_id.is_some(),
            "note_add creates a resource, so it must carry a temp_id"
        );
    }

    // ---------------------------------------------------------------
    // Purity: the same input value never mutates across calls.
    // ---------------------------------------------------------------

    #[test]
    fn mapping_is_pure_same_reference_twice_leaves_action_unchanged() {
        // The function takes only `&TaskAction`, so it cannot consume or
        // mutate the payload; prove it by mapping the same action value
        // twice and asserting the action still equals its own pre-call
        // clone.
        let action = TaskAction::Add(add_payload_full());
        let before = action.clone();
        let first = task_action_to_commands(&action).expect("first call maps");
        let second = task_action_to_commands(&action).expect("second call maps");
        assert_eq!(action, before, "the action must survive mapping unchanged");
        // Purity of everything except uuid/temp_id minting: the args
        // payloads are structurally identical across calls.
        assert_eq!(first[0].args, second[0].args);
        assert_eq!(first[0].kind, second[0].kind);
    }
}
