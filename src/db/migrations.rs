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

    #[test]
    fn fresh_db_starts_at_zero_with_no_objects() {
        let conn = Connection::open_in_memory().unwrap();
        assert_eq!(user_version(&conn).unwrap(), 0);
        assert_eq!(schema_objects(&conn).len(), 0);
    }

    #[test]
    fn run_repeatedly_is_stable() {
        let mut conn = Connection::open_in_memory().unwrap();
        let first = run(&mut conn).unwrap();
        let objects = schema_objects(&conn);
        for _ in 0..5 {
            assert_eq!(run(&mut conn).unwrap(), first);
            assert_eq!(schema_objects(&conn), objects);
        }
        assert_eq!(user_version(&conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn run_exact_object_set() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        let objs = schema_objects(&conn);
        let tables: Vec<&str> = objs
            .iter()
            .filter(|(t, _)| t == "table")
            .map(|(_, n)| n.as_str())
            .collect();
        assert_eq!(
            tables,
            [
                "day_orders",
                "filters",
                "items",
                "labels",
                "meta",
                "notes",
                "projects",
                "sections",
                "user"
            ]
        );
        let indices = objs.iter().filter(|(t, _)| t == "index").count();
        assert_eq!(indices, 5);
    }

    #[test]
    fn indices_cover_expected_columns() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        let cols = |idx: &str| -> Vec<String> {
            let mut stmt = conn.prepare(&format!("PRAGMA index_info({idx})")).unwrap();
            stmt.query_map([], |r| r.get::<_, String>(2))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap()
        };
        assert_eq!(
            cols("idx_items_project_order"),
            ["project_id", "child_order"]
        );
        assert_eq!(
            cols("idx_items_section_order"),
            ["section_id", "child_order"]
        );
        assert_eq!(cols("idx_items_parent_order"), ["parent_id", "child_order"]);
        assert_eq!(cols("idx_items_due_date"), ["due_date"]);
        assert_eq!(cols("idx_items_checked"), ["checked"]);
    }

    #[test]
    fn resource_tables_have_raw_json_column() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        for t in [
            "projects",
            "items",
            "sections",
            "labels",
            "notes",
            "filters",
            "user",
            "day_orders",
        ] {
            let mut stmt = conn.prepare(&format!("PRAGMA table_info({t})")).unwrap();
            let names: Vec<String> = stmt
                .query_map([], |r| r.get(1))
                .unwrap()
                .collect::<rusqlite::Result<_>>()
                .unwrap();
            assert!(
                names.contains(&"raw_json".to_string()),
                "{t} lacks raw_json"
            );
        }
    }

    #[test]
    fn run_preserves_existing_data() {
        let mut conn = Connection::open_in_memory().unwrap();
        run(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('sync_token', 'abc')",
            [],
        )
        .unwrap();
        run(&mut conn).unwrap();
        let v: String = conn
            .query_row("SELECT value FROM meta WHERE key='sync_token'", [], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(v, "abc");
    }

    #[test]
    fn run_upgrades_from_version_zero_with_preexisting_tables() {
        // Simulates a crashed/partial earlier attempt: IF NOT EXISTS must tolerate it.
        let mut conn = Connection::open_in_memory().unwrap();
        conn.execute_batch("CREATE TABLE meta (key TEXT PRIMARY KEY, value TEXT);")
            .unwrap();
        assert_eq!(run(&mut conn).unwrap(), SCHEMA_VERSION);
    }

    #[test]
    fn future_versions_report_exact_found_and_supported() {
        for found in [SCHEMA_VERSION + 1, 1000, i32::MAX] {
            let mut conn = Connection::open_in_memory().unwrap();
            conn.pragma_update(None, "user_version", found).unwrap();
            match run(&mut conn).unwrap_err() {
                crate::error::AppError::Db(DbError::UnsupportedSchemaVersion {
                    found: f,
                    supported,
                }) => {
                    assert_eq!(f, found);
                    assert_eq!(supported, SCHEMA_VERSION);
                }
                other => panic!("unexpected {other:?}"),
            }
            // Refusal must not touch the database or version.
            assert_eq!(schema_objects(&conn).len(), 0);
            assert_eq!(user_version(&conn).unwrap(), found);
        }
    }

    #[test]
    fn current_version_with_no_tables_is_not_remigrated() {
        let mut conn = Connection::open_in_memory().unwrap();
        conn.pragma_update(None, "user_version", SCHEMA_VERSION)
            .unwrap();
        assert_eq!(run(&mut conn).unwrap(), SCHEMA_VERSION);
        assert_eq!(schema_objects(&conn).len(), 0);
    }

    #[test]
    fn file_backed_db_persists_version_across_connections() {
        let dir = std::env::temp_dir().join(format!("todoist-tui-t303-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("db.sqlite");
        let _ = std::fs::remove_file(&path);
        {
            let mut c = Connection::open(&path).unwrap();
            assert_eq!(run(&mut c).unwrap(), SCHEMA_VERSION);
        }
        let mut c2 = Connection::open(&path).unwrap();
        let objs = schema_objects(&c2);
        assert_eq!(run(&mut c2).unwrap(), SCHEMA_VERSION);
        assert_eq!(schema_objects(&c2), objs);
        drop(c2);
        let _ = std::fs::remove_dir_all(&dir);
    }

    #[test]
    fn failed_migration_rolls_back_and_keeps_version() {
        let mut conn = Connection::open_in_memory().unwrap();
        // A legacy `items` table lacking the indexed columns makes CREATE INDEX fail
        // after earlier tables were created; everything must roll back.
        conn.execute_batch("CREATE TABLE items (id TEXT PRIMARY KEY);")
            .unwrap();
        let err = run(&mut conn).unwrap_err();
        assert!(matches!(
            err,
            crate::error::AppError::Db(DbError::Migrate { from: 0, to: 1, .. })
        ));
        assert_eq!(user_version(&conn).unwrap(), 0);
        let tables: Vec<_> = schema_objects(&conn)
            .into_iter()
            .filter(|(t, _)| t == "table")
            .collect();
        assert_eq!(tables, [("table".to_string(), "items".to_string())]);
    }
}
