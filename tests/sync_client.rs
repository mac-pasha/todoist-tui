//! Wiremock integration tests for the EPIC-03 [`SyncClient`] (T-209): the
//! request the client actually puts on the wire, and the retry / status-code
//! behaviour an end-to-end caller observes.
//!
//! Everything here runs against a local `wiremock` server — no request ever
//! leaves `127.0.0.1`, the suite passes with no network access, and every
//! token is an obvious fake (`tok_fake_*`). The client under test is the
//! production [`SyncClient`], not a stand-in: request path, `Authorization`
//! header, JSON body, the T-206 retry loop and the status→[`ApiError`]
//! mapping all execute for real. The only seams replaced are the two T-206
//! test seams the production type was built with:
//!
//! * [`RecordingSleeper`] instead of [`TokioSleeper`], so backoff delays are
//!   *recorded* rather than slept; the whole file therefore finishes in
//!   milliseconds even when it asserts a 2-second `Retry-After` wait, and
//!   wall-clock assertions prove the absence of real sleeping.
//! * A deterministic (jitter-free) [`RetryPolicy`], so the recorded delays are
//!   exactly `base_delay * 2^(n-1)` and can be asserted, not just bounded.
//!
//! The 200 bodies come from `tests/fixtures/sync_full.json` (the T-208
//! fixture), so a success path here decodes the same realistic full-sync
//! payload the deserialisation suite pins down.
//!
//! ## Why the `#[path]` includes below
//!
//! `todoist-tui` is a binary-only crate (a single `[[bin]]`, no `[lib]`), so
//! an integration test cannot `use todoist_tui::api::SyncClient`. Following
//! the approach T-208 established in `tests/sync_fixture.rs`, the production
//! sources this suite needs are compiled into this test binary verbatim via
//! `#[path]`, so the assertions run against the real types and the real
//! client rather than a re-declared copy that would keep passing after a
//! source change:
//!
//! * `src/error.rs` — `AppError` / `ApiError`.
//! * `src/auth/token.rs` — `Token` (and, transitively, `src/log.rs`'s
//!   `Secret` redaction helper, which needs `src/config/paths.rs`).
//! * `src/api/sync.rs` + `src/api/model/mod.rs` — the request/response
//!   envelope and the domain structs.
//! * `src/api/retry.rs` — `RetryPolicy` + the `RecordingSleeper` double.
//! * `src/api/client.rs` — `SyncClient` itself.
//!
//! Nothing is re-declared and no `src/` file is modified. Two consequences,
//! both shared with `tests/sync_fixture.rs`: individual files are named
//! rather than `src/api/mod.rs` /
//! `src/auth/mod.rs` / `src/config/mod.rs`, so unrelated modules (the CLI,
//! the keyring sources) are not linked into this binary; and the `mod`
//! shims at the bottom of this header re-export the includes under the
//! `crate::api::…` / `crate::auth::…` / `crate::config::…` paths that the
//! included files' own `#[cfg(test)]` modules refer to. Those embedded unit
//! tests therefore run a second time here (they are pure and fast) — the
//! same accepted trade-off as T-208.

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

// Included verbatim: not every `pub` item in it is exercised by this suite,
// and `dead_code` is per-crate, so the unused production constructors would
// otherwise fail `clippy -D warnings`.
#[allow(dead_code)]
#[path = "../src/api/client.rs"]
mod client;

/// Path shim for the `#[cfg(test)]` modules embedded in the included files:
/// they name their own types as `crate::config::…`, so re-exporting the
/// includes under those module names keeps the paths resolvable without
/// editing `src/`.
#[allow(unused_imports)]
mod config {
    pub(crate) use crate::paths;

    /// Stand-in for `src/config/mod.rs`'s test-only env lock, which
    /// `config::paths`'s embedded tests import. Keeping the types apart
    /// means no production file is edited; the included `paths` tests are
    /// pure path/environment tests with no shared state across modules in
    /// this binary.
    pub(crate) mod test_support {
        pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    }
}

/// Path shim for the `crate::auth::…` paths used by the included sources.
#[allow(unused_imports)]
mod auth {
    pub(crate) use crate::token::Token;
}

/// Path shim for the `crate::api::…` paths used by the included sources and
/// by this suite.
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

use std::time::{Duration, Instant};

use serde_json::{json, Value};
use wiremock::http::{HeaderMap, HeaderValue, Method};
use wiremock::matchers::{method, path};
use wiremock::{Match, Mock, MockServer, Request, ResponseTemplate};

use crate::api::client::SyncClient;
use crate::api::retry::{RecordingSleeper, RetryPolicy};
use crate::api::sync::{SyncRequest, SyncResponse, RESOURCE_TYPES};
use crate::auth::Token;
use crate::error::{ApiError, AppError};

/// Fixture token for every test in this file. Fake — never a real
/// credential, and identical to the value `src/api/client.rs`'s own tests and
/// the EPIC-02 auth suite use.
const FAKE_TOKEN: &str = "tok_fake_abc123";

/// The `Authorization` header value the client must send for [`FAKE_TOKEN`].
const EXPECTED_AUTH_HEADER: &str = "Bearer tok_fake_abc123";

/// The committed T-208 full-sync fixture, compiled in so these tests need no
/// file IO and no network. Used as the body of every mocked 200 response.
fn fixture_text() -> &'static str {
    include_str!("fixtures/sync_full.json")
}

fn fake_token() -> Token {
    Token::new(FAKE_TOKEN.to_string()).expect("the fake token is non-empty")
}

/// A retry policy with jitter disabled (jitter source fixed at `0.0`), so
/// recorded delays are exactly `base_delay * 2^(n-1)` capped at 60s and can
/// be asserted rather than merely bounded — same helper shape as the
/// `no_jitter_policy` in `src/api/client.rs`'s own test module.
fn no_jitter_policy(max_attempts: u32, base_delay: Duration) -> RetryPolicy {
    let mut policy = RetryPolicy::new();
    policy.max_attempts = max_attempts;
    policy.base_delay = base_delay;
    policy.max_delay = Duration::from_secs(60);
    policy.jitter_fraction = 0.0;
    policy.with_jitter(|| 0.0)
}

/// Builds a client pointed at `base_url`, with `clock` recording every
/// requested sleep instead of sleeping and a jitter-free policy of
/// `max_attempts` attempts spaced from `base_delay`.
///
/// `base_url` is always a loopback URL here (a mock server's, or a closed
/// port); the client's scheme gate accepts those in debug builds only, which
/// is exactly the seam that keeps the real API out of reach from this file.
fn client_at(
    base_url: &str,
    clock: &RecordingSleeper,
    (max_attempts, base_delay): (u32, Duration),
) -> SyncClient<RecordingSleeper> {
    SyncClient::with_clock(fake_token(), base_url, clock.clone())
        .expect("a loopback http:// base URL is accepted in debug builds")
        // `RetryPolicy`'s jitter source is a boxed `FnMut` (not `Clone`), so
        // the factory rebuilds an equivalent policy per `sync` call.
        .with_retry_policy(move || no_jitter_policy(max_attempts, base_delay))
}

/// [`client_at`] pointed at `server`.
fn client_for(
    server: &MockServer,
    clock: &RecordingSleeper,
    policy: (u32, Duration),
) -> SyncClient<RecordingSleeper> {
    client_at(&server.uri(), clock, policy)
}

/// Every request the mock server received, in arrival order.
async fn received(server: &MockServer) -> Vec<Request> {
    server
        .received_requests()
        .await
        .expect("the mock server records received requests")
}

/// A matcher that accepts only the exact request shape the Sync API contract
/// requires: `sync_token: "*"`, exactly the eight `resource_types` with no
/// extras, and the fake bearer token. Used to pin the request *content*, not
/// just the method and path.
struct ExactSyncRequest;

impl Match for ExactSyncRequest {
    fn matches(&self, request: &Request) -> bool {
        if request.method != wiremock::http::Method::POST {
            return false;
        }
        if request.url.path() != "/api/v1/sync" {
            return false;
        }
        let auth_ok = request
            .headers
            .get("authorization")
            .and_then(|value| value.to_str().ok())
            == Some(EXPECTED_AUTH_HEADER);
        if !auth_ok {
            return false;
        }
        let Ok(body) = request.body_json::<Value>() else {
            return false;
        };
        let expected_resource_types: Vec<Value> = RESOURCE_TYPES.iter().map(|r| json!(r)).collect();
        body["sync_token"] == json!("*")
            && body["resource_types"] == Value::Array(expected_resource_types)
    }
}

/// Hand-builds a wiremock [`Request`] for the matcher self-check; wiremock's
/// own `Request` fields are public, but constructing one by hand is only
/// worth a helper once, not eight times.
fn sync_request(method: Method, path: &str, auth: Option<&str>, body: &Value) -> Request {
    let mut headers = HeaderMap::new();
    if let Some(value) = auth {
        headers.insert(
            "authorization",
            HeaderValue::from_str(value).expect("ASCII"),
        );
    }
    Request {
        url: format!("http://127.0.0.1:1234{path}")
            .parse()
            .expect("valid loopback URL"),
        method,
        headers,
        body: body.to_string().into_bytes(),
    }
}

/// Runs the shape matcher against a hand-built request.
fn is_exact_sync_request(request: &Request) -> bool {
    ExactSyncRequest.matches(request)
}

/// Self-check for [`ExactSyncRequest`]: a matcher that accepted everything
/// would make the request-shape test above vacuous, so the matcher is fed
/// hand-built requests itself. Wiremock cannot report *why* a matcher
/// rejected a request, so a matcher-only failure would otherwise surface as
/// the unhelpful 418 from the fallback mock.
#[test]
fn exact_sync_request_matcher_rejects_every_off_shape_variant() {
    let good_body = json!({
        "sync_token": "*",
        "resource_types": RESOURCE_TYPES,
    });
    assert!(
        is_exact_sync_request(&sync_request(
            Method::POST,
            "/api/v1/sync",
            Some(EXPECTED_AUTH_HEADER),
            &good_body
        )),
        "the exact contract shape must match"
    );

    let mut nine_resource_types = good_body.clone();
    nine_resource_types["resource_types"] = json!(RESOURCE_TYPES
        .iter()
        .chain(["premium_user"].iter())
        .collect::<Vec<_>>());
    let mut seven_resource_types = good_body.clone();
    seven_resource_types["resource_types"] = json!(RESOURCE_TYPES[..7]);
    let incremental_body = json!({
        "sync_token": "fakeSyncTokenForT208Fixture0001",
        "resource_types": RESOURCE_TYPES,
    });

    let auth = Some(EXPECTED_AUTH_HEADER);
    let off_shape: [(&str, Request); 8] = [
        (
            "wrong method",
            sync_request(Method::GET, "/api/v1/sync", auth, &good_body),
        ),
        (
            "wrong path",
            sync_request(Method::POST, "/sync/v9/sync", auth, &good_body),
        ),
        (
            "missing Authorization header",
            sync_request(Method::POST, "/api/v1/sync", None, &good_body),
        ),
        (
            "wrong bearer token",
            sync_request(
                Method::POST,
                "/api/v1/sync",
                Some("Bearer tok_fake_other"),
                &good_body,
            ),
        ),
        (
            "raw token without the Bearer scheme",
            sync_request(Method::POST, "/api/v1/sync", Some(FAKE_TOKEN), &good_body),
        ),
        (
            "incremental sync token instead of `\"*\"`",
            sync_request(Method::POST, "/api/v1/sync", auth, &incremental_body),
        ),
        (
            "nine resource types",
            sync_request(Method::POST, "/api/v1/sync", auth, &nine_resource_types),
        ),
        (
            "seven resource types",
            sync_request(Method::POST, "/api/v1/sync", auth, &seven_resource_types),
        ),
    ];
    for (label, bad) in off_shape {
        assert!(!is_exact_sync_request(&bad), "must not match {label}");
    }

    // A non-JSON body must be rejected too, not panic.
    let mut truncated = sync_request(Method::POST, "/api/v1/sync", auth, &good_body);
    truncated.body = b"{".to_vec();
    assert!(
        !is_exact_sync_request(&truncated),
        "a non-JSON body must not match"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 1: request shape (sync_token, the eight resource types, the
// bearer header)
// ---------------------------------------------------------------------------

/// The mock only answers 200 when the request carries `sync_token: "*"`,
/// exactly the eight resource types and `Authorization: Bearer
/// tok_fake_abc123`; a catch-all mounted at lower priority would answer 418
/// otherwise. Success therefore proves the request shape, and the recorded
/// request is inspected as well so the assertion does not depend on the
/// matcher being wired correctly.
#[tokio::test]
async fn request_has_full_sync_token_eight_resource_types_and_bearer_auth() {
    let server = MockServer::start().await;
    Mock::given(ExactSyncRequest)
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture_text()))
        .expect(1)
        .mount(&server)
        .await;
    // Fails the test with a diagnostic status if the shape is wrong.
    Mock::given(method("POST"))
        .respond_with(ResponseTemplate::new(418))
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock, (1, Duration::from_millis(1)));

    let response = client
        .sync(&SyncRequest::full())
        .await
        .expect("a 200 with the T-208 fixture body must decode");
    assert_eq!(
        response.sync_token,
        serde_json::from_str::<Value>(fixture_text()).unwrap()["sync_token"],
        "the decoded response must be the fixture, not a fallback"
    );

    server.verify().await;
    let requests = received(&server).await;
    assert_eq!(requests.len(), 1, "one attempt, one request");

    // Header: exactly one bearer token, and it is the injected fake one.
    let request = &requests[0];
    assert_eq!(
        request
            .headers
            .get("authorization")
            .expect("the client must send an Authorization header")
            .to_str()
            .expect("the header is ASCII"),
        EXPECTED_AUTH_HEADER,
    );

    // Body: `sync_token: "*"` and exactly eight resource types, no extras.
    let body = request
        .body_json::<Value>()
        .expect("the request body must be JSON");
    assert_eq!(body["sync_token"], json!("*"), "a full sync sends `\"*\"`");

    let resource_types = body["resource_types"]
        .as_array()
        .expect("resource_types must be a JSON array");
    assert_eq!(
        resource_types.len(),
        8,
        "a full sync requests exactly the eight resource types, got {resource_types:?}"
    );
    assert_eq!(
        resource_types
            .iter()
            .map(|v| v.as_str().expect("resource type is a string"))
            .collect::<Vec<_>>(),
        RESOURCE_TYPES.to_vec(),
        "the eight resource types must be RESOURCE_TYPES, in order, with no extras"
    );

    // No empty `commands` key either: the body is exactly these two fields.
    let mut keys = body
        .as_object()
        .expect("the request body is a JSON object")
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    keys.sort();
    assert_eq!(
        keys,
        vec!["resource_types".to_string(), "sync_token".to_string()],
        "a command-less full sync must send only sync_token and resource_types"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 2: two 500s then a 200 -> success, three requests, increasing
// recorded delays
// ---------------------------------------------------------------------------

#[tokio::test]
async fn two_server_errors_then_success_retries_three_times_with_increasing_backoff() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(500))
        .up_to_n_times(2)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture_text()))
        .expect(1)
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock, (4, Duration::from_millis(20)));

    let response = client
        .sync(&SyncRequest::full())
        .await
        .expect("a transient 5xx must be retried until it succeeds");
    assert!(
        !response.sync_token.is_empty(),
        "the successful attempt's body must be decoded"
    );

    server.verify().await;
    assert_eq!(
        received(&server).await.len(),
        3,
        "two failures plus the successful attempt must be exactly three requests"
    );

    // Jitter is disabled, so the recorded delays are exactly base * 2^(n-1).
    let delays = clock.recorded();
    assert_eq!(
        delays,
        vec![Duration::from_millis(20), Duration::from_millis(40)],
        "backoff must grow exponentially between the three attempts"
    );
    assert!(
        delays.windows(2).all(|pair| pair[0] < pair[1]),
        "recorded delays must be strictly increasing, got {delays:?}"
    );

    // Idempotent retry: every attempt re-sends the same full-sync request
    // byte for byte (no attempt counter, no mutated sync token), so a retry
    // after a 5xx can never duplicate or skip a resource pull.
    let requests = received(&server).await;
    let first = &requests[0];
    for (index, retry) in requests.iter().enumerate().skip(1) {
        assert_eq!(
            retry.body,
            first.body,
            "attempt {} must re-send the identical body, not a mutated one",
            index + 1
        );
        assert_eq!(
            retry.headers.get("authorization"),
            first.headers.get("authorization"),
            "attempt {} must re-send the same Authorization header",
            index + 1
        );
    }
}

// ---------------------------------------------------------------------------
// Acceptance 3: 429 + Retry-After: 2 then 200 -> the retry_after hint wins,
// with no real sleeping
// ---------------------------------------------------------------------------

#[tokio::test]
async fn rate_limit_retry_after_hint_is_recorded_and_never_actually_slept() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(
            ResponseTemplate::new(429)
                .insert_header("retry-after", "2")
                .set_body_string(r#"{"error": "RATE_LIMIT_EXCEEDED"}"#),
        )
        .up_to_n_times(1)
        .mount(&server)
        .await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_string(fixture_text()))
        .expect(1)
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    // A 1ms backoff would be the computed delay; the `Retry-After: 2` hint
    // must override it.
    let client = client_for(&server, &clock, (2, Duration::from_millis(1)));

    let started = Instant::now();
    let response = client
        .sync(&SyncRequest::full())
        .await
        .expect("a 429 followed by a 200 must succeed on the second attempt");
    let elapsed = started.elapsed();
    assert_ne!(
        response.sync_token, "",
        "the successful attempt's body must be decoded"
    );

    server.verify().await;
    assert_eq!(
        received(&server).await.len(),
        2,
        "one rate-limited attempt plus the successful retry"
    );

    let delays = clock.recorded();
    assert_eq!(delays.len(), 1, "exactly one wait between the two attempts");
    assert!(
        delays[0] >= Duration::from_secs(2),
        "the Retry-After: 2 hint must override the 1ms computed backoff, got {:?}",
        delays[0]
    );
    // The clock double records instead of sleeping, so a 2s hint costs
    // nothing: had the client really slept, this would take >= 2s.
    assert!(
        elapsed < Duration::from_secs(1),
        "the suite must not sleep for real, but the call took {elapsed:?}"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 4: 401 -> Unauthorized immediately, exactly one request
// ---------------------------------------------------------------------------

#[tokio::test]
async fn unauthorized_is_returned_immediately_after_exactly_one_request() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(401))
        .expect(1)
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    // A generous budget: the 401 must still not be retried.
    let client = client_for(&server, &clock, (4, Duration::from_millis(1)));

    let err = client
        .sync(&SyncRequest::full())
        .await
        .expect_err("a 401 must be an error");
    assert!(
        matches!(err, AppError::Api(ApiError::Unauthorized)),
        "expected ApiError::Unauthorized, got {err:?}"
    );

    server.verify().await;
    assert_eq!(
        received(&server).await.len(),
        1,
        "a non-retryable 401 must be sent exactly once"
    );
    assert_eq!(
        clock.recorded(),
        Vec::<Duration>::new(),
        "a non-retryable status must not sleep at all"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 5: unknown fields are tolerated; a malformed body is a Decode
// error
// ---------------------------------------------------------------------------

/// A 200 whose JSON carries unknown fields at the top level *and* inside a
/// resource decodes fine: the model is additive-compatible, so a future API
/// field must not break a sync.
#[tokio::test]
async fn unknown_fields_top_level_and_inside_resources_still_decode() {
    let mut body: Value =
        serde_json::from_str(fixture_text()).expect("the T-208 fixture is valid JSON");
    body["a_field_from_the_future"] = json!({ "nested": [1, 2, 3] });
    body["items"][0]["another_unknown_item_field"] = json!("ignored");
    body["items"][0]["due"]["unknown_due_field"] = json!(true);
    body["user"]["unknown_user_field"] = json!({ "nested": "value" });

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_string(body.to_string()))
        .expect(1)
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock, (2, Duration::from_millis(1)));

    let response: SyncResponse = client
        .sync(&SyncRequest::full())
        .await
        .expect("unknown fields must be ignored, not rejected");

    let known: SyncResponse =
        serde_json::from_str(fixture_text()).expect("the fixture itself decodes");
    assert_eq!(
        response.sync_token, known.sync_token,
        "the known fields must survive alongside the unknown ones"
    );
    assert_eq!(
        response.items.len(),
        known.items.len(),
        "no item may be dropped because a sibling field was unknown"
    );

    server.verify().await;
    assert_eq!(received(&server).await.len(), 1, "a 200 is never retried");
}

/// A malformed JSON body (`{`) is a decode failure, and decode failures are
/// not retryable — one request, no sleeps.
#[tokio::test]
async fn malformed_json_body_is_a_decode_error() {
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_string("{"))
        .expect(1)
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock, (4, Duration::from_millis(1)));

    let err = client
        .sync(&SyncRequest::full())
        .await
        .expect_err("a truncated body must not decode");
    assert!(
        matches!(err, AppError::Api(ApiError::Decode { .. })),
        "expected ApiError::Decode, got {err:?}"
    );

    server.verify().await;
    assert_eq!(
        received(&server).await.len(),
        1,
        "a decode error is not retryable"
    );
    assert_eq!(
        clock.recorded(),
        Vec::<Duration>::new(),
        "a decode error must not sleep"
    );
}

// ---------------------------------------------------------------------------
// Acceptance 6: persistent 500s -> RetriesExhausted with the policy's
// max_attempts, matching the mock's request count
// ---------------------------------------------------------------------------

#[tokio::test]
async fn persistent_server_errors_exhaust_the_policy_budget() {
    const MAX_ATTEMPTS: u32 = 4;

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(500))
        .mount(&server)
        .await;

    let clock = RecordingSleeper::new();
    let client = client_for(&server, &clock, (MAX_ATTEMPTS, Duration::from_millis(5)));

    let err = client
        .sync(&SyncRequest::full())
        .await
        .expect_err("a permanently failing server must give up");

    let attempts_from_error = match err {
        AppError::Api(ApiError::RetriesExhausted { attempts }) => attempts,
        other => panic!("expected ApiError::RetriesExhausted, got {other:?}"),
    };
    assert_eq!(
        attempts_from_error, MAX_ATTEMPTS,
        "the error must report the policy's max_attempts"
    );

    let request_count = received(&server).await.len();
    assert_eq!(
        u32::try_from(request_count).expect("request count fits in u32"),
        MAX_ATTEMPTS,
        "the mock must have seen exactly as many attempts as the error reports"
    );
    assert_eq!(
        u32::try_from(clock.recorded().len()).expect("sleep count fits in u32"),
        MAX_ATTEMPTS - 1,
        "there is one sleep between each pair of attempts and none after the last"
    );
    // Nothing really slept: MAX_ATTEMPTS * a few ms of loopback traffic only.
    assert!(
        clock.recorded().iter().all(|d| *d < Duration::from_secs(1)),
        "the injected policy keeps the recorded backoff sub-second"
    );
}

// ---------------------------------------------------------------------------
// External service down: a refused connection (no listener at all) is the
// harshest "API unreachable" case and must behave like any other transient
// failure
// ---------------------------------------------------------------------------

/// The API is unreachable: every attempt fails at the transport layer
/// (`ApiError::Transport`, mapped from `reqwest::Error`), which the retry loop
/// treats as retryable. The run therefore ends with
/// `RetriesExhausted { attempts: max_attempts }` — the same shape a persistent
/// 5xx produces, so callers need no separate "server is down" branch.
#[tokio::test]
async fn a_closed_loopback_port_is_a_transient_failure_that_retries_then_gives_up() {
    const MAX_ATTEMPTS: u32 = 3;

    // Bind an ephemeral port, then drop the listener: the port is now closed
    // (and nothing else will claim it during the test), so each connect
    // attempt is refused immediately rather than hanging.
    let dead_port = {
        let listener =
            std::net::TcpListener::bind(("127.0.0.1", 0)).expect("bind an ephemeral loopback port");
        listener.local_addr().expect("local addr").port()
    };

    let clock = RecordingSleeper::new();
    let client = client_at(
        &format!("http://127.0.0.1:{dead_port}"),
        &clock,
        (MAX_ATTEMPTS, Duration::from_millis(5)),
    );

    let err = client
        .sync(&SyncRequest::full())
        .await
        .expect_err("a refused connection cannot succeed");
    assert!(
        matches!(
            err,
            AppError::Api(ApiError::RetriesExhausted { attempts }) if attempts == MAX_ATTEMPTS
        ),
        "expected RetriesExhausted {{ attempts: {MAX_ATTEMPTS} }}, got {err:?}"
    );

    let delays = clock.recorded();
    assert_eq!(
        delays.len(),
        usize::try_from(MAX_ATTEMPTS - 1).expect("attempt count fits in usize"),
        "a transport failure sleeps between attempts like any other retryable error"
    );
    assert!(
        delays.windows(2).all(|pair| pair[0] <= pair[1]),
        "the recorded backoff must not shrink, got {delays:?}"
    );
}

// ---------------------------------------------------------------------------
// Guard rails: no real network, no real credential
// ---------------------------------------------------------------------------

/// The suite is only meaningful if it cannot reach the real API and never
/// carries a real-looking token: every mocked server is loopback, and the
/// fake token is the documented `tok_fake_*` value.
#[test]
fn suite_uses_loopback_mocks_and_an_obviously_fake_token() {
    assert!(
        FAKE_TOKEN.starts_with("tok_fake_"),
        "only fake tokens may appear in tests"
    );
    assert_eq!(
        EXPECTED_AUTH_HEADER,
        format!("Bearer {FAKE_TOKEN}"),
        "the header assertion must be derived from the fake token"
    );

    // The T-208 fixture is what every 200 response serves; confirm it is a
    // full-sync reply so the "success" assertions above mean something.
    let fixture: Value = serde_json::from_str(fixture_text()).expect("fixture is JSON");
    assert_eq!(fixture["full_sync"], Value::Bool(true));
    assert_ne!(
        fixture["sync_token"].as_str().unwrap_or_default(),
        "",
        "the fixture must carry a real-looking sync token"
    );
}
