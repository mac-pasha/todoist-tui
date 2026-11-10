//! Acceptance suite: full offline -> online cycle against the mock Sync API (`T-1102`, EPIC-12).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

mod support;

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::Arc;
use std::time::Instant;

use rusqlite::{params, Connection};
use serde_json::{json, Value};
use support::{MockSyncApi, TestHome};
use uuid::Uuid;
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

#[tokio::test]
async fn full_then_incremental_sync_populates_and_updates_cache() {
    let server = MockServer::start().await;
    let full_body = include_str!("fixtures/sync_full_small.json").to_string();
    let inc_body = include_str!("fixtures/sync_incremental.json").to_string();

    // Incremental matcher (higher priority) matches when sync_token == "fakeSyncTokenFull0001"
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .and(body_partial_json(
            json!({ "sync_token": "fakeSyncTokenFull0001" }),
        ))
        .respond_with(ResponseTemplate::new(200).set_body_string(inc_body))
        .with_priority(1)
        .mount(&server)
        .await;

    // Initial full sync matcher matches sync_token == "*"
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .and(body_partial_json(json!({ "sync_token": "*" })))
        .respond_with(ResponseTemplate::new(200).set_body_string(full_body))
        .with_priority(2)
        .mount(&server)
        .await;

    let home = TestHome::new(&server.uri());

    // First sync -> sends "*"
    let out1 = home.command().arg("sync").output().unwrap();
    assert_eq!(
        out1.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out1.stderr)
    );

    // Second sync -> sends "fakeSyncTokenFull0001", applies sync_incremental.json
    let out2 = home.command().arg("sync").output().unwrap();
    assert_eq!(
        out2.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out2.stderr)
    );

    let tokens: Vec<String> = server
        .received_requests()
        .await
        .unwrap()
        .into_iter()
        .map(|r| {
            r.body_json::<Value>().unwrap()["sync_token"]
                .as_str()
                .unwrap()
                .to_string()
        })
        .collect();
    assert_eq!(tokens, vec!["*", "fakeSyncTokenFull0001"]);

    let conn = Connection::open(home.state_dir().join("cache.db")).unwrap();
    let updated: String = conn
        .query_row(
            "SELECT content FROM items WHERE id = '6X4VfakeItm0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(updated, "Buy oat milk (updated)");

    let deleted_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE id = '6X4VfakeItm0002'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(deleted_count, 0);

    let added_count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE id = '6X4VfakeItm0005'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(added_count, 1);
}

struct SyncCycleAckResponder {
    uuids: Arc<std::sync::Mutex<Vec<String>>>,
}

impl Respond for SyncCycleAckResponder {
    fn respond(&self, req: &Request) -> ResponseTemplate {
        let body: Value = req.body_json().unwrap();
        let mut status = serde_json::Map::new();
        let mut mapping = serde_json::Map::new();
        if let Some(cmds) = body.get("commands").and_then(Value::as_array) {
            let mut g = self.uuids.lock().unwrap();
            for c in cmds {
                let u = c["uuid"].as_str().unwrap().to_string();
                g.push(u.clone());
                status.insert(u, json!("ok"));
                if let Some(tid) = c.get("temp_id").and_then(Value::as_str) {
                    mapping.insert(tid.to_string(), json!("6X4VfakeSrvCycle1"));
                }
            }
        }
        ResponseTemplate::new(200).set_body_json(json!({
            "sync_token": "tok_flushed_1",
            "full_sync": false,
            "sync_status": Value::Object(status),
            "temp_id_mapping": Value::Object(mapping),
            "items": [],
            "projects": [],
            "sections": [],
            "labels": [],
            "notes": [],
            "filters": []
        }))
    }
}

#[tokio::test]
async fn offline_mutation_queues_durably_flushes_once_and_maps_temp_id() {
    let start = Instant::now();
    // 1. Offline add with stopped server -> exits 0 and queues durably
    let dropped = MockSyncApi::transport_drop().await;
    let home = TestHome::new(&dropped.base_url());

    let out_add = home
        .command()
        .args(["add", "Offline milk"])
        .output()
        .unwrap();
    assert_eq!(out_add.status.code(), Some(0));
    let stdout_add = String::from_utf8_lossy(&out_add.stdout);
    assert!(
        stdout_add.contains("queued (offline)"),
        "stdout: {stdout_add}"
    );

    // 2. Also enqueue a durable Sync API `item_add` with temp_id to verify uuid idempotency + temp_id_mapping
    let cmd_uuid = Uuid::new_v4().to_string();
    {
        let conn = Connection::open(home.state_dir().join("cache.db")).unwrap();
        let raw_item = json!({
            "id": "tmp_cycle_1",
            "user_id": "6X4VfakeUsr0001",
            "project_id": "6X4VfakePrj0001",
            "section_id": null,
            "parent_id": null,
            "child_order": 10,
            "content": "Temp mapped task",
            "description": "",
            "priority": 1,
            "labels": [],
            "due": null,
            "deadline": null,
            "checked": false,
            "is_deleted": false,
            "added_at": "2026-11-10T00:00:00Z",
            "completed_at": null,
            "responsible_uid": null
        });
        conn.execute(
            "INSERT INTO items (id, project_id, section_id, parent_id, child_order, content, description, priority, due_date, checked, is_deleted, raw_json) VALUES (?1, ?2, NULL, NULL, 10, 'Temp mapped task', '', 1, NULL, 0, 0, ?3)",
            params!["tmp_cycle_1", "6X4VfakePrj0001", raw_item.to_string()],
        )
        .unwrap();
        conn.execute(
            "INSERT INTO commands (uuid, type, args, temp_id, created_at, state) VALUES (?1, 'item_add', ?2, 'tmp_cycle_1', 1700000000, 'pending')",
            params![cmd_uuid, json!({"content": "Temp mapped task", "project_id": "6X4VfakePrj0001"}).to_string()],
        )
        .unwrap();
    }

    // 3. Restart against live mock server: quick-add + sync flush exactly once + replace tmp_cycle_1
    let server = MockServer::start().await;
    let qa_calls = Arc::new(AtomicUsize::new(0));
    let sync_cmd_uuids = Arc::new(std::sync::Mutex::new(Vec::<String>::new()));

    let qa_clone = Arc::clone(&qa_calls);
    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/quick"))
        .respond_with(move |_req: &Request| {
            qa_clone.fetch_add(1, Ordering::SeqCst);
            ResponseTemplate::new(200).set_body_json(json!({
                "id": "6X4VfakeQaCycle1",
                "user_id": "6X4VfakeUsr0001",
                "project_id": "6X4VfakePrj0001",
                "content": "Offline milk",
                "description": "",
                "priority": 1,
                "child_order": 1,
                "checked": false,
                "is_deleted": false,
                "labels": [],
                "due": null,
                "added_at": "2026-11-10T00:00:00Z"
            }))
        })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(SyncCycleAckResponder {
            uuids: Arc::clone(&sync_cmd_uuids),
        })
        .mount(&server)
        .await;

    let out_sync1 = home
        .command()
        .env("TODOIST_API_BASE_URL", server.uri())
        .arg("sync")
        .output()
        .unwrap();
    assert_eq!(
        out_sync1.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out_sync1.stderr)
    );
    let out1_str = String::from_utf8_lossy(&out_sync1.stdout);
    assert!(
        out1_str.contains("2 flushed, 0 pending"),
        "stdout: {out1_str}"
    );

    // Second sync -> 0 commands replayed (uuid idempotency)
    let out_sync2 = home
        .command()
        .env("TODOIST_API_BASE_URL", server.uri())
        .arg("sync")
        .output()
        .unwrap();
    assert_eq!(out_sync2.status.code(), Some(0));
    let out2_str = String::from_utf8_lossy(&out_sync2.stdout);
    assert!(
        out2_str.contains("0 flushed, 0 pending"),
        "stdout: {out2_str}"
    );

    assert_eq!(qa_calls.load(Ordering::SeqCst), 1);
    assert_eq!(sync_cmd_uuids.lock().unwrap().as_slice(), &[cmd_uuid]);

    // Verify temp_id replaced with server ID in cache
    let conn = Connection::open(home.state_dir().join("cache.db")).unwrap();
    let old_cnt: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE id = 'tmp_cycle_1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let new_cnt: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE id = '6X4VfakeSrvCycle1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(old_cnt, 0);
    assert_eq!(new_cnt, 1);
    assert!(start.elapsed().as_secs() < 30);
}

#[tokio::test]
async fn unauthorized_preserves_cache_and_429_retries_to_success() {
    // 1. Seed cache with initial sync
    let ok_api = MockSyncApi::full_sync().await;
    let home = TestHome::new(&ok_api.base_url());
    assert_eq!(
        home.command().arg("sync").output().unwrap().status.code(),
        Some(0)
    );

    // 2. Next sync receives 401 -> surfaces auth error AND leaves local cache intact
    let unauth_api = MockSyncApi::unauthorized().await;
    let out_401 = home
        .command()
        .env("TODOIST_API_BASE_URL", unauth_api.base_url())
        .arg("sync")
        .output()
        .unwrap();
    assert_ne!(out_401.status.code(), Some(0));
    let err_401 = String::from_utf8_lossy(&out_401.stderr).to_ascii_lowercase();
    assert!(
        err_401.contains("unauthoriz"),
        "expected unauthorized error, got: {err_401}"
    );
    let conn = Connection::open(home.state_dir().join("cache.db")).unwrap();
    let cached_items: i64 = conn
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert!(cached_items >= 2, "401 must not wipe local cache");

    // 3. Server responds once with 429 Retry-After: 0, then 200 -> retries & succeeds cleanly
    let rate_server = MockServer::start().await;
    let inc_body = include_str!("fixtures/sync_incremental.json").to_string();
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "0"))
        .up_to_n_times(1)
        .with_priority(1)
        .mount(&rate_server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_string(inc_body))
        .with_priority(2)
        .mount(&rate_server)
        .await;

    let out_429 = home
        .command()
        .env("TODOIST_API_BASE_URL", rate_server.uri())
        .arg("sync")
        .output()
        .unwrap();
    assert_eq!(
        out_429.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out_429.stderr)
    );
}
