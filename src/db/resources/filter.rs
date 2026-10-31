//! Persistence for the Sync API `filter` resource (T-305, EPIC-04).
//!
//! [`upsert`] maps a [`Filter`] onto the `filters` table's typed columns
//! (`name`, `query`, `item_order`, `is_deleted`, `is_favorite`) from
//! `schema.sql`, while the full `Filter` rides along verbatim in
//! `raw_json` so no field — modelled or not — is ever lost to a column
//! this task didn't need for the read path.

use rusqlite::{named_params, Connection};

use crate::api::model::{Filter, Id};
use crate::error::{DbError, Result};

/// Inserts `filter`, or overwrites the existing row with the same id.
///
/// Idempotent: calling `upsert` twice with the same (or an updated)
/// `Filter` leaves exactly one row for that id, matching the Sync API's
/// own upsert-by-id semantics for incremental syncs. Every mapped column
/// is overwritten on conflict.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if `filter` cannot be serialised to JSON
/// (not expected in practice — `Filter` has no fallible `Serialize`
/// impls) or [`DbError::Sqlite`] if the write fails.
pub fn upsert(conn: &Connection, filter: &Filter) -> Result<()> {
    let raw_json = serde_json::to_string(filter).map_err(|source| DbError::Decode { source })?;

    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO filters (
                id, name, query, item_order, is_deleted, is_favorite,
                raw_json
            ) VALUES (
                :id, :name, :query, :item_order, :is_deleted, :is_favorite,
                :raw_json
            )
            ON CONFLICT (id) DO UPDATE SET
                name = excluded.name,
                query = excluded.query,
                item_order = excluded.item_order,
                is_deleted = excluded.is_deleted,
                is_favorite = excluded.is_favorite,
                raw_json = excluded.raw_json",
        )
        .map_err(|source| DbError::Sqlite { source })?;

    stmt.execute(named_params! {
        ":id": filter.id.as_str(),
        ":name": filter.name,
        ":query": filter.query,
        ":item_order": filter.item_order,
        ":is_deleted": filter.is_deleted,
        ":is_favorite": filter.is_favorite,
        ":raw_json": raw_json,
    })
    .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

/// Deletes the filter row with the given id, if one exists.
///
/// Deleting an id with no matching row is not an error (mirrors the Sync
/// API's own idempotent delete-by-id semantics).
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the delete fails.
pub fn delete(conn: &Connection, id: &Id) -> Result<()> {
    let mut stmt = conn
        .prepare_cached("DELETE FROM filters WHERE id = :id")
        .map_err(|source| DbError::Sqlite { source })?;

    stmt.execute(named_params! { ":id": id.as_str() })
        .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for `filter` row persistence (T-305, EPIC-04). All ids,
    //! names and queries are fake fixture data; no real credentials or
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

    fn fixture() -> Filter {
        Filter {
            id: Id::new("f1"),
            name: "Today".to_string(),
            query: "today".to_string(),
            color: "berry_red".to_string(),
            item_order: 1,
            is_favorite: true,
            is_deleted: false,
        }
    }

    fn row(conn: &Connection, id: &str) -> (String, String, i64, bool, bool, String) {
        conn.query_row(
            "SELECT name, query, item_order, is_deleted, is_favorite, raw_json \
             FROM filters WHERE id = ?1",
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
        conn.query_row("SELECT COUNT(*) FROM filters", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn upsert_then_fetch_round_trips_every_mapped_column() {
        let conn = test_conn();
        let filter = fixture();

        upsert(&conn, &filter).unwrap();

        let (name, query, item_order, is_deleted, is_favorite, _raw) = row(&conn, "f1");
        assert_eq!(name, filter.name);
        assert_eq!(query, filter.query);
        assert_eq!(item_order, filter.item_order);
        assert_eq!(is_deleted, filter.is_deleted);
        assert_eq!(is_favorite, filter.is_favorite);
    }

    #[test]
    fn upsert_is_idempotent() {
        let conn = test_conn();
        let filter = fixture();

        upsert(&conn, &filter).unwrap();
        upsert(&conn, &filter).unwrap();

        assert_eq!(row_count(&conn), 1);
        let first = row(&conn, "f1");
        let second = row(&conn, "f1");
        assert_eq!(first, second);
    }

    #[test]
    fn upsert_of_changed_resource_overwrites_every_column() {
        let conn = test_conn();
        let mut filter = fixture();
        upsert(&conn, &filter).unwrap();

        filter.name = "Overdue".to_string();
        filter.query = "overdue".to_string();
        filter.item_order = 4;
        filter.is_favorite = false;
        filter.is_deleted = true;
        upsert(&conn, &filter).unwrap();

        assert_eq!(row_count(&conn), 1);
        let (name, query, item_order, is_deleted, is_favorite, _raw) = row(&conn, "f1");
        assert_eq!(name, "Overdue");
        assert_eq!(query, "overdue");
        assert_eq!(item_order, 4);
        assert!(is_deleted);
        assert!(!is_favorite);
    }

    #[test]
    fn raw_json_round_trips_unmodelled_fields() {
        let conn = test_conn();
        let filter = fixture();
        upsert(&conn, &filter).unwrap();

        let (.., raw_json) = row(&conn, "f1");
        let parsed: Filter = serde_json::from_str(&raw_json).unwrap();
        let reserialised = serde_json::to_string(&parsed).unwrap();
        let original = serde_json::to_string(&filter).unwrap();
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
        let filter = fixture();
        upsert(&conn, &filter).unwrap();

        delete(&conn, &filter.id).unwrap();

        assert_eq!(row_count(&conn), 0);
    }
}
