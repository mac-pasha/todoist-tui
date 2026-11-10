//! `temp_id_mapping` resolution across queued commands and cached rows (T-407, EPIC-05).
//!
//! When a batch of commands containing resource-creating writes (such as
//! `project_add` or `item_add`) succeeds, the Todoist Sync API returns a
//! `temp_id_mapping` translating client-generated temporary IDs into
//! permanent server-assigned IDs ([`Id`]).
//!
//! [`resolve_temp_ids`] rewrites every occurrence of each mapped temp ID in
//! the `args` JSON and `temp_id` column of queued commands and invokes a
//! caller-supplied hook within the same SQLite [`Transaction`] so cached
//! resource rows (EPIC-04) are updated atomically. Rewriting is strictly
//! JSON value-aware ([`rewrite_json_value`]): only exact string values (and
//! object keys) matching a key in `mapping` are replaced, never substrings
//! inside user text such as task titles or note bodies.

use std::collections::{HashMap, HashSet};

use rusqlite::{params, Connection, Transaction};
use serde_json::Value;

use crate::api::model::Id;
use crate::error::{QueueError, Result};
use crate::queue::command::CommandState;

/// Cache tables that store Todoist resource IDs or foreign-key references.
const CACHE_TABLES: [&str; 4] = ["projects", "sections", "items", "notes"];

/// Scalar ID / foreign-key columns inspected by [`rewrite_sqlite_cache_temp_ids`].
const CACHE_ID_COLUMNS: [&str; 5] = ["id", "parent_id", "project_id", "section_id", "item_id"];

/// Recursively rewrites exact string values (and object keys) in `value` that
/// match keys in `mapping` to their corresponding real [`Id`].
///
/// Only full string matches are replaced: a string value that merely contains
/// a temp ID as a substring (e.g. `"content": "discuss temp_proj_1 plan"`) is
/// left untouched. Returns `true` if at least one string or key in `value`
/// was rewritten.
#[allow(clippy::implicit_hasher)]
pub fn rewrite_json_value(value: &mut Value, mapping: &HashMap<String, Id>) -> bool {
    if mapping.is_empty() {
        return false;
    }

    match value {
        Value::String(s) => {
            if let Some(real_id) = mapping.get(s.as_str()) {
                real_id.as_str().clone_into(s);
                true
            } else {
                false
            }
        }
        Value::Array(items) => {
            let mut changed = false;
            for item in items {
                if rewrite_json_value(item, mapping) {
                    changed = true;
                }
            }
            changed
        }
        Value::Object(map) => {
            let mut changed = false;
            for entry in map.values_mut() {
                if rewrite_json_value(entry, mapping) {
                    changed = true;
                }
            }

            let matching_keys: Vec<String> = map
                .keys()
                .filter(|k| mapping.contains_key(k.as_str()))
                .cloned()
                .collect();

            for old_key in matching_keys {
                if let (Some(real_id), Some(entry_val)) =
                    (mapping.get(old_key.as_str()), map.remove(&old_key))
                {
                    map.insert(real_id.as_str().to_owned(), entry_val);
                    changed = true;
                }
            }

            changed
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => false,
    }
}

/// Resolves every temp ID in `mapping` across queued commands (`pending`,
/// `failed`, and `in_flight`) and invokes `cache_rewriter` inside the same
/// SQLite [`Transaction`].
///
/// Even when `mapping` is empty, `cache_rewriter(tx, mapping)` is still
/// invoked before returning `Ok(0)`. Returns the number of `commands` rows
/// whose `args` or `temp_id` was updated.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] on a SQLite failure,
/// [`QueueError::Serialize`] if a stored command's `args` JSON is malformed,
/// or any error returned by `cache_rewriter`.
#[allow(clippy::implicit_hasher)]
pub fn resolve_temp_ids<F>(
    tx: &Transaction<'_>,
    mapping: &HashMap<String, Id>,
    cache_rewriter: F,
) -> Result<usize>
where
    F: FnOnce(&Transaction<'_>, &HashMap<String, Id>) -> Result<()>,
{
    if mapping.is_empty() {
        cache_rewriter(tx, mapping)?;
        return Ok(0);
    }

    let rows = load_command_rows(tx)?;
    let mut updated = 0usize;

    for (uuid, raw_args, temp_id) in rows {
        let mut args_val: Value =
            serde_json::from_str(&raw_args).map_err(|source| QueueError::Serialize { source })?;
        let args_changed = rewrite_json_value(&mut args_val, mapping);

        let mut resolved_temp_id = temp_id;
        let mut temp_id_changed = false;
        if let Some(ref current_tid) = resolved_temp_id {
            if let Some(real_id) = mapping.get(current_tid.as_str()) {
                resolved_temp_id = Some(real_id.as_str().to_owned());
                temp_id_changed = true;
            }
        }

        if args_changed || temp_id_changed {
            let new_args = serde_json::to_string(&args_val)
                .map_err(|source| QueueError::Serialize { source })?;
            tx.execute(
                "UPDATE commands SET args = ?1, temp_id = ?2 WHERE uuid = ?3",
                params![new_args, resolved_temp_id, uuid],
            )
            .map_err(|source| QueueError::Storage { source })?;
            updated = updated.saturating_add(1);
        }
    }

    cache_rewriter(tx, mapping)?;
    Ok(updated)
}

/// Resolves temp IDs across the `commands` table only, with a no-op cache
/// rewriter.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] on a SQLite failure or
/// [`QueueError::Serialize`] if a stored command's `args` JSON cannot be
/// decoded.
#[allow(clippy::implicit_hasher)]
pub fn resolve_command_temp_ids(
    tx: &Transaction<'_>,
    mapping: &HashMap<String, Id>,
) -> Result<usize> {
    resolve_temp_ids(tx, mapping, |_, _| Ok(()))
}

/// Rewrites temp IDs in the local SQLite cache tables (`projects`,
/// `sections`, `items`, and `notes`) when present in `tx`.
///
/// Can be passed directly as the `cache_rewriter` callback to
/// [`resolve_temp_ids`]. Inspects each table's existing columns via
/// `PRAGMA table_info`, updating matching scalar ID columns (`id`,
/// `parent_id`, `project_id`, `section_id`, `item_id`) and value-rewriting
/// `raw_json` via [`rewrite_json_value`].
///
/// # Errors
///
/// Returns [`QueueError::Storage`] on a SQLite failure or
/// [`QueueError::Serialize`] if a cached row's `raw_json` is malformed JSON.
#[allow(clippy::implicit_hasher)]
pub fn rewrite_sqlite_cache_temp_ids(
    tx: &Transaction<'_>,
    mapping: &HashMap<String, Id>,
) -> Result<()> {
    if mapping.is_empty() {
        return Ok(());
    }

    for table in CACHE_TABLES {
        let cols = table_columns(tx, table)?;
        if cols.is_empty() {
            continue;
        }

        for id_col in CACHE_ID_COLUMNS {
            if cols.contains(id_col) {
                let sql = format!("UPDATE {table} SET {id_col} = ?1 WHERE {id_col} = ?2");
                for (temp_id, real_id) in mapping {
                    tx.execute(&sql, params![real_id.as_str(), temp_id])
                        .map_err(|source| QueueError::Storage { source })?;
                }
            }
        }

        if cols.contains("raw_json") {
            rewrite_table_raw_json(tx, table, mapping)?;
        }
    }

    Ok(())
}

/// Verifies that no [`CommandState::Pending`] command in `commands` still
/// carries or references an unresolved temporary ID.
///
/// A string is considered an unresolved temp ID if it matches an entry in
/// `known_temp_ids` or starts with a temporary ID prefix (`"temp_"`,
/// `"tmp_"`, or `"tmp-"`). Only exact JSON string values (and object keys)
/// and the `temp_id` column are checked — never substrings within free-form
/// text.
///
/// # Errors
///
/// Returns [`QueueError::UnresolvedTempId`] on the first unresolved temp ID
/// found, [`QueueError::Storage`] on a SQLite failure, or
/// [`QueueError::Serialize`] if a stored `args` column is invalid JSON.
pub fn verify_no_temp_ids<I, S>(conn: &Connection, known_temp_ids: I) -> Result<()>
where
    I: IntoIterator<Item = S>,
    S: AsRef<str>,
{
    let known: HashSet<String> = known_temp_ids
        .into_iter()
        .map(|s| s.as_ref().to_owned())
        .collect();

    let mut stmt = conn
        .prepare(
            "SELECT args, temp_id FROM commands \
             WHERE state = ?1 ORDER BY created_at, rowid",
        )
        .map_err(|source| QueueError::Storage { source })?;

    let rows = stmt
        .query_map([CommandState::Pending.as_str()], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, Option<String>>(1)?))
        })
        .map_err(|source| QueueError::Storage { source })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|source| QueueError::Storage { source })?;

    for (raw_args, temp_id) in rows {
        if let Some(tid) = temp_id {
            if is_unresolved_temp_id(&tid, &known) {
                return Err(QueueError::UnresolvedTempId { temp_id: tid }.into());
            }
        }

        let args_val: Value =
            serde_json::from_str(&raw_args).map_err(|source| QueueError::Serialize { source })?;
        if let Some(unresolved) = find_unresolved_in_value(&args_val, &known) {
            return Err(QueueError::UnresolvedTempId {
                temp_id: unresolved,
            }
            .into());
        }
    }

    Ok(())
}

/// Loads `(uuid, args, temp_id)` for all queued commands in `pending`,
/// `failed`, or `in_flight` state.
fn load_command_rows(tx: &Transaction<'_>) -> Result<Vec<(String, String, Option<String>)>> {
    let mut stmt = tx
        .prepare(
            "SELECT uuid, args, temp_id FROM commands \
             WHERE state IN ('pending', 'failed', 'in_flight') \
             ORDER BY created_at, rowid",
        )
        .map_err(|source| QueueError::Storage { source })?;

    let rows = stmt
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(|source| QueueError::Storage { source })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|source| QueueError::Storage { source })?;

    Ok(rows)
}

/// Returns the set of column names for `table` (empty when `table` does not
/// exist).
fn table_columns(tx: &Transaction<'_>, table: &str) -> Result<HashSet<String>> {
    let sql = format!("PRAGMA table_info({table})");
    let mut stmt = tx
        .prepare(&sql)
        .map_err(|source| QueueError::Storage { source })?;
    let cols = stmt
        .query_map([], |row| row.get::<_, String>(1))
        .map_err(|source| QueueError::Storage { source })?
        .collect::<rusqlite::Result<HashSet<String>>>()
        .map_err(|source| QueueError::Storage { source })?;
    Ok(cols)
}

/// Rewrites `raw_json` values in `table` using [`rewrite_json_value`].
fn rewrite_table_raw_json(
    tx: &Transaction<'_>,
    table: &str,
    mapping: &HashMap<String, Id>,
) -> Result<()> {
    let select_sql = format!("SELECT rowid, raw_json FROM {table}");
    let rows = {
        let mut stmt = tx
            .prepare(&select_sql)
            .map_err(|source| QueueError::Storage { source })?;
        let mapped = stmt
            .query_map([], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
            })
            .map_err(|source| QueueError::Storage { source })?;
        mapped
            .collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| QueueError::Storage { source })?
    };

    let update_sql = format!("UPDATE {table} SET raw_json = ?1 WHERE rowid = ?2");
    for (rowid, raw_json) in rows {
        let mut val: Value =
            serde_json::from_str(&raw_json).map_err(|source| QueueError::Serialize { source })?;
        if rewrite_json_value(&mut val, mapping) {
            let updated_json =
                serde_json::to_string(&val).map_err(|source| QueueError::Serialize { source })?;
            tx.execute(&update_sql, params![updated_json, rowid])
                .map_err(|source| QueueError::Storage { source })?;
        }
    }

    Ok(())
}

/// Checks whether a single string value is an unresolved temp ID.
fn is_unresolved_temp_id(candidate: &str, known: &HashSet<String>) -> bool {
    known.contains(candidate)
        || candidate.starts_with("temp_")
        || candidate.starts_with("tmp_")
        || candidate.starts_with("tmp-")
}

/// Searches `value` depth-first for any string or object key matching
/// [`is_unresolved_temp_id`].
fn find_unresolved_in_value(value: &Value, known: &HashSet<String>) -> Option<String> {
    match value {
        Value::String(s) => {
            if is_unresolved_temp_id(s, known) {
                Some(s.clone())
            } else {
                None
            }
        }
        Value::Array(items) => items
            .iter()
            .find_map(|item| find_unresolved_in_value(item, known)),
        Value::Object(map) => {
            for (key, val) in map {
                if is_unresolved_temp_id(key, known) {
                    return Some(key.clone());
                }
                if let Some(found) = find_unresolved_in_value(val, known) {
                    return Some(found);
                }
            }
            None
        }
        Value::Null | Value::Bool(_) | Value::Number(_) => None,
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Unit tests for T-407 temp ID mapping resolution and verification.
    //!
    //! All IDs, temp IDs, project/task names and UUIDs are fake fixture data.
    use super::*;
    use crate::error::AppError;
    use crate::queue::command::QueuedCommand;
    use crate::queue::schema;
    use crate::queue::store::CommandStore;
    use serde_json::json;
    use uuid::Uuid;

    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        schema::apply(&conn).expect("apply queue schema");
        conn
    }

    fn fixture_uuid(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    fn make_mapping(pairs: &[(&str, &str)]) -> HashMap<String, Id> {
        pairs
            .iter()
            .map(|(temp, real)| ((*temp).to_string(), Id::from(*real)))
            .collect()
    }

    #[test]
    fn value_aware_rewrite_replaces_exact_values_but_not_substrings() {
        let mapping = make_mapping(&[("temp_proj_1", "real_proj_1"), ("temp_sec_1", "real_sec_1")]);

        let mut payload = json!({
            "project_id": "temp_proj_1",
            "content": "discuss temp_proj_1 plan with team",
            "description": "prefix_temp_proj_1 and temp_proj_1_suffix",
            "nested": {
                "section_id": "temp_sec_1",
                "tags": ["temp_proj_1", "keep_temp_proj_1_intact"],
                "temp_proj_1": "value_under_temp_key"
            }
        });

        let changed = rewrite_json_value(&mut payload, &mapping);
        assert!(changed);

        assert_eq!(payload["project_id"], json!("real_proj_1"));
        assert_eq!(
            payload["content"],
            json!("discuss temp_proj_1 plan with team"),
            "substring inside content must not be modified"
        );
        assert_eq!(
            payload["description"],
            json!("prefix_temp_proj_1 and temp_proj_1_suffix"),
            "partial matches must not be modified"
        );
        assert_eq!(payload["nested"]["section_id"], json!("real_sec_1"));
        assert_eq!(
            payload["nested"]["tags"],
            json!(["real_proj_1", "keep_temp_proj_1_intact"])
        );
        assert_eq!(
            payload["nested"]["real_proj_1"],
            json!("value_under_temp_key"),
            "object key matching temp_id should be rewritten"
        );
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn resolve_temp_ids_rewrites_pending_and_failed_commands_and_invokes_cache_hook() {
        let mut conn = fresh_conn();
        conn.execute(
            "CREATE TABLE cache_standin (id TEXT PRIMARY KEY, project_id TEXT NOT NULL)",
            [],
        )
        .expect("create standin table");
        conn.execute(
            "INSERT INTO cache_standin (id, project_id) VALUES ('temp_item_1', 'temp_proj_1')",
            [],
        )
        .expect("insert standin row");

        let store = CommandStore::new(&conn);
        let pending_cmd = QueuedCommand {
            uuid: fixture_uuid(1),
            kind: "item_add".to_string(),
            args: json!({
                "content": "discuss temp_proj_1 plan",
                "project_id": "temp_proj_1",
            }),
            temp_id: Some("temp_item_1".to_string()),
            state: CommandState::Pending,
            attempts: 0,
            created_at: 1_000,
            error: None,
        };
        let failed_cmd = QueuedCommand {
            uuid: fixture_uuid(2),
            kind: "item_move".to_string(),
            args: json!({
                "id": "temp_item_1",
                "project_id": "temp_proj_1",
            }),
            temp_id: None,
            state: CommandState::Failed,
            attempts: 1,
            created_at: 2_000,
            error: Some("previous error".to_string()),
        };
        store.insert(&pending_cmd).expect("insert pending");
        store.insert(&failed_cmd).expect("insert failed");

        let mapping = make_mapping(&[
            ("temp_proj_1", "real_proj_1"),
            ("temp_item_1", "real_item_1"),
        ]);

        let tx = conn.transaction().expect("begin tx");
        let updated = resolve_temp_ids(&tx, &mapping, |tx, map| {
            for (temp_id, real_id) in map {
                tx.execute(
                    "UPDATE cache_standin SET id = ?1 WHERE id = ?2",
                    params![real_id.as_str(), temp_id],
                )
                .map_err(|source| QueueError::Storage { source })?;
                tx.execute(
                    "UPDATE cache_standin SET project_id = ?1 WHERE project_id = ?2",
                    params![real_id.as_str(), temp_id],
                )
                .map_err(|source| QueueError::Storage { source })?;
            }
            Ok(())
        })
        .expect("resolve_temp_ids");
        tx.commit().expect("commit tx");

        assert_eq!(updated, 2);

        let pending_after = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending_after.len(), 1);
        assert_eq!(pending_after[0].temp_id.as_deref(), Some("real_item_1"));
        assert_eq!(
            pending_after[0].args,
            json!({
                "content": "discuss temp_proj_1 plan",
                "project_id": "real_proj_1",
            })
        );

        let failed_args_str: String = conn
            .query_row(
                "SELECT args FROM commands WHERE uuid = ?1",
                [fixture_uuid(2).to_string()],
                |row| row.get(0),
            )
            .expect("query failed args");
        let failed_args: Value = serde_json::from_str(&failed_args_str).expect("parse json");
        assert_eq!(
            failed_args,
            json!({
                "id": "real_item_1",
                "project_id": "real_proj_1",
            })
        );

        let cached_row: (String, String) = conn
            .query_row("SELECT id, project_id FROM cache_standin", [], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .expect("query cache_standin");
        assert_eq!(
            cached_row,
            ("real_item_1".to_string(), "real_proj_1".to_string())
        );
    }

    #[test]
    fn scenario_project_add_flushed_then_pending_item_add_rewritten_leaves_no_temp_id() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let temp_a = "temp_proj_A";
        let real_a = "real_proj_9001";

        let project_add = QueuedCommand {
            uuid: fixture_uuid(10),
            kind: "project_add".to_string(),
            args: json!({ "name": "New Project" }),
            temp_id: Some(temp_a.to_string()),
            state: CommandState::InFlight,
            attempts: 1,
            created_at: 1_000,
            error: None,
        };
        store.insert(&project_add).expect("insert project_add");

        let item_add = QueuedCommand {
            uuid: fixture_uuid(11),
            kind: "item_add".to_string(),
            args: json!({
                "content": "First task in new project",
                "project_id": temp_a,
            }),
            temp_id: Some("temp_item_B".to_string()),
            state: CommandState::Pending,
            attempts: 0,
            created_at: 2_000,
            error: None,
        };
        store.insert(&item_add).expect("insert item_add");

        let mapping = make_mapping(&[(temp_a, real_a)]);
        let tx = conn.transaction().expect("begin tx");
        let updated = resolve_command_temp_ids(&tx, &mapping).expect("resolve");
        tx.commit().expect("commit");

        assert_eq!(updated, 2);

        // Query asserts no row in `commands` contains `temp_a` in args or temp_id.
        let leftover: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands \
                 WHERE args LIKE '%' || ?1 || '%' OR IFNULL(temp_id, '') LIKE '%' || ?1 || '%'",
                [temp_a],
                |row| row.get(0),
            )
            .expect("count leftover temp_a");
        assert_eq!(leftover, 0, "no row in commands may contain {temp_a}");

        let pending = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].args["project_id"], json!(real_a));
    }

    #[test]
    fn rewrite_sqlite_cache_temp_ids_updates_resource_tables_and_raw_json() {
        let mut conn = fresh_conn();
        conn.execute_batch(
            "CREATE TABLE projects (id TEXT PRIMARY KEY, parent_id TEXT, raw_json TEXT NOT NULL);
             CREATE TABLE sections (id TEXT PRIMARY KEY, project_id TEXT NOT NULL, raw_json TEXT NOT NULL);
             CREATE TABLE items (id TEXT PRIMARY KEY, project_id TEXT, section_id TEXT, parent_id TEXT, raw_json TEXT NOT NULL);
             CREATE TABLE notes (id TEXT PRIMARY KEY, item_id TEXT NOT NULL, raw_json TEXT NOT NULL);",
        )
        .expect("create cache tables");

        conn.execute(
            "INSERT INTO projects (id, parent_id, raw_json) VALUES ('temp_p1', NULL, ?1)",
            [json!({"id": "temp_p1", "name": "Keep temp_p1 in title"}).to_string()],
        )
        .expect("insert project");
        conn.execute(
            "INSERT INTO sections (id, project_id, raw_json) VALUES ('temp_s1', 'temp_p1', ?1)",
            [json!({"id": "temp_s1", "project_id": "temp_p1"}).to_string()],
        )
        .expect("insert section");
        conn.execute(
            "INSERT INTO items (id, project_id, section_id, parent_id, raw_json) \
             VALUES ('temp_i1', 'temp_p1', 'temp_s1', NULL, ?1)",
            [
                json!({"id": "temp_i1", "project_id": "temp_p1", "section_id": "temp_s1"})
                    .to_string(),
            ],
        )
        .expect("insert item");
        conn.execute(
            "INSERT INTO notes (id, item_id, raw_json) VALUES ('temp_n1', 'temp_i1', ?1)",
            [json!({"id": "temp_n1", "item_id": "temp_i1"}).to_string()],
        )
        .expect("insert note");

        let mapping = make_mapping(&[
            ("temp_p1", "real_p1"),
            ("temp_s1", "real_s1"),
            ("temp_i1", "real_i1"),
            ("temp_n1", "real_n1"),
        ]);

        let tx = conn.transaction().expect("begin tx");
        resolve_temp_ids(&tx, &mapping, rewrite_sqlite_cache_temp_ids).expect("resolve");
        tx.commit().expect("commit");

        let (proj_id, proj_raw): (String, String) = conn
            .query_row("SELECT id, raw_json FROM projects", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .expect("read project");
        assert_eq!(proj_id, "real_p1");
        let proj_json: Value = serde_json::from_str(&proj_raw).expect("parse proj json");
        assert_eq!(proj_json["id"], json!("real_p1"));
        assert_eq!(proj_json["name"], json!("Keep temp_p1 in title"));

        let (item_id, item_proj, item_sec): (String, String, String) = conn
            .query_row("SELECT id, project_id, section_id FROM items", [], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?))
            })
            .expect("read item");
        assert_eq!(item_id, "real_i1");
        assert_eq!(item_proj, "real_p1");
        assert_eq!(item_sec, "real_s1");

        let (note_id, note_item): (String, String) = conn
            .query_row("SELECT id, item_id FROM notes", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .expect("read note");
        assert_eq!(note_id, "real_n1");
        assert_eq!(note_item, "real_i1");
    }

    #[test]
    fn verify_no_temp_ids_detects_unresolved_temp_id_and_passes_when_resolved() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let cmd = QueuedCommand {
            uuid: fixture_uuid(20),
            kind: "item_add".to_string(),
            args: json!({
                "content": "discuss temp_proj_1 plan",
                "project_id": "temp_proj_unresolved",
            }),
            temp_id: None,
            state: CommandState::Pending,
            attempts: 0,
            created_at: 1_000,
            error: None,
        };
        store.insert(&cmd).expect("insert cmd");

        let err = verify_no_temp_ids(&conn, ["temp_proj_unresolved"])
            .expect_err("unresolved temp id must fail verification");
        match err {
            AppError::Queue(QueueError::UnresolvedTempId { temp_id }) => {
                assert_eq!(temp_id, "temp_proj_unresolved");
            }
            other => panic!("expected Queue(UnresolvedTempId), got {other:?}"),
        }

        // Now resolve it and verify again: the substring inside `content` must not trigger an error.
        let mapping = make_mapping(&[("temp_proj_unresolved", "real_proj_42")]);
        let tx = conn.transaction().expect("begin tx");
        resolve_command_temp_ids(&tx, &mapping).expect("resolve");
        tx.commit().expect("commit");

        verify_no_temp_ids(&conn, ["temp_proj_unresolved"])
            .expect("verification should succeed after resolution");
    }
}
