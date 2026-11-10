//! Batch flush request builder for the offline command queue (T-406, EPIC-05).
//!
//! [`build_flush_batch`] selects up to `max_batch` [`CommandState::Pending`]
//! commands in enqueue order (`created_at`, then `rowid`), transitions those
//! rows to [`CommandState::InFlight`] with `attempts` incremented inside a
//! single SQLite transaction, and projects them into a [`SyncRequest`] ready
//! for the sync engine (EPIC-06) to submit.
//!
//! No HTTP calls occur in this module. A flush is a pure command submission,
//! so [`SyncRequest::resource_types`] is left empty (see §6 of
//! `docs/manual/epic-05-error-classes.md`); the caller in EPIC-06 may
//! populate `sync_token` and `resource_types` if it combines a flush with an
//! incremental resource pull.

use rusqlite::Connection;
use uuid::Uuid;

use crate::api::sync::SyncRequest;
use crate::error::{QueueError, Result};
use crate::queue::command::{CommandState, QueuedCommand};
use crate::queue::store::CommandStore;

/// Default maximum number of pending commands bundled into a single `/sync`
/// flush request, as specified in §6 of
/// `docs/manual/epic-05-error-classes.md`.
pub const DEFAULT_MAX_BATCH: usize = 100;

/// Builds a flush batch of up to [`DEFAULT_MAX_BATCH`] pending commands.
///
/// Convenience wrapper around [`build_flush_batch`]; see its documentation
/// for transactional semantics and return value details.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] when the underlying SQLite transaction or
/// queries fail, or [`QueueError::Serialize`] /
/// [`QueueError::UnknownCommandType`] when a stored row is corrupt.
pub fn build_default_flush_batch(
    conn: &mut Connection,
) -> Result<Option<(Vec<QueuedCommand>, SyncRequest)>> {
    build_flush_batch(conn, DEFAULT_MAX_BATCH)
}

/// Fetches up to `max_batch` [`CommandState::Pending`] commands in enqueue
/// order, marks them [`CommandState::InFlight`] with `attempts` incremented in
/// one transaction, and builds the corresponding [`SyncRequest`].
///
/// When `max_batch == 0` or the queue has no pending commands, returns
/// `Ok(None)` without building an empty `/sync` request.
///
/// The returned [`SyncRequest`] is a pure command submission: `sync_token`
/// defaults to `"*"`, `resource_types` is empty, and `commands` holds each
/// command's [`QueuedCommand::to_sync_command`] JSON projection in the exact
/// enqueue order of the returned `Vec<QueuedCommand>`.
///
/// # Errors
///
/// Returns [`QueueError::Storage`] if starting/committing the transaction or
/// reading/updating the `commands` table fails, or a non-retryable decode
/// error if a stored command row cannot be deserialised.
pub fn build_flush_batch(
    conn: &mut Connection,
    max_batch: usize,
) -> Result<Option<(Vec<QueuedCommand>, SyncRequest)>> {
    if max_batch == 0 {
        return Ok(None);
    }

    let limit = u32::try_from(max_batch).unwrap_or(u32::MAX);
    let tx = conn
        .transaction()
        .map_err(|source| QueueError::Storage { source })?;

    let store = CommandStore::new(&tx);
    let mut batch = store.pending(limit)?;
    if batch.is_empty() {
        return Ok(None);
    }

    let uuids: Vec<Uuid> = batch.iter().map(|cmd| cmd.uuid).collect();
    store.mark_in_flight(&uuids)?;

    for cmd in &mut batch {
        cmd.state = CommandState::InFlight;
        cmd.attempts = cmd.attempts.saturating_add(1);
    }

    let commands = batch.iter().map(QueuedCommand::to_sync_command).collect();
    let request = SyncRequest {
        sync_token: "*".to_string(),
        resource_types: Vec::new(),
        commands,
    };

    tx.commit()
        .map_err(|source| QueueError::Storage { source })?;

    Ok(Some((batch, request)))
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    //! Unit tests for T-406 batch flush request building.
    //!
    //! All UUIDs, temp IDs and command payloads are fake fixture data.
    use super::*;
    use crate::error::AppError;
    use crate::queue::schema;
    use serde_json::json;

    fn fresh_conn() -> Connection {
        let conn = Connection::open_in_memory().expect("open in-memory db");
        schema::apply(&conn).expect("apply queue schema");
        conn
    }

    fn fixture_uuid(index: u16) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[14..16].copy_from_slice(&index.to_be_bytes());
        Uuid::from_bytes(bytes)
    }

    fn fixture_command(index: u16, created_at: i64) -> QueuedCommand {
        QueuedCommand {
            uuid: fixture_uuid(index),
            kind: "item_add".to_string(),
            args: json!({
                "content": format!("fixture task {index}"),
                "project_id": "proj_fake_1",
            }),
            temp_id: Some(format!("temp_item_{index}")),
            state: CommandState::Pending,
            attempts: 0,
            created_at,
            error: None,
        }
    }

    #[test]
    fn empty_queue_returns_none_without_building_request() {
        let mut conn = fresh_conn();
        let result = build_flush_batch(&mut conn, DEFAULT_MAX_BATCH).expect("flush empty queue");
        assert!(result.is_none(), "empty queue must return None");
    }

    #[test]
    fn zero_max_batch_returns_none_and_leaves_pending_untouched() {
        let mut conn = fresh_conn();
        let cmd = fixture_command(1, 1_000);
        CommandStore::new(&conn).insert(&cmd).expect("insert");

        let result = build_flush_batch(&mut conn, 0).expect("flush with max_batch 0");
        assert!(result.is_none());

        let counts = CommandStore::new(&conn).count_by_state().expect("counts");
        assert_eq!(counts.pending, 1);
        assert_eq!(counts.in_flight, 0);
    }

    #[test]
    fn hundred_pending_commands_produce_single_request_in_enqueue_order() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);
        let mut inserted = Vec::with_capacity(DEFAULT_MAX_BATCH);

        for i in 0..u16::try_from(DEFAULT_MAX_BATCH).expect("100 fits u16") {
            let cmd = fixture_command(i, 1_000 + i64::from(i));
            store.insert(&cmd).expect("insert command");
            inserted.push(cmd);
        }

        let (batch, request) = build_default_flush_batch(&mut conn)
            .expect("build_default_flush_batch")
            .expect("batch present");

        assert_eq!(batch.len(), DEFAULT_MAX_BATCH);
        assert_eq!(request.commands.len(), DEFAULT_MAX_BATCH);
        assert_eq!(request.sync_token, "*");
        assert!(
            request.resource_types.is_empty(),
            "flush request must have empty resource_types for pure command submission"
        );

        for ((got_cmd, got_wire), expected_cmd) in batch
            .iter()
            .zip(request.commands.iter())
            .zip(inserted.iter())
        {
            assert_eq!(got_cmd.uuid, expected_cmd.uuid);
            assert_eq!(got_cmd.state, CommandState::InFlight);
            assert_eq!(got_cmd.attempts, 1);
            assert_eq!(got_wire, &expected_cmd.to_sync_command());
        }

        let counts = CommandStore::new(&conn).count_by_state().expect("counts");
        assert_eq!(counts.pending, 0);
        assert_eq!(counts.in_flight, 100);
        assert_eq!(counts.failed, 0);

        // A second flush immediately afterwards finds no pending commands.
        let second = build_default_flush_batch(&mut conn).expect("second flush");
        assert!(second.is_none());
    }

    #[test]
    fn pending_exceeding_max_batch_flushes_only_first_max_batch_in_enqueue_order() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        // Insert 5 commands; indices 10 and 20 share created_at to verify rowid tie-breaking.
        let c1 = fixture_command(10, 1_000);
        let c2 = fixture_command(20, 1_000);
        let c3 = fixture_command(30, 2_000);
        let c4 = fixture_command(40, 3_000);
        let c5 = fixture_command(50, 4_000);
        for cmd in [&c1, &c2, &c3, &c4, &c5] {
            store.insert(cmd).expect("insert");
        }

        let (batch, request) = build_flush_batch(&mut conn, 3)
            .expect("flush batch of 3")
            .expect("some batch");

        let flushed_uuids: Vec<Uuid> = batch.iter().map(|c| c.uuid).collect();
        assert_eq!(flushed_uuids, vec![c1.uuid, c2.uuid, c3.uuid]);
        assert_eq!(
            request.commands,
            vec![
                c1.to_sync_command(),
                c2.to_sync_command(),
                c3.to_sync_command()
            ]
        );

        for cmd in &batch {
            assert_eq!(cmd.state, CommandState::InFlight);
            assert_eq!(cmd.attempts, 1);
        }

        // Verify persisted states and attempts in SQLite.
        let counts = CommandStore::new(&conn).count_by_state().expect("counts");
        assert_eq!(counts.in_flight, 3);
        assert_eq!(counts.pending, 2);

        let remaining = CommandStore::new(&conn).pending(10).expect("remaining");
        let remaining_uuids: Vec<Uuid> = remaining.iter().map(|c| c.uuid).collect();
        assert_eq!(remaining_uuids, vec![c4.uuid, c5.uuid]);
        for cmd in &remaining {
            assert_eq!(cmd.state, CommandState::Pending);
            assert_eq!(cmd.attempts, 0);
        }
    }

    #[test]
    fn ignores_already_in_flight_and_failed_commands() {
        let mut conn = fresh_conn();
        let store = CommandStore::new(&conn);

        let mut in_flight = fixture_command(1, 500);
        in_flight.state = CommandState::InFlight;
        in_flight.attempts = 1;
        store.insert(&in_flight).expect("insert in_flight");

        let mut failed = fixture_command(2, 600);
        failed.state = CommandState::Failed;
        failed.error = Some("rejected".to_string());
        store.insert(&failed).expect("insert failed");

        let mut pending = fixture_command(3, 700);
        pending.temp_id = None;
        pending.kind = "item_close".to_string();
        pending.args = json!({ "id": "item_fake_99" });
        store.insert(&pending).expect("insert pending");

        let (batch, request) = build_flush_batch(&mut conn, 10)
            .expect("flush")
            .expect("some batch");

        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].uuid, pending.uuid);
        assert_eq!(batch[0].state, CommandState::InFlight);
        assert_eq!(batch[0].attempts, 1);
        assert_eq!(request.commands, vec![pending.to_sync_command()]);
    }

    #[test]
    fn missing_table_returns_storage_error() {
        let mut conn = Connection::open_in_memory().expect("open bare db");
        let res = build_flush_batch(&mut conn, DEFAULT_MAX_BATCH);
        assert!(matches!(
            res,
            Err(AppError::Queue(QueueError::Storage { .. }))
        ));
    }
}
