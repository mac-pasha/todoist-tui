//! E2E test: `logout` wipes token, SQLite cache, pending queue, and `sync_token` so the
//! next start behaves identically to a clean first run (`T-1106`, EPIC-12).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

mod support;

use std::io::Write;
use std::process::Stdio;

use serde_json::{json, Value};
use support::{MockSyncApi, TestHome, FAKE_TOKEN};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

#[tokio::test]
async fn logout_wipes_everything_and_next_start_performs_clean_full_sync_without_replay() {
    let full_body = include_str!("fixtures/sync_full_small.json").to_string();
    let api = MockSyncApi::full_sync().await;
    let home = TestHome::without_token(&api.base_url());

    // 1. Authenticate against mock
    {
        let mut child = home
            .command()
            .arg("auth")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{FAKE_TOKEN}\n").as_bytes())
            .unwrap();
        let out = child.wait_with_output().unwrap();
        assert_eq!(out.status.code(), Some(0));
    }
    assert!(home.token_file().exists());

    // 2. Sync to populate SQLite cache + sync_token
    {
        let out = home.command().arg("sync").output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    assert!(home.state_dir().join("cache.db").exists());

    // 3. Queue one offline mutation while server is stopped
    {
        let dropped = MockSyncApi::transport_drop().await;
        let out = home
            .command()
            .env("TODOIST_API_BASE_URL", dropped.base_url())
            .args(["add", "Task never replayed after logout"])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
    }

    // 4. Logout -> exits 0, wipes token + cache.db + -wal/-shm
    {
        let out = home.command().arg("logout").output().unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert!(
            !stdout.trim().is_empty(),
            "logout should print confirmation"
        );
    }

    // Explicit directory listing assertions after logout
    assert!(!home.token_file().exists(), "token file must be gone");
    for suffix in ["cache.db", "cache.db-wal", "cache.db-shm"] {
        assert!(
            !home.state_dir().join(suffix).exists(),
            "{suffix} must be removed"
        );
    }

    // 5. Idempotent second logout also exits 0
    {
        let out = home.command().arg("logout").output().unwrap();
        assert_eq!(out.status.code(), Some(0));
    }

    // 6. Start again & re-authenticate against a fresh mock server
    let fresh_server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_string(full_body))
        .mount(&fresh_server)
        .await;

    {
        let mut child = home
            .command()
            .env("TODOIST_API_BASE_URL", fresh_server.uri())
            .arg("auth")
            .stdin(Stdio::piped())
            .stdout(Stdio::piped())
            .stderr(Stdio::piped())
            .spawn()
            .unwrap();
        child
            .stdin
            .as_mut()
            .unwrap()
            .write_all(format!("{FAKE_TOKEN}\n").as_bytes())
            .unwrap();
        assert_eq!(child.wait_with_output().unwrap().status.code(), Some(0));
    }

    {
        let out = home
            .command()
            .env("TODOIST_API_BASE_URL", fresh_server.uri())
            .arg("sync")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
    }

    // Verify post-logout restart sent sync_token == "*" and ZERO stale offline commands
    let reqs = fresh_server.received_requests().await.unwrap();
    let sync_reqs: Vec<Value> = reqs
        .into_iter()
        .filter(|r| r.url.path() == "/api/v1/sync")
        .filter_map(|r| r.body_json::<Value>().ok())
        .filter(|v| v.get("sync_token").is_some())
        .collect();
    assert_ne!(sync_reqs.len(), 0);
    let first_restart = &sync_reqs[0];
    assert_eq!(first_restart["sync_token"], json!("*"));
    let cmds = first_restart["commands"]
        .as_array()
        .map_or(&[][..], Vec::as_slice);
    assert_eq!(
        cmds.len(),
        0,
        "pre-logout offline mutation must NOT replay after logout"
    );
}
