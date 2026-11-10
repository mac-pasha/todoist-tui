//! Smoke tests for the shared integration harness in `tests/support/`
//! (T-1101, EPIC-12): every `MockSyncApi` scenario driven by the production
//! `SyncClient`, and `TestHome` wired through the real binary.
//!
//! The crate is bin-only, so the production sources are compiled into this
//! test binary with the same `#[path]` includes as `tests/sync_client.rs`.

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

/// Path shim for the `#[cfg(test)]` modules of the included files.
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

use std::time::Duration;

use serde_json::json;

use crate::api::client::SyncClient;
use crate::api::retry::{RecordingSleeper, RetryPolicy};
use crate::api::sync::SyncRequest;
use crate::auth::Token;
use crate::error::{ApiError, AppError};
use crate::support::mock_api::{FULL_SYNC_TOKEN, INCREMENTAL_SYNC_TOKEN};
use crate::support::{MockSyncApi, TestHome, FAKE_TOKEN};

fn client(base_url: &str, clock: &RecordingSleeper) -> SyncClient<RecordingSleeper> {
    let token = Token::new(FAKE_TOKEN.to_string()).unwrap();
    SyncClient::with_clock(token, base_url, clock.clone())
        .unwrap()
        .with_retry_policy(|| {
            let mut policy = RetryPolicy::new();
            policy.max_attempts = 3;
            policy.base_delay = Duration::from_millis(1);
            policy.jitter_fraction = 0.0;
            policy.with_jitter(|| 0.0)
        })
}

#[tokio::test]
async fn full_sync_serves_the_fixture_and_records_the_request() {
    let api = MockSyncApi::full_sync().await;
    let clock = RecordingSleeper::new();
    let response = client(&api.base_url(), &clock)
        .sync(&SyncRequest::full())
        .await
        .unwrap();
    assert_eq!(response.sync_token, FULL_SYNC_TOKEN);
    assert!(response.full_sync);
    assert_eq!(api.received_sync_tokens().await, vec!["*".to_string()]);
}

#[tokio::test]
async fn incremental_is_keyed_by_the_sync_token() {
    let api = MockSyncApi::full_then_incremental().await;
    let clock = RecordingSleeper::new();
    let client = client(&api.base_url(), &clock);

    let full = client.sync(&SyncRequest::full()).await.unwrap();
    let incremental = client
        .sync(&SyncRequest::incremental(full.sync_token))
        .await
        .unwrap();

    assert_eq!(incremental.sync_token, INCREMENTAL_SYNC_TOKEN);
    assert!(!incremental.full_sync);
    assert_eq!(incremental.items.len(), 1);
}

#[tokio::test]
async fn received_commands_returns_each_commands_array_once() {
    let api = MockSyncApi::full_sync().await;
    let clock = RecordingSleeper::new();
    let client = client(&api.base_url(), &clock);

    let mut with_command = SyncRequest::full();
    with_command.commands = vec![json!({"type": "item_add", "uuid": "cmd-fake-uuid-0001"})];
    client.sync(&SyncRequest::full()).await.unwrap();
    client.sync(&with_command).await.unwrap();

    let commands = api.received_commands().await;
    assert_eq!(commands.len(), 2);
    assert_eq!(commands[0], Vec::<serde_json::Value>::new());
    assert_eq!(commands[1].len(), 1);
    assert_eq!(commands[1][0]["uuid"], "cmd-fake-uuid-0001");
}

#[tokio::test]
async fn unauthorized_maps_to_unauthorized_without_retrying() {
    let api = MockSyncApi::unauthorized().await;
    let clock = RecordingSleeper::new();
    let err = client(&api.base_url(), &clock)
        .sync(&SyncRequest::full())
        .await
        .unwrap_err();
    assert!(
        matches!(err, AppError::Api(ApiError::Unauthorized)),
        "{err}"
    );
    assert_eq!(api.received_commands().await.len(), 1);
    assert_eq!(clock.recorded(), Vec::<Duration>::new());
}

#[tokio::test]
async fn rate_limited_once_is_retried_after_the_retry_after_delay() {
    let api = MockSyncApi::rate_limited_once_then_full_sync(2).await;
    let clock = RecordingSleeper::new();
    let response = client(&api.base_url(), &clock)
        .sync(&SyncRequest::full())
        .await
        .unwrap();
    assert_eq!(response.sync_token, FULL_SYNC_TOKEN);
    assert_eq!(api.received_commands().await.len(), 2);
    assert_eq!(clock.recorded(), vec![Duration::from_secs(2)]);
}

#[tokio::test]
async fn persistent_rate_limit_exhausts_retries() {
    let api = MockSyncApi::rate_limited(1).await;
    let clock = RecordingSleeper::new();
    let err = client(&api.base_url(), &clock)
        .sync(&SyncRequest::full())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            AppError::Api(ApiError::RetriesExhausted { attempts: 3 })
        ),
        "{err}"
    );
    assert_eq!(api.received_commands().await.len(), 3);
}

#[tokio::test]
async fn server_error_is_retried_then_recovers() {
    let api = MockSyncApi::server_error_once_then_full_sync(503).await;
    let clock = RecordingSleeper::new();
    client(&api.base_url(), &clock)
        .sync(&SyncRequest::full())
        .await
        .unwrap();
    assert_eq!(api.received_commands().await.len(), 2);
}

#[tokio::test]
async fn persistent_server_error_exhausts_retries() {
    let api = MockSyncApi::server_error(500).await;
    let clock = RecordingSleeper::new();
    let err = client(&api.base_url(), &clock)
        .sync(&SyncRequest::full())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            AppError::Api(ApiError::RetriesExhausted { attempts: 3 })
        ),
        "{err}"
    );
}

#[tokio::test]
async fn transport_drop_refuses_connections() {
    let api = MockSyncApi::transport_drop().await;
    let clock = RecordingSleeper::new();
    let err = client(&api.base_url(), &clock)
        .sync(&SyncRequest::full())
        .await
        .unwrap_err();
    assert!(
        matches!(
            err,
            AppError::Api(ApiError::RetriesExhausted { attempts: 3 })
        ),
        "a refused connection is a retryable transport error: {err}"
    );
    assert_eq!(
        api.received_commands().await,
        Vec::<Vec<serde_json::Value>>::new()
    );
}

#[test]
fn test_home_is_isolated_holds_the_fake_token_and_is_removed_on_drop() {
    let home = TestHome::new("http://127.0.0.1:9");
    let root = home.root().to_path_buf();
    assert!(root.is_dir());
    assert_eq!(
        std::fs::read_to_string(home.token_file()).unwrap(),
        "tok_fake_abc123"
    );
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = std::fs::metadata(home.token_file())
            .unwrap()
            .permissions()
            .mode();
        assert_eq!(mode & 0o777, 0o600);
    }
    drop(home);
    assert!(!root.exists(), "TestHome must remove its dir on drop");
}

#[test]
fn binary_sees_the_sandboxed_token_file() {
    // `sync` now runs the real command after passing auth guard: with a token
    // pointing at the closed test port it reports offline, without one it
    // fails the auth guard first. Either way it proves the binary reads the
    // token from the TestHome sandbox.
    let with = TestHome::new("http://127.0.0.1:9");
    let out = with.command().arg("sync").output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(!out.status.success());
    assert!(err.contains("sync failed: offline"), "{err}");

    let without = TestHome::without_token("http://127.0.0.1:9");
    let out = without.command().arg("sync").output().unwrap();
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(err.contains("not authenticated"), "{err}");
    assert!(!err.contains(FAKE_TOKEN));
}
