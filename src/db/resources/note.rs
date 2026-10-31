//! Persistence for the Sync API `note` resource (T-306, EPIC-04).
//!
//! [`upsert`] keys notes by id (same upsert-by-id semantics as
//! [`super::item::upsert`]) and records each note's parent in the `notes`
//! table's `item_id` column so notes can be fetched per task via
//! [`fetch_for_item`].
//!
//! `schema.sql`'s `notes.item_id` is `NOT NULL`, but [`Note::item_id`] is
//! `Option<Id>` because a note can instead be a project-level comment,
//! carrying [`Note::project_id`] and no `item_id`. Rather than widening the
//! schema (out of scope for this task — see `CLAUDE.md`'s "coordinate via
//! `schema.sql`" rule), project-level notes are stored with an empty
//! string in `item_id`: that sentinel satisfies the `NOT NULL` constraint
//! and can never collide with a real Sync API item id (ids are always
//! non-empty), so [`fetch_for_item`] only needs to special-case an empty
//! `item_id` to keep project-level notes out of its results. Either way,
//! the complete [`Note`] — including whichever of `item_id`/`project_id`
//! was actually set — always rides along verbatim in `raw_json`, so the
//! parent id is never lost.

use rusqlite::{params, Connection, OptionalExtension};
use serde_json::Value;

use crate::api::model::{Id, Note};
use crate::error::{DbError, Result};

/// Inserts `note`, or overwrites the existing row with the same id.
///
/// Idempotent: calling `upsert` twice with the same (or an updated) `Note`
/// leaves exactly one row for that id, matching the Sync API's own
/// upsert-by-id semantics for incremental syncs.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if `note` cannot be serialised to JSON (not
/// expected in practice — `Note` has no fallible `Serialize` impls) or
/// [`DbError::Sqlite`] if the write fails.
pub fn upsert(conn: &Connection, note: &Note) -> Result<()> {
    let raw_json = serde_json::to_string(note).map_err(|source| DbError::Decode { source })?;

    // See the module doc for why project-level notes (no `item_id`) fall
    // back to an empty-string sentinel rather than `NULL`.
    let item_id = note.item_id.as_ref().map_or("", Id::as_str);

    conn.execute(
        "INSERT INTO notes (id, item_id, content, posted_at, is_deleted, raw_json)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT(id) DO UPDATE SET
            item_id = excluded.item_id,
            content = excluded.content,
            posted_at = excluded.posted_at,
            is_deleted = excluded.is_deleted,
            raw_json = excluded.raw_json",
        params![
            note.id.as_str(),
            item_id,
            note.content,
            note.posted_at,
            note.is_deleted,
            raw_json,
        ],
    )
    .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

/// Deletes the note with `id`, if it exists.
///
/// Deleting a missing id is not an error: it leaves the table in the same
/// state the caller wanted (no row for that id), matching the Sync API's
/// own idempotent-delete semantics.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the delete statement fails.
pub fn delete(conn: &Connection, id: &str) -> Result<()> {
    conn.execute("DELETE FROM notes WHERE id = ?1", params![id])
        .map_err(|source| DbError::Sqlite { source })?;
    Ok(())
}

/// Fetches the note with `id`, decoded from its stored `raw_json`, or
/// `None` if no row exists for that id.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails, or [`DbError::Decode`]
/// if the stored `raw_json` can no longer be deserialised into a [`Note`].
pub fn fetch(conn: &Connection, id: &str) -> Result<Option<Note>> {
    let raw_json: Option<String> = conn
        .query_row(
            "SELECT raw_json FROM notes WHERE id = ?1",
            params![id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| DbError::Sqlite { source })?;

    raw_json
        .map(|raw_json| decode_raw_json(&raw_json))
        .transpose()
}

/// Fetches every note attached to item `item_id`, decoded from their
/// stored `raw_json`, ordered by `posted_at` (oldest first).
///
/// `item_id` is never an empty string for a real item, and project-level
/// notes are stored with an empty-string sentinel (see the module doc), so
/// an empty `item_id` short-circuits to an empty result rather than
/// spuriously matching every project-level note in the table.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails, or [`DbError::Decode`]
/// if a stored `raw_json` can no longer be deserialised into a [`Note`].
pub fn fetch_for_item(conn: &Connection, item_id: &str) -> Result<Vec<Note>> {
    if item_id.is_empty() {
        return Ok(Vec::new());
    }

    let mut stmt = conn
        .prepare("SELECT raw_json FROM notes WHERE item_id = ?1 ORDER BY posted_at ASC")
        .map_err(|source| DbError::Sqlite { source })?;

    let rows = stmt
        .query_map(params![item_id], |row| row.get::<_, String>(0))
        .map_err(|source| DbError::Sqlite { source })?;

    let mut notes = Vec::new();
    for raw_json in rows {
        let raw_json = raw_json.map_err(|source| DbError::Sqlite { source })?;
        notes.push(decode_raw_json(&raw_json)?);
    }

    Ok(notes)
}

/// Decodes a stored `notes.raw_json` value back into a [`Note`].
fn decode_raw_json(raw_json: &str) -> Result<Note> {
    serde_json::from_str::<Value>(raw_json)
        .and_then(serde_json::from_value)
        .map_err(|source| DbError::Decode { source }.into())
}

#[cfg(test)]
mod tests {
    //! Unit tests for T-306 `note` persistence.
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

    fn item_note(id: &str, item_id: &str, content: &str) -> Note {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "item_id": item_id,
            "content": content,
            "posted_at": "2024-05-01T10:00:00Z",
            "is_deleted": false,
        }))
        .expect("valid note fixture")
    }

    fn project_note(id: &str, project_id: &str, content: &str) -> Note {
        serde_json::from_value(serde_json::json!({
            "id": id,
            "project_id": project_id,
            "content": content,
            "posted_at": "2024-05-01T10:00:00Z",
            "is_deleted": false,
        }))
        .expect("valid note fixture")
    }

    #[test]
    fn upsert_then_fetch_roundtrips_item_note() {
        let conn = test_conn();
        let note = item_note("note-1", "item-1", "remember the fridge");

        upsert(&conn, &note).unwrap();

        let fetched = fetch(&conn, "note-1").unwrap();
        assert_eq!(fetched, Some(note));
    }

    #[test]
    fn upsert_is_idempotent() {
        let conn = test_conn();
        let note = item_note("note-1", "item-1", "first content");

        upsert(&conn, &note).unwrap();
        upsert(&conn, &note).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);

        let updated = item_note("note-1", "item-1", "edited content");
        upsert(&conn, &updated).unwrap();

        let count: i64 = conn
            .query_row("SELECT COUNT(*) FROM notes", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(fetch(&conn, "note-1").unwrap(), Some(updated));
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
        let note = item_note("note-1", "item-1", "remember the fridge");
        upsert(&conn, &note).unwrap();

        delete(&conn, "note-1").unwrap();

        assert_eq!(fetch(&conn, "note-1").unwrap(), None);
    }

    #[test]
    fn fetch_for_item_returns_only_that_items_notes() {
        let conn = test_conn();
        upsert(&conn, &item_note("note-1", "item-1", "first")).unwrap();
        upsert(&conn, &item_note("note-2", "item-1", "second")).unwrap();
        upsert(&conn, &item_note("note-3", "item-2", "other item")).unwrap();
        upsert(&conn, &project_note("note-4", "project-1", "project note")).unwrap();

        let notes = fetch_for_item(&conn, "item-1").unwrap();

        assert_eq!(notes.len(), 2);
        assert!(notes
            .iter()
            .all(|note| note.item_id.as_ref().map(Id::as_str) == Some("item-1")));
    }

    #[test]
    fn project_level_note_round_trips_project_id() {
        let conn = test_conn();
        let note = project_note("note-4", "project-1", "project note");

        upsert(&conn, &note).unwrap();

        let fetched = fetch(&conn, "note-4").unwrap().expect("note stored");
        assert_eq!(
            fetched.project_id.as_ref().map(Id::as_str),
            Some("project-1")
        );
        assert_eq!(fetched.item_id, None);

        // Project-level notes must not show up in a per-item fetch.
        assert_eq!(fetch_for_item(&conn, "").unwrap(), Vec::new());
    }
}
