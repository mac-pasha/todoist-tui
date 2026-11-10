//! Per-command `sync_status` processing, error classification, and rollback hooks (T-408, EPIC-05).
//!
//! Implements the error classification and keep-vs-rollback matrix defined in
//! `docs/manual/epic-05-error-classes.md`:
//! - [`classify`] maps a [`CommandError`] to an [`ErrorClass`] by inspecting
//!   `error_code` only (never the server's human-readable `error` text),
//!   defaulting to [`ErrorClass::Unknown`] when `error_code` is missing or
//!   unrecognised.
//! - [`apply_sync_status`] processes the outcomes of a flushed command batch:
//!   - `"ok"` commands are removed from the `commands` table;
//!   - errored commands are classified, invoke the caller's `rollback` hook
//!     when [`ErrorClass::should_rollback`] is `true`, and are marked
//!     [`CommandState::Failed`] with their `error` message stored in the
//!     `error` column;
//!   - commands absent from `sync_status` (ambiguous outcome) return to
//!     [`CommandState::Pending`] with their original `uuid` so a subsequent
//!     flush safely re-sends them under UUID idempotency.
//! - [`failed_commands`] queries all failed commands for UI surfacing (EPIC-07).

use std::collections::HashMap;

use rusqlite::{params, Connection, Transaction};

use crate::api::sync::{CommandError, CommandStatus};
use crate::error::{QueueError, Result};
use crate::queue::command::{CommandState, QueuedCommand};
use crate::queue::store::CommandStore;

/// Classification of a per-command Sync API error, matching §2 and §3 of
/// `docs/manual/epic-05-error-classes.md` 1:1.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ErrorClass {
    /// The command targets an object that does not exist server-side.
    NotFound,
    /// The command itself is malformed, or built on stale/invalid state.
    InvalidArgument,
    /// The server refuses the action for this account or project.
    PermissionDenied,
    /// The command was throttled; nothing is wrong with the change itself.
    RateLimited,
    /// A server-side error expected to clear on its own.
    ServerTransient,
    /// Fallback for any unlisted or absent `error_code` (§4 of
    /// `docs/manual/epic-05-error-classes.md`).
    Unknown,
}

impl ErrorClass {
    /// Returns `true` if this error class is documented as retryable (`KEEP`)
    /// in §3 of `docs/manual/epic-05-error-classes.md`.
    #[must_use]
    pub const fn is_retryable(self) -> bool {
        matches!(self, Self::RateLimited | Self::ServerTransient)
    }

    /// Returns `true` if the optimistic local cache mutation applied at
    /// enqueue time must be rolled back (`ROLLED BACK` in §3 of
    /// `docs/manual/epic-05-error-classes.md`).
    #[must_use]
    pub const fn should_rollback(self) -> bool {
        !self.is_retryable()
    }
}

/// Classifies a per-command [`CommandError`] into an [`ErrorClass`] according
/// to the matrix in §3 and §4 of `docs/manual/epic-05-error-classes.md`.
///
/// Classification inspects `err.error_code` **only** — never `err.error`
/// message text. Unlisted or absent `error_code` values map to the single
/// documented fallback [`ErrorClass::Unknown`].
#[must_use]
pub fn classify(err: &CommandError) -> ErrorClass {
    match err.error_code {
        Some(32 | 404 | 418) => ErrorClass::NotFound,
        Some(15 | 400 | 409) => ErrorClass::InvalidArgument,
        Some(401 | 403) => ErrorClass::PermissionDenied,
        Some(420) => ErrorClass::RateLimited,
        Some(500 | 503) => ErrorClass::ServerTransient,
        Some(_) | None => ErrorClass::Unknown,
    }
}

/// Summary of a [`apply_sync_status`] pass over a flushed batch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct StatusOutcome {
    /// Number of commands confirmed `"ok"` and deleted from `commands`.
    pub succeeded: usize,
    /// Number of commands rejected with an error and marked `failed`.
    pub failed: usize,
    /// Number of commands absent from `sync_status` and returned to `pending`.
    pub requeued: usize,
}

/// Projection of a [`CommandState::Failed`] row for UI surfacing (EPIC-07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FailedCommand {
    /// Hyphenated UUID of the failed command.
    pub uuid: String,
    /// Sync API command type tag (e.g. `"item_add"`, `"project_delete"`).
    pub cmd_type: String,
    /// Number of flush attempts recorded for this command.
    pub attempts: u32,
    /// Human-readable error text stored in `commands.error`.
    pub error: String,
}

impl FailedCommand {
    /// Alias for [`FailedCommand::cmd_type`] matching [`QueuedCommand::kind`].
    #[must_use]
    pub fn kind(&self) -> &str {
        &self.cmd_type
    }
}

/// Applies per-command `sync_status` entries to the `commands` table for
/// `batch` and invokes `rollback` for every errored command whose
/// [`ErrorClass::should_rollback`] is `true`.
///
/// For each command in `batch`:
/// - [`CommandStatus::Ok`]: deletes the row from `commands`.
/// - [`CommandStatus::Error`]: classifies the error via [`classify`]; if
///   [`ErrorClass::should_rollback`] is `true`, calls
///   `rollback(tx, cmd, class)?`; marks the row [`CommandState::Failed`] with
///   the server's `error` text (or `"command failed"` if absent) stored in the
///   `error` column.
/// - Absent from `sync_status`: moves the row from `in_flight` back to
///   [`CommandState::Pending`] with its original `uuid` and `attempts` intact
///   so a later flush safely re-sends it.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] on any SQLite failure or any error returned
/// by `rollback`.
#[allow(clippy::implicit_hasher)]
pub fn apply_sync_status<F>(
    tx: &Transaction<'_>,
    batch: &[QueuedCommand],
    sync_status: &HashMap<String, CommandStatus>,
    mut rollback: F,
) -> Result<StatusOutcome>
where
    F: FnMut(&Transaction<'_>, &QueuedCommand, ErrorClass) -> Result<()>,
{
    let store = CommandStore::new(tx);
    let mut outcome = StatusOutcome::default();

    for cmd in batch {
        let uuid_str = cmd.uuid.to_string();
        match sync_status.get(&uuid_str) {
            Some(CommandStatus::Ok(_)) => {
                store.remove(cmd.uuid)?;
                outcome.succeeded = outcome.succeeded.saturating_add(1);
            }
            Some(CommandStatus::Error(err)) => {
                let class = classify(err);
                if class.should_rollback() {
                    rollback(tx, cmd, class)?;
                }
                let msg = err.error.as_deref().unwrap_or("command failed");
                store.mark_failed(cmd.uuid, msg)?;
                outcome.failed = outcome.failed.saturating_add(1);
            }
            None => {
                tx.execute(
                    "UPDATE commands SET state = ?1 WHERE uuid = ?2",
                    params![CommandState::Pending.as_str(), uuid_str],
                )
                .map_err(|source| QueueError::Storage { source })?;
                outcome.requeued = outcome.requeued.saturating_add(1);
            }
        }
    }

    Ok(outcome)
}

/// Returns all commands currently in [`CommandState::Failed`], ordered by
/// `created_at` then `rowid` (enqueue order), for UI surfacing (EPIC-07).
///
/// # Errors
///
/// Returns [`QueueError::Storage`] when querying the `commands` table fails.
pub fn failed_commands(conn: &Connection) -> Result<Vec<FailedCommand>> {
    let mut stmt = conn
        .prepare(
            "SELECT uuid, type, attempts, COALESCE(error, '') \
             FROM commands WHERE state = ?1 ORDER BY created_at, rowid",
        )
        .map_err(|source| QueueError::Storage { source })?;

    let rows = stmt
        .query_map([CommandState::Failed.as_str()], |row| {
            Ok(FailedCommand {
                uuid: row.get(0)?,
                cmd_type: row.get(1)?,
                attempts: row.get(2)?,
                error: row.get(3)?,
            })
        })
        .map_err(|source| QueueError::Storage { source })?
        .collect::<rusqlite::Result<Vec<_>>>()
        .map_err(|source| QueueError::Storage { source })?;

    Ok(rows)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]
mod tests {
    //! Unit tests for T-408 `sync_status` processing, classification, and rollback.
    //!
    //! All UUIDs, temp IDs, error texts, and payloads are fake fixture data.
    use super::*;
    use crate::queue::flush::build_flush_batch;
    use crate::queue::schema;
    use serde_json::json;
    use uuid::Uuid;

    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        schema::apply(&conn).expect("apply queue schema");
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
            args: json!({ "id": format!("item_fake_{n}") }),
            temp_id: None,
            state: CommandState::Pending,
            attempts: 0,
            created_at,
            error: None,
        }
    }

    fn make_cmd_error(code: Option<i64>, message: Option<&str>) -> CommandError {
        CommandError {
            error_code: code,
            error: message.map(ToOwned::to_owned),
            extra: HashMap::new(),
        }
    }

    #[test]
    fn classify_table_driven_covers_every_class_and_unknown_fallback() {
        let cases: &[(Option<i64>, &str, ErrorClass, bool, bool)] = &[
            // (error_code, error_text, expected_class, retryable, should_rollback)
            (
                Some(32),
                "item does not exist",
                ErrorClass::NotFound,
                false,
                true,
            ),
            (
                Some(404),
                "Project not found",
                ErrorClass::NotFound,
                false,
                true,
            ),
            (
                Some(418),
                "temp id cannot be resolved",
                ErrorClass::NotFound,
                false,
                true,
            ),
            (
                Some(15),
                "invalid argument",
                ErrorClass::InvalidArgument,
                false,
                true,
            ),
            (
                Some(400),
                "bad request",
                ErrorClass::InvalidArgument,
                false,
                true,
            ),
            (
                Some(409),
                "conflict",
                ErrorClass::InvalidArgument,
                false,
                true,
            ),
            (
                Some(401),
                "unauthorized",
                ErrorClass::PermissionDenied,
                false,
                true,
            ),
            (
                Some(403),
                "User does not have permission to do this action",
                ErrorClass::PermissionDenied,
                false,
                true,
            ),
            (
                Some(420),
                "Too many requests. Try again later.",
                ErrorClass::RateLimited,
                true,
                false,
            ),
            (
                Some(500),
                "Internal server error",
                ErrorClass::ServerTransient,
                true,
                false,
            ),
            (
                Some(503),
                "Service temporarily unavailable",
                ErrorClass::ServerTransient,
                true,
                false,
            ),
            // Unlisted error_code -> Unknown fallback
            (
                Some(999),
                "item does not exist",
                ErrorClass::Unknown,
                false,
                true,
            ),
            // Absent error_code -> Unknown fallback regardless of error message text
            (
                None,
                "Internal server error",
                ErrorClass::Unknown,
                false,
                true,
            ),
        ];

        for &(code, text, expected_class, expected_retryable, expected_rollback) in cases {
            let err = make_cmd_error(code, Some(text));
            let got = classify(&err);
            assert_eq!(
                got, expected_class,
                "mismatch for code {code:?} (text {text:?})"
            );
            assert_eq!(got.is_retryable(), expected_retryable);
            assert_eq!(got.should_rollback(), expected_rollback);
        }
    }

    #[test]
    fn rollback_hook_invoked_for_rollback_classes_and_skipped_for_keep_classes() {
        let mut conn = fresh_conn();

        // One command per ErrorClass (6 total).
        let class_codes: &[(u8, Option<i64>, ErrorClass, bool)] = &[
            (1, Some(32), ErrorClass::NotFound, true),
            (2, Some(15), ErrorClass::InvalidArgument, true),
            (3, Some(403), ErrorClass::PermissionDenied, true),
            (4, Some(420), ErrorClass::RateLimited, false),
            (5, Some(500), ErrorClass::ServerTransient, false),
            (6, Some(777), ErrorClass::Unknown, true),
        ];

        let store = CommandStore::new(&conn);
        for &(id, _, _, _) in class_codes {
            let cmd = fixture_command(id, "item_update", i64::from(id) * 1_000);
            store.insert(&cmd).expect("insert");
        }

        let (batch, _) = build_flush_batch(&mut conn, 10)
            .expect("flush")
            .expect("batch");

        let mut sync_status = HashMap::new();
        for &(id, code, _, _) in class_codes {
            sync_status.insert(
                fixture_uuid(id).to_string(),
                CommandStatus::Error(make_cmd_error(code, Some(&format!("err-{id}")))),
            );
        }

        let mut rolled_back: Vec<(Uuid, ErrorClass)> = Vec::new();
        let tx = conn.transaction().expect("begin tx");
        let outcome = apply_sync_status(&tx, &batch, &sync_status, |_tx, cmd, class| {
            rolled_back.push((cmd.uuid, class));
            Ok(())
        })
        .expect("apply_sync_status");
        tx.commit().expect("commit");

        assert_eq!(
            outcome,
            StatusOutcome {
                succeeded: 0,
                failed: 6,
                requeued: 0,
            }
        );

        let expected_rollbacks: Vec<(Uuid, ErrorClass)> = class_codes
            .iter()
            .filter(|&&(_, _, _, should_rb)| should_rb)
            .map(|&(id, _, class, _)| (fixture_uuid(id), class))
            .collect();
        assert_eq!(rolled_back, expected_rollbacks);
    }

    #[test]
    fn mixed_response_removes_ok_marks_error_failed_and_does_not_stall_queue() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let cmd_ok = fixture_command(1, "item_add", 1_000);
        let cmd_err = fixture_command(2, "item_delete", 2_000);
        store.insert(&cmd_ok).expect("insert ok cmd");
        store.insert(&cmd_err).expect("insert err cmd");

        let (batch, _) = build_flush_batch(&mut conn, 10)
            .expect("flush")
            .expect("batch");
        assert_eq!(batch.len(), 2);

        let mut sync_status = HashMap::new();
        sync_status.insert(cmd_ok.uuid.to_string(), CommandStatus::Ok("ok".to_string()));
        sync_status.insert(
            cmd_err.uuid.to_string(),
            CommandStatus::Error(make_cmd_error(Some(32), Some("item does not exist"))),
        );

        let mut rollback_calls = Vec::new();
        let tx = conn.transaction().expect("begin tx");
        let outcome = apply_sync_status(&tx, &batch, &sync_status, |_tx, cmd, class| {
            rollback_calls.push((cmd.uuid, class));
            Ok(())
        })
        .expect("apply_sync_status");
        tx.commit().expect("commit");

        assert_eq!(
            outcome,
            StatusOutcome {
                succeeded: 1,
                failed: 1,
                requeued: 0,
            }
        );
        assert_eq!(rollback_calls, vec![(cmd_err.uuid, ErrorClass::NotFound)]);

        // `pending()` returns nothing — the failed command is out of the retry loop.
        let pending = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending, Vec::<QueuedCommand>::new());

        let counts = CommandStore::new(&conn).count_by_state().expect("counts");
        assert_eq!(counts.pending, 0);
        assert_eq!(counts.in_flight, 0);
        assert_eq!(counts.failed, 1);

        // `failed_commands()` exposes the failed command with uuid, type, attempts, and error text.
        let failed = failed_commands(&conn).expect("failed_commands");
        assert_eq!(
            failed,
            vec![FailedCommand {
                uuid: cmd_err.uuid.to_string(),
                cmd_type: "item_delete".to_string(),
                attempts: 1,
                error: "item does not exist".to_string(),
            }]
        );
        assert_eq!(failed[0].kind(), "item_delete");
    }

    #[test]
    fn in_flight_command_absent_from_sync_status_returns_to_pending_with_same_uuid() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let cmd_ok = fixture_command(10, "item_complete", 1_000);
        let cmd_missing = fixture_command(11, "item_add", 2_000);
        store.insert(&cmd_ok).expect("insert cmd_ok");
        store.insert(&cmd_missing).expect("insert cmd_missing");

        let original_uuid = cmd_missing.uuid;

        let (batch, _) = build_flush_batch(&mut conn, 10)
            .expect("flush")
            .expect("batch");

        // Server response includes only `cmd_ok`; `cmd_missing` is omitted from `sync_status`.
        let mut sync_status = HashMap::new();
        sync_status.insert(cmd_ok.uuid.to_string(), CommandStatus::Ok("ok".to_string()));

        let tx = conn.transaction().expect("begin tx");
        let outcome = apply_sync_status(&tx, &batch, &sync_status, |_, _, _| {
            panic!("rollback must not be called for missing sync_status entry");
        })
        .expect("apply_sync_status");
        tx.commit().expect("commit");

        assert_eq!(
            outcome,
            StatusOutcome {
                succeeded: 1,
                failed: 0,
                requeued: 1,
            }
        );

        let pending = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(
            pending[0].uuid, original_uuid,
            "requeued command must retain its original uuid for server deduplication"
        );
        assert_eq!(pending[0].state, CommandState::Pending);
        assert_eq!(
            pending[0].attempts, 1,
            "attempts count from the flush must be preserved"
        );
    }

    #[test]
    fn failed_commands_returns_all_failed_rows_in_enqueue_order() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let c1 = fixture_command(1, "project_add", 1_000);
        let c2 = fixture_command(2, "item_add", 2_000);
        let c3 = fixture_command(3, "item_update", 3_000);
        for cmd in [&c1, &c2, &c3] {
            store.insert(cmd).expect("insert");
        }

        let (batch, _) = build_flush_batch(&mut conn, 10)
            .expect("flush")
            .expect("batch");

        let mut sync_status = HashMap::new();
        sync_status.insert(
            c1.uuid.to_string(),
            CommandStatus::Error(make_cmd_error(Some(15), Some("invalid project name"))),
        );
        sync_status.insert(c2.uuid.to_string(), CommandStatus::Ok("ok".to_string()));
        sync_status.insert(
            c3.uuid.to_string(),
            CommandStatus::Error(make_cmd_error(None, None)),
        );

        let tx = conn.transaction().expect("begin tx");
        apply_sync_status(&tx, &batch, &sync_status, |_, _, _| Ok(())).expect("apply");
        tx.commit().expect("commit");

        let failed = failed_commands(&conn).expect("failed_commands");
        assert_eq!(
            failed,
            vec![
                FailedCommand {
                    uuid: c1.uuid.to_string(),
                    cmd_type: "project_add".to_string(),
                    attempts: 1,
                    error: "invalid project name".to_string(),
                },
                FailedCommand {
                    uuid: c3.uuid.to_string(),
                    cmd_type: "item_update".to_string(),
                    attempts: 1,
                    error: "command failed".to_string(),
                },
            ]
        );
    }
}
