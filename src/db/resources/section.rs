//! Persistence for the Sync API `section` resource (T-305, EPIC-04).
//!
//! [`upsert`] maps a [`Section`] onto the `sections` table's typed columns
//! (`project_id`, `name`, `section_order`, `is_archived`, `is_deleted`)
//! from `schema.sql`, while the full `Section` rides along verbatim in
//! `raw_json` so no field — modelled or not — is ever lost to a column
//! this task didn't need for the read path.

use rusqlite::{named_params, Connection};

use crate::api::model::{Id, Section};
use crate::error::{DbError, Result};

/// Inserts `section`, or overwrites the existing row with the same id.
///
/// Idempotent: calling `upsert` twice with the same (or an updated)
/// `Section` leaves exactly one row for that id, matching the Sync API's
/// own upsert-by-id semantics for incremental syncs. Every mapped column
/// is overwritten on conflict.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if `section` cannot be serialised to JSON
/// (not expected in practice — `Section` has no fallible `Serialize`
/// impls) or [`DbError::Sqlite`] if the write fails.
pub fn upsert(conn: &Connection, section: &Section) -> Result<()> {
    let raw_json = serde_json::to_string(section).map_err(|source| DbError::Decode { source })?;

    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO sections (
                id, project_id, name, section_order, is_archived,
                is_deleted, raw_json
            ) VALUES (
                :id, :project_id, :name, :section_order, :is_archived,
                :is_deleted, :raw_json
            )
            ON CONFLICT (id) DO UPDATE SET
                project_id = excluded.project_id,
                name = excluded.name,
                section_order = excluded.section_order,
                is_archived = excluded.is_archived,
                is_deleted = excluded.is_deleted,
                raw_json = excluded.raw_json",
        )
        .map_err(|source| DbError::Sqlite { source })?;

    stmt.execute(named_params! {
        ":id": section.id.as_str(),
        ":project_id": section.project_id.as_str(),
        ":name": section.name,
        ":section_order": section.section_order,
        ":is_archived": section.is_archived,
        ":is_deleted": section.is_deleted,
        ":raw_json": raw_json,
    })
    .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

/// Deletes the section row with the given id, if one exists.
///
/// Deleting an id with no matching row is not an error (mirrors the Sync
/// API's own idempotent delete-by-id semantics).
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the delete fails.
pub fn delete(conn: &Connection, id: &Id) -> Result<()> {
    let mut stmt = conn
        .prepare_cached("DELETE FROM sections WHERE id = :id")
        .map_err(|source| DbError::Sqlite { source })?;

    stmt.execute(named_params! { ":id": id.as_str() })
        .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for `section` row persistence (T-305, EPIC-04). All ids
    //! and names are fake fixture data; no real credentials or personal
    //! data.
    use rusqlite::Connection;

    use super::*;
    use crate::db::migrations;

    /// Opens a fresh in-memory database with migrations applied, same
    /// pattern as [`migrations::tests`].
    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        conn
    }

    fn fixture() -> Section {
        Section {
            id: Id::new("s1"),
            name: "Produce".to_string(),
            project_id: Id::new("p1"),
            section_order: 2,
            is_deleted: false,
            is_archived: false,
        }
    }

    fn row(conn: &Connection, id: &str) -> (String, String, i64, bool, bool, String) {
        conn.query_row(
            "SELECT project_id, name, section_order, is_archived, is_deleted, raw_json \
             FROM sections WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap()
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM sections", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn upsert_then_fetch_round_trips_every_mapped_column() {
        let conn = test_conn();
        let section = fixture();

        upsert(&conn, &section).unwrap();

        let (project_id, name, section_order, is_archived, is_deleted, _raw) = row(&conn, "s1");
        assert_eq!(project_id, section.project_id.as_str());
        assert_eq!(name, section.name);
        assert_eq!(section_order, section.section_order);
        assert_eq!(is_archived, section.is_archived);
        assert_eq!(is_deleted, section.is_deleted);
    }

    #[test]
    fn upsert_is_idempotent() {
        let conn = test_conn();
        let section = fixture();

        upsert(&conn, &section).unwrap();
        upsert(&conn, &section).unwrap();

        assert_eq!(row_count(&conn), 1);
        let first = row(&conn, "s1");
        let second = row(&conn, "s1");
        assert_eq!(first, second);
    }

    #[test]
    fn upsert_of_changed_resource_overwrites_every_column() {
        let conn = test_conn();
        let mut section = fixture();
        upsert(&conn, &section).unwrap();

        section.name = "Dairy".to_string();
        section.project_id = Id::new("p2");
        section.section_order = 7;
        section.is_archived = true;
        section.is_deleted = true;
        upsert(&conn, &section).unwrap();

        assert_eq!(row_count(&conn), 1);
        let (project_id, name, section_order, is_archived, is_deleted, _raw) = row(&conn, "s1");
        assert_eq!(project_id, "p2");
        assert_eq!(name, "Dairy");
        assert_eq!(section_order, 7);
        assert!(is_archived);
        assert!(is_deleted);
    }

    #[test]
    fn raw_json_round_trips_unmodelled_fields() {
        let conn = test_conn();
        let section = fixture();
        upsert(&conn, &section).unwrap();

        let (.., raw_json) = row(&conn, "s1");
        let parsed: Section = serde_json::from_str(&raw_json).unwrap();
        let reserialised = serde_json::to_string(&parsed).unwrap();
        let original = serde_json::to_string(&section).unwrap();
        assert_eq!(reserialised, original);
    }

    #[test]
    fn delete_missing_id_is_ok() {
        let conn = test_conn();

        delete(&conn, &Id::new("does-not-exist")).unwrap();

        assert_eq!(row_count(&conn), 0);
    }

    #[test]
    fn delete_removes_the_row() {
        let conn = test_conn();
        let section = fixture();
        upsert(&conn, &section).unwrap();

        delete(&conn, &section.id).unwrap();

        assert_eq!(row_count(&conn), 0);
    }
}
