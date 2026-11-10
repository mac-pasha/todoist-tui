//! Process-level integration tests for `add`/`sync` online & offline->online cycle (`T-1009`, EPIC-11).

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

use rusqlite::Connection;
use serde_json::json;
use support::TestHome;
use wiremock::matchers::{header, method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

const AUTH_EXIT_CODE: i32 = 3;
const OFFLINE_EXIT_CODE: i32 = 4;

fn dead_base_url() -> String {
    let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let addr = listener.local_addr().unwrap();
    drop(listener);
    format!("http://{addr}")
}

#[tokio::test]
async fn online_add_caches_item_and_prints_added() {
    let server = MockServer::start().await;
    let home = TestHome::new(&server.uri());

    let qa_response = json!({
        "id": "6X4VfakeQAOnline1",
        "user_id": "6X4VfakeUsr1",
        "project_id": "6X4VfakeInbox",
        "content": "Buy oat milk",
        "description": "",
        "priority": 1,
        "child_order": 1,
        "checked": false,
        "is_deleted": false,
        "labels": [],
        "due": { "date": "2026-11-11", "string": "tomorrow", "is_recurring": false },
        "added_at": "2026-11-10T10:00:00Z"
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/quick"))
        .and(header("authorization", "Bearer tok_fake_abc123"))
        .respond_with(ResponseTemplate::new(200).set_body_json(qa_response))
        .expect(1)
        .mount(&server)
        .await;

    let mut cmd = home.command();
    cmd.args(["add", "Buy oat milk tomorrow"]);
    let out = cmd.output().unwrap();

    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    let stdout = String::from_utf8_lossy(&out.stdout);
    assert!(stdout.contains("added: Buy oat milk"), "stdout: {stdout}");

    // Verify cached in SQLite DB
    let conn = Connection::open(home.state_dir().join("cache.db")).unwrap();
    let content: String = conn
        .query_row(
            "SELECT content FROM items WHERE id = '6X4VfakeQAOnline1'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(content, "Buy oat milk");
}

struct QaResponder {
    count: Arc<AtomicUsize>,
}

impl Respond for QaResponder {
    fn respond(&self, _req: &Request) -> ResponseTemplate {
        self.count.fetch_add(1, Ordering::SeqCst);
        ResponseTemplate::new(200).set_body_json(json!({
            "id": "6X4VfakeQAFlushed1",
            "user_id": "6X4VfakeUsr1",
            "project_id": "6X4VfakeWork",
            "content": "Buy coffee",
            "description": "",
            "priority": 1,
            "child_order": 1,
            "checked": false,
            "is_deleted": false,
            "labels": [],
            "due": null,
            "added_at": "2026-11-10T10:00:00Z"
        }))
    }
}

#[tokio::test]
async fn offline_add_then_reconnect_sync_flushes_once() {
    let dead_url = dead_base_url();
    let home = TestHome::new(&dead_url);

    // 1. Offline add -> exit 0, prints queued (offline)
    {
        let mut cmd = home.command();
        cmd.args(["add", "Buy coffee #Work tomorrow"]);
        let out = cmd.output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            stdout.contains("queued (offline): Buy coffee #Work tomorrow"),
            "stdout: {stdout}"
        );
    }

    // Verify 1 durable command persisted on disk
    {
        let conn = Connection::open(home.state_dir().join("cache.db")).unwrap();
        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM commands WHERE state = 'pending'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
    }

    // 2. Offline sync -> exit 4, stderr message, empty stdout
    {
        let mut cmd = home.command();
        cmd.arg("sync");
        let out = cmd.output().unwrap();
        assert_eq!(out.status.code(), Some(OFFLINE_EXIT_CODE));
        assert!(out.stdout.is_empty(), "stdout must be empty when offline");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("sync failed: offline"), "stderr: {stderr}");
    }

    // 3. Server comes back -> first sync flushes quick-add once, second sync does 0 quick-adds
    let server = MockServer::start().await;
    let qa_count = Arc::new(AtomicUsize::new(0));
    let qa_clone = Arc::clone(&qa_count);

    Mock::given(method("POST"))
        .and(path("/api/v1/tasks/quick"))
        .respond_with(QaResponder { count: qa_clone })
        .mount(&server)
        .await;

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sync_token": "tok_synced_after_qa",
            "full_sync": false,
            "sync_status": {},
            "temp_id_mapping": {},
            "items": [],
            "projects": [],
            "sections": [],
            "labels": [],
            "notes": [],
            "filters": []
        })))
        .mount(&server)
        .await;

    // First sync
    {
        let mut cmd = home.command();
        cmd.env("TODOIST_API_BASE_URL", server.uri()).arg("sync");
        let out = cmd.output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("1 flushed, 0 pending"), "stdout: {stdout}");
    }
    assert_eq!(qa_count.load(Ordering::SeqCst), 1);

    // Second sync -> 0 quick-add calls, 0 flushed
    {
        let mut cmd = home.command();
        cmd.env("TODOIST_API_BASE_URL", server.uri()).arg("sync");
        let out = cmd.output().unwrap();
        assert_eq!(out.status.code(), Some(0));
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(stdout.contains("0 flushed, 0 pending"), "stdout: {stdout}");
    }
    assert_eq!(qa_count.load(Ordering::SeqCst), 1);
}

#[tokio::test]
async fn unauthenticated_commands_fail_with_auth_code_and_hit_no_network() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(200))
        .expect(0)
        .mount(&server)
        .await;

    let home = TestHome::without_token(&server.uri());
    for args in [vec!["sync"], vec!["add", "task"], vec!["today"]] {
        let mut cmd = home.command();
        cmd.args(&args);
        let out = cmd.output().unwrap();
        assert_eq!(out.status.code(), Some(AUTH_EXIT_CODE), "args: {args:?}");
        let stderr = String::from_utf8_lossy(&out.stderr);
        assert!(stderr.contains("not authenticated"));
    }
}
