//! `commands` table DDL (T-404, EPIC-05).
//!
//! One row per [`QueuedCommand`], keyed by its Sync API dedup `uuid`. The
//! caller owns the connection (EPIC-04 opens/migrates it); [`apply`] takes a
//! borrowed [`Connection`] — a `&Transaction` derefs to the same type, so
//! migration-runner callers work too.
//!
//! Every statement is `CREATE ... IF NOT EXISTS`, so [`apply`] is idempotent:
//! running it twice against the same connection succeeds and leaves exactly
//! one table and one index. Registering this DDL as EPIC-04 migration 2 is a
//! one-line change in `src/db/migrations.rs`, which is outside this task's
//! file scope; until that lands, callers apply it directly via [`apply`].

use rusqlite::Connection;

use crate::error::{QueueError, Result};

/// DDL for the offline command queue's `commands` table and its
/// drain-order index, ready to hand to a migration runner.
pub const COMMANDS_DDL: &str = "
CREATE TABLE IF NOT EXISTS commands (
    uuid TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    args TEXT NOT NULL,
    temp_id TEXT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    error TEXT NULL
);

CREATE INDEX IF NOT EXISTS idx_commands_state_created ON commands (state, created_at);
";

/// Applies [`COMMANDS_DDL`] to `conn`. Safe to call repeatedly.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] when the DDL fails to execute.
pub fn apply(conn: &Connection) -> Result<()> {
    conn.execute_batch(COMMANDS_DDL)
        .map_err(|source| QueueError::Storage { source })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    //! T-404 schema tests: column shape, index, idempotency, error mapping.
    use super::*;
    use crate::error::AppError;

    /// `(name, type, notnull, default, pk)` rows from `PRAGMA table_info`.
    type Col = (String, String, bool, Option<String>, bool);

    fn columns(conn: &Connection) -> Vec<Col> {
        let mut stmt = conn
            .prepare("PRAGMA table_info(commands)")
            .expect("prepare table_info");
        let rows = stmt
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, i64>(3)? != 0,
                    r.get::<_, Option<String>>(4)?,
                    r.get::<_, i64>(5)? != 0,
                ))
            })
            .expect("query table_info");
        rows.collect::<rusqlite::Result<Vec<_>>>()
            .expect("collect columns")
    }

    fn count_master(conn: &Connection, kind: &str, name: &str) -> i64 {
        conn.query_row(
            "SELECT COUNT(*) FROM sqlite_master WHERE type = ?1 AND name = ?2",
            [kind, name],
            |r| r.get(0),
        )
        .expect("count sqlite_master")
    }

    fn fresh() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        apply(&conn).expect("apply");
        conn
    }

    #[test]
    fn ddl_creates_commands_table_with_exact_columns() {
        let conn = fresh();
        let col = |n: &str, t: &str, nn: bool, d: Option<&str>, pk: bool| -> Col {
            (n.into(), t.into(), nn, d.map(String::from), pk)
        };
        let expected = vec![
            col("uuid", "TEXT", false, None, true),
            col("type", "TEXT", true, None, false),
            col("args", "TEXT", true, None, false),
            col("temp_id", "TEXT", false, None, false),
            col("state", "TEXT", true, None, false),
            col("attempts", "INTEGER", true, Some("0"), false),
            col("created_at", "INTEGER", true, None, false),
            col("error", "TEXT", false, None, false),
        ];
        assert_eq!(columns(&conn), expected);
    }

    #[test]
    fn ddl_creates_index_on_state_then_created_at() {
        let conn = fresh();
        let idx: Vec<String> = conn
            .prepare("PRAGMA index_list(commands)")
            .expect("prepare index_list")
            .query_map([], |r| r.get::<_, String>(1))
            .expect("query index_list")
            .collect::<rusqlite::Result<_>>()
            .expect("collect");
        assert!(
            idx.iter().any(|n| n == "idx_commands_state_created"),
            "index missing: {idx:?}"
        );
        let cols: Vec<String> = conn
            .prepare("PRAGMA index_info(idx_commands_state_created)")
            .expect("prepare index_info")
            .query_map([], |r| r.get::<_, String>(2))
            .expect("query index_info")
            .collect::<rusqlite::Result<_>>()
            .expect("collect");
        assert_eq!(cols, vec!["state", "created_at"]);
    }

    #[test]
    fn apply_twice_succeeds_and_leaves_one_table_and_one_index() {
        let conn = fresh();
        apply(&conn).expect("second apply");
        assert_eq!(count_master(&conn, "table", "commands"), 1);
        assert_eq!(
            count_master(&conn, "index", "idx_commands_state_created"),
            1
        );
    }

    #[test]
    fn apply_twice_keeps_existing_rows() {
        let conn = fresh();
        conn.execute(
            "INSERT INTO commands (uuid, type, args, state, created_at) \
             VALUES ('u-1', 'item_add', '{}', 'pending', 1)",
            [],
        )
        .expect("insert");
        apply(&conn).expect("second apply");
        let n: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
            .expect("count");
        assert_eq!(n, 1, "re-apply must not drop data");
    }

    #[test]
    fn attempts_defaults_to_zero_and_not_null_columns_reject_null() {
        let conn = fresh();
        conn.execute(
            "INSERT INTO commands (uuid, type, args, state, created_at) \
             VALUES ('u-1', 'item_add', '{}', 'pending', 1)",
            [],
        )
        .expect("insert without attempts");
        let attempts: i64 = conn
            .query_row("SELECT attempts FROM commands", [], |r| r.get(0))
            .expect("read attempts");
        assert_eq!(attempts, 0);
        let res = conn.execute(
            "INSERT INTO commands (uuid, type, args, state, created_at) \
             VALUES ('u-2', NULL, '{}', 'pending', 1)",
            [],
        );
        assert!(res.is_err(), "NULL type must be rejected");
    }

    #[test]
    fn apply_failure_maps_to_queue_storage_error() {
        let conn = Connection::open_in_memory().expect("open");
        // A view named `commands` turns CREATE TABLE IF NOT EXISTS into a
        // no-op, but CREATE INDEX on a view fails.
        conn.execute_batch(
            "CREATE VIEW commands AS SELECT 1 AS uuid, 1 AS state, 1 AS created_at;",
        )
        .expect("create view");
        match apply(&conn) {
            Err(AppError::Queue(QueueError::Storage { .. })) => {}
            other => panic!("expected Queue(Storage), got {other:?}"),
        }
    }
}
