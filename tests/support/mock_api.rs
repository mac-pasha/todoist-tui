//! [`MockSyncApi`]: a scripted `wiremock` server for `POST /api/v1/sync`.
//!
//! One named constructor per scenario. Every 200 body is the committed
//! `tests/fixtures/sync_full.json` fixture (or a small inline incremental
//! body), so no real account data appears anywhere. Nothing leaves
//! `127.0.0.1`.

use std::net::TcpStream;
use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wiremock::matchers::{body_partial_json, method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// The sync token carried by `tests/fixtures/sync_full.json`.
pub const FULL_SYNC_TOKEN: &str = "fakeSyncTokenForT208Fixture0001";

/// Sync token returned by the inline incremental body.
pub const INCREMENTAL_SYNC_TOKEN: &str = "fakeIncrementalToken0002";

const SYNC_PATH: &str = "/api/v1/sync";

/// The committed full-sync fixture, compiled in (no file IO at run time).
pub fn full_sync_body() -> &'static str {
    include_str!("../fixtures/sync_full.json")
}

/// A small incremental response: `full_sync: false`, a new token, one
/// changed item. Fake ids only.
pub fn incremental_body() -> Value {
    json!({
        "sync_token": INCREMENTAL_SYNC_TOKEN,
        "full_sync": false,
        "items": [{
            "id": "6X4VfakeItm0099",
            "user_id": "6X4VfakeUsr0001",
            "project_id": "6X4VfakePrj0001",
            "section_id": null,
            "parent_id": null,
            "child_order": 9,
            "content": "Incremental fake item",
            "description": "",
            "priority": 1,
            "labels": [],
            "due": null,
            "deadline": null,
            "checked": false,
            "is_deleted": false,
            "added_at": "2024-05-02T08:00:00Z",
            "completed_at": null,
            "responsible_uid": null
        }]
    })
}

/// A scripted Sync API server. Dropping it shuts the server down.
pub struct MockSyncApi {
    /// `None` once the server has been shut down (transport-drop scenario).
    server: Option<MockServer>,
    base_url: String,
}

impl MockSyncApi {
    /// Answers every `POST /api/v1/sync` with the full-sync fixture.
    pub async fn full_sync() -> Self {
        Self::with_mocks(vec![ok_mock(full_sync_body().to_string(), 5, None)]).await
    }

    /// Full sync for `sync_token: "*"` (the fixture), and the incremental
    /// body for a request carrying the fixture's token
    /// ([`FULL_SYNC_TOKEN`]). Any other token gets a 400.
    pub async fn full_then_incremental() -> Self {
        Self::with_mocks(vec![
            keyed_mock("*", full_sync_body().to_string()),
            keyed_mock(FULL_SYNC_TOKEN, incremental_body().to_string()),
            Mock::given(method("POST"))
                .and(path(SYNC_PATH))
                .respond_with(ResponseTemplate::new(400))
                .with_priority(9),
        ])
        .await
    }

    /// Answers every request with `401 Unauthorized`.
    pub async fn unauthorized() -> Self {
        Self::with_mocks(vec![status_mock(401, None, 5, None)]).await
    }

    /// Answers every request with `429` and `Retry-After: <secs>`.
    pub async fn rate_limited(retry_after_secs: u64) -> Self {
        Self::with_mocks(vec![status_mock(429, Some(retry_after_secs), 5, None)]).await
    }

    /// First request gets `429` + `Retry-After`, every later one the
    /// full-sync fixture: a retry must succeed.
    pub async fn rate_limited_once_then_full_sync(retry_after_secs: u64) -> Self {
        Self::with_mocks(vec![
            status_mock(429, Some(retry_after_secs), 1, Some(1)),
            ok_mock(full_sync_body().to_string(), 5, None),
        ])
        .await
    }

    /// Answers every request with the given 5xx `status`.
    pub async fn server_error(status: u16) -> Self {
        assert!((500..600).contains(&status), "{status} is not a 5xx");
        Self::with_mocks(vec![status_mock(status, None, 5, None)]).await
    }

    /// First request gets the 5xx `status`, every later one the fixture.
    pub async fn server_error_once_then_full_sync(status: u16) -> Self {
        assert!((500..600).contains(&status), "{status} is not a 5xx");
        Self::with_mocks(vec![
            status_mock(status, None, 1, Some(1)),
            ok_mock(full_sync_body().to_string(), 5, None),
        ])
        .await
    }

    /// A transport drop: the server was started and then shut down, so its
    /// `base_url()` refuses connections. Returns only once the listener is
    /// verified closed, so the first request is reliably refused.
    ///
    /// # Panics
    ///
    /// If the listener is still accepting after 2 seconds.
    pub async fn transport_drop() -> Self {
        // `builder().start()` is an exclusive (non-pooled) server: dropping
        // it really closes the socket, unlike a pooled `MockServer::start`.
        let server = MockServer::builder().start().await;
        let base_url = server.uri();
        let addr = *server.address();
        drop(server);

        let deadline = Instant::now() + Duration::from_secs(2);
        while TcpStream::connect_timeout(&addr, Duration::from_millis(100)).is_ok() {
            assert!(
                Instant::now() < deadline,
                "mock server at {addr} still accepting after shutdown"
            );
            std::thread::sleep(Duration::from_millis(5));
        }
        Self {
            server: None,
            base_url,
        }
    }

    async fn with_mocks(mocks: Vec<Mock>) -> Self {
        let server = MockServer::builder().start().await;
        for mock in mocks {
            server.register(mock).await;
        }
        Self {
            base_url: server.uri(),
            server: Some(server),
        }
    }

    /// Base URL (`http://127.0.0.1:<port>`, no trailing slash) to hand to
    /// the binary (`TODOIST_API_BASE_URL`) or `SyncClient`.
    pub fn base_url(&self) -> String {
        self.base_url.clone()
    }

    /// The JSON body of every `POST /api/v1/sync` received, in arrival order.
    pub async fn received_bodies(&self) -> Vec<Value> {
        let Some(server) = &self.server else {
            return Vec::new();
        };
        server
            .received_requests()
            .await
            .expect("the mock server records received requests")
            .iter()
            .filter(|r| r.method == wiremock::http::Method::POST && r.url.path() == SYNC_PATH)
            .map(|r| r.body_json::<Value>().expect("sync request body is JSON"))
            .collect()
    }

    /// The `commands` array of every request seen, in arrival order. A
    /// request without `commands` contributes an empty vec, so
    /// `received_commands().len()` is the request count.
    pub async fn received_commands(&self) -> Vec<Vec<Value>> {
        self.received_bodies()
            .await
            .into_iter()
            .map(|body| body["commands"].as_array().cloned().unwrap_or_default())
            .collect()
    }

    /// The `sync_token` of every request seen, in arrival order.
    pub async fn received_sync_tokens(&self) -> Vec<String> {
        self.received_bodies()
            .await
            .iter()
            .map(|body| body["sync_token"].as_str().unwrap_or_default().to_string())
            .collect()
    }
}

fn base_mock() -> wiremock::MockBuilder {
    Mock::given(method("POST")).and(path(SYNC_PATH))
}

fn ok_mock(body: String, priority: u8, times: Option<u64>) -> Mock {
    let mock = base_mock()
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(body),
        )
        .with_priority(priority);
    match times {
        Some(n) => mock.up_to_n_times(n),
        None => mock,
    }
}

fn keyed_mock(token: &str, body: String) -> Mock {
    base_mock()
        .and(body_partial_json(json!({ "sync_token": token })))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(body),
        )
        .with_priority(1)
}

fn status_mock(status: u16, retry_after: Option<u64>, priority: u8, times: Option<u64>) -> Mock {
    let mut template = ResponseTemplate::new(status);
    if let Some(secs) = retry_after {
        template = template.insert_header("retry-after", secs.to_string().as_str());
    }
    let mock = base_mock().respond_with(template).with_priority(priority);
    match times {
        Some(n) => mock.up_to_n_times(n),
        None => mock,
    }
}
