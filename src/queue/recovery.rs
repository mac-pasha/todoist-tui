//! Crash and restart recovery for the offline command queue (T-409, EPIC-05).
//!
//! If the process exits or crashes while a flush batch is in flight (after
//! [`crate::queue::flush::build_flush_batch`] marks commands
//! [`CommandState::InFlight`], before [`crate::queue::status::apply_sync_status`]
//! commits their outcomes), those commands remain in `state = 'in_flight'` on
//! disk.
//!
//! [`recover_on_start`] runs during startup before the sync engine begins
//! draining the queue:
//! - Any `in_flight` command whose `attempts` exceeds [`MAX_ATTEMPTS`] (`5`,
//!   per §7 of `docs/manual/epic-05-error-classes.md`) is transitioned to
//!   [`CommandState::Failed`] with error [`MAX_ATTEMPTS_ERROR`]
//!   (`"max attempts exceeded"`) so a persistently failing command never
//!   churns forever.
//! - Every other `in_flight` command returns to [`CommandState::Pending`]
//!   with its `uuid`, `type`, `args`, `temp_id`, `attempts`, and `created_at`
//!   preserved, so the next flush re-sends the exact same command `uuid` and
//!   the Todoist Sync API deduplicates any command it had already applied.

use rusqlite::{params, Connection};

use crate::error::{QueueError, Result};
use crate::queue::command::CommandState;

/// Maximum number of flush attempts allowed per command before startup
/// recovery marks an `in_flight` command as permanently failed (§7 of
/// `docs/manual/epic-05-error-classes.md`).
pub const MAX_ATTEMPTS: u32 = 5;

/// Error message recorded on commands whose `attempts` exceeds
/// [`MAX_ATTEMPTS`] during recovery.
pub const MAX_ATTEMPTS_ERROR: &str = "max attempts exceeded";

/// Requeues `in_flight` commands back to `pending` on startup, or marks them
/// `failed` when their `attempts` count exceeds [`MAX_ATTEMPTS`].
///
/// Runs inside a single SQLite transaction:
/// 1. Rows with `state = 'in_flight'` and `attempts > MAX_ATTEMPTS` are
///    marked `state = 'failed'` with `error = "max attempts exceeded"`.
/// 2. All remaining `state = 'in_flight'` rows (`attempts <= MAX_ATTEMPTS`)
///    are updated to `state = 'pending'` without altering `uuid`, `type`,
///    `args`, `temp_id`, `attempts`, or `created_at`.
///
/// Rows already in `pending` or `failed` are never modified. Calling this
/// function a second time when no `in_flight` rows remain is a no-op that
/// returns `Ok(0)`.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] if starting/committing the transaction or
/// updating the `commands` table fails.
pub fn recover_on_start(conn: &mut Connection) -> Result<usize> {
    let tx = conn
        .transaction()
        .map_err(|source| QueueError::Storage { source })?;

    tx.execute(
        "UPDATE commands SET state = ?1, error = ?2 \
         WHERE state = ?3 AND attempts > ?4",
        params![
            CommandState::Failed.as_str(),
            MAX_ATTEMPTS_ERROR,
            CommandState::InFlight.as_str(),
            MAX_ATTEMPTS,
        ],
    )
    .map_err(|source| QueueError::Storage { source })?;

    let recovered = tx
        .execute(
            "UPDATE commands SET state = ?1 WHERE state = ?2",
            params![
                CommandState::Pending.as_str(),
                CommandState::InFlight.as_str(),
            ],
        )
        .map_err(|source| QueueError::Storage { source })?;

    tx.commit()
        .map_err(|source| QueueError::Storage { source })?;

    Ok(recovered)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Unit tests for T-409 startup recovery of `in_flight` commands.
    //!
    //! All UUIDs, temp IDs, and command payloads are fake fixture data.
    use super::*;
    use crate::queue::command::QueuedCommand;
    use crate::queue::flush::build_flush_batch;
    use crate::queue::schema;
    use crate::queue::status::{failed_commands, FailedCommand};
    use crate::queue::store::CommandStore;
    use serde_json::json;
    use tempfile::TempDir;
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

    fn fixture_command(n: u8, created_at: i64) -> QueuedCommand {
        QueuedCommand {
            uuid: fixture_uuid(n),
            kind: "item_add".to_string(),
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

    #[test]
    fn recover_on_start_moves_in_flight_back_to_pending_preserving_all_fields_and_attempts() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let mut cmd1 = fixture_command(1, 1_000);
        cmd1.state = CommandState::InFlight;
        cmd1.attempts = 1;

        let mut cmd2 = fixture_command(2, 2_000);
        cmd2.kind = "project_add".to_string();
        cmd2.args = json!({ "name": "Fixture Project" });
        cmd2.temp_id = Some("temp_proj_2".to_string());
        cmd2.state = CommandState::InFlight;
        cmd2.attempts = MAX_ATTEMPTS; // Boundary: attempts == MAX_ATTEMPTS (5) is still recovered.

        store.insert(&cmd1).expect("insert cmd1");
        store.insert(&cmd2).expect("insert cmd2");

        let recovered = recover_on_start(&mut conn).expect("recover_on_start");
        assert_eq!(recovered, 2);

        let pending = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending.len(), 2);

        let mut expected1 = cmd1;
        expected1.state = CommandState::Pending;
        let mut expected2 = cmd2;
        expected2.state = CommandState::Pending;

        assert_eq!(pending, vec![expected1, expected2]);
    }

    #[test]
    fn command_exceeding_max_attempts_is_marked_failed_instead_of_requeued() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let mut within_cap = fixture_command(1, 1_000);
        within_cap.state = CommandState::InFlight;
        within_cap.attempts = MAX_ATTEMPTS;

        let mut over_cap = fixture_command(2, 2_000);
        over_cap.state = CommandState::InFlight;
        over_cap.attempts = MAX_ATTEMPTS + 1;

        store.insert(&within_cap).expect("insert within_cap");
        store.insert(&over_cap).expect("insert over_cap");

        let recovered = recover_on_start(&mut conn).expect("recover_on_start");
        assert_eq!(
            recovered, 1,
            "only within_cap should be requeued to pending"
        );

        let pending = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].uuid, within_cap.uuid);
        assert_eq!(pending[0].attempts, MAX_ATTEMPTS);

        let failed = failed_commands(&conn).expect("failed_commands");
        assert_eq!(
            failed,
            vec![FailedCommand {
                uuid: over_cap.uuid.to_string(),
                cmd_type: "item_add".to_string(),
                attempts: MAX_ATTEMPTS + 1,
                error: MAX_ATTEMPTS_ERROR.to_string(),
            }]
        );
    }

    #[test]
    fn on_disk_reopen_recovers_same_uuids_in_same_order() {
        let temp_dir = TempDir::new().expect("create tempdir");
        let db_path = temp_dir.path().join("queue_recovery_test.sqlite");

        let mut expected_uuids = Vec::new();
        {
            let mut conn = Connection::open(&db_path).expect("open on-disk db");
            schema::apply(&conn).expect("apply schema");
            let store = CommandStore::new(&conn);

            for i in 1..=5_u8 {
                let cmd = fixture_command(i, i64::from(i) * 1_000);
                expected_uuids.push(cmd.uuid);
                store.insert(&cmd).expect("insert pending command");
            }

            // Transition all 5 to `in_flight` (simulating a crash mid-flush) before dropping conn.
            let (batch, _) = build_flush_batch(&mut conn, 10)
                .expect("build_flush_batch")
                .expect("batch present");
            assert_eq!(batch.len(), 5);
        }

        // Reopen the connection to the same on-disk DB file and run recovery.
        let mut reopened = Connection::open(&db_path).expect("reopen on-disk db");
        let recovered = recover_on_start(&mut reopened).expect("recover_on_start");
        assert_eq!(recovered, 5);

        let pending = CommandStore::new(&reopened).pending(10).expect("pending");
        let got_uuids: Vec<Uuid> = pending.iter().map(|c| c.uuid).collect();
        assert_eq!(
            got_uuids, expected_uuids,
            "recovered commands must have the exact same uuids in the exact same order"
        );
        for cmd in &pending {
            assert_eq!(cmd.attempts, 1, "incremented attempts must be preserved");
            assert_eq!(cmd.state, CommandState::Pending);
        }
    }

    #[test]
    fn failed_rows_are_never_auto_requeued_by_recovery() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let mut failed_cmd = fixture_command(10, 1_000);
        failed_cmd.state = CommandState::Failed;
        failed_cmd.attempts = 1;
        failed_cmd.error = Some("item does not exist".to_string());
        store.insert(&failed_cmd).expect("insert failed_cmd");

        let mut in_flight_cmd = fixture_command(11, 2_000);
        in_flight_cmd.state = CommandState::InFlight;
        in_flight_cmd.attempts = 2;
        store.insert(&in_flight_cmd).expect("insert in_flight_cmd");

        let recovered = recover_on_start(&mut conn).expect("recover_on_start");
        assert_eq!(recovered, 1);

        let pending = CommandStore::new(&conn).pending(10).expect("pending");
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].uuid, in_flight_cmd.uuid);

        let failed = failed_commands(&conn).expect("failed_commands");
        assert_eq!(
            failed,
            vec![FailedCommand {
                uuid: failed_cmd.uuid.to_string(),
                cmd_type: "item_add".to_string(),
                attempts: 1,
                error: "item does not exist".to_string(),
            }]
        );
    }

    #[test]
    fn recovery_is_idempotent_second_call_returns_zero_and_changes_nothing() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let mut cmd1 = fixture_command(1, 1_000);
        cmd1.state = CommandState::InFlight;
        cmd1.attempts = 2;
        store.insert(&cmd1).expect("insert cmd1");

        let mut cmd2 = fixture_command(2, 2_000);
        cmd2.state = CommandState::InFlight;
        cmd2.attempts = MAX_ATTEMPTS + 1;
        store.insert(&cmd2).expect("insert cmd2");

        let first = recover_on_start(&mut conn).expect("first recovery");
        assert_eq!(first, 1);

        let pending_after_first = CommandStore::new(&conn).pending(10).expect("pending 1");
        let failed_after_first = failed_commands(&conn).expect("failed 1");

        let second = recover_on_start(&mut conn).expect("second recovery");
        assert_eq!(second, 0, "second recovery call must return 0");

        let pending_after_second = CommandStore::new(&conn).pending(10).expect("pending 2");
        let failed_after_second = failed_commands(&conn).expect("failed 2");

        assert_eq!(pending_after_second, pending_after_first);
        assert_eq!(failed_after_second, failed_after_first);
    }
}
