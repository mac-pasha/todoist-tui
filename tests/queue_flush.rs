//! Integration tests for queue flushing, temp-id rewrite, and mixed `sync_status` (T-411, EPIC-05).

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

#[path = "../src/db/migrations.rs"]
mod db_migrations;

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

#[allow(unused_imports)]
mod db {
    pub(crate) use crate::db_migrations as migrations;
}

use std::collections::HashMap;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Duration;

use client::SyncClient;
use queue::flush::build_flush_batch;
use queue::status::{apply_sync_status, failed_commands};
use queue::store::CommandStore;
use queue::temp_ids::{resolve_temp_ids, rewrite_sqlite_cache_temp_ids};
use queue::{schema, QueuedCommand, DEFAULT_MAX_BATCH};
use retry::{RecordingSleeper, RetryPolicy};
use rusqlite::Connection;
use serde_json::json;
use sync::SyncResponse;
use token::Token;
use uuid::Uuid;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const FAKE_TOKEN: &str = "tok_fake_flush100";
const EXPECTED_AUTH_HEADER: &str = "Bearer tok_fake_flush100";

fn fake_token() -> Token {
    Token::new(FAKE_TOKEN.to_string()).expect("valid token")
}

fn no_jitter_policy(max_attempts: u32, base_delay: Duration) -> RetryPolicy {
    let mut policy = RetryPolicy::new();
    policy.max_attempts = max_attempts;
    policy.base_delay = base_delay;
    policy.max_delay = Duration::from_secs(60);
    policy.jitter_fraction = 0.0;
    policy.with_jitter(|| 0.0)
}

fn client_for(server: &MockServer, clock: &RecordingSleeper) -> SyncClient<RecordingSleeper> {
    SyncClient::with_clock(fake_token(), &server.uri(), clock.clone())
        .expect("loopback http url accepted in debug builds")
        .with_retry_policy(move || no_jitter_policy(3, Duration::from_millis(5)))
}

fn setup_test_db() -> Connection {
    let conn = Connection::open_in_memory().unwrap();
    schema::apply(&conn).unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS projects (
            id TEXT PRIMARY KEY,
            name TEXT NOT NULL,
            raw_json TEXT NOT NULL
        )",
        [],
    )
    .unwrap();
    conn.execute(
        "CREATE TABLE IF NOT EXISTS items (
            id TEXT PRIMARY KEY,
            content TEXT NOT NULL,
            project_id TEXT,
            raw_json TEXT NOT NULL
        )",
        [],
    )
    .unwrap();
    conn
}

#[tokio::test]
async fn test_hundred_pending_commands_flush_in_single_request_ordered() {
    let server = MockServer::start().await;
    let mut conn = setup_test_db();

    let mut expected_uuids = Vec::with_capacity(100);
    let store = CommandStore::new(&conn);
    for i in 0..100 {
        let cmd = QueuedCommand::new("item_add", json!({"content": format!("task {i}")}), None);
        expected_uuids.push(cmd.uuid);
        store.insert(&cmd).unwrap();
    }

    let mut sync_status_map = serde_json::Map::new();
    for u in &expected_uuids {
        sync_status_map.insert(u.to_string(), json!("ok"));
    }

    let fake_response = json!({
        "sync_token": "tok_after_100",
        "full_sync": false,
        "sync_status": sync_status_map,
        "temp_id_mapping": {},
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .and(header("authorization", EXPECTED_AUTH_HEADER))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fake_response))
        .expect(1)
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock);

    let (batch, req) = build_flush_batch(&mut conn, 100)
        .unwrap()
        .expect("batch present");
    assert_eq!(batch.len(), 100);
    for (i, cmd) in batch.iter().enumerate() {
        assert_eq!(cmd.uuid, expected_uuids[i]);
    }

    let res = client.sync(&req).await.unwrap();
    assert_eq!(res.sync_token, "tok_after_100");

    let tx = conn.transaction().unwrap();
    let status_outcome =
        apply_sync_status(&tx, &batch, &res.sync_status, |_tx, _cmd, _cls| Ok(())).unwrap();
    tx.commit().unwrap();

    assert_eq!(status_outcome.succeeded, 100);
    assert_eq!(status_outcome.failed, 0);

    let remaining = CommandStore::new(&conn).pending(100).unwrap();
    assert_eq!(remaining.len(), 0);
}

#[tokio::test]
async fn test_temp_id_mapping_rewrites_commands_and_cache() {
    let server = MockServer::start().await;
    let mut conn = setup_test_db();

    // Insert dummy project row in projects table with temp id
    conn.execute(
        "INSERT INTO projects (id, name, raw_json)
         VALUES ('tmp_proj_A', 'Test Project', '{\"id\":\"tmp_proj_A\",\"name\":\"Test Project\"}')",
        [],
    ).unwrap();

    let cmd_proj = QueuedCommand::new(
        "project_add",
        json!({"name": "Test Project"}),
        Some("tmp_proj_A".to_string()),
    );
    let cmd_item = QueuedCommand::new(
        "item_add",
        json!({"content": "Task 1", "project_id": "tmp_proj_A"}),
        Some("tmp_item_1".to_string()),
    );

    let store = CommandStore::new(&conn);
    store.insert(&cmd_proj).unwrap();
    store.insert(&cmd_item).unwrap();

    let fake_res = json!({
        "sync_token": "tok_temp_id",
        "full_sync": false,
        "sync_status": {
            cmd_proj.uuid.to_string(): "ok",
            cmd_item.uuid.to_string(): "ok"
        },
        "temp_id_mapping": {
            "tmp_proj_A": "real_proj_123",
            "tmp_item_1": "real_item_456"
        }
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fake_res))
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock);

    let (batch, req) = build_flush_batch(&mut conn, DEFAULT_MAX_BATCH)
        .unwrap()
        .expect("batch present");
    let res = client.sync(&req).await.unwrap();

    let tx = conn.transaction().unwrap();
    // Resolve temp ids on queue and cache
    let remapped = resolve_temp_ids(&tx, &res.temp_id_mapping, |t, map| {
        rewrite_sqlite_cache_temp_ids(t, map)
    })
    .unwrap();
    assert_eq!(remapped, 2);

    // Apply sync status (removes ok commands)
    apply_sync_status(&tx, &batch, &res.sync_status, |_tx, _cmd, _cls| Ok(())).unwrap();
    tx.commit().unwrap();

    // Verify no commands row contains tmp_proj_A
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM commands WHERE temp_id = 'tmp_proj_A' OR args LIKE '%tmp_proj_A%'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);

    // Verify cache project row updated from tmp_proj_A to real_proj_123
    let proj_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE id = 'tmp_proj_A'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(proj_count, 0);

    let real_proj_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM projects WHERE id = 'real_proj_123'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(real_proj_count, 1);
}

#[tokio::test]
async fn test_mixed_sync_status_removes_ok_and_marks_failed_without_stall() {
    let mut conn = setup_test_db();

    let fixture_str = include_str!("fixtures/sync_commands_mixed.json");
    let response: SyncResponse = serde_json::from_str(fixture_str).unwrap();

    let ok_uuid = Uuid::parse_str("c0000000-0000-4000-8000-000000000001").unwrap();
    let fail_uuid = Uuid::parse_str("c0000000-0000-4000-8000-000000000002").unwrap();
    let pending_uuid = Uuid::parse_str("c0000000-0000-4000-8000-000000000003").unwrap();

    let mut cmd_ok = QueuedCommand::new("item_add", json!({"content": "ok"}), None);
    cmd_ok.uuid = ok_uuid;
    let mut cmd_fail = QueuedCommand::new("item_delete", json!({"id": "123"}), None);
    cmd_fail.uuid = fail_uuid;
    let mut cmd_pending = QueuedCommand::new("item_update", json!({"id": "456"}), None);
    cmd_pending.uuid = pending_uuid;

    let store = CommandStore::new(&conn);
    store.insert(&cmd_ok).unwrap();
    store.insert(&cmd_fail).unwrap();
    store.insert(&cmd_pending).unwrap();

    let (batch, _req) = build_flush_batch(&mut conn, 2)
        .unwrap()
        .expect("batch of 2");
    assert_eq!(batch.len(), 2);

    let rollback_calls = Arc::new(AtomicUsize::new(0));
    let rb_clone = Arc::clone(&rollback_calls);

    let tx = conn.transaction().unwrap();
    let outcome = apply_sync_status(&tx, &batch, &response.sync_status, move |_t, _cmd, cls| {
        if cls.should_rollback() {
            rb_clone.fetch_add(1, Ordering::SeqCst);
        }
        Ok(())
    })
    .unwrap();
    tx.commit().unwrap();

    assert_eq!(outcome.succeeded, 1);
    assert_eq!(outcome.failed, 1);
    assert_eq!(rollback_calls.load(Ordering::SeqCst), 1);

    // Failing command is marked failed with error message
    let failed = failed_commands(&conn).unwrap();
    assert_eq!(failed.len(), 1);
    assert_eq!(failed[0].uuid, fail_uuid.to_string());
    assert!(failed[0].error.contains("Item not found"));

    // Next build_flush_batch still returns remaining pending command (does not stall)
    let (next_batch, _next_req) = build_flush_batch(&mut conn, 10)
        .unwrap()
        .expect("next batch");
    assert_eq!(next_batch.len(), 1);
    assert_eq!(next_batch[0].uuid, pending_uuid);
}

#[tokio::test]
async fn test_rollback_vs_keep_class_behavior() {
    let mut conn = setup_test_db();

    let rollback_uuid = Uuid::new_v4();
    let keep_uuid = Uuid::new_v4();

    let mut cmd_rb = QueuedCommand::new("item_add", json!({"content": "rb"}), None);
    cmd_rb.uuid = rollback_uuid;
    let mut cmd_kp = QueuedCommand::new("item_add", json!({"content": "kp"}), None);
    cmd_kp.uuid = keep_uuid;

    let store = CommandStore::new(&conn);
    store.insert(&cmd_rb).unwrap();
    store.insert(&cmd_kp).unwrap();

    let (batch, _req) = build_flush_batch(&mut conn, 2).unwrap().expect("batch");

    let mut status_map = HashMap::new();
    // Error code 32 is NotFound -> Rollback
    status_map.insert(
        rollback_uuid.to_string(),
        sync::CommandStatus::Error(sync::CommandError {
            error_code: Some(32),
            error: Some("Not found".to_string()),
            extra: HashMap::new(),
        }),
    );
    // Error code 420 is RateLimited -> Keep
    status_map.insert(
        keep_uuid.to_string(),
        sync::CommandStatus::Error(sync::CommandError {
            error_code: Some(420),
            error: Some("Rate limited".to_string()),
            extra: HashMap::new(),
        }),
    );

    let rolled_back = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rb_ref = Arc::clone(&rolled_back);

    let tx = conn.transaction().unwrap();
    apply_sync_status(&tx, &batch, &status_map, move |_t, cmd, cls| {
        if cls.should_rollback() {
            rb_ref.lock().unwrap().push(cmd.uuid);
        }
        Ok(())
    })
    .unwrap();
    tx.commit().unwrap();

    let rb_list = rolled_back.lock().unwrap();
    assert_eq!(rb_list.len(), 1);
    assert_eq!(rb_list[0], rollback_uuid);
}

#[tokio::test]
async fn test_wiremock_verifies_authorization_and_path() {
    let server = MockServer::start().await;
    let mut conn = setup_test_db();

    let cmd = QueuedCommand::new("item_close", json!({"id": "item_999"}), None);
    let store = CommandStore::new(&conn);
    store.insert(&cmd).unwrap();

    let fake_res = json!({
        "sync_token": "tok_auth_check",
        "full_sync": false,
        "sync_status": {
            cmd.uuid.to_string(): "ok"
        },
        "temp_id_mapping": {}
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .and(header("authorization", EXPECTED_AUTH_HEADER))
        .respond_with(ResponseTemplate::new(200).set_body_json(&fake_res))
        .expect(1)
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock);

    let (batch, req) = build_flush_batch(&mut conn, 10).unwrap().expect("batch");

    let res = client.sync(&req).await.unwrap();
    assert_eq!(res.sync_token, "tok_auth_check");

    let tx = conn.transaction().unwrap();
    apply_sync_status(&tx, &batch, &res.sync_status, |_tx, _cmd, _cls| Ok(())).unwrap();
    tx.commit().unwrap();
}
