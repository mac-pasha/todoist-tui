//! Persistence for the Sync API `user` resource (T-305, EPIC-04).
//!
//! `user` is a single logical row describing the authenticated account
//! (see `schema.sql`'s comment on the `user` table): unlike the other
//! resources in this module, a `user` payload is never one of several
//! independently keyed rows, so [`upsert`] both writes the given [`User`]
//! and removes any other row that might be left over from a previous
//! account, keeping the table at exactly zero or one rows at all times.
//!
//! [`upsert`] maps a [`User`] onto the `user` table's typed columns
//! (`full_name`, `email`) from `schema.sql`, while the full `User` rides
//! along verbatim in `raw_json` so no field — modelled or not — is ever
//! lost to a column this task didn't need for the read path.

use rusqlite::{named_params, Connection};

use crate::api::model::{Id, User};
use crate::error::{DbError, Result};

/// Inserts or replaces the single `user` row with `user`.
///
/// Idempotent: calling `upsert` twice with the same (or an updated)
/// `User` leaves exactly one row, with every mapped column overwritten on
/// conflict. If a different account's row is already present (a
/// different id), it is removed first so the table never holds more than
/// one account's data at a time.
///
/// # Errors
///
/// Returns [`DbError::Decode`] if `user` cannot be serialised to JSON
/// (not expected in practice — `User` has no fallible `Serialize` impls)
/// or [`DbError::Sqlite`] if the write fails.
pub fn upsert(conn: &Connection, user: &User) -> Result<()> {
    let raw_json = serde_json::to_string(user).map_err(|source| DbError::Decode { source })?;

    let mut delete_others = conn
        .prepare_cached("DELETE FROM user WHERE id <> :id")
        .map_err(|source| DbError::Sqlite { source })?;
    delete_others
        .execute(named_params! { ":id": user.id.as_str() })
        .map_err(|source| DbError::Sqlite { source })?;

    let mut stmt = conn
        .prepare_cached(
            "INSERT INTO user (id, full_name, email, raw_json)
             VALUES (:id, :full_name, :email, :raw_json)
             ON CONFLICT (id) DO UPDATE SET
                full_name = excluded.full_name,
                email = excluded.email,
                raw_json = excluded.raw_json",
        )
        .map_err(|source| DbError::Sqlite { source })?;

    stmt.execute(named_params! {
        ":id": user.id.as_str(),
        ":full_name": user.full_name,
        ":email": user.email,
        ":raw_json": raw_json,
    })
    .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

/// Deletes the user row with the given id, if one exists.
///
/// Deleting an id with no matching row is not an error (mirrors the Sync
/// API's own idempotent delete-by-id semantics).
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the delete fails.
pub fn delete(conn: &Connection, id: &Id) -> Result<()> {
    let mut stmt = conn
        .prepare_cached("DELETE FROM user WHERE id = :id")
        .map_err(|source| DbError::Sqlite { source })?;

    stmt.execute(named_params! { ":id": id.as_str() })
        .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    //! Unit tests for `user` row persistence (T-305, EPIC-04). All ids,
    //! names and email addresses are fake fixture data; no real
    //! credentials or personal data.
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

    fn fixture() -> User {
        User {
            id: Id::new("u1"),
            full_name: "Ada Example".to_string(),
            email: "ada@example.test".to_string(),
            color: "#ff8581".to_string(),
            tz_info: crate::api::model::TzInfo::default(),
        }
    }

    fn row(conn: &Connection, id: &str) -> (String, String, String) {
        conn.query_row(
            "SELECT full_name, email, raw_json FROM user WHERE id = ?1",
            [id],
            |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
        )
        .unwrap()
    }

    fn row_count(conn: &Connection) -> i64 {
        conn.query_row("SELECT COUNT(*) FROM user", [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    fn upsert_then_fetch_round_trips_every_mapped_column() {
        let conn = test_conn();
        let user = fixture();

        upsert(&conn, &user).unwrap();

        let (full_name, email, _raw) = row(&conn, "u1");
        assert_eq!(full_name, user.full_name);
        assert_eq!(email, user.email);
    }

    #[test]
    fn upsert_is_idempotent() {
        let conn = test_conn();
        let user = fixture();

        upsert(&conn, &user).unwrap();
        upsert(&conn, &user).unwrap();

        assert_eq!(row_count(&conn), 1);
        let first = row(&conn, "u1");
        let second = row(&conn, "u1");
        assert_eq!(first, second);
    }

    #[test]
    fn upsert_of_changed_resource_overwrites_every_column() {
        let conn = test_conn();
        let mut user = fixture();
        upsert(&conn, &user).unwrap();

        user.full_name = "Grace Example".to_string();
        user.email = "grace@example.test".to_string();
        upsert(&conn, &user).unwrap();

        assert_eq!(row_count(&conn), 1);
        let (full_name, email, _raw) = row(&conn, "u1");
        assert_eq!(full_name, "Grace Example");
        assert_eq!(email, "grace@example.test");
    }

    #[test]
    fn raw_json_round_trips_unmodelled_fields() {
        let conn = test_conn();
        let user = fixture();
        upsert(&conn, &user).unwrap();

        let (_, _, raw_json) = row(&conn, "u1");
        let parsed: User = serde_json::from_str(&raw_json).unwrap();
        let reserialised = serde_json::to_string(&parsed).unwrap();
        let original = serde_json::to_string(&user).unwrap();
        assert_eq!(reserialised, original);
    }

    #[test]
    fn upsert_with_a_different_id_replaces_the_single_row() {
        let conn = test_conn();
        let first_user = fixture();
        upsert(&conn, &first_user).unwrap();

        let mut second_user = fixture();
        second_user.id = Id::new("u2");
        second_user.full_name = "Second Account".to_string();
        upsert(&conn, &second_user).unwrap();

        assert_eq!(row_count(&conn), 1);
        let (full_name, _, _) = row(&conn, "u2");
        assert_eq!(full_name, "Second Account");
        let old_row_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM user WHERE id = 'u1'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(old_row_count, 0);
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
        let user = fixture();
        upsert(&conn, &user).unwrap();

        delete(&conn, &user.id).unwrap();

        assert_eq!(row_count(&conn), 0);
    }
}
