//! Automated log scan proving no API token or sensitive PII leaks into log files (`T-1104`, EPIC-12).

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

use serde_json::json;
use support::{MockSyncApi, TestHome, FAKE_TOKEN};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

fn read_all_logs(state_dir: &std::path::Path) -> String {
    let mut out = String::new();
    for entry in std::fs::read_dir(state_dir).unwrap() {
        let entry = entry.unwrap();
        let name = entry.file_name();
        let name_str = name.to_string_lossy();
        if name_str.starts_with("todoist-tui.log") {
            out.push_str(&std::fs::read_to_string(entry.path()).unwrap());
        }
    }
    out
}

fn contains_secret_leak(haystack: &str, needle: &str) -> bool {
    haystack.contains(needle)
}

#[test]
fn negative_control_grep_detects_planted_token() {
    let sample = format!("DEBUG auth: raw_header=Bearer {FAKE_TOKEN}");
    assert!(contains_secret_leak(&sample, FAKE_TOKEN));
}

#[tokio::test]
async fn full_cli_exercise_trace_log_redacts_token_and_leaks_zero_secrets_or_pii() {
    let server = MockServer::start().await;
    let sync_body = json!({
        "sync_token": "tok_log_scan_1",
        "full_sync": true,
        "temp_id_mapping": {},
        "sync_status": {},
        "user": {
            "id": "6X4VfakeUsrLog",
            "full_name": "Sensitive Account Name",
            "email": "sensitive.user@example.invalid",
            "inbox_project_id": "6X4VfakePrjLog"
        },
        "projects": [{
            "id": "6X4VfakePrjLog",
            "name": "Inbox",
            "parent_id": null,
            "child_order": 0,
            "color": "grey",
            "is_favorite": false,
            "is_archived": false,
            "is_deleted": false,
            "view_style": "list"
        }],
        "sections": [],
        "labels": [],
        "items": [],
        "notes": [],
        "filters": []
    });

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(sync_body))
        .mount(&server)
        .await;

    let home = TestHome::without_token(&server.uri());

    // 1. Auth via piped token
    {
        let mut child = home
            .command()
            .env("RUST_LOG", "trace")
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
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // 2. Online Sync
    {
        let out = home
            .command()
            .env("RUST_LOG", "trace")
            .arg("sync")
            .output()
            .unwrap();
        assert_eq!(
            out.status.code(),
            Some(0),
            "stderr: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }

    // 3. Offline queued mutation carrying sensitive fake PII content (should not leak raw content or phone/email)
    let dropped = MockSyncApi::transport_drop().await;
    let sensitive_email = "private-pii@example.invalid";
    let sensitive_phone = "+1-555-0199-8877";
    let sensitive_content = format!("Call {sensitive_phone} or email {sensitive_email}");
    {
        let out = home
            .command()
            .env("TODOIST_API_BASE_URL", dropped.base_url())
            .env("RUST_LOG", "trace")
            .args(["add", &sensitive_content])
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
    }

    // 4. Logout
    {
        let out = home
            .command()
            .env("RUST_LOG", "trace")
            .arg("logout")
            .output()
            .unwrap();
        assert_eq!(out.status.code(), Some(0));
    }

    let logs = read_all_logs(&home.state_dir());
    assert!(
        !logs.is_empty(),
        "log file must be non-empty under RUST_LOG=trace"
    );

    // Verify all exercised phases produced log records
    assert!(
        logs.contains("auth command finished"),
        "missing auth log phase"
    );
    assert!(
        logs.contains("logout command finished"),
        "missing logout log phase"
    );
    assert!(
        logs.contains("***"),
        "expected redaction marker `***` in trace logs; got:\n{logs}"
    );

    // Zero occurrences of token or sensitive PII / raw task payload
    assert!(
        !contains_secret_leak(&logs, FAKE_TOKEN),
        "TOKEN LEAKED in logs!"
    );
    assert!(
        !logs.contains(sensitive_email),
        "sensitive email leaked in logs!"
    );
    assert!(
        !logs.contains(sensitive_phone),
        "sensitive phone leaked in logs!"
    );
    assert!(
        !logs.contains("sensitive.user@example.invalid"),
        "user fixture email leaked in logs!"
    );
}
