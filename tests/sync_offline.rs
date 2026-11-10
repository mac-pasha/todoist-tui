//! Integration test: offline resilience, reconnect flush, 401 error, and LWW conflict resolution (`T-512`, EPIC-06).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

#[path = "../src/error.rs"]
mod error;

#[path = "../src/config/mod.rs"]
mod config;

#[path = "../src/log.rs"]
mod log;

#[path = "../src/auth/mod.rs"]
mod auth;

#[path = "../src/api/mod.rs"]
mod api;

#[path = "../src/db/mod.rs"]
mod db;

#[path = "../src/queue/mod.rs"]
mod queue;

#[path = "../src/sync/mod.rs"]
mod sync;

mod support;

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::client::SyncClient;
use api::model::Id;
use api::retry::{RecordingSleeper, RetryPolicy};
use auth::Token;
use config::Config;
use db::{migrations, queries};
use queue::{enqueue, QueuedCommand};
use rusqlite::{params, Connection};
use serde_json::{json, Value};
use support::sync_fixtures::{incremental_sync_response, sample_item, sample_project};
use sync::{SqliteSyncStore, SyncEngine, SyncState};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn setup_store() -> Arc<SqliteSyncStore> {
    let mut conn = Connection::open_in_memory().unwrap();
    migrations::run(&mut conn).unwrap();
    Arc::new(SqliteSyncStore::new(conn))
}

fn no_jitter_policy(max_attempts: u32, base_delay: Duration) -> RetryPolicy {
    let mut policy = RetryPolicy::new();
    policy.max_attempts = max_attempts;
    policy.base_delay = base_delay;
    policy.max_delay = Duration::from_secs(60);
    policy.jitter_fraction = 0.0;
    policy.with_jitter(|| 0.0)
}

#[tokio::test]
async fn offline_outage_then_reconnect_flushes_uuids_once_and_no_secrets_emitted() {
    struct CaptureResponder {
        seen: Arc<Mutex<Vec<String>>>,
    }
    impl Respond for CaptureResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = request.body_json().unwrap();
            let mut status = serde_json::Map::new();
            if let Some(cmds) = body.get("commands").and_then(Value::as_array) {
                let mut guard = self.seen.lock().unwrap();
                for c in cmds {
                    let u = c["uuid"].as_str().unwrap().to_string();
                    guard.push(u.clone());
                    status.insert(u, json!("ok"));
                }
            }
            let res = incremental_sync_response(
                "tok_reconnected",
                vec![],
                Value::Object(status),
                json!({"tmp_offline_2": "real_offline_2"}),
            );
            ResponseTemplate::new(200).set_body_json(res)
        }
    }

    let store = setup_store();

    // Seed local project and item
    let proj_json = sample_project("proj_1", "OfflineProj");
    let item_json = sample_item("item_1", "proj_1", "Original Title");
    {
        let conn = store.shared_connection();
        let guard = conn.lock().unwrap();
        guard
            .execute(
                "INSERT INTO projects (id, name, color, is_favorite, raw_json)
                 VALUES (?1, ?2, 'charcoal', 0, ?3)",
                params!["proj_1", "OfflineProj", proj_json.to_string()],
            )
            .unwrap();
        guard
            .execute(
                "INSERT INTO items (id, project_id, content, description, priority, child_order, checked, is_deleted, added_at, raw_json)
                 VALUES (?1, ?2, ?3, '', 1, 1, 0, 0, '2026-11-01T10:00:00Z', ?4)",
                params!["item_1", "proj_1", "Original Title", item_json.to_string()],
            )
            .unwrap();
    }

    // Bind and drop a TCP listener to guarantee a connection-refused URL (offline)
    let dead_listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let dead_addr = dead_listener.local_addr().unwrap();
    drop(dead_listener);
    let dead_url = format!("http://{dead_addr}");

    let token = Token::new("tok_fake_offline_secret_123".to_string()).unwrap();
    let clock = RecordingSleeper::new();
    let dead_client = Arc::new(
        SyncClient::with_clock(token, &dead_url, clock)
            .unwrap()
            .with_retry_policy(|| no_jitter_policy(1, Duration::from_millis(1))),
    );

    let cfg = Config {
        sync_interval: Duration::from_millis(50),
        ..Config::default()
    };

    let handle = SyncEngine::spawn(dead_client, Arc::clone(&store), Arc::clone(&store), &cfg);
    let mut sub = handle.subscribe();

    // Wait for Syncing -> Offline
    let mut saw_offline = false;
    for _ in 0..5 {
        if let Ok(Some(ev)) = tokio::time::timeout(Duration::from_secs(1), sub.recv()).await {
            assert!(!format!("{ev:?}").contains("tok_fake_offline_secret_123"));
            if ev == SyncState::Offline {
                saw_offline = true;
                break;
            }
        }
    }
    assert!(
        saw_offline,
        "Engine must transition to Offline when transport fails"
    );

    // Local edits still work while offline
    let cmd1 = QueuedCommand::new(
        "item_update",
        json!({"id": "item_1", "content": "Offline Edit 1"}),
        None,
    );
    let cmd2 = QueuedCommand::new(
        "item_add",
        json!({"content": "Offline Added Task", "project_id": "proj_1"}),
        Some("tmp_offline_2".to_string()),
    );
    let u1 = cmd1.uuid;
    let u2 = cmd2.uuid;

    {
        let conn = store.shared_connection();
        let mut guard = conn.lock().unwrap();
        enqueue(&mut guard, &cmd1, |tx| {
            tx.execute(
                "UPDATE items SET content = 'Offline Edit 1' WHERE id = 'item_1'",
                [],
            )
            .map_err(|s| error::DbError::Sqlite { source: s })?;
            Ok(())
        })
        .unwrap();
        enqueue(&mut guard, &cmd2, |_tx| Ok(())).unwrap();
    }

    handle.shutdown();
    handle.stopped().await;

    // Now simulate reconnect with a live mock server capturing every received UUID
    let server = MockServer::start().await;
    let seen_uuids = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen_uuids);

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(CaptureResponder { seen: seen_clone })
        .mount(&server)
        .await;

    let live_token = Token::new("tok_fake_offline_secret_123".to_string()).unwrap();
    let live_client = Arc::new(
        SyncClient::with_clock(live_token, &server.uri(), RecordingSleeper::new())
            .unwrap()
            .with_retry_policy(|| no_jitter_policy(2, Duration::from_millis(5))),
    );

    let handle2 = SyncEngine::spawn(live_client, Arc::clone(&store), Arc::clone(&store), &cfg);
    let mut sub2 = handle2.subscribe();
    handle2.trigger().unwrap();

    loop {
        let ev = tokio::time::timeout(Duration::from_secs(2), sub2.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!format!("{ev:?}").contains("tok_fake_offline_secret_123"));
        if matches!(ev, SyncState::Synced { .. }) {
            break;
        }
    }

    handle2.shutdown();
    handle2.stopped().await;

    // Verify each UUID was seen exactly once
    let recorded = seen_uuids.lock().unwrap().clone();
    assert_eq!(recorded.len(), 2);
    let set: HashSet<String> = recorded.iter().cloned().collect();
    assert_eq!(set.len(), 2);
    assert!(set.contains(&u1.to_string()));
    assert!(set.contains(&u2.to_string()));
}

#[tokio::test]
async fn unauthorized_401_produces_error_state_and_preserves_cache() {
    let server = MockServer::start().await;
    let store = setup_store();

    let proj_json = sample_project("proj_keep", "KeepMe");
    {
        let conn = store.shared_connection();
        let guard = conn.lock().unwrap();
        guard
            .execute(
                "INSERT INTO projects (id, name, color, is_favorite, raw_json)
                 VALUES (?1, ?2, 'charcoal', 0, ?3)",
                params!["proj_keep", "KeepMe", proj_json.to_string()],
            )
            .unwrap();
    }

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(401).set_body_string("Unauthorized"))
        .mount(&server)
        .await;

    let token = Token::new("tok_fake_401_secret".to_string()).unwrap();
    let client = Arc::new(
        SyncClient::with_clock(token, &server.uri(), RecordingSleeper::new())
            .unwrap()
            .with_retry_policy(|| no_jitter_policy(1, Duration::from_millis(1))),
    );

    let cfg = Config {
        sync_interval: Duration::from_secs(60),
        ..Config::default()
    };

    let handle = SyncEngine::spawn(client, Arc::clone(&store), Arc::clone(&store), &cfg);
    let mut sub = handle.subscribe();
    handle.trigger().unwrap();

    let mut events = Vec::new();
    loop {
        let ev = tokio::time::timeout(Duration::from_secs(2), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(!format!("{ev:?}").contains("tok_fake_401_secret"));
        let is_err = matches!(ev, SyncState::Error { .. });
        events.push(ev);
        if is_err {
            break;
        }
    }

    assert!(events.iter().any(|e| matches!(e, SyncState::Syncing)));
    assert!(!events.iter().any(|e| matches!(e, SyncState::Offline)));

    // Cache preserved
    {
        let conn = store.shared_connection();
        let guard = conn.lock().unwrap();
        let projs = queries::all_projects(&guard).unwrap();
        assert_eq!(projs.len(), 1);
        assert_eq!(projs[0].name, "KeepMe");
    }

    handle.shutdown();
    handle.stopped().await;
}

#[tokio::test]
async fn conflict_lww_keeps_local_pending_edit_over_remote_response() {
    let server = MockServer::start().await;
    let store = setup_store();

    // Seed project + pending local edit on item_conflict
    let proj_json = sample_project("proj_c", "ConflictProj");
    {
        let conn = store.shared_connection();
        let guard = conn.lock().unwrap();
        guard
            .execute(
                "INSERT INTO projects (id, name, color, is_favorite, raw_json)
                 VALUES (?1, ?2, 'charcoal', 0, ?3)",
                params!["proj_c", "ConflictProj", proj_json.to_string()],
            )
            .unwrap();
    }

    let cmd = QueuedCommand::new(
        "item_update",
        json!({"id": "item_conflict", "content": "Local Wins"}),
        None,
    );
    {
        let conn = store.shared_connection();
        let mut guard = conn.lock().unwrap();
        enqueue(&mut guard, &cmd, |_tx| Ok(())).unwrap();
    }

    // Mock returns remote item_conflict with "Remote Value" and omits cmd uuid from sync_status
    // (or returns error/rate-limit) so cmd stays pending -> FR-18 LWW replays Local Wins on top!
    let remote_item = sample_item("item_conflict", "proj_c", "Remote Value");
    let res_body =
        incremental_sync_response("tok_conflict", vec![remote_item], json!({}), json!({}));

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(res_body))
        .mount(&server)
        .await;

    let token = Token::new("tok_fake_conflict".to_string()).unwrap();
    let client = Arc::new(
        SyncClient::with_clock(token, &server.uri(), RecordingSleeper::new())
            .unwrap()
            .with_retry_policy(|| no_jitter_policy(1, Duration::from_millis(1))),
    );

    let cfg = Config {
        sync_interval: Duration::from_secs(60),
        ..Config::default()
    };

    let handle = SyncEngine::spawn(client, Arc::clone(&store), Arc::clone(&store), &cfg);
    let mut sub = handle.subscribe();
    handle.trigger().unwrap();

    loop {
        let ev = tokio::time::timeout(Duration::from_secs(2), sub.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(ev, SyncState::Synced { .. }) {
            break;
        }
    }

    // Check final cached item content -> Local Wins
    {
        let conn = store.shared_connection();
        let guard = conn.lock().unwrap();
        let item = queries::item_by_id(&guard, &Id::from("item_conflict"))
            .unwrap()
            .unwrap();
        assert_eq!(item.content, "Local Wins");
    }

    handle.shutdown();
    handle.stopped().await;
}
