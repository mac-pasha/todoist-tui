//! Integration tests for write-queue durability across restart and
//! exactly-once flush (`T-410`, `EPIC-05`).
//!
//! Exercises [`crate::queue::enqueue`], [`crate::queue::flush::build_flush_batch`],
//! [`crate::queue::recovery::recover_on_start`],
//! [`crate::queue::status::apply_sync_status`], and the production
//! [`crate::api::client::SyncClient`] against a local `wiremock` server on
//! `127.0.0.1` using an on-disk SQLite database in a temporary directory.
//!
//! Backoff delays use [`RecordingSleeper`] so no test ever sleeps for real.
//! Every token (`tok_fake_abc123`) and identifier (`6X4Vfake...`) is synthetic.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

#[path = "../src/error.rs"]
mod error;

#[path = "../src/config/paths.rs"]
mod paths;

#[path = "../src/log.rs"]
mod log;

#[path = "../src/auth/token.rs"]
mod token;

#[path = "../src/api/model/mod.rs"]
mod model;

#[path = "../src/api/sync.rs"]
mod sync;

#[path = "../src/api/retry.rs"]
mod retry;

#[path = "../src/api/client.rs"]
mod client;

#[path = "../src/queue/mod.rs"]
mod queue;

#[allow(unused_imports)]
mod config {
    pub(crate) use crate::paths;

    pub(crate) mod test_support {
        pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    }
}

#[allow(unused_imports)]
mod auth {
    pub(crate) use crate::token::Token;
}

#[allow(unused_imports)]
mod api {
    pub(crate) use crate::client;
    pub(crate) use crate::model;
    pub(crate) use crate::retry;
    pub(crate) use crate::sync;
    pub(crate) use crate::sync::{
        CommandError, CommandStatus, SyncRequest, SyncResponse, RESOURCE_TYPES,
    };
}

use std::collections::HashMap;
use std::time::{Duration, Instant};

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use tempfile::TempDir;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

use crate::api::client::SyncClient;
use crate::api::retry::{RecordingSleeper, RetryPolicy};
use crate::auth::Token;
use crate::error::{ApiError, AppError, DbError};
use crate::queue::{
    apply_sync_status, build_flush_batch, enqueue, recover_on_start, schema, CommandState,
    CommandStateCounts, CommandStore, QueuedCommand, DEFAULT_MAX_BATCH,
};

const FAKE_TOKEN: &str = "tok_fake_abc123";
const EXPECTED_AUTH_HEADER: &str = "Bearer tok_fake_abc123";

fn fake_token() -> Token {
    Token::new(FAKE_TOKEN.to_string()).expect("fake token is non-empty")
}

fn no_jitter_policy(max_attempts: u32, base_delay: Duration) -> RetryPolicy {
    let mut policy = RetryPolicy::new();
    policy.max_attempts = max_attempts;
    policy.base_delay = base_delay;
    policy.max_delay = Duration::from_secs(60);
    policy.jitter_fraction = 0.0;
    policy.with_jitter(|| 0.0)
}

fn client_for(
    server: &MockServer,
    clock: &RecordingSleeper,
    (max_attempts, base_delay): (u32, Duration),
) -> SyncClient<RecordingSleeper> {
    SyncClient::with_clock(fake_token(), &server.uri(), clock.clone())
        .expect("loopback http:// URL accepted in debug builds")
        .with_retry_policy(move || no_jitter_policy(max_attempts, base_delay))
}

fn open_db_with_standin_cache(db_path: &std::path::Path) -> Connection {
    let conn = Connection::open(db_path).expect("open on-disk sqlite db");
    schema::apply(&conn).expect("apply commands schema");
    conn.execute(
        "CREATE TABLE IF NOT EXISTS cache_items (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            project_id TEXT NOT NULL
        )",
        [],
    )
    .expect("create stand-in cache_items table");
    conn
}

fn count_rows(conn: &Connection, table: &str) -> i64 {
    let sql = format!("SELECT COUNT(*) FROM {table}");
    conn.query_row(&sql, [], |r| r.get(0)).expect("count rows")
}

fn extract_wire_uuids(requests: &[wiremock::Request]) -> Vec<Uuid> {
    let mut seen = Vec::new();
    for req in requests {
        let body: Value = req.body_json().expect("request body must be valid JSON");
        if let Some(cmds) = body.get("commands").and_then(Value::as_array) {
            for cmd in cmds {
                let uuid_str = cmd["uuid"].as_str().expect("command uuid string");
                seen.push(Uuid::parse_str(uuid_str).expect("valid uuid on wire"));
            }
        }
    }
    seen
}

// ---------------------------------------------------------------------------
// Acceptance 1: enqueue + optimistic apply where local apply fails -> neither
// a queued command nor the mutated row exists (transaction abort simulates crash)
// ---------------------------------------------------------------------------

#[test]
fn enqueue_abort_when_local_apply_fails_leaves_neither_command_nor_cache_row() {
    let temp_dir = TempDir::new().expect("create tempdir");
    let db_path = temp_dir.path().join("abort_test.sqlite");
    let mut conn = open_db_with_standin_cache(&db_path);

    let cmd = QueuedCommand::new(
        "item_add",
        json!({
            "content": "Buy oat milk",
            "project_id": "6X4VfakePrj0001",
        }),
        Some("temp_fake_item_0001".to_string()),
    );

    let result = enqueue(&mut conn, &cmd, |tx| {
        tx.execute(
            "INSERT INTO cache_items (id, content, project_id) VALUES (?1, ?2, ?3)",
            params!["temp_fake_item_0001", "Buy oat milk", "6X4VfakePrj0001"],
        )
        .map_err(|source| DbError::Sqlite { source })?;

        // Simulate crash / failure mid-transaction after the optimistic row was inserted.
        Err(DbError::Sqlite {
            source: rusqlite::Error::InvalidQuery,
        }
        .into())
    });

    assert!(result.is_err(), "enqueue must fail when local_apply fails");
    assert_eq!(
        count_rows(&conn, "commands"),
        0,
        "no queued command row may exist after transaction abort"
    );
    assert_eq!(
        count_rows(&conn, "cache_items"),
        0,
        "no optimistic cache row may exist after transaction abort"
    );

    // Reopen from disk to prove nothing was persisted across connections either.
    drop(conn);
    let reopened = open_db_with_standin_cache(&db_path);
    assert_eq!(count_rows(&reopened, "commands"), 0);
    assert_eq!(count_rows(&reopened, "cache_items"), 0);
    assert_eq!(
        CommandStore::new(&reopened)
            .pending(10)
            .expect("pending")
            .len(),
        0
    );
}

// ---------------------------------------------------------------------------
// Acceptance 2 & 3: N=5 commands enqueued, connection dropped mid-flight
// (rows left `in_flight`), DB reopened, `recover_on_start` run, batch flushed
// against a wiremock `/sync` that records every received command `uuid` ->
// each uuid is seen exactly once across both "runs", and recorded uuids are
// identical to the pre-restart snapshot (no regeneration).
// ---------------------------------------------------------------------------

#[tokio::test]
async fn five_commands_survive_mid_flight_restart_with_identical_uuids_and_flush_once() {
    let temp_dir = TempDir::new().expect("create tempdir");
    let db_path = temp_dir.path().join("durability_restart.sqlite");

    let server = MockServer::start().await;

    // Pre-restart ("run 1"): enqueue N=5 commands, snapshot their UUIDs, mark
    // them `in_flight` via `build_flush_batch`, then drop the connection before
    // the batch ever reaches the network (simulating a process kill mid-flush).
    let mut pre_restart_uuids = Vec::with_capacity(5);
    {
        let mut conn = open_db_with_standin_cache(&db_path);
        for idx in 1..=5_u32 {
            let temp_id = format!("temp_fake_item_{idx:04}");
            let content = format!("Durable task #{idx}");
            let mut cmd = QueuedCommand::new(
                "item_add",
                json!({
                    "content": content,
                    "project_id": "6X4VfakePrj0001",
                }),
                Some(temp_id.clone()),
            );
            cmd.created_at = 1_700_000_000 + i64::from(idx);
            pre_restart_uuids.push(cmd.uuid);

            enqueue(&mut conn, &cmd, |tx| {
                tx.execute(
                    "INSERT INTO cache_items (id, content, project_id) VALUES (?1, ?2, ?3)",
                    params![temp_id, content, "6X4VfakePrj0001"],
                )
                .map_err(|source| DbError::Sqlite { source })?;
                Ok(())
            })
            .expect("enqueue command");
        }

        let (in_flight_batch, _unsent_request) = build_flush_batch(&mut conn, DEFAULT_MAX_BATCH)
            .expect("build_flush_batch in run 1")
            .expect("5 pending commands must produce a batch");
        assert_eq!(in_flight_batch.len(), 5);
        assert_eq!(
            in_flight_batch
                .iter()
                .map(|c| c.uuid)
                .collect::<Vec<Uuid>>(),
            pre_restart_uuids
        );

        let counts = CommandStore::new(&conn)
            .count_by_state()
            .expect("count_by_state");
        assert_eq!(
            counts,
            CommandStateCounts {
                pending: 0,
                in_flight: 5,
                failed: 0,
            }
        );
        // `conn` dropped here mid-flight without sending `_unsent_request` or
        // calling `apply_sync_status`.
    }

    // Mount the wiremock `/api/v1/sync` endpoint that marks all 5 pre-restart
    // UUIDs as `"ok"`.
    let mut sync_status_json = serde_json::Map::new();
    for uuid in &pre_restart_uuids {
        sync_status_json.insert(uuid.to_string(), json!("ok"));
    }
    let response_body = json!({
        "sync_token": "fakeSyncTokenAfterRestart0001",
        "full_sync": false,
        "temp_id_mapping": {},
        "sync_status": sync_status_json,
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .and(header("authorization", EXPECTED_AUTH_HEADER))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .expect(1)
        .mount(&server)
        .await;

    // Post-restart ("run 2"): reopen the same on-disk SQLite DB, run
    // `recover_on_start`, verify UUIDs match the pre-restart snapshot without
    // regeneration, and flush against wiremock.
    let mut reopened = open_db_with_standin_cache(&db_path);
    assert_eq!(
        count_rows(&reopened, "cache_items"),
        5,
        "optimistic cache rows must survive restart"
    );

    let recovered_count = recover_on_start(&mut reopened).expect("recover_on_start");
    assert_eq!(recovered_count, 5, "all 5 in_flight commands must recover");

    let pending_after_recovery = CommandStore::new(&reopened)
        .pending(10)
        .expect("pending after recovery");
    let recovered_uuids: Vec<Uuid> = pending_after_recovery.iter().map(|c| c.uuid).collect();
    assert_eq!(
        recovered_uuids, pre_restart_uuids,
        "recovered commands must preserve the exact pre-restart UUIDs and order"
    );
    for cmd in &pending_after_recovery {
        assert_eq!(cmd.state, CommandState::Pending);
        assert_eq!(cmd.attempts, 1, "attempt counter from run 1 is preserved");
    }

    let (batch, request) = build_flush_batch(&mut reopened, DEFAULT_MAX_BATCH)
        .expect("build_flush_batch in run 2")
        .expect("recovered commands must build a flush batch");
    assert_eq!(batch.len(), 5);

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock, (2, Duration::from_millis(5)));
    let sync_response = client.sync(&request).await.expect("flush request succeeds");

    let tx = reopened.transaction().expect("begin status tx");
    let outcome = apply_sync_status(&tx, &batch, &sync_response.sync_status, |_, _, _| Ok(()))
        .expect("apply_sync_status");
    tx.commit().expect("commit status tx");

    assert_eq!(outcome.succeeded, 5);
    assert_eq!(outcome.failed, 0);
    assert_eq!(outcome.requeued, 0);

    // Every command was acknowledged and removed; a subsequent flush is a no-op.
    assert_eq!(
        CommandStore::new(&reopened)
            .count_by_state()
            .expect("counts"),
        CommandStateCounts {
            pending: 0,
            in_flight: 0,
            failed: 0,
        }
    );
    assert!(
        build_flush_batch(&mut reopened, DEFAULT_MAX_BATCH)
            .expect("second flush")
            .is_none(),
        "flushed commands must not be sent a second time"
    );

    server.verify().await;
    let received_requests = server
        .received_requests()
        .await
        .expect("wiremock records requests");
    assert_eq!(
        received_requests.len(),
        1,
        "exactly one /sync request sent across both runs"
    );

    let wire_uuids = extract_wire_uuids(&received_requests);
    assert_eq!(
        wire_uuids, pre_restart_uuids,
        "wiremock must observe the exact pre-restart UUIDs in enqueue order"
    );

    // Assert each UUID was seen on the wirepertaining to the flush exactly once.
    let mut counts_by_uuid: HashMap<Uuid, usize> = HashMap::new();
    for u in &wire_uuids {
        *counts_by_uuid.entry(*u).or_insert(0) += 1;
    }
    for expected_uuid in &pre_restart_uuids {
        assert_eq!(
            counts_by_uuid.get(expected_uuid).copied(),
            Some(1),
            "uuid {expected_uuid} must be seen on the wire exactly once"
        );
    }
    assert!(clock.recorded().is_empty(), "no retry sleep on 200 OK");
}

// ---------------------------------------------------------------------------
// Acceptance 4: a wiremock 500 on the flush leaves all commands re-flushable
// with unchanged uuids and no duplicate rows
// ---------------------------------------------------------------------------

#[tokio::test]
async fn wiremock_500_on_flush_leaves_commands_reflushable_with_unchanged_uuids_and_no_duplicates()
{
    let started = Instant::now();
    let temp_dir = TempDir::new().expect("create tempdir");
    let db_path = temp_dir.path().join("flush_500.sqlite");
    let mut conn = open_db_with_standin_cache(&db_path);

    let mut initial_uuids = Vec::with_capacity(5);
    for idx in 1..=5_u32 {
        let mut cmd = QueuedCommand::new(
            "item_add",
            json!({
                "content": format!("Task after 500 #{idx}"),
                "project_id": "6X4VfakePrj0001",
            }),
            Some(format!("temp_fake_500_{idx}")),
        );
        cmd.created_at = 1_700_001_000 + i64::from(idx);
        initial_uuids.push(cmd.uuid);
        enqueue(&mut conn, &cmd, |_| Ok(())).expect("enqueue");
    }

    // Attempt 1: build batch and flush against a server returning HTTP 500.
    let (batch1, req1) = build_flush_batch(&mut conn, DEFAULT_MAX_BATCH)
        .expect("build_flush_batch 1")
        .expect("batch 1 present");
    assert_eq!(
        batch1.iter().map(|c| c.uuid).collect::<Vec<_>>(),
        initial_uuids
    );

    let failing_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .and(header("authorization", EXPECTED_AUTH_HEADER))
        .respond_with(ResponseTemplate::new(500))
        .mount(&failing_server)
        .await;

    let clock = RecordingSleeper::new();
    let bad_client = client_for(&failing_server, &clock, (2, Duration::from_millis(10)));
    let err = bad_client
        .sync(&req1)
        .await
        .expect_err("HTTP 500 must fail after exhausting retry policy");
    assert!(matches!(
        err,
        AppError::Api(ApiError::RetriesExhausted { attempts: 2 })
    ));
    assert_eq!(clock.recorded(), vec![Duration::from_millis(10)]);

    // Recover the in-flight commands after the failed HTTP flush.
    let recovered = recover_on_start(&mut conn).expect("recover after 500");
    assert_eq!(recovered, 5);

    // Verify no duplicate rows and unchanged UUIDs.
    assert_eq!(
        count_rows(&conn, "commands"),
        5,
        "failed flush must not create duplicate command rows"
    );
    let pending = CommandStore::new(&conn)
        .pending(10)
        .expect("pending after 500 recovery");
    assert_eq!(
        pending.iter().map(|c| c.uuid).collect::<Vec<_>>(),
        initial_uuids,
        "UUIDs must remain unchanged after 500 failure and recovery"
    );

    // Attempt 2: re-flush against a healthy 200 OK mock server and verify
    // the exact same UUIDs are sent and acknowledged.
    let (batch2, req2) = build_flush_batch(&mut conn, DEFAULT_MAX_BATCH)
        .expect("build_flush_batch 2")
        .expect("batch 2 present");
    assert_eq!(
        batch2.iter().map(|c| c.uuid).collect::<Vec<_>>(),
        initial_uuids
    );
    assert_eq!(
        req1.commands, req2.commands,
        "re-flushed wire commands must be byte-for-byte identical to the first attempt"
    );

    let mut sync_status_json = serde_json::Map::new();
    for uuid in &initial_uuids {
        sync_status_json.insert(uuid.to_string(), json!("ok"));
    }
    let ok_body = json!({
        "sync_token": "fakeSyncTokenAfter500Recovery",
        "full_sync": false,
        "temp_id_mapping": {},
        "sync_status": sync_status_json,
    });

    let ok_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .and(header("authorization", EXPECTED_AUTH_HEADER))
        .respond_with(ResponseTemplate::new(200).set_body_json(&ok_body))
        .expect(1)
        .mount(&ok_server)
        .await;

    let ok_clock = RecordingSleeper::new();
    let ok_client = client_for(&ok_server, &ok_clock, (1, Duration::from_millis(5)));
    let resp = ok_client
        .sync(&req2)
        .await
        .expect("second flush against 200 OK server must succeed");

    let tx = conn.transaction().expect("begin tx");
    let outcome =
        apply_sync_status(&tx, &batch2, &resp.sync_status, |_, _, _| Ok(())).expect("apply status");
    tx.commit().expect("commit tx");

    assert_eq!(outcome.succeeded, 5);
    assert_eq!(count_rows(&conn, "commands"), 0);
    assert!(
        started.elapsed() < Duration::from_secs(5),
        "test must complete without real sleeping"
    );
}
