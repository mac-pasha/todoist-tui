//! Integration test: scheduled and on-demand sync against a mock API (`T-511`, EPIC-06).

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

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use api::client::SyncClient;
use api::model::Id;
use api::retry::{RecordingSleeper, RetryPolicy};
use auth::Token;
use config::Config;
use db::{migrations, queries};
use queue::{enqueue, QueuedCommand};
use rusqlite::Connection;
use serde_json::{json, Value};
use support::sync_fixtures::{
    full_sync_response, incremental_sync_response, sample_item, sample_project,
};
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

fn mock_client(server: &MockServer) -> Arc<SyncClient<RecordingSleeper>> {
    let token = Token::new("tok_fake_abc123".to_string()).unwrap();
    let clock = RecordingSleeper::new();

    Arc::new(
        SyncClient::with_clock(token, &server.uri(), clock)
            .unwrap()
            .with_retry_policy(|| no_jitter_policy(2, Duration::from_millis(5))),
    )
}

#[tokio::test]
async fn full_then_incremental_sync_and_remote_item_cached() {
    struct SeqResponder {
        count: AtomicUsize,
        full_body: Value,
        inc_body: Value,
    }
    impl Respond for SeqResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let idx = self.count.fetch_add(1, Ordering::SeqCst);
            let req_json: Value = request.body_json().unwrap();
            if idx == 0 {
                assert_eq!(req_json["sync_token"], "*");
                ResponseTemplate::new(200).set_body_json(&self.full_body)
            } else {
                assert_eq!(req_json["sync_token"], "tok_sync_1");
                ResponseTemplate::new(200).set_body_json(&self.inc_body)
            }
        }
    }

    let server = MockServer::start().await;
    let store = setup_store();

    // First response: full sync returning proj_1 & item_1
    let full_body = full_sync_response(
        "tok_sync_1",
        vec![sample_item("item_1", "proj_1", "Initial Remote Task")],
        vec![sample_project("proj_1", "Work")],
    );

    // Second response: incremental sync returning item_2
    let inc_body = incremental_sync_response(
        "tok_sync_2",
        vec![sample_item("item_2", "proj_1", "Second Remote Task")],
        json!({}),
        json!({}),
    );

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(SeqResponder {
            count: AtomicUsize::new(0),
            full_body,
            inc_body,
        })
        .mount(&server)
        .await;

    let cfg = Config {
        sync_interval: Duration::from_millis(200),
        ..Config::default()
    };

    let handle = SyncEngine::spawn(
        mock_client(&server),
        Arc::clone(&store),
        Arc::clone(&store),
        &cfg,
    );

    let mut sub = handle.subscribe();

    // Trigger immediate first sync (full)
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

    // Verify item_1 in SQLite cache
    {
        let conn = store.shared_connection();
        let guard = conn.lock().unwrap();
        let item = queries::item_by_id(&guard, &Id::from("item_1"))
            .unwrap()
            .unwrap();
        assert_eq!(item.content, "Initial Remote Task");
    }

    // Trigger second sync (incremental)
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

    // Verify item_2 appeared in cache
    {
        let conn = store.shared_connection();
        let guard = conn.lock().unwrap();
        let item2 = queries::item_by_id(&guard, &Id::from("item_2"))
            .unwrap()
            .unwrap();
        assert_eq!(item2.content, "Second Remote Task");
    }

    handle.shutdown();
    handle.stopped().await;
}

#[tokio::test]
async fn local_pending_command_flushed_and_visible_within_cycle() {
    let server = MockServer::start().await;
    let store = setup_store();

    // Enqueue a local item_add command
    let cmd = QueuedCommand::new(
        "item_add",
        json!({"content": "Local Task", "project_id": "proj_1"}),
        Some("tmp_local_1".to_string()),
    );
    let cmd_uuid = cmd.uuid.to_string();

    {
        let conn = store.shared_connection();
        let mut guard = conn.lock().unwrap();
        enqueue(&mut guard, &cmd, |_tx| Ok(())).unwrap();
    }

    let response_body = incremental_sync_response(
        "tok_after_local",
        vec![sample_item("real_item_100", "proj_1", "Local Task")],
        json!({ cmd_uuid: "ok" }),
        json!({ "tmp_local_1": "real_item_100" }),
    );

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(&response_body))
        .expect(1)
        .mount(&server)
        .await;

    let cfg = Config {
        sync_interval: Duration::from_secs(60),
        ..Config::default()
    };

    let start = Instant::now();
    let handle = SyncEngine::spawn(
        mock_client(&server),
        Arc::clone(&store),
        Arc::clone(&store),
        &cfg,
    );

    let mut sub = handle.subscribe();
    handle.trigger().unwrap();

    loop {
        let ev = tokio::time::timeout(Duration::from_secs(3), sub.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(ev, SyncState::Synced { .. }) {
            break;
        }
    }

    assert!(start.elapsed() < Duration::from_secs(5));

    // Verify item was resolved and cached under real_item_100
    {
        let conn = store.shared_connection();
        let guard = conn.lock().unwrap();
        let item = queries::item_by_id(&guard, &Id::from("real_item_100"))
            .unwrap()
            .unwrap();
        assert_eq!(item.content, "Local Task");
    }

    handle.shutdown();
    handle.stopped().await;
}

#[tokio::test]
async fn overlapping_triggers_during_slow_response_coalesce() {
    struct SlowResponder {
        count: Arc<AtomicUsize>,
    }
    impl Respond for SlowResponder {
        fn respond(&self, _request: &Request) -> ResponseTemplate {
            self.count.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200)
                .set_delay(Duration::from_millis(120))
                .set_body_json(incremental_sync_response(
                    "tok_slow",
                    vec![],
                    json!({}),
                    json!({}),
                ))
        }
    }

    let server = MockServer::start().await;
    let store = setup_store();

    let count = Arc::new(AtomicUsize::new(0));
    let count_clone = Arc::clone(&count);

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(SlowResponder { count: count_clone })
        .mount(&server)
        .await;

    let cfg = Config {
        sync_interval: Duration::from_secs(60),
        ..Config::default()
    };

    let handle = SyncEngine::spawn(
        mock_client(&server),
        Arc::clone(&store),
        Arc::clone(&store),
        &cfg,
    );

    let mut sub = handle.subscribe();

    // First trigger starts cycle 1
    handle.trigger().unwrap();

    // Wait for cycle 1 to emit Syncing
    loop {
        let ev = sub.recv().await.unwrap();
        if ev == SyncState::Syncing {
            break;
        }
    }

    // Fire 5 triggers while cycle 1 is sleeping in HTTP mock -> must coalesce into 1 follow-up cycle
    for _ in 0..5 {
        handle.trigger().unwrap();
    }

    // Wait for two Synced events
    let mut synced = 0;
    while synced < 2 {
        let ev = tokio::time::timeout(Duration::from_secs(2), sub.recv())
            .await
            .unwrap()
            .unwrap();
        if matches!(ev, SyncState::Synced { .. }) {
            synced += 1;
        }
    }

    // Brief yield to verify no 3rd run starts
    tokio::time::sleep(Duration::from_millis(50)).await;
    assert_eq!(count.load(Ordering::SeqCst), 2);

    handle.shutdown();
    handle.stopped().await;
}
