//! Persistence for the Sync API `item` resource (T-306, EPIC-04).
//!
//! [`upsert`] maps an [`Item`] onto the `items` table's typed columns
//! (`project_id`, `section_id`, `parent_id`, `child_order`, `checked`,
//! `priority`, `content`, `description`) plus a flattened `due_date`
//! column (the `due` object's `date`, falling back to the `deadline`
//! object's `date` when there is no `due`) for the read-path indices in
//! `schema.sql`, while the full `Item` — including the complete `due` and
//! `deadline` objects — rides along verbatim in `raw_json` so no field is
//! ever lost to a column this task didn't model.
//!
//! Completed items (`checked: true`) are written exactly like any other
//! item: per REQ-001 D3 (completed-item retention), nothing in this module
//! filters or drops checked items, and [`fetch`] can read them straight
//! back.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::api::model::Item;
use crate::error::{DbError, Result};

/// Inserts `item`, or overwrites the existing row with the same id.
///
/// Idempotent: calling `upsert` twice with the same (or an updated) `Item`
/// leaves exactly one row for that id, matching the Sync API's own
/// upsert-by-id semantics for incremental syncs.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if `item` cannot be serialised to JSON (not
/// expected in practice — `Item` has no fallible `Serialize` impls) or
/// [`DbError::Sqlite`] if the write fails.
pub fn upsert(conn: &Connection, item: &Item) -> Result<()> {
    let raw_json = serde_json::to_string(item).map_err(|source| DbError::Decode { source })?;
    let labels_json =
        serde_json::to_string(&item.labels).map_err(|source| DbError::Decode { source })?;

    // Flatten the `due`/`deadline` objects down to a single sortable/
    // filterable date string: `due.date` takes priority since it is the
    // user-facing "when is this due" date, `deadline.date` is the fallback
    // for items that only carry a deadline.
    let due_date = item.due.as_ref().map(|due| due.date.as_str()).or_else(|| {
        item.deadline
            .as_ref()
            .map(|deadline| deadline.date.as_str())
    });

    conn.execute(
        "INSERT INTO items (
            id, project_id, section_id, parent_id, content, description,
            child_order, checked, is_deleted, priority, due_date, labels,
            added_at, raw_json
         ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT(id) DO UPDATE SET
            project_id = excluded.project_id,
            section_id = excluded.section_id,
            parent_id = excluded.parent_id,
            content = excluded.content,
            description = excluded.description,
            child_order = excluded.child_order,
            checked = excluded.checked,
            is_deleted = excluded.is_deleted,
            priority = excluded.priority,
            due_date = excluded.due_date,
            labels = excluded.labels,
            added_at = excluded.added_at,
            raw_json = excluded.raw_json",
        params![
            item.id.as_str(),
            item.project_id.as_str(),
            item.section_id.as_ref().map(crate::api::model::Id::as_str),
            item.parent_id.as_ref().map(crate::api::model::Id::as_str),
            item.content,
            item.description,
            item.child_order,
            item.checked,
            item.is_deleted,
            i64::from(item.priority.get()),
            due_date,
            labels_json,
            item.added_at,
            raw_json,
        ],
    )
    .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

/// Deletes the item with `id`, if it exists.
///
/// Deleting a missing id is not an error: it leaves the table in the same
/// state the caller wanted (no row for that id), matching the Sync API's
/// own idempotent-delete semantics.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the delete statement fails.
pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM items WHERE id = ?1", params![id])
        .map_err(|source| DbError::Sqlite { source })?;
    Ok(())
}

/// Fetches the item with `id`, decoded from its stored `raw_json`, or
/// `None` if no row exists for that id.
///
/// Completed (`checked: true`) items are returned just like any other row
/// — this function applies no `checked`/`is_deleted` filtering.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails, or [`DbError::Decode`]
/// if the stored `raw_json` can no longer be deserialised into an
/// [`Item`].
pub fn fetch(conn: &Connection, id: &str) -> Result<Option<Item>> {
    let raw_json: Option<String> = conn
        .query_row(
            "SELECT raw_json FROM items WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| DbError::Sqlite { source })?;

    raw_json
        .map(|raw_json| decode_raw_json(&raw_json))
        .transpose()
}

/// Decodes a stored `items.raw_json` value back into an [`Item`].
fn decode_raw_json(raw_json: &str) -> Result<Item> {
    serde_json::from_str::<Value>(raw_json)
        .and_then(serde_json::from_value)
        .map_err(|source| DbError::Decode { source }.into())
}

#[cfg(test)]
mod tests {
    //! Unit tests for T-306 `item` persistence.
    //!
    //! All fixture data is fake: no real ids, tokens or personal
    //! information.

    use rusqlite::Connection;

    use super::*;
    use crate::db::migrations;

    /// An in-memory database with the T-303 schema applied.
    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        migrations::run(&mut conn).expect("run migrations");
        conn
    }

    /// Minimal, mandatory-fields-only item fixture.
    fn item(id: &str, checked: bool) -> Item {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "content": "Buy milk",
            "project_id": "project-1",
            "child_order": 1,
            "priority": 2,
            "checked": checked,
            "added_at": "2024-05-01T10:00:00Z",
            "user_id": "user-1",
        }))
        .expect("valid item fixture")
    }

    #[test]
    fn upsert_then_fetch_roundtrips_item() {
        let conn = test_conn();
        let item = item("item-1", false);

        upsert(&conn, &item).unwrap();

        assert_eq!(fetch(&conn, "item-1").unwrap(), Some(item));
    }

    #[test]
    fn completed_item_is_retained_and_readable() {
        // REQ-001 D3: completed items must be stored, not dropped.
        let conn = test_conn();
        let completed = item("item-1", true);

        upsert(&conn, &completed).unwrap();

        let fetched = fetch(&conn, "item-1").unwrap().expect("item stored");
        assert!(fetched.checked);
        assert_eq!(fetched, completed);
    }

    #[test]
    fn upsert_is_idempotent() {
        let conn = test_conn();
        let original = item("item-1", false);

        upsert(&conn, &original).unwrap();
        upsert(&conn, &original).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let updated = item("item-1", true);
        upsert(&conn, &updated).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM items", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(fetch(&conn, "item-1").unwrap(), Some(updated));
    }

    #[test]
    fn delete_missing_id_is_ok() {
        let conn = test_conn();
        delete(&conn, "does-not-exist").unwrap();
        assert_eq!(fetch(&conn, "does-not-exist").unwrap(), None);
    }

    #[test]
    fn delete_removes_row() {
        let conn = test_conn();
        let item = item("item-1", false);
        upsert(&conn, &item).unwrap();

        delete(&conn, "item-1").unwrap();

        assert_eq!(fetch(&conn, "item-1").unwrap(), None);
    }

    #[test]
    fn upsert_flattens_due_date_falling_back_to_deadline() {
        let conn = test_conn();
        let with_due: Item = serde_json::from_value(serde_json::json!({
            "id": "item-due",
            "content": "Buy milk",
            "project_id": "project-1",
            "child_order": 1,
            "priority": 2,
            "due": { "date": "2024-05-01" },
            "added_at": "2024-05-01T10:00:00Z",
            "user_id": "user-1",
        }))
        .unwrap();
        upsert(&conn, &with_due).unwrap();

        let due_date: Option<String> = conn
            .query_row(
                "SELECT due_date FROM items WHERE id = ?1",
                params!["item-due"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(due_date.as_deref(), Some("2024-05-01"));

        let with_deadline_only: Item = serde_json::from_value(serde_json::json!({
            "id": "item-deadline",
            "content": "Buy milk",
            "project_id": "project-1",
            "child_order": 1,
            "priority": 2,
            "deadline": { "date": "2024-05-03" },
            "added_at": "2024-05-01T10:00:00Z",
            "user_id": "user-1",
        }))
        .unwrap();
        upsert(&conn, &with_deadline_only).unwrap();

        let due_date: Option<String> = conn
            .query_row(
                "SELECT due_date FROM items WHERE id = ?1",
                params!["item-deadline"],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(due_date.as_deref(), Some("2024-05-03"));
    }
}
