//! Security integration tests for HTTPS-only enforcement (T-1105, EPIC-12).
//!
//! `SyncClient::with_clock` (src/api/client.rs) is the enforcement point and
//! raises `ApiError::InsecureBaseUrl`; the `auth` path enforces the same rule
//! with `AuthError::InsecureBaseUrl` (src/auth/validate.rs).
//!
//! Debug builds deliberately accept plain-`http://` *loopback* URLs (the
//! harness mock listens on `http://127.0.0.1:<port>`), so "rejected" cases use
//! `0.0.0.0` with the mock's port: not a loopback prefix, yet it would reach
//! the mock if anything dialled it. The mock must record zero hits.
//!
//! Nothing here writes outside its temp HOME (only `TestHome` is used).

#![allow(clippy::unwrap_used, clippy::expect_used)]

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

#[allow(dead_code)]
#[path = "../src/api/client.rs"]
mod client;

/// Path shim for the included files.
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

mod support;

use crate::api::client::SyncClient;
use crate::api::sync::SyncRequest;
use crate::auth::Token;
use crate::error::{ApiError, AppError};
use crate::support::{MockSyncApi, TestHome, FAKE_TOKEN};

fn token() -> Token {
    Token::new(FAKE_TOKEN.to_string()).unwrap()
}

/// `http://0.0.0.0:<mock port>`: non-loopback-prefix, but routes to the mock.
fn insecure_url_pointing_at(api: &MockSyncApi) -> String {
    let url = api.base_url().replace("127.0.0.1", "0.0.0.0");
    assert!(url.starts_with("http://0.0.0.0:"), "{url}");
    url
}

#[tokio::test]
async fn http_base_url_is_rejected_and_no_request_is_attempted() {
    let api = MockSyncApi::full_sync().await;
    let url = insecure_url_pointing_at(&api);

    let err = SyncClient::new(token(), &url)
        .map(|_| ())
        .expect_err("http:// must be rejected");
    assert!(
        matches!(err, AppError::Api(ApiError::InsecureBaseUrl)),
        "{err:?}"
    );
    assert!(api.received_commands().await.is_empty(), "mock saw a hit");
}

#[tokio::test]
async fn non_https_schemes_and_schemeless_hosts_are_rejected() {
    for bad in [
        "http://api.todoist.com",
        "ftp://api.todoist.com",
        "api.todoist.com",
        "",
    ] {
        let err = SyncClient::new(token(), bad)
            .map(|_| ())
            .expect_err("must be rejected");
        assert!(
            matches!(err, AppError::Api(ApiError::InsecureBaseUrl)),
            "{bad:?}: {err:?}"
        );
    }
}

/// Proves the check is scheme-based, not a blanket block: `https://` is
/// accepted at construction. The mock speaks plaintext, so the request itself
/// fails at the transport layer (TLS handshake against a non-TLS listener) and
/// never produces a valid HTTP request on the mock.
#[tokio::test]
async fn https_base_url_is_accepted() {
    let api = MockSyncApi::full_sync().await;
    let url = api.base_url().replace("http://", "https://");

    let client = SyncClient::new(token(), &url).expect("https:// must be accepted");
    let err = client
        .sync(&SyncRequest::full())
        .await
        .expect_err("plaintext mock cannot satisfy TLS");
    assert!(
        !matches!(err, AppError::Api(ApiError::InsecureBaseUrl)),
        "https must not be flagged insecure: {err:?}"
    );
    assert_eq!(
        api.received_commands().await,
        Vec::<Vec<serde_json::Value>>::new()
    );
}

/// Same rule through the real binary: `auth` with an insecure base URL fails
/// with the non-HTTPS message, never dials the mock and stores nothing.
#[tokio::test]
async fn auth_binary_refuses_http_base_url_without_dialling() {
    use std::io::Write;
    use std::process::Stdio;

    let api = MockSyncApi::full_sync().await;
    let home = TestHome::without_token(&insecure_url_pointing_at(&api));
    let mut child = home
        .command()
        .arg("auth")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .unwrap();
    let mut stdin = child.stdin.take().unwrap();
    stdin
        .write_all(format!("{FAKE_TOKEN}\n").as_bytes())
        .unwrap();
    drop(stdin);
    let out = child.wait_with_output().unwrap();

    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success(), "auth must fail: {stderr}");
    assert!(stderr.contains("non-HTTPS"), "stderr: {stderr}");
    assert!(
        !stderr.contains(FAKE_TOKEN) && !String::from_utf8_lossy(&out.stdout).contains(FAKE_TOKEN)
    );
    assert!(api.received_commands().await.is_empty(), "mock saw a hit");
    assert!(!home.token_file().exists(), "token must not be stored");
}

/// The debug-only loopback exemption is matched with `starts_with`, so a
/// hostile host such as `http://localhost.evil.example` passes the gate.
#[tokio::test]
#[ignore = "BUG T-1105: loopback exemption is a string prefix match (http://localhost.evil.example accepted in debug builds)"]
async fn loopback_exemption_does_not_match_lookalike_hosts() {
    for bad in [
        "http://localhost.evil.example",
        "http://127.0.0.1.evil.example",
    ] {
        let err = SyncClient::new(token(), bad)
            .map(|_| ())
            .expect_err("lookalike host must be rejected");
        assert!(matches!(err, AppError::Api(ApiError::InsecureBaseUrl)));
    }
}
