//! `commands` table CRUD store (T-404, EPIC-05).
//!
//! [`CommandStore`] wraps a borrowed [`Connection`] (the caller — EPIC-04's
//! cache module — owns opening/migrating it; a `&Transaction` derefs to the
//! same type) and offers the operations the sync engine's drain loop
//! (EPIC-06) will need: enqueue, pull the next batch in enqueue order, flip
//! state on submit/failure, delete on success, and look up a command by the
//! temp id it's waiting on.

use rusqlite::{params, Connection};
use uuid::Uuid;

use crate::error::{QueueError, Result};
use crate::queue::command::{CommandState, QueuedCommand};

/// CRUD access to the `commands` table ([`crate::queue::schema::COMMANDS_DDL`]).
pub struct CommandStore<'a> {
    conn: &'a Connection,
}

impl<'a> CommandStore<'a> {
    /// Wraps `conn`; does not itself apply the schema — call
    /// [`crate::queue::schema::apply`] first.
    pub fn new(conn: &'a Connection) -> Self {
        Self { conn }
    }

    /// Inserts `command`. A duplicate `uuid` is an explicit no-op (`INSERT
    /// OR IGNORE`): the Sync API's own deduplication means a retried
    /// submission may hand back an already-queued command's uuid, and that
    /// must never create a second row or error out.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Storage`] on a SQLite failure.
    pub fn insert(&self, command: &QueuedCommand) -> Result<()> {
        self.conn
            .execute(
                "INSERT OR IGNORE INTO commands \
                 (uuid, type, args, temp_id, state, attempts, created_at, error) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8)",
                params![
                    command.uuid.to_string(),
                    command.kind,
                    command.args.to_string(),
                    command.temp_id,
                    command.state.as_str(),
                    command.attempts,
                    command.created_at,
                    command.error,
                ],
            )
            .map_err(|source| QueueError::Storage { source })?;
        Ok(())
    }

    /// Returns up to `limit` commands still in [`CommandState::Pending`],
    /// ordered by `created_at` then `rowid` (enqueue order) — never
    /// `in_flight` or `failed` rows.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Storage`] on a SQLite failure, or a decode
    /// error bubbled up from a row whose `state`/`args` cannot be parsed.
    pub fn pending(&self, limit: u32) -> Result<Vec<QueuedCommand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT uuid, type, args, temp_id, state, attempts, created_at, error \
                 FROM commands WHERE state = ?1 ORDER BY created_at, rowid LIMIT ?2",
            )
            .map_err(|source| QueueError::Storage { source })?;

        let rows = stmt
            .query_map(
                params![CommandState::Pending.as_str(), limit],
                row_to_command,
            )
            .map_err(|source| QueueError::Storage { source })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| QueueError::Storage { source }.into())
    }

    /// Flips every command in `uuids` to [`CommandState::InFlight`] and
    /// bumps its `attempts`. Not atomic across `uuids` by itself: wrap the
    /// call in a caller-owned transaction when all-or-nothing matters.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Storage`] on a SQLite failure.
    pub fn mark_in_flight(&self, uuids: &[Uuid]) -> Result<()> {
        for uuid in uuids {
            self.conn
                .execute(
                    "UPDATE commands SET state = ?1, attempts = attempts + 1 WHERE uuid = ?2",
                    params![CommandState::InFlight.as_str(), uuid.to_string()],
                )
                .map_err(|source| QueueError::Storage { source })?;
        }
        Ok(())
    }

    /// Flips `uuid` to [`CommandState::Failed`], recording `error`.
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Storage`] on a SQLite failure.
    pub fn mark_failed(&self, uuid: Uuid, error: &str) -> Result<()> {
        self.conn
            .execute(
                "UPDATE commands SET state = ?1, error = ?2 WHERE uuid = ?3",
                params![CommandState::Failed.as_str(), error, uuid.to_string()],
            )
            .map_err(|source| QueueError::Storage { source })?;
        Ok(())
    }

    /// Deletes `uuid` (e.g. once the Sync API has confirmed it).
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Storage`] on a SQLite failure.
    pub fn remove(&self, uuid: Uuid) -> Result<()> {
        self.conn
            .execute(
                "DELETE FROM commands WHERE uuid = ?1",
                params![uuid.to_string()],
            )
            .map_err(|source| QueueError::Storage { source })?;
        Ok(())
    }

    /// Returns every command still referencing `temp_id` (i.e. waiting on
    /// it to be resolved to a real Todoist id).
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Storage`] on a SQLite failure.
    pub fn referencing_temp_id(&self, temp_id: &str) -> Result<Vec<QueuedCommand>> {
        let mut stmt = self
            .conn
            .prepare(
                "SELECT uuid, type, args, temp_id, state, attempts, created_at, error \
                 FROM commands WHERE temp_id = ?1 ORDER BY created_at, rowid",
            )
            .map_err(|source| QueueError::Storage { source })?;

        let rows = stmt
            .query_map(params![temp_id], row_to_command)
            .map_err(|source| QueueError::Storage { source })?;

        rows.collect::<rusqlite::Result<Vec<_>>>()
            .map_err(|source| QueueError::Storage { source }.into())
    }

    /// Returns the number of commands currently in each [`CommandState`]
    /// (`(pending, in_flight, failed)`).
    ///
    /// # Errors
    ///
    /// Returns [`QueueError::Storage`] on a SQLite failure.
    pub fn count_by_state(&self) -> Result<CommandStateCounts> {
        let mut counts = CommandStateCounts::default();
        let mut stmt = self
            .conn
            .prepare("SELECT state, COUNT(*) FROM commands GROUP BY state")
            .map_err(|source| QueueError::Storage { source })?;
        let rows = stmt
            .query_map([], |row| {
                Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
            })
            .map_err(|source| QueueError::Storage { source })?;

        for row in rows {
            let (state, count) = row.map_err(|source| QueueError::Storage { source })?;
            // COUNT(*) is never negative; fall back to 0 rather than panic.
            let count = u64::try_from(count).unwrap_or(0);
            match state.as_str() {
                "pending" => counts.pending = count,
                "in_flight" => counts.in_flight = count,
                "failed" => counts.failed = count,
                // An unrecognised state can only mean a future build wrote
                // this row; ignore it here rather than failing the whole
                // count (same spirit as `DbError::UnsupportedSchemaVersion`
                // guarding the migration runner itself).
                _ => {}
            }
        }
        Ok(counts)
    }
}

/// Count of commands per [`CommandState`], as returned by
/// [`CommandStore::count_by_state`].
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CommandStateCounts {
    /// Number of commands in [`CommandState::Pending`].
    pub pending: u64,
    /// Number of commands in [`CommandState::InFlight`].
    pub in_flight: u64,
    /// Number of commands in [`CommandState::Failed`].
    pub failed: u64,
}

/// Decodes one `commands` row into a [`QueuedCommand`].
fn row_to_command(row: &rusqlite::Row<'_>) -> rusqlite::Result<QueuedCommand> {
    let uuid: String = row.get(0)?;
    let kind: String = row.get(1)?;
    let args: String = row.get(2)?;
    let temp_id: Option<String> = row.get(3)?;
    let state: String = row.get(4)?;
    let attempts: u32 = row.get(5)?;
    let created_at: i64 = row.get(6)?;
    let error: Option<String> = row.get(7)?;

    let uuid = Uuid::parse_str(&uuid).map_err(|err| decode_err("uuid", err))?;
    let args = serde_json::from_str(&args).map_err(|err| decode_err("args", err))?;
    let state = state
        .parse::<CommandState>()
        .map_err(|err| decode_err("state", err))?;

    Ok(QueuedCommand {
        uuid,
        kind,
        args,
        temp_id,
        state,
        attempts,
        created_at,
        error,
    })
}

/// Decode failure detail for one stored `commands` row. Surfaced only
/// through `QueueError::Storage`'s source chain (never in user-facing
/// text), so it stays crate-private.
fn decode_err(
    field: &'static str,
    source: impl std::error::Error + Send + Sync + 'static,
) -> rusqlite::Error {
    rusqlite::Error::FromSqlConversionFailure(
        0,
        rusqlite::types::Type::Text,
        Box::new(RowDecodeError {
            field,
            source: Box::new(source),
        }),
    )
}

#[derive(Debug, thiserror::Error)]
#[error("stored command row has an undecodable {field}: {source}")]
struct RowDecodeError {
    field: &'static str,
    #[source]
    source: Box<dyn std::error::Error + Send + Sync>,
}

#[cfg(test)]
mod tests {
    //! T-404 store tests: CRUD semantics over an in-memory connection.
    use super::*;
    use crate::error::AppError;
    use crate::queue::schema;

    fn fresh() -> Connection {
        let conn = Connection::open_in_memory().expect("open");
        schema::apply(&conn).expect("apply");
        conn
    }

    /// Fixed fake uuid (`v4`-shaped, but hand-written: fixture data only).
    fn uuid(n: u8) -> Uuid {
        let mut bytes = [0u8; 16];
        bytes[15] = n;
        Uuid::from_bytes(bytes)
    }

    fn command(id: u8, created_at: i64) -> QueuedCommand {
        QueuedCommand {
            uuid: uuid(id),
            kind: "item_add".into(),
            args: serde_json::json!({ "content": format!("fixture item {id}") }),
            temp_id: Some(format!("tmp-fixture-{id}")),
            state: CommandState::Pending,
            attempts: 0,
            created_at,
            error: None,
        }
    }

    fn stored(conn: &Connection, id: u8) -> QueuedCommand {
        conn.query_row(
            "SELECT uuid, type, args, temp_id, state, attempts, created_at, error \
             FROM commands WHERE uuid = ?1",
            [uuid(id).to_string()],
            row_to_command,
        )
        .expect("stored row")
    }

    fn insert(conn: &Connection, cmd: &QueuedCommand) {
        CommandStore::new(conn).insert(cmd).expect("insert");
    }

    // ---------------------------------------------------------------
    // insert + duplicate-uuid semantics.
    // ---------------------------------------------------------------

    #[test]
    fn insert_persists_every_field() {
        let conn = fresh();
        let mut cmd = command(1, 1_000);
        cmd.kind = "project_add".into();
        cmd.temp_id = None;
        cmd.error = Some("fixture rejection".into());
        cmd.attempts = 3;
        cmd.state = CommandState::Failed;
        insert(&conn, &cmd);
        assert_eq!(stored(&conn, 1), cmd);
    }

    #[test]
    fn duplicate_uuid_insert_is_noop_and_keeps_original_row_untouched() {
        let conn = fresh();
        let original = command(1, 1_000);
        insert(&conn, &original);

        let mut changed = command(1, 1_000);
        changed.kind = "item_delete".into();
        changed.created_at = 9_999;
        changed.state = CommandState::Failed;
        changed.attempts = 7;
        changed.error = Some("should never be stored".into());
        insert(&conn, &changed);

        let rows: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
            .expect("count");
        assert_eq!(rows, 1, "duplicate uuid must not create a second row");
        assert_eq!(stored(&conn, 1), original, "original row must survive");
    }

    #[test]
    fn insert_without_table_maps_to_queue_storage_error() {
        let conn = Connection::open_in_memory().expect("open");
        match CommandStore::new(&conn).insert(&command(1, 0)) {
            Err(AppError::Queue(QueueError::Storage { .. })) => {}
            other => panic!("expected Queue(Storage), got {other:?}"),
        }
    }

    // ---------------------------------------------------------------
    // pending: filtering, ordering, limit.
    // ---------------------------------------------------------------

    /// 5 commands: `0`,`1` pending; `2` `in_flight`; `3` failed; `4` pending
    /// (`created_at` tie with `1`, so `rowid` breaks it).
    fn mixed_fixture(conn: &Connection) {
        insert(conn, &command(0, 3_000));
        insert(conn, &command(1, 2_000));
        let mut flying = command(2, 1_000);
        flying.state = CommandState::InFlight;
        flying.attempts = 1;
        insert(conn, &flying);
        let mut dead = command(3, 500);
        dead.state = CommandState::Failed;
        dead.error = Some("fixture error".into());
        insert(conn, &dead);
        insert(conn, &command(4, 2_000));
    }

    #[test]
    fn pending_returns_only_pending_rows_regardless_of_created_at() {
        let conn = fresh();
        mixed_fixture(&conn);
        let got = CommandStore::new(&conn).pending(100).expect("pending");
        let mut ids: Vec<u8> = got
            .iter()
            .map(|c| c.uuid.as_bytes()[15])
            .collect::<Vec<_>>();
        ids.sort_unstable();
        assert_eq!(ids, vec![0, 1, 4], "in_flight/failed rows must be excluded");
    }

    #[test]
    fn pending_orders_by_created_at_then_enqueue_rowid() {
        let conn = fresh();
        mixed_fixture(&conn);
        let got = CommandStore::new(&conn).pending(100).expect("pending");
        let order: Vec<u8> = got.iter().map(|c| c.uuid.as_bytes()[15]).collect();
        assert_eq!(order, vec![1, 4, 0], "created_at asc, ties by rowid");
    }

    #[test]
    fn pending_limit_is_honoured_and_zero_yields_nothing() {
        let conn = fresh();
        mixed_fixture(&conn);
        let store = CommandStore::new(&conn);
        let got = store.pending(2).expect("pending(2)");
        assert_eq!(
            got.iter()
                .map(|c| c.uuid.as_bytes()[15])
                .collect::<Vec<_>>(),
            vec![1, 4]
        );
        assert_eq!(
            store.pending(0).expect("pending(0)"),
            Vec::new(),
            "limit 0 must return nothing"
        );
    }

    #[test]
    fn pending_decodes_round_tripped_fields_exactly() {
        let conn = fresh();
        let mut cmd = command(1, 2_000);
        cmd.kind = "item_update".into();
        cmd.args = serde_json::json!({ "id": "6XG7QQFxQcQQ", "nested": { "k": [1, null] } });
        insert(&conn, &cmd);
        assert_eq!(
            CommandStore::new(&conn).pending(10).expect("pending"),
            vec![cmd]
        );
    }

    #[test]
    fn pending_on_empty_table_returns_empty_vec() {
        let conn = fresh();
        assert_eq!(
            CommandStore::new(&conn).pending(10).expect("pending"),
            Vec::new()
        );
    }

    // ---------------------------------------------------------------
    // mark_in_flight / mark_failed / remove.
    // ---------------------------------------------------------------

    #[test]
    fn mark_in_flight_sets_state_and_bumps_attempts() {
        let conn = fresh();
        insert(&conn, &command(1, 1_000));
        insert(&conn, &command(2, 2_000));
        CommandStore::new(&conn)
            .mark_in_flight(&[uuid(1), uuid(2)])
            .expect("mark_in_flight");
        for id in [1, 2] {
            assert_eq!(stored(&conn, id).state, CommandState::InFlight);
            assert_eq!(stored(&conn, id).attempts, 1);
        }
    }

    #[test]
    fn mark_in_flight_accumulates_attempts_across_calls() {
        let conn = fresh();
        insert(&conn, &command(1, 1_000));
        let store = CommandStore::new(&conn);
        store.mark_in_flight(&[uuid(1)]).expect("first");
        store.mark_in_flight(&[uuid(1)]).expect("second");
        assert_eq!(stored(&conn, 1).attempts, 2);
    }

    #[test]
    fn mark_in_flight_empty_slice_and_unknown_uuid_are_ok() {
        let conn = fresh();
        insert(&conn, &command(1, 1_000));
        let store = CommandStore::new(&conn);
        store.mark_in_flight(&[]).expect("empty slice");
        store.mark_in_flight(&[uuid(9)]).expect("unknown uuid");
        assert_eq!(stored(&conn, 1).state, CommandState::Pending);
        assert_eq!(stored(&conn, 1).attempts, 0);
    }

    #[test]
    fn mark_failed_sets_state_and_error_and_still_excluded_from_pending() {
        let conn = fresh();
        insert(&conn, &command(1, 1_000));
        let store = CommandStore::new(&conn);
        store
            .mark_failed(uuid(1), "sync rejected fixture command")
            .expect("mark_failed");
        let row = stored(&conn, 1);
        assert_eq!(row.state, CommandState::Failed);
        assert_eq!(row.error.as_deref(), Some("sync rejected fixture command"));
        assert_eq!(store.pending(10).expect("pending"), Vec::new());
    }

    #[test]
    fn mark_failed_on_unknown_uuid_is_ok() {
        let conn = fresh();
        CommandStore::new(&conn)
            .mark_failed(uuid(9), "no such fixture row")
            .expect("mark_failed unknown");
    }

    #[test]
    fn remove_deletes_row_and_unknown_uuid_is_ok() {
        let conn = fresh();
        insert(&conn, &command(1, 1_000));
        insert(&conn, &command(2, 2_000));
        let store = CommandStore::new(&conn);
        store.remove(uuid(1)).expect("remove");
        store.remove(uuid(9)).expect("remove unknown");
        let left: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
            .expect("count");
        assert_eq!(left, 1);
        assert_eq!(stored(&conn, 2).uuid, uuid(2));
    }

    // ---------------------------------------------------------------
    // referencing_temp_id.
    // ---------------------------------------------------------------

    #[test]
    fn referencing_temp_id_matches_only_that_temp_id() {
        let conn = fresh();
        let mut a = command(1, 1_000);
        a.temp_id = Some("tmp-fixture-shared".into());
        let mut b = command(2, 2_000);
        b.temp_id = Some("tmp-fixture-shared".into());
        let mut c = command(3, 3_000);
        c.temp_id = Some("tmp-fixture-other".into());
        insert(&conn, &a);
        insert(&conn, &b);
        insert(&conn, &c);

        let got = CommandStore::new(&conn)
            .referencing_temp_id("tmp-fixture-shared")
            .expect("lookup");
        assert_eq!(got.len(), 2);
        assert!(got
            .iter()
            .all(|c| c.temp_id.as_deref() == Some("tmp-fixture-shared")));
    }

    #[test]
    fn referencing_temp_id_matches_across_states_and_returns_empty_on_miss() {
        let conn = fresh();
        let mut failed = command(1, 1_000);
        failed.state = CommandState::Failed;
        failed.error = Some("fixture error".into());
        insert(&conn, &failed);
        let store = CommandStore::new(&conn);
        assert_eq!(
            store
                .referencing_temp_id("tmp-fixture-1")
                .expect("lookup")
                .len(),
            1,
            "lookup is by temp_id alone, regardless of state"
        );
        assert_eq!(
            store
                .referencing_temp_id("tmp-fixture-never-inserted")
                .expect("lookup miss"),
            Vec::new()
        );
    }

    #[test]
    fn referencing_temp_id_null_temp_id_rows_never_match_a_string() {
        let conn = fresh();
        let mut bare = command(1, 1_000);
        bare.temp_id = None;
        insert(&conn, &bare);
        assert_eq!(
            CommandStore::new(&conn)
                .referencing_temp_id("")
                .expect("lookup"),
            Vec::new()
        );
    }

    // ---------------------------------------------------------------
    // count_by_state.
    // ---------------------------------------------------------------

    #[test]
    fn count_by_state_all_zero_on_empty_table() {
        let conn = fresh();
        assert_eq!(
            CommandStore::new(&conn).count_by_state().expect("counts"),
            CommandStateCounts::default()
        );
    }

    #[test]
    fn count_by_state_counts_each_state_independently() {
        let conn = fresh();
        let mut flying = command(1, 1_000);
        flying.state = CommandState::InFlight;
        let mut dead = command(2, 2_000);
        dead.state = CommandState::Failed;
        insert(&conn, &command(3, 3_000));
        insert(&conn, &command(4, 4_000));
        insert(&conn, &flying);
        insert(&conn, &dead);
        assert_eq!(
            CommandStore::new(&conn).count_by_state().expect("counts"),
            CommandStateCounts {
                pending: 2,
                in_flight: 1,
                failed: 1,
            }
        );
    }

    #[test]
    fn count_by_state_single_state_leaves_others_zero() {
        let conn = fresh();
        let mut dead = command(1, 1_000);
        dead.state = CommandState::Failed;
        insert(&conn, &dead);
        assert_eq!(
            CommandStore::new(&conn).count_by_state().expect("counts"),
            CommandStateCounts {
                pending: 0,
                in_flight: 0,
                failed: 1,
            }
        );
    }

    #[test]
    fn count_by_state_ignores_unrecognised_state_strings() {
        let conn = fresh();
        conn.execute(
            "INSERT INTO commands (uuid, type, args, state, created_at) \
             VALUES ('u-future', 'item_add', '{}', 'zapped', 1)",
            [],
        )
        .expect("insert foreign state");
        insert(&conn, &command(1, 1_000));
        assert_eq!(
            CommandStore::new(&conn).count_by_state().expect("counts"),
            CommandStateCounts {
                pending: 1,
                in_flight: 0,
                failed: 0,
            }
        );
    }
}
