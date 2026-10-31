//! Persistence for the Sync API `project` resource (T-305, EPIC-04).
//!
//! [`upsert`] maps a [`Project`] onto the `projects` table's typed columns
//! (`name`, `parent_id`, `child_order`, `color`, `is_archived`,
//! `is_deleted`, `is_favorite`) from `schema.sql`, while the full
//! `Project` rides along verbatim in `raw_json` so no field — modelled or
//! not — is ever lost to a column this task didn't need for the read
//! path.

use rusqlite::{named_params, Connection};

use crate::api::model::{Id, Project};
use crate::error::{DbError, Result};

/// Inserts `project`, or overwrites the existing row with the same id.
///
/// Idempotent: calling `upsert` twice with the same (or an updated)
/// `Project` leaves exactly one row for that id, matching the Sync API's
/// own upsert-by-id semantics for incremental syncs. Every mapped column
/// is overwritten on conflict — including setting `parent_id` back to
/// `NULL` when [`Project::parent_id`] is `None` — so a changed resource
/// never leaves a stale value behind.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if `project` cannot be serialised to JSON
/// (not expected in practice — `Project` has no fallible `Serialize`
/// impls) or [`DbError::Sqlite`] if the write fails.
pub fn upsert(conn: &Connection, project: &Project) -> Result<()> {
    let raw_json = serde_json::to_string(project).map_err(|source| DbError::Decode { source })?;

    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO projects (
                id, name, parent_id, child_order, color, is_archived,
                is_deleted, is_favorite, raw_json
            ) VALUES (
                :id, :name, :parent_id, :child_order, :color, :is_archived,
                :is_deleted, :is_favorite, :raw_json
            )
            ON CONFLICT (id) DO UPDATE SET
                name = excluded.name,
                parent_id = excluded.parent_id,
                child_order = excluded.child_order,
                color = excluded.color,
                is_archived = excluded.is_archived,
                is_deleted = excluded.is_deleted,
                is_favorite = excluded.is_favorite,
                raw_json = excluded.raw_json",
        )
        .map_err(|source| DbError::Sqlite { source })?;

    stmt.execute(named_params! {
        ":id": project.id.as_str(),
        ":name": project.name,
        ":parent_id": project.parent_id.as_ref().map(Id::as_str),
        ":child_order": project.child_order,
        ":color": project.color,
        ":is_archived": project.is_archived,
        ":is_deleted": project.is_deleted,
        ":is_favorite": project.is_favorite,
        ":raw_json": raw_json,
    })
    .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

/// Deletes the project row with the given id, if one exists.
///
/// Deleting an id with no matching row is not an error (mirrors the Sync
/// API's own idempotent delete-by-id semantics).
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the delete fails.
pub fn delete(conn: &Connection, id: &Id) -> Result<()> {
    let mut stmt = conn
        .prepare_cached("DELETE FROM projects WHERE id = :id")
        .map_err(|source| DbError::Sqlite { source })?;

    stmt.execute(named_params! { ":id": id.as_str() })
        .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for `project` row persistence (T-305, EPIC-04). All ids,
    //! names and colours are fake fixture data; no real credentials or
    //! personal data.
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

    fn fixture() -> Project {
        Project {
            id: Id::new("p1"),
            name: "Groceries".to_string(),
            parent_id: Some(Id::new("p0")),
            child_order: 3,
            color: "berry_red".to_string(),
            is_favorite: true,
            is_archived: false,
            is_deleted: false,
            view_style: "list".to_string(),
        }
    }

    #[allow(clippy::type_complexity)]
    fn row(
        conn: &Connection,
        id: &str,
    ) -> (
        String,
        Option<String>,
        i64,
        String,
        bool,
        bool,
        bool,
        String,
    ) {
        conn.query_row(
            "SELECT name, parent_id, child_order, color, is_archived, \
             is_deleted, is_favorite, raw_json FROM projects WHERE id = ?1",
            [id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                    r.get(6)?,
                    r.get(7)?,
                ))
            },
        )
        .unwrap()
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM projects", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn upsert_then_fetch_round_trips_every_mapped_column() {
        let conn = test_conn();
        let project = fixture();

        upsert(&conn, &project).unwrap();

        let (name, parent_id, child_order, color, is_archived, is_deleted, is_favorite, _raw) =
            row(&conn, "p1");
        assert_eq!(name, project.name);
        assert_eq!(parent_id.as_deref(), Some("p0"));
        assert_eq!(child_order, project.child_order);
        assert_eq!(color, project.color);
        assert_eq!(is_archived, project.is_archived);
        assert_eq!(is_deleted, project.is_deleted);
        assert_eq!(is_favorite, project.is_favorite);
    }

    #[test]
    fn upsert_is_idempotent() {
        let conn = test_conn();
        let project = fixture();

        upsert(&conn, &project).unwrap();
        upsert(&conn, &project).unwrap();

        assert_eq!(row_count(&conn), 1);
        let first = row(&conn, "p1");
        let second = row(&conn, "p1");
        assert_eq!(first, second);
    }

    #[test]
    fn upsert_of_changed_resource_overwrites_columns_including_null() {
        let conn = test_conn();
        let mut project = fixture();
        upsert(&conn, &project).unwrap();

        // Change every mapped column, and clear the nullable `parent_id`.
        project.name = "Shopping".to_string();
        project.parent_id = None;
        project.child_order = 9;
        project.color = "mint_green".to_string();
        project.is_archived = true;
        project.is_deleted = true;
        project.is_favorite = false;
        upsert(&conn, &project).unwrap();

        assert_eq!(row_count(&conn), 1);
        let (name, parent_id, child_order, color, is_archived, is_deleted, is_favorite, _raw) =
            row(&conn, "p1");
        assert_eq!(name, "Shopping");
        assert_eq!(parent_id, None);
        assert_eq!(child_order, 9);
        assert_eq!(color, "mint_green");
        assert!(is_archived);
        assert!(is_deleted);
        assert!(!is_favorite);
    }

    #[test]
    fn raw_json_round_trips_unmodelled_fields() {
        let conn = test_conn();
        let project = fixture();
        upsert(&conn, &project).unwrap();

        let (.., raw_json) = row(&conn, "p1");
        let parsed: Project = serde_json::from_str(&raw_json).unwrap();
        let reserialised = serde_json::to_string(&parsed).unwrap();
        let original = serde_json::to_string(&project).unwrap();
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
        let project = fixture();
        upsert(&conn, &project).unwrap();

        delete(&conn, &project.id).unwrap();

        assert_eq!(row_count(&conn), 0);
    }
}
