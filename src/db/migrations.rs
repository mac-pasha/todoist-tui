//! Idempotent schema migration runner (T-303, EPIC-04).
//!
//! Versioning uses `PRAGMA user_version` as the single source of truth —
//! no separate `migrations` table — so "has migration N been applied?" is
//! just "is `user_version >= N`?". Each migration is a plain SQL string
//! (the base schema is embedded with `include_str!` so it ships inside the
//! binary and stays in lockstep with this file); `run` applies every
//! pending step in its own transaction, bumping `user_version` to that
//! step's number before committing, so a crash mid-migration cannot leave
//! the database on a version it hasn't actually reached.
//!
//! EPIC-05 adds a `commands` table as migration 2 by appending another
//! entry to [`MIGRATIONS`] — the runner itself does not change.

use rusqlite::Connection;

use crate::error::{DbError, Result};

/// Base schema: all tables and indices needed by the read queries (T-303).
const SCHEMA_V1: &str = include_str!("schema.sql");

/// Highest schema version this build knows how to read or migrate to.
///
/// A database whose `user_version` exceeds this (e.g. written by a newer
/// build) is refused in [`run`] rather than migrated, since this build has
/// no idea what columns/tables that future version expects.
// `MIGRATIONS.len()` is a handful of entries at most (one per schema
// version this crate will ever ship) and known at compile time, so the
// usize -> i32 cast can never truncate or wrap in practice; a `const fn`
// can't use `try_from`/`?`, so the lint is allowed locally instead of
// widening this to a non-const lookup.
#[allow(clippy::cast_possible_truncation, clippy::cast_possible_wrap)]
pub const SCHEMA_VERSION: i32 = MIGRATIONS.len() as i32;

/// Ordered list of migrations, index `0` is version `1`, index `1` is
/// version `2`, and so on — `PRAGMA user_version` after applying index `i`
/// is always `i + 1`. Appending a new `&str` here (e.g. EPIC-05's
/// `commands` table) adds migration `MIGRATIONS.len()` without touching
/// [`run`].
const MIGRATIONS: &[&str] = &[SCHEMA_V1];

/// Applies every migration not yet reflected in `conn`'s `PRAGMA
/// user_version`, each inside its own transaction, and returns the
/// resulting `user_version`.
///
/// Running this twice against the same connection/file is a no-op the
/// second time: every DDL statement in [`MIGRATIONS`] is `CREATE ... IF
/// NOT EXISTS`, and no pending migration remains once `user_version`
/// already equals [`SCHEMA_VERSION`], so nothing is re-applied and no
/// error is raised.
///
/// # Errors
///
/// Returns [`DbError::UnsupportedSchemaVersion`] when `conn`'s current
/// `user_version` is already greater than [`SCHEMA_VERSION`] (a newer
/// build wrote this database), and [`DbError::Migrate`] when a migration
/// step's SQL fails to execute or `user_version` cannot be updated.
pub fn run(conn: &mut Connection) -> Result<i32> {
    let current = user_version(conn)?;

    if current > SCHEMA_VERSION {
        return Err(DbError::UnsupportedSchemaVersion {
            found: current,
            supported: SCHEMA_VERSION,
        }
        .into());
    }

    for (index, sql) in MIGRATIONS.iter().enumerate() {
        let target = i32::try_from(index + 1).unwrap_or(i32::MAX);
        if target <= current {
            continue;
        }

        let tx = conn.transaction().map_err(|source| DbError::Migrate {
            from: current,
            to: target,
            source,
        })?;

        tx.execute_batch(sql).map_err(|source| DbError::Migrate {
            from: current,
            to: target,
            source,
        })?;

        tx.pragma_update(None, "user_version", target)
            .map_err(|source| DbError::Migrate {
                from: current,
                to: target,
                source,
            })?;

        tx.commit().map_err(|source| DbError::Migrate {
            from: current,
            to: target,
            source,
        })?;
    }

    user_version(conn)
}

/// Reads `PRAGMA user_version` off `conn`.
fn user_version(conn: &Connection) -> Result<i32> {
    conn.query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|source| DbError::Sqlite { source }.into())
}

#[cfg(test)]
mod tests {
    use rusqlite::Connection;

    use super::*;

    fn schema_objects(conn: &Connection) -> Vec<(String, String)> {
        let mut stmt = conn
            .prepare(
                "SELECT type, name FROM sqlite_master \
                 WHERE type IN ('table', 'index') AND name NOT LIKE 'sqlite_%' \
                 ORDER BY type, name",
            )
            .unwrap();
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap()
            .collect::<rusqlite::Result<Vec<(String, String)>>>()
            .unwrap()
    }

    #[test]
    fn run_creates_expected_tables_and_indices() {
        let mut conn = Connection::open_in_memory().unwrap();

        let version = run(&mut conn).unwrap();

        assert_eq!(version, SCHEMA_VERSION);

        let names: Vec<String> = schema_objects(&conn)
            .into_iter()
            .map(|(_, name)| name)
            .collect();
        for table in [
            "projects",
            "items",
            "sections",
            "labels",
            "notes",
            "filters",
            "user",
            "day_orders",
            "meta",
        ] {
            assert!(names.contains(&table.to_string()), "missing table {table}");
        }
        for index in [
            "idx_items_project_order",
            "idx_items_section_order",
            "idx_items_parent_order",
            "idx_items_due_date",
            "idx_items_checked",
        ] {
            assert!(names.contains(&index.to_string()), "missing index {index}");
        }
    }

    #[test]
    fn run_twice_is_a_no_op() {
        let mut conn = Connection::open_in_memory().unwrap();

        let first = run(&mut conn).unwrap();
        let objects_after_first = schema_objects(&conn);

        let second = run(&mut conn).unwrap();
        let objects_after_second = schema_objects(&conn);

        assert_eq!(first, second);
        assert_eq!(objects_after_first, objects_after_second);
        assert_eq!(objects_after_first.len(), objects_after_second.len());
    }

    #[test]
    fn run_rejects_newer_schema_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION + 1)
            .unwrap();

        let err = run(&mut conn).unwrap_err();

        match err {
            crate::error::AppError::Db(DbError::UnsupportedSchemaVersion { found, supported }) => {
                assert_eq!(found, SCHEMA_VERSION + 1);
                assert_eq!(supported, SCHEMA_VERSION);
            }
            other => panic!("expected UnsupportedSchemaVersion, got {other:?}"),
        }
    }
}
