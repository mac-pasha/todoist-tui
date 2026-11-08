//! Pure constructors for structure commands (T-802, EPIC-09).
//!
//! Every function validates its input (names only; ids are opaque strings
//! the caller took from the cache) and returns a [`QueuedCommand`] carrying
//! the real Todoist Sync API command `type` tag and `args` shape. `uuid` is
//! minted by [`QueuedCommand::new`]; `*_add` commands also carry a fresh
//! v4 `temp_id` so later queued commands can reference the not-yet-real
//! resource before the Sync API resolves it. No IO, no cache, no enqueue.

use serde_json::{json, Value};
use uuid::Uuid;

use crate::error::{Result, StructureError};
use crate::queue::QueuedCommand;

/// Maximum accepted name length, in Unicode scalar values (`chars`),
/// counted on the trimmed name. Matches Todoist's 120-character limit for
/// project and section names; also applied to labels (Todoist allows
/// slightly more, the stricter single bound is deliberate and cheap to
/// loosen).
pub const MAX_NAME_LEN: usize = 120;

/// Sync API command type tags, one constant per wire command.
pub const PROJECT_ADD: &str = "project_add";
/// Sync API command type tag for [`project_update`].
pub const PROJECT_UPDATE: &str = "project_update";
/// Sync API command type tag for [`project_archive`].
pub const PROJECT_ARCHIVE: &str = "project_archive";
/// Sync API command type tag for [`project_unarchive`].
pub const PROJECT_UNARCHIVE: &str = "project_unarchive";
/// Sync API command type tag for [`project_delete`].
pub const PROJECT_DELETE: &str = "project_delete";
/// Sync API command type tag for [`section_add`].
pub const SECTION_ADD: &str = "section_add";
/// Sync API command type tag for [`section_update`].
pub const SECTION_UPDATE: &str = "section_update";
/// Sync API command type tag for [`section_reorder`].
pub const SECTION_REORDER: &str = "section_reorder";
/// Sync API command type tag for [`section_delete`].
pub const SECTION_DELETE: &str = "section_delete";
/// Sync API command type tag for [`label_add`].
pub const LABEL_ADD: &str = "label_add";
/// Sync API command type tag for [`label_update`].
pub const LABEL_UPDATE: &str = "label_update";
/// Sync API command type tag for [`label_delete`].
pub const LABEL_DELETE: &str = "label_delete";

/// Trims `name` and checks it is non-empty and at most [`MAX_NAME_LEN`]
/// characters long; returns the trimmed name on success.
fn validate_name(name: &str) -> std::result::Result<&str, StructureError> {
    let trimmed = name.trim();
    if trimmed.is_empty() {
        return Err(StructureError::EmptyName);
    }
    let len = trimmed.chars().count();
    if len > MAX_NAME_LEN {
        return Err(StructureError::NameTooLong { len });
    }
    Ok(trimmed)
}

/// Fresh v4 uuid string used as a `temp_id` for resource-creating commands.
fn new_temp_id() -> String {
    Uuid::new_v4().to_string()
}

/// Builds a single-target `{id}` command (archive / unarchive / delete).
fn id_only(kind: &str, id: &str) -> QueuedCommand {
    QueuedCommand::new(kind, json!({ "id": id }), None)
}

/// Builds a rename command with args `{id, name}`.
fn rename(kind: &str, id: &str, name: &str) -> Result<QueuedCommand> {
    let name = validate_name(name)?;
    Ok(QueuedCommand::new(
        kind,
        json!({ "id": id, "name": name }),
        None,
    ))
}

/// `project_add` — args `{name}` plus `parent_id` only when `Some`, so a
/// top-level project never sends a null parent. Carries a fresh `temp_id`.
pub fn project_add(name: &str, parent_id: Option<&str>) -> Result<QueuedCommand> {
    let name = validate_name(name)?;
    let mut args = json!({ "name": name });
    if let Some(parent_id) = parent_id {
        args["parent_id"] = Value::from(parent_id);
    }
    Ok(QueuedCommand::new(PROJECT_ADD, args, Some(new_temp_id())))
}

/// `project_update` (rename) — args `{id, name}`.
pub fn project_update(id: &str, name: &str) -> Result<QueuedCommand> {
    rename(PROJECT_UPDATE, id, name)
}

/// `project_archive` — args `{id}`. Reversible; wire type is distinct from
/// [`project_delete`]'s, and the two are never interchangeable.
pub fn project_archive(id: &str) -> QueuedCommand {
    id_only(PROJECT_ARCHIVE, id)
}

/// `project_unarchive` — args `{id}`.
pub fn project_unarchive(id: &str) -> QueuedCommand {
    id_only(PROJECT_UNARCHIVE, id)
}

/// `project_delete` — args `{id}`. Destructive; wire type is distinct from
/// [`project_archive`]'s, and the two are never interchangeable.
pub fn project_delete(id: &str) -> QueuedCommand {
    id_only(PROJECT_DELETE, id)
}

/// `section_add` — args `{name, project_id}`. Carries a fresh `temp_id`.
pub fn section_add(name: &str, project_id: &str) -> Result<QueuedCommand> {
    let name = validate_name(name)?;
    Ok(QueuedCommand::new(
        SECTION_ADD,
        json!({ "name": name, "project_id": project_id }),
        Some(new_temp_id()),
    ))
}

/// `section_update` (rename) — args `{id, name}`.
pub fn section_update(id: &str, name: &str) -> Result<QueuedCommand> {
    rename(SECTION_UPDATE, id, name)
}

/// `section_reorder` — args `{sections: [{id, section_order}, ...]}`, one
/// entry per `(&id, order)` pair, in the order given.
pub fn section_reorder(sections: &[(&str, i64)]) -> QueuedCommand {
    let entries: Vec<Value> = sections
        .iter()
        .map(|(id, order)| json!({ "id": id, "section_order": order }))
        .collect();
    QueuedCommand::new(SECTION_REORDER, json!({ "sections": entries }), None)
}

/// `section_delete` — args `{id}`.
pub fn section_delete(id: &str) -> QueuedCommand {
    id_only(SECTION_DELETE, id)
}

/// `label_add` — args `{name}`. Carries a fresh `temp_id`.
pub fn label_add(name: &str) -> Result<QueuedCommand> {
    let name = validate_name(name)?;
    Ok(QueuedCommand::new(
        LABEL_ADD,
        json!({ "name": name }),
        Some(new_temp_id()),
    ))
}

/// `label_update` (rename) — args `{id, name}`.
pub fn label_update(id: &str, name: &str) -> Result<QueuedCommand> {
    rename(LABEL_UPDATE, id, name)
}

/// `label_delete` — args `{id}`.
pub fn label_delete(id: &str) -> QueuedCommand {
    id_only(LABEL_DELETE, id)
}

#[cfg(test)]
mod tests {
    //! Unit tests for the T-802 structure command builders (EPIC-09).
    //! All ids/names are fake fixture data.
    use super::*;
    use crate::error::AppError;
    use crate::queue::CommandState;

    fn ok(r: Result<QueuedCommand>) -> QueuedCommand {
        r.expect("constructor should succeed")
    }

    fn structure_err(r: Result<QueuedCommand>) -> StructureError {
        match r.expect_err("constructor should fail") {
            AppError::Structure(e) => e,
            other => panic!("expected AppError::Structure, got {other:?}"),
        }
    }

    /// One command per constructor, all with valid fake input.
    fn all_commands() -> Vec<QueuedCommand> {
        vec![
            ok(project_add("P", None)),
            ok(project_update("p1", "P")),
            project_archive("p1"),
            project_unarchive("p1"),
            project_delete("p1"),
            ok(section_add("S", "p1")),
            ok(section_update("s1", "S")),
            section_reorder(&[("s1", 1)]),
            section_delete("s1"),
            ok(label_add("L")),
            ok(label_update("l1", "L")),
            label_delete("l1"),
        ]
    }

    // ---- uuid / temp_id freshness ----

    #[test]
    fn every_constructor_mints_a_distinct_v4_uuid_per_call() {
        let a = all_commands();
        let b = all_commands();
        assert_eq!(a.len(), 12);
        for (x, y) in a.iter().zip(b.iter()) {
            assert_eq!(x.kind, y.kind);
            assert_ne!(x.uuid, y.uuid, "{} reused a uuid", x.kind);
            assert_eq!(x.uuid.get_version_num(), 4, "{}", x.kind);
        }
    }

    #[test]
    fn add_constructors_mint_distinct_v4_temp_ids_per_call() {
        let pairs = [
            (ok(project_add("P", None)), ok(project_add("P", None))),
            (ok(section_add("S", "p1")), ok(section_add("S", "p1"))),
            (ok(label_add("L")), ok(label_add("L"))),
        ];
        for (x, y) in pairs {
            let (tx, ty) = (x.temp_id.expect("temp_id"), y.temp_id.expect("temp_id"));
            assert_ne!(tx, ty, "{} reused a temp_id", x.kind);
            assert_eq!(
                Uuid::parse_str(&tx)
                    .expect("temp_id parses as uuid")
                    .get_version_num(),
                4
            );
            assert_ne!(tx, x.uuid.to_string(), "temp_id must differ from uuid");
        }
    }

    #[test]
    fn temp_id_only_on_add_commands() {
        for c in all_commands() {
            let is_add = matches!(c.kind.as_str(), PROJECT_ADD | SECTION_ADD | LABEL_ADD);
            assert_eq!(c.temp_id.is_some(), is_add, "{}", c.kind);
            assert_eq!(c.to_sync_command().get("temp_id").is_some(), is_add);
        }
    }

    #[test]
    fn new_commands_start_pending_with_no_attempts() {
        for c in all_commands() {
            assert_eq!(c.state, CommandState::Pending, "{}", c.kind);
            assert_eq!(c.attempts, 0);
            assert!(c.error.is_none());
        }
    }

    // ---- project_add parent_id ----

    #[test]
    fn project_add_without_parent_has_no_parent_id_key() {
        let c = ok(project_add("Home", None));
        assert_eq!(c.kind, "project_add");
        assert_eq!(c.args, json!({ "name": "Home" }));
        assert!(
            c.args
                .as_object()
                .expect("object")
                .get("parent_id")
                .is_none(),
            "parent_id key must be absent entirely, not null"
        );
        let wire = serde_json::to_string(&c.to_sync_command()).expect("serialise");
        assert!(!wire.contains("parent_id"), "{wire}");
        assert!(!wire.contains("null"), "{wire}");
    }

    #[test]
    fn project_add_with_parent_serialises_parent_id() {
        let c = ok(project_add("Kitchen", Some("123")));
        assert_eq!(c.args, json!({ "name": "Kitchen", "parent_id": "123" }));
        assert_eq!(c.to_sync_command()["args"]["parent_id"], "123");
    }

    #[test]
    fn project_add_empty_parent_string_is_passed_through_not_dropped() {
        // ids are opaque; Some("") is still Some.
        let c = ok(project_add("X", Some("")));
        assert_eq!(c.args["parent_id"], "");
    }

    // ---- archive vs delete ----

    #[test]
    fn archive_unarchive_delete_have_distinct_wire_types() {
        let a = project_archive("p1");
        let u = project_unarchive("p1");
        let d = project_delete("p1");
        assert_eq!(a.to_sync_command()["type"], "project_archive");
        assert_eq!(u.to_sync_command()["type"], "project_unarchive");
        assert_eq!(d.to_sync_command()["type"], "project_delete");
        assert_ne!(a.kind, d.kind);
        assert_ne!(a.kind, u.kind);
        assert_ne!(u.kind, d.kind);
        assert_ne!(a.to_sync_command()["type"], d.to_sync_command()["type"]);
        assert_eq!(
            a.args, d.args,
            "same args shape: only `type` tells them apart"
        );
    }

    #[test]
    fn all_constructors_emit_distinct_wire_types() {
        let kinds: std::collections::HashSet<String> =
            all_commands().into_iter().map(|c| c.kind).collect();
        assert_eq!(kinds.len(), 12);
    }

    // ---- args shapes ----

    #[test]
    fn args_shapes_match_sync_api() {
        assert_eq!(
            ok(project_update("p1", "N")).args,
            json!({ "id": "p1", "name": "N" })
        );
        assert_eq!(project_archive("p1").args, json!({ "id": "p1" }));
        assert_eq!(project_unarchive("p1").args, json!({ "id": "p1" }));
        assert_eq!(project_delete("p1").args, json!({ "id": "p1" }));
        assert_eq!(
            ok(section_add("S", "p1")).args,
            json!({ "name": "S", "project_id": "p1" })
        );
        assert_eq!(
            ok(section_update("s1", "N")).args,
            json!({ "id": "s1", "name": "N" })
        );
        assert_eq!(section_delete("s1").args, json!({ "id": "s1" }));
        assert_eq!(ok(label_add("L")).args, json!({ "name": "L" }));
        assert_eq!(
            ok(label_update("l1", "N")).args,
            json!({ "id": "l1", "name": "N" })
        );
        assert_eq!(label_delete("l1").args, json!({ "id": "l1" }));
    }

    // ---- name validation ----

    /// Every name-taking constructor.
    type NameCtor = fn(&str) -> Result<QueuedCommand>;
    fn name_ctors() -> Vec<(&'static str, NameCtor)> {
        vec![
            ("project_add", |n| project_add(n, None)),
            ("project_update", |n| project_update("p1", n)),
            ("section_add", |n| section_add(n, "p1")),
            ("section_update", |n| section_update("s1", n)),
            ("label_add", label_add),
            ("label_update", |n| label_update("l1", n)),
        ]
    }

    #[test]
    fn empty_and_whitespace_names_are_rejected_everywhere() {
        for (label, ctor) in name_ctors() {
            for bad in ["", " ", "   ", "\t\n", "\u{00a0}\u{3000}"] {
                assert!(
                    matches!(structure_err(ctor(bad)), StructureError::EmptyName),
                    "{label} with {bad:?}"
                );
            }
        }
    }

    #[test]
    fn max_length_names_accepted_ascii_and_multibyte() {
        assert_eq!(MAX_NAME_LEN, 120);
        let ascii = "a".repeat(120);
        let multi = "é".repeat(120); // 240 bytes, 120 chars
        let cjk = "名".repeat(120);
        for (label, ctor) in name_ctors() {
            for name in [&ascii, &multi, &cjk] {
                let c = ok(ctor(name));
                assert_eq!(c.args["name"], name.as_str(), "{label}");
            }
        }
    }

    #[test]
    fn over_max_length_names_rejected_with_char_count() {
        let ascii = "a".repeat(121);
        let multi = "é".repeat(121);
        let long = "b".repeat(5000);
        for (label, ctor) in name_ctors() {
            for (name, want) in [(&ascii, 121), (&multi, 121), (&long, 5000)] {
                match structure_err(ctor(name)) {
                    StructureError::NameTooLong { len } => assert_eq!(len, want, "{label}"),
                    other => panic!("{label}: expected NameTooLong, got {other:?}"),
                }
            }
        }
    }

    #[test]
    fn name_is_trimmed_before_length_check_and_stored_trimmed() {
        let padded = format!("  {}  ", "a".repeat(120));
        for (label, ctor) in name_ctors() {
            let c = ok(ctor(&padded));
            assert_eq!(c.args["name"], "a".repeat(120).as_str(), "{label}");
            let c = ok(ctor("\t Inbox \n"));
            assert_eq!(c.args["name"], "Inbox", "{label}");
        }
        // trimmed length 121 still rejected; len excludes padding.
        let padded_long = format!(" {} ", "a".repeat(121));
        match structure_err(project_add(&padded_long, None)) {
            StructureError::NameTooLong { len } => assert_eq!(len, 121),
            other => panic!("expected NameTooLong, got {other:?}"),
        }
    }

    #[test]
    fn interior_whitespace_is_preserved() {
        assert_eq!(ok(label_add("  a  b  ")).args["name"], "a  b");
    }

    #[test]
    fn invalid_name_yields_structure_prefixed_app_error() {
        let e = project_add("", Some("p9")).expect_err("empty name");
        assert!(e.to_string().starts_with("structure: "), "{e}");
    }

    // ---- section_reorder ----

    #[test]
    fn section_reorder_empty_slice_yields_empty_sections_array() {
        let c = section_reorder(&[]);
        assert_eq!(c.kind, "section_reorder");
        assert_eq!(c.args, json!({ "sections": [] }));
        assert!(c.temp_id.is_none());
    }

    #[test]
    fn section_reorder_single_entry() {
        let c = section_reorder(&[("s1", 1)]);
        assert_eq!(
            c.args,
            json!({ "sections": [{ "id": "s1", "section_order": 1 }] })
        );
    }

    #[test]
    fn section_reorder_preserves_input_order_and_values() {
        let c = section_reorder(&[
            ("s3", 1),
            ("s1", 2),
            ("s2", 3),
            ("s9", -4),
            ("s0", i64::MAX),
        ]);
        let got = c.args["sections"].as_array().expect("array");
        let ids: Vec<&str> = got.iter().map(|e| e["id"].as_str().expect("id")).collect();
        assert_eq!(ids, ["s3", "s1", "s2", "s9", "s0"]);
        let orders: Vec<i64> = got
            .iter()
            .map(|e| e["section_order"].as_i64().expect("order"))
            .collect();
        assert_eq!(orders, [1, 2, 3, -4, i64::MAX]);
        for e in got {
            assert_eq!(e.as_object().expect("object").len(), 2);
        }
    }

    #[test]
    fn section_reorder_duplicate_ids_are_not_deduplicated() {
        // Builder is dumb by design: caller owns uniqueness.
        let c = section_reorder(&[("s1", 1), ("s1", 2)]);
        assert_eq!(c.args["sections"].as_array().expect("array").len(), 2);
    }
}
