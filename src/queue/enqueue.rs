//! Atomic command enqueue with optimistic local cache apply (T-405, EPIC-05).
//!
//! Every mutating user action (EPIC-08/09) records a [`QueuedCommand`] in the
//! `commands` table and applies its optimistic mutation to the local SQLite
//! cache inside a single [`rusqlite::Transaction`]. Either both writes commit
//! together or both roll back, so a crash or error mid-enqueue never leaves a
//! phantom cache row without a queued command or an unapplied command in the
//! queue.
//!
//! This module is completely transport-agnostic and performs no network I/O.

use rusqlite::{Connection, Transaction};

use crate::error::{QueueError, Result};
use crate::log::Secret;
use crate::queue::command::QueuedCommand;
use crate::queue::store::CommandStore;

/// Enqueues `command` into the `commands` table and runs `local_apply` inside
/// a single SQLite transaction, committing only when both succeed.
///
/// If either `local_apply` or the `commands` insert returns an error, the
/// transaction is rolled back so neither the optimistic cache mutation nor the
/// queued command row persists.
///
/// Command arguments may contain user-authored task/note text and are never
/// logged at `info` level; at `debug` level they are wrapped in [`Secret`] so
/// only the redaction marker (`***`) is rendered.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] when starting or committing the SQLite
/// transaction or inserting into `commands` fails, or any error returned by
/// `local_apply`.
pub fn enqueue<F>(conn: &mut Connection, command: &QueuedCommand, local_apply: F) -> Result<()>
where
    F: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let tx = conn
        .transaction()
        .map_err(|source| QueueError::Storage { source })?;

    let local_res = local_apply(&tx);
    let insert_res = CommandStore::new(&tx).insert(command);
    local_res?;
    insert_res?;

    tx.commit()
        .map_err(|source| QueueError::Storage { source })?;

    tracing::debug!(
        uuid = %command.uuid,
        kind = %command.kind,
        args = ?Secret::new(&command.args),
        "enqueued command"
    );

    Ok(())
}

/// Enqueues an ordered slice of `commands` and runs `local_apply` inside a
/// single SQLite transaction, committing only when all inserts and
/// `local_apply` succeed.
///
/// Commands are inserted in slice order and their `created_at` timestamps are
/// kept monotonically non-decreasing across the batch so
/// [`CommandStore::pending`] (which orders by `created_at ASC, rowid ASC`)
/// always returns the batch in the exact slice order supplied by the caller
/// (for example, `project_add` with temporary ID `A` followed by `item_add`
/// referencing `A`).
///
/// # Errors
///
/// Returns [`QueueError::Storage`] when starting or committing the SQLite
/// transaction or inserting any command fails, or any error returned by
/// `local_apply`.
pub fn enqueue_many<F>(
    conn: &mut Connection,
    commands: &[QueuedCommand],
    local_apply: F,
) -> Result<()>
where
    F: FnOnce(&Transaction<'_>) -> Result<()>,
{
    let tx = conn
        .transaction()
        .map_err(|source| QueueError::Storage { source })?;

    let local_res = local_apply(&tx);
    let insert_res = insert_ordered_batch(&tx, commands);
    local_res?;
    insert_res?;

    tx.commit()
        .map_err(|source| QueueError::Storage { source })?;

    for cmd in commands {
        tracing::debug!(
            uuid = %cmd.uuid,
            kind = %cmd.kind,
            args = ?Secret::new(&cmd.args),
            "enqueued command"
        );
    }

    Ok(())
}

/// Inserts `commands` in slice order, clamping any decreasing `created_at`
/// value to the previous command's timestamp so `ORDER BY created_at, rowid`
/// preserves the caller's batch order.
fn insert_ordered_batch(tx: &Transaction<'_>, commands: &[QueuedCommand]) -> Result<()> {
    let store = CommandStore::new(tx);
    let mut min_created_at: Option<i64> = None;

    for cmd in commands {
        let effective_created_at = match min_created_at {
            Some(prev) if cmd.created_at < prev => prev,
            _ => cmd.created_at,
        };
        min_created_at = Some(effective_created_at);

        if effective_created_at == cmd.created_at {
            store.insert(cmd)?;
        } else {
            let mut adjusted = cmd.clone();
            adjusted.created_at = effective_created_at;
            store.insert(&adjusted)?;
        }
    }

    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Unit tests for T-405 atomic enqueue and optimistic local apply.
    //!
    //! Uses a throwaway `cache_items` table to stand in for the EPIC-04 cache
    //! tables. All UUIDs, temp IDs, tokens, and payloads are fake fixtures.
    use std::io;
    use std::sync::{Arc, Mutex};

    use rusqlite::params;
    use serde_json::json;
    use tracing_subscriber::layer::SubscriberExt;
    use tracing_subscriber::EnvFilter;
    use uuid::Uuid;

    use super::*;
    use crate::error::{AppError, DbError};
    use crate::log::REDACTED;
    use crate::queue::command::CommandState;
    use crate::queue::schema;

    fn setup_db() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        schema::apply(&conn).expect("apply queue schema");
        conn.execute(
            "CREATE TABLE cache_items (id TEXT PRIMARY KEY, content TEXT NOT NULL, project_id TEXT)",
            [],
        )
        .expect("create stand-in cache table");
        conn
    }

    fn fixture_uuid(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    fn fixture_command(n: u8, kind: &str, created_at: i64) -> QueuedCommand {
        QueuedCommand {
            uuid: fixture_uuid(n),
            kind: kind.to_string(),
            args: json!({
                "content": format!("fixture task {n}"),
                "project_id": "proj_fake_1",
            }),
            temp_id: Some(format!("temp_item_{n}")),
            state: CommandState::Pending,
            attempts: 0,
            created_at,
            error: None,
        }
    }

    fn count_rows(conn: &Connection, table: &str) -> i64 {
        let sql = format!("SELECT COUNT(*) FROM {table}");
        conn.query_row(&sql, [], |row| row.get(0))
            .expect("count rows")
    }

    #[test]
    fn enqueue_commits_command_and_optimistic_cache_mutation_in_one_transaction() {
        let mut conn = setup_db();
        let cmd = fixture_command(1, "item_add", 1_000);

        enqueue(&mut conn, &cmd, |tx| {
            tx.execute(
                "INSERT INTO cache_items (id, content, project_id) VALUES (?1, ?2, ?3)",
                params!["temp_item_1", "fixture task 1", "proj_fake_1"],
            )
            .map_err(|source| DbError::Sqlite { source })?;
            Ok(())
        })
        .expect("enqueue succeeds");

        let pending = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending, vec![cmd]);

        let cached: (String, String, String) = conn
            .query_row(
                "SELECT id, content, project_id FROM cache_items WHERE id = 'temp_item_1'",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .expect("cached item exists");
        assert_eq!(
            cached,
            (
                "temp_item_1".to_string(),
                "fixture task 1".to_string(),
                "proj_fake_1".to_string()
            )
        );
    }

    #[test]
    fn local_apply_error_rolls_back_both_queued_command_and_cache_mutation() {
        let mut conn = setup_db();
        let cmd = fixture_command(2, "item_add", 1_000);

        let err = enqueue(&mut conn, &cmd, |tx| {
            // Perform a real mutation before failing so we prove the transaction rolls it back.
            tx.execute(
                "INSERT INTO cache_items (id, content, project_id) VALUES ('temp_item_2', 'partial', 'proj_fake_1')",
                [],
            )
            .map_err(|source| DbError::Sqlite { source })?;

            Err(DbError::Sqlite {
                source: rusqlite::Error::InvalidQuery,
            }
            .into())
        })
        .expect_err("enqueue must fail when local_apply errors");

        assert!(matches!(err, AppError::Db(DbError::Sqlite { .. })));
        assert_eq!(
            count_rows(&conn, "commands"),
            0,
            "queued command row must not exist after local_apply failure"
        );
        assert_eq!(
            count_rows(&conn, "cache_items"),
            0,
            "mutated cache row must not exist after local_apply failure"
        );
    }

    #[test]
    fn command_insert_failure_rolls_back_optimistic_cache_mutation() {
        // Open connection with cache_items table, and install a BEFORE INSERT trigger on
        // `commands` that aborts the command insert after `local_apply` has mutated `cache_items`.
        let mut conn = setup_db();
        conn.execute_batch(
            "CREATE TRIGGER reject_command_insert BEFORE INSERT ON commands \
             BEGIN \
                 SELECT RAISE(ABORT, 'simulated commands table write failure'); \
             END;",
        )
        .expect("create abort trigger");

        let cmd = fixture_command(3, "item_add", 1_000);
        let mut local_apply_ran = false;

        let err = enqueue(&mut conn, &cmd, |tx| {
            local_apply_ran = true;
            tx.execute(
                "INSERT INTO cache_items (id, content, project_id) VALUES ('temp_item_3', 'optimistic row', 'proj_fake_1')",
                [],
            )
            .map_err(|source| DbError::Sqlite { source })?;
            Ok(())
        })
        .expect_err("enqueue must fail when commands insert fails");

        assert!(local_apply_ran, "local_apply must have executed in the tx");
        assert!(matches!(err, AppError::Queue(QueueError::Storage { .. })));
        assert_eq!(
            count_rows(&conn, "commands"),
            0,
            "no command row may exist when commands insert fails"
        );
        assert_eq!(
            count_rows(&conn, "cache_items"),
            0,
            "optimistic cache mutation must be rolled back when commands insert fails"
        );
    }

    #[test]
    fn enqueue_many_writes_ordered_batch_in_single_transaction() {
        let mut conn = setup_db();
        conn.execute(
            "CREATE TABLE cache_projects (id TEXT PRIMARY KEY, name TEXT NOT NULL)",
            [],
        )
        .expect("create cache_projects");

        let temp_proj = "temp_proj_A";
        let project_add = QueuedCommand {
            uuid: fixture_uuid(10),
            kind: "project_add".to_string(),
            args: json!({ "name": "New Project Fixture" }),
            temp_id: Some(temp_proj.to_string()),
            state: CommandState::Pending,
            attempts: 0,
            created_at: 2_000,
            error: None,
        };
        // Even if the second command has an earlier or identical timestamp, enqueue_many
        // must preserve the slice order in `pending()`.
        let item_add = QueuedCommand {
            uuid: fixture_uuid(11),
            kind: "item_add".to_string(),
            args: json!({
                "content": "First task in temp project",
                "project_id": temp_proj,
            }),
            temp_id: Some("temp_item_B".to_string()),
            state: CommandState::Pending,
            attempts: 0,
            created_at: 1_500,
            error: None,
        };

        let batch = vec![project_add.clone(), item_add.clone()];
        enqueue_many(&mut conn, &batch, |tx| {
            tx.execute(
                "INSERT INTO cache_projects (id, name) VALUES (?1, ?2)",
                params![temp_proj, "New Project Fixture"],
            )
            .map_err(|source| DbError::Sqlite { source })?;
            tx.execute(
                "INSERT INTO cache_items (id, content, project_id) VALUES (?1, ?2, ?3)",
                params!["temp_item_B", "First task in temp project", temp_proj],
            )
            .map_err(|source| DbError::Sqlite { source })?;
            Ok(())
        })
        .expect("enqueue_many succeeds");

        let pending = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending.len(), 2);
        assert_eq!(pending[0].uuid, project_add.uuid);
        assert_eq!(pending[0].kind, "project_add");
        assert_eq!(pending[0].temp_id.as_deref(), Some(temp_proj));
        assert_eq!(pending[1].uuid, item_add.uuid);
        assert_eq!(pending[1].kind, "item_add");
        assert_eq!(pending[1].args["project_id"], json!(temp_proj));

        assert_eq!(count_rows(&conn, "cache_projects"), 1);
        assert_eq!(count_rows(&conn, "cache_items"), 1);
    }

    #[test]
    fn enqueue_many_rolls_back_entire_batch_on_error() {
        let mut conn = setup_db();
        let c1 = fixture_command(20, "project_add", 1_000);
        let c2 = fixture_command(21, "item_add", 1_000);

        let res = enqueue_many(&mut conn, &[c1, c2], |tx| {
            tx.execute(
                "INSERT INTO cache_items (id, content, project_id) VALUES ('temp_item_20', 'task', 'proj_fake_1')",
                [],
            )
            .map_err(|source| DbError::Sqlite { source })?;
            Err(DbError::Sqlite {
                source: rusqlite::Error::InvalidQuery,
            }
            .into())
        });

        assert!(res.is_err());
        assert_eq!(count_rows(&conn, "commands"), 0);
        assert_eq!(count_rows(&conn, "cache_items"), 0);
    }

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);

    impl SharedBuf {
        fn contents(&self) -> String {
            let bytes = self
                .0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .clone();
            String::from_utf8(bytes).expect("valid utf8 log output")
        }
    }

    struct BufWriter(Arc<Mutex<Vec<u8>>>);

    impl io::Write for BufWriter {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner)
                .extend_from_slice(buf);
            Ok(buf.len())
        }

        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    impl<'a> tracing_subscriber::fmt::MakeWriter<'a> for SharedBuf {
        type Writer = BufWriter;

        fn make_writer(&'a self) -> Self::Writer {
            BufWriter(Arc::clone(&self.0))
        }
    }

    #[test]
    fn command_args_never_logged_at_info_and_redacted_via_secret_at_debug() {
        let mut conn = setup_db();
        let mut cmd = fixture_command(99, "note_add", 1_000);
        cmd.args = json!({
            "content": "secret_note_body",
            "token_hint": "tok_fake_abc123",
        });

        // 1. At `info` filter level: nothing from enqueue is logged, and args never appear.
        let info_buf = SharedBuf::default();
        let info_subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("info"))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(info_buf.clone())
                    .with_ansi(false),
            );
        tracing::subscriber::with_default(info_subscriber, || {
            enqueue(&mut conn, &cmd, |_| Ok(())).expect("enqueue at info");
        });
        let info_logs = info_buf.contents();
        assert!(
            !info_logs.contains("secret_note_body"),
            "args payload must never appear in info logs: {info_logs}"
        );
        assert!(
            !info_logs.contains("tok_fake_abc123"),
            "token fixture must never appear in info logs: {info_logs}"
        );
        assert!(
            info_logs.is_empty(),
            "enqueue must not emit info-level logs, got: {info_logs}"
        );

        // 2. At `debug` filter level: event is logged with `Secret` redaction (`***`),
        //    so raw args still never appear in log output.
        let debug_buf = SharedBuf::default();
        let debug_subscriber = tracing_subscriber::registry()
            .with(EnvFilter::new("debug"))
            .with(
                tracing_subscriber::fmt::layer()
                    .with_writer(debug_buf.clone())
                    .with_ansi(false),
            );
        let mut cmd2 = fixture_command(100, "note_add", 2_000);
        cmd2.args = json!({
            "content": "secret_note_body",
            "token_hint": "tok_fake_abc123",
        });
        tracing::subscriber::with_default(debug_subscriber, || {
            enqueue(&mut conn, &cmd2, |_| Ok(())).expect("enqueue at debug");
        });
        let debug_logs = debug_buf.contents();
        assert!(
            !debug_logs.contains("secret_note_body"),
            "args payload must be redacted by Secret at debug level: {debug_logs}"
        );
        assert!(
            !debug_logs.contains("tok_fake_abc123"),
            "token fixture must be redacted by Secret at debug level: {debug_logs}"
        );
        assert!(
            debug_logs.contains(REDACTED),
            "debug log must use Secret redaction marker: {debug_logs}"
        );
    }
}
