//! HTTPS Sync API transport and status mapping (T-207, EPIC-03).
//!
//! [`SyncClient`] owns a single reused `reqwest::Client` (`https_only`, a
//! connect timeout and a request timeout, mirroring the named-constant
//! pattern in `src/auth/validate.rs`) and exposes [`SyncClient::sync`],
//! which `POST`s a [`SyncRequest`] to `{base}/api/v1/sync` with a bearer
//! token, runs the attempt through the T-206 [`retry`] helper, and maps
//! the HTTP response to a typed [`SyncResponse`] or an [`ApiError`].
//!
//! Unlike `src/auth/validate.rs` (one throwaway client per call, no
//! retries, no response parsing), this client is built once via
//! [`SyncClient::new`]/[`SyncClient::with_clock`] and reused across every
//! `sync` call, and understands the full Sync API response shape and
//! retry/backoff behaviour.
//!
//! The token is only ever read through [`Token::expose`] when building the
//! `Authorization` header; it is never formatted into the request URL, a
//! log line or an error. [`SyncClient`] has a hand-written [`Debug`] impl
//! (the retry-policy factory closure it stores is not `Debug`) that prints
//! the token through [`Token`]'s own redacting `Debug` impl, never the raw
//! value.
//!
//! The base URL must use the `https://` scheme; any other scheme is
//! rejected with [`ApiError::InsecureBaseUrl`] before any network activity
//! happens (`reqwest::Client::builder().build()` performs no I/O, and the
//! scheme gate runs before it). The same debug-only loopback exception as
//! `src/auth/validate.rs` applies: in a `debug_assertions` build only,
//! `http://127.0.0.1`, `http://localhost` and `http://[::1]` are also
//! accepted, so local test harnesses can point this client at a plaintext
//! mock server; this can never be reached in a release binary.

use std::fmt;
use std::time::Duration;

use reqwest::{Client, StatusCode};

use crate::api::retry::{retry, RetryPolicy, Sleeper, TokioSleeper};
use crate::api::sync::{SyncRequest, SyncResponse};
use crate::auth::Token;
use crate::error::{ApiError, Result};

/// Connect timeout applied to the reused HTTP client (named instead of a
/// bare literal, same rationale as `src/auth/validate.rs`'s
/// `REQUEST_TIMEOUT`).
const CONNECT_TIMEOUT: Duration = Duration::from_secs(10);

/// Overall per-request timeout applied to the reused HTTP client.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);

/// A reused HTTPS client for the Todoist Sync API.
///
/// Generic over the [`Sleeper`] used by the retry helper so tests can
/// supply a non-sleeping recording double instead of
/// [`TokioSleeper`] (the production default); see [`SyncClient::with_clock`].
pub struct SyncClient<C: Sleeper = TokioSleeper> {
    http: Client,
    token: Token,
    base_url: String,
    make_policy: Box<dyn Fn() -> RetryPolicy + Send + Sync>,
    clock: C,
}

impl SyncClient<TokioSleeper> {
    /// Builds a client with the production [`TokioSleeper`] clock and the
    /// default [`RetryPolicy`].
    ///
    /// # Errors
    ///
    /// - [`ApiError::InsecureBaseUrl`] if `base_url` does not use the
    ///   `https://` scheme (debug-only loopback exception aside).
    /// - [`ApiError::Transport`] if the underlying `reqwest::Client` could
    ///   not be built.
    pub fn new(token: Token, base_url: &str) -> Result<Self> {
        Self::with_clock(token, base_url, TokioSleeper)
    }
}

impl<C: Sleeper> SyncClient<C> {
    /// Builds a client with an injected clock (and the default
    /// [`RetryPolicy`] factory); use [`SyncClient::with_retry_policy`] to
    /// also override the retry policy.
    ///
    /// # Errors
    ///
    /// Same as [`SyncClient::new`].
    pub fn with_clock(token: Token, base_url: &str, clock: C) -> Result<Self> {
        if !is_acceptable_base_url_scheme(base_url) {
            return Err(ApiError::InsecureBaseUrl.into());
        }

        let http = Client::builder()
            .https_only(https_only_for(base_url))
            .connect_timeout(CONNECT_TIMEOUT)
            .timeout(REQUEST_TIMEOUT)
            .build()
            .map_err(|source| ApiError::Transport { source })?;

        Ok(Self {
            http,
            token,
            base_url: base_url.to_string(),
            make_policy: Box::new(RetryPolicy::new),
            clock,
        })
    }

    /// Replaces the retry-policy factory: called once per [`Self::sync`]
    /// call to build a fresh [`RetryPolicy`] (a factory rather than a
    /// stored policy because `RetryPolicy`'s jitter source is a boxed
    /// `FnMut`, not `Clone`). Tests pass a closure returning a
    /// deterministic policy (e.g. `|| RetryPolicy::new().with_jitter(|| 0.0)`).
    #[must_use]
    pub fn with_retry_policy(
        mut self,
        make_policy: impl Fn() -> RetryPolicy + Send + Sync + 'static,
    ) -> Self {
        self.make_policy = Box::new(make_policy);
        self
    }

    /// `POST`s `req` to `{base}/api/v1/sync` with a bearer-token
    /// `Authorization` header and a JSON body. Each attempt — sending the
    /// request *and* mapping the resulting status code via
    /// [`map_response`] — runs inside the T-206 [`retry`] helper, so a
    /// retryable mapped outcome (`RateLimited`/`Server`, see
    /// [`ApiError::is_retryable`]) is retried exactly like a transport
    /// failure, not just a `send()` error.
    ///
    /// # Errors
    ///
    /// - [`ApiError::Unauthorized`] on HTTP 401/403.
    /// - [`ApiError::RateLimited`] on HTTP 429 (`retry_after` parsed from a
    ///   delta-seconds `Retry-After` header, `None` when absent or not a
    ///   plain integer).
    /// - [`ApiError::Server`] on HTTP 5xx.
    /// - [`ApiError::Transport`] if sending the request failed at the
    ///   transport level.
    /// - [`ApiError::Decode`] if a success response body could not be
    ///   parsed into [`SyncResponse`].
    /// - [`ApiError::RetriesExhausted`] once the retry budget is spent on a
    ///   retryable failure.
    pub async fn sync(&self, req: &SyncRequest) -> Result<SyncResponse> {
        let url = format!("{}/api/v1/sync", self.base_url);

        let response = retry((self.make_policy)(), &self.clock, || async {
            let response = self
                .http
                .post(&url)
                .bearer_auth(self.token.expose())
                .json(req)
                .send()
                .await
                .map_err(|source| ApiError::Transport { source })?;
            map_response(response).await
        })
        .await?;

        Ok(response)
    }
}

impl<C: Sleeper> fmt::Debug for SyncClient<C> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("SyncClient")
            .field("http", &self.http)
            .field("token", &self.token)
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

/// Maps a raw HTTP response to a [`SyncResponse`] or [`ApiError`],
/// consuming the response (needed to read the body on the success path).
async fn map_response(response: reqwest::Response) -> std::result::Result<SyncResponse, ApiError> {
    let status = response.status();
    match status {
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(ApiError::Unauthorized),
        StatusCode::TOO_MANY_REQUESTS => {
            let retry_after = parse_retry_after(response.headers());
            Err(ApiError::RateLimited { retry_after })
        }
        status if status.is_server_error() => Err(ApiError::Server {
            status: status.as_u16(),
        }),
        status if status.is_success() => response
            .json::<SyncResponse>()
            .await
            .map_err(|source| ApiError::Decode { source }),
        status => Err(ApiError::Server {
            status: status.as_u16(),
        }),
    }
}

/// Parses the `Retry-After` header in its delta-seconds form (a plain
/// non-negative integer number of seconds). Returns `None` when the header
/// is absent, not valid UTF-8, or not a plain integer (e.g. the HTTP-date
/// form) — callers fall back to the retry policy's own computed backoff in
/// that case.
fn parse_retry_after(headers: &reqwest::header::HeaderMap) -> Option<Duration> {
    headers
        .get(reqwest::header::RETRY_AFTER)?
        .to_str()
        .ok()?
        .trim()
        .parse::<u64>()
        .ok()
        .map(Duration::from_secs)
}

/// Returns whether the `reqwest` client built for `base_url` should enforce
/// `https_only` in a release build: always `true`, no exceptions. Mirrors
/// `src/auth/validate.rs::https_only_for`.
#[cfg(not(debug_assertions))]
fn https_only_for(_base_url: &str) -> bool {
    true
}

/// Debug-build-only variant of [`https_only_for`]: relaxes `https_only`
/// only for the same loopback exceptions [`is_acceptable_base_url_scheme`]
/// accepts via the `http://` branch, so an `https://` base URL in a debug
/// build still gets the same strict client as release. Mirrors
/// `src/auth/validate.rs::https_only_for`.
#[cfg(debug_assertions)]
fn https_only_for(base_url: &str) -> bool {
    !(base_url.starts_with("http://127.0.0.1")
        || base_url.starts_with("http://localhost")
        || base_url.starts_with("http://[::1]"))
}

/// Returns whether `base_url`'s scheme is acceptable: `https://` only, no
/// exceptions whatsoever in a release build. Mirrors
/// `src/auth/validate.rs::is_acceptable_base_url_scheme`.
#[cfg(not(debug_assertions))]
fn is_acceptable_base_url_scheme(base_url: &str) -> bool {
    base_url.starts_with("https://")
}

/// Debug-build-only variant of [`is_acceptable_base_url_scheme`]: also
/// accepts plain `http://` when the host is a loopback address, so local
/// test harnesses can point this client at a plaintext mock server. This
/// function only exists in `debug_assertions` builds, so it can never be
/// compiled into a release binary. Mirrors
/// `src/auth/validate.rs::is_acceptable_base_url_scheme`.
#[cfg(debug_assertions)]
fn is_acceptable_base_url_scheme(base_url: &str) -> bool {
    base_url.starts_with("https://")
        || base_url.starts_with("http://127.0.0.1")
        || base_url.starts_with("http://localhost")
        || base_url.starts_with("http://[::1]")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::retry::RecordingSleeper;
    use crate::error::AppError;

    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    fn fixture_token() -> Token {
        Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token")
    }

    #[test]
    fn rejects_plain_http_base_url_without_any_network_call() {
        let err = SyncClient::with_clock(
            fixture_token(),
            "http://api.todoist.com",
            RecordingSleeper::new(),
        )
        .expect_err("http:// must be rejected");
        assert!(matches!(err, AppError::Api(ApiError::InsecureBaseUrl)));
    }

    #[test]
    fn rejects_non_https_schemes_and_scheme_less_hosts() {
        for bad in ["ftp://api.todoist.com", "api.todoist.com", ""] {
            let err = SyncClient::with_clock(fixture_token(), bad, RecordingSleeper::new())
                .expect_err("non-https base URL must be rejected");
            assert!(matches!(err, AppError::Api(ApiError::InsecureBaseUrl)));
        }
    }

    #[test]
    fn default_retry_after_parsing_rejects_http_date_and_missing_header() {
        let mut headers = reqwest::header::HeaderMap::new();
        assert_eq!(parse_retry_after(&headers), None);

        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("Wed, 21 Oct 2015 07:28:00 GMT"),
        );
        assert_eq!(parse_retry_after(&headers), None);

        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("120"),
        );
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(120)));
    }

    #[test]
    fn fixture_token_absent_from_client_debug_and_every_error_it_can_build() {
        let client = SyncClient::with_clock(
            fixture_token(),
            "https://api.todoist.com",
            RecordingSleeper::new(),
        )
        .expect("valid https base url builds a client");
        let client_debug = format!("{client:?}");
        assert!(
            !client_debug.contains(FIXTURE_TOKEN),
            "client Debug leaked the token: {client_debug}"
        );

        // Every `ApiError` variant this module can construct, rendered via
        // `Debug` (which for `thiserror` enums also includes `Display`-free
        // struct fields) — none of them ever carry the token, since it is
        // only ever read through `Token::expose` when building the
        // `Authorization` header.
        let errors: Vec<AppError> = vec![
            ApiError::InsecureBaseUrl.into(),
            ApiError::Unauthorized.into(),
            ApiError::RateLimited {
                retry_after: Some(Duration::from_secs(1)),
            }
            .into(),
            ApiError::Server { status: 500 }.into(),
            ApiError::RetriesExhausted { attempts: 3 }.into(),
        ];
        for err in errors {
            let rendered = format!("{err:?}");
            assert!(
                !rendered.contains(FIXTURE_TOKEN),
                "error Debug leaked the token: {rendered}"
            );
        }
    }
}

/// Loopback `wiremock` coverage added by the unit tester (T-207): the
/// production `SyncClient` is https-only, but the debug-only loopback
/// exception (see [`is_acceptable_base_url_scheme`]) lets these tests drive
/// the *real* client — request path, bearer header, JSON body, retry loop
/// and status mapping — against a plaintext local mock server, exactly like
/// the `wiremock_tests` module in `src/auth/validate.rs`.
///
/// Status-mapping assertions go through [`map_response`] directly (with a
/// raw `reqwest::Response` obtained from a throwaway plain-HTTP client)
/// because the retry helper collapses every retryable failure (429/5xx) into
/// `ApiError::RetriesExhausted`, which would hide the mapping under test.
#[cfg(test)]
mod wiremock_tests {
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::api::retry::RecordingSleeper;
    use crate::error::AppError;

    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    fn fixture_token() -> Token {
        Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty fake token")
    }

    /// A retry policy with jitter disabled so recorded sleeps are exact.
    fn no_jitter_policy(max_attempts: u32, base_delay: Duration) -> RetryPolicy {
        let mut policy = RetryPolicy::new();
        policy.max_attempts = max_attempts;
        policy.base_delay = base_delay;
        policy.max_delay = Duration::from_secs(60);
        policy.jitter_fraction = 0.0;
        policy.with_jitter(|| 0.0)
    }

    /// Sends one request to the mock server with a plain (non-https-only)
    /// throwaway client and hands the raw response to [`map_response`], so
    /// the status mapping is observable without the retry wrapper.
    async fn map_response_for(server: &MockServer) -> std::result::Result<SyncResponse, ApiError> {
        let raw = Client::new()
            .post(format!("{}/api/v1/sync", server.uri()))
            .send()
            .await
            .expect("loopback mock server must accept the request");
        map_response(raw).await
    }

    // --- Retry-After parsing edge cases -----------------------------------

    #[test]
    fn retry_after_zero_and_padded_delta_seconds_are_parsed() {
        let mut headers = reqwest::header::HeaderMap::new();

        // `Retry-After: 0` is the "retry immediately" hint and must parse as
        // `Some(0s)`, not be confused with "header absent" (`None`).
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("0"),
        );
        assert_eq!(parse_retry_after(&headers), Some(Duration::ZERO));

        // Surrounding optional whitespace is trimmed before parsing.
        headers.insert(
            reqwest::header::RETRY_AFTER,
            reqwest::header::HeaderValue::from_static("  7  "),
        );
        assert_eq!(parse_retry_after(&headers), Some(Duration::from_secs(7)));
    }

    #[test]
    fn retry_after_rejects_non_numeric_and_out_of_range_forms() {
        let mut headers = reqwest::header::HeaderMap::new();
        for bad in [
            "",                               // empty
            "   ",                            // whitespace only
            "abc",                            // garbage
            "1.5",                            // fractional seconds (not delta-seconds)
            "-3",                             // negative
            "5s",                             // unit suffix
            "0x10",                           // hex
            "999999999999999999999999999999", // overflows u64
        ] {
            headers.insert(
                reqwest::header::RETRY_AFTER,
                reqwest::header::HeaderValue::from_str(bad).expect("valid header value"),
            );
            assert_eq!(
                parse_retry_after(&headers),
                None,
                "Retry-After {bad:?} must not parse"
            );
        }
    }

    // --- Status mapping through map_response ------------------------------

    #[tokio::test]
    async fn both_401_and_403_map_to_unauthorized() {
        for status in [401_u16, 403] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/sync"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;

            let err = map_response_for(&server)
                .await
                .expect_err("auth failure must be an error");
            assert!(
                matches!(err, ApiError::Unauthorized),
                "status {status} must map to Unauthorized, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn retry_after_header_variants_reach_the_rate_limited_error() {
        // (header value or None, expected parsed retry_after)
        let cases: [(Option<&'static str>, Option<Duration>); 4] = [
            (None, None),
            (Some("0"), Some(Duration::ZERO)),
            (Some("42"), Some(Duration::from_secs(42))),
            (Some("Wed, 21 Oct 2015 07:28:00 GMT"), None),
        ];

        for (header, expected) in cases {
            let server = MockServer::start().await;
            let mut template = ResponseTemplate::new(429);
            if let Some(value) = header {
                template = template.insert_header("retry-after", value);
            }
            Mock::given(method("POST"))
                .and(path("/api/v1/sync"))
                .respond_with(template)
                .mount(&server)
                .await;

            let err = map_response_for(&server)
                .await
                .expect_err("429 must be an error");
            match err {
                ApiError::RateLimited { retry_after } => assert_eq!(
                    retry_after, expected,
                    "Retry-After {header:?} must parse to {expected:?}"
                ),
                other => panic!("429 must map to RateLimited, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn server_error_statuses_carry_their_status_code() {
        for status in [500_u16, 502, 503, 599] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/sync"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;

            let err = map_response_for(&server)
                .await
                .expect_err("5xx must be an error");
            assert!(
                matches!(err, ApiError::Server { status: s } if s == status),
                "status {status} must map to Server {{ status: {status} }}, got {err:?}"
            );
        }
    }

    /// Non-special 4xx statuses (anything that is neither 401/403 nor 429)
    /// fall through to the catch-all `ApiError::Server { status }` arm. That
    /// naming is arguably wrong for a client error, but it is the documented
    /// behaviour of [`map_response`] (acceptance only pins 401/403, 429 and
    /// 5xx); pinned here so a future refactor cannot silently change it.
    #[tokio::test]
    async fn other_client_error_statuses_fall_through_to_server() {
        for status in [400_u16, 404, 409, 418] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/sync"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;

            let err = map_response_for(&server)
                .await
                .expect_err("non-2xx must be an error");
            assert!(
                matches!(err, ApiError::Server { status: s } if s == status),
                "status {status} must reach the catch-all Server arm, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn malformed_and_truncated_success_bodies_map_to_decode() {
        for body in [
            "not json at all",
            "",
            r#"{"sync_token": "abc", "items": [{"#,
            r#"{"full_sync": "not-a-bool"}"#,
        ] {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/sync"))
                .respond_with(ResponseTemplate::new(200).set_body_string(body))
                .mount(&server)
                .await;

            let err = map_response_for(&server)
                .await
                .expect_err("unparseable body must be an error");
            assert!(
                matches!(err, ApiError::Decode { .. }),
                "body {body:?} must map to Decode, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn minimal_success_body_decodes_into_a_sync_response() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"sync_token": "next_tok"}"#),
            )
            .mount(&server)
            .await;

        let response = map_response_for(&server).await.expect("valid body decodes");
        assert_eq!(response.sync_token, "next_tok");
        assert!(!response.full_sync);
        assert_eq!(response.items.len(), 0);
        assert!(response.user.is_none());
    }

    // --- Real client over loopback: request shape + retry injection -------

    #[tokio::test]
    async fn sync_posts_bearer_json_to_api_v1_sync_without_leaking_the_token_in_the_url() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"sync_token": "s2"}"#))
            .mount(&server)
            .await;

        let clock = RecordingSleeper::new();
        let client = SyncClient::with_clock(fixture_token(), &server.uri(), clock)
            .expect("loopback base url is accepted in debug builds");
        let response = client
            .sync(&SyncRequest::full())
            .await
            .expect("mock server returns a valid body");
        assert_eq!(response.sync_token, "s2");

        let requests = server
            .received_requests()
            .await
            .expect("mock server records requests");
        assert_eq!(
            requests.len(),
            1,
            "exactly one attempt, no spurious retries"
        );
        let request = &requests[0];
        assert_eq!(request.url.path(), "/api/v1/sync");
        assert_eq!(
            request
                .headers
                .get("authorization")
                .expect("Authorization header present")
                .to_str()
                .expect("header is ASCII"),
            format!("Bearer {}", fixture_token().expose()),
            "bearer token must be sent verbatim in the Authorization header"
        );
        assert!(
            !request.url.as_str().contains(FIXTURE_TOKEN),
            "token must never appear in the request URL: {}",
            request.url
        );
        assert_eq!(
            request
                .body_json::<SyncRequest>()
                .expect("request body is the serialised SyncRequest"),
            SyncRequest::full()
        );
    }

    /// How a transport-level failure surfaces from `sync`: the send error is
    /// mapped to `ApiError::Transport` (retryable), so [`retry`] converts it
    /// into `ApiError::RetriesExhausted { attempts }` — even with a
    /// one-attempt budget. Pinned because acceptance item 4 names
    /// `ApiError::Transport` as the transport-failure mapping and it is only
    /// observable one layer down (inside the retried closure).
    #[tokio::test]
    async fn transport_failure_surfaces_as_retries_exhausted_from_sync() {
        let dead_port = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
                .expect("bind an ephemeral loopback port");
            listener.local_addr().expect("local addr").port()
        };

        let clock = RecordingSleeper::new();
        let client = SyncClient::with_clock(
            fixture_token(),
            &format!("http://127.0.0.1:{dead_port}"),
            clock,
        )
        .expect("loopback base url is accepted in debug builds")
        .with_retry_policy(|| no_jitter_policy(1, Duration::from_millis(1)));

        let err = client
            .sync(&SyncRequest::full())
            .await
            .expect_err("connection refused must be an error");
        assert!(
            matches!(
                err,
                AppError::Api(ApiError::RetriesExhausted { attempts: 1 })
            ),
            "expected RetriesExhausted {{ attempts: 1 }}, got {err:?}"
        );
        assert_eq!(
            client.clock.recorded(),
            Vec::<Duration>::new(),
            "a one-attempt budget must not sleep"
        );
    }

    #[tokio::test]
    async fn unauthorized_is_not_retried_and_never_sleeps() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let clock = RecordingSleeper::new();
        let client = SyncClient::with_clock(fixture_token(), &server.uri(), clock)
            .expect("loopback base url is accepted in debug builds");
        let err = client
            .sync(&SyncRequest::full())
            .await
            .expect_err("401 must be an error");
        assert!(matches!(err, AppError::Api(ApiError::Unauthorized)));

        let requests = server
            .received_requests()
            .await
            .expect("mock server records requests");
        assert_eq!(requests.len(), 1, "401 must not be retried");
        assert_eq!(
            client.clock.recorded(),
            Vec::<Duration>::new(),
            "401 must not sleep before giving up"
        );
    }

    /// Retry policy + clock injection, exercised end-to-end through the real
    /// `sync` path: a closed loopback port makes every attempt fail at the
    /// transport layer (a genuinely retryable `ApiError::Transport`), so the
    /// injected policy's attempt budget and the injected clock's recorded
    /// sleeps are both observable.
    #[tokio::test]
    async fn injected_policy_and_clock_drive_the_retry_loop_end_to_end() {
        // Reserve a free loopback port, then drop the listener so nothing is
        // accepting connections on it: every attempt is refused instantly.
        let dead_port = {
            let listener = std::net::TcpListener::bind(("127.0.0.1", 0))
                .expect("bind an ephemeral loopback port");
            listener.local_addr().expect("local addr").port()
        };

        let clock = RecordingSleeper::new();
        let client = SyncClient::with_clock(
            fixture_token(),
            &format!("http://127.0.0.1:{dead_port}"),
            clock,
        )
        .expect("loopback base url is accepted in debug builds")
        .with_retry_policy(|| no_jitter_policy(3, Duration::from_millis(10)));

        let err = client
            .sync(&SyncRequest::full())
            .await
            .expect_err("nothing listens on the reserved port");
        assert!(
            matches!(
                err,
                AppError::Api(ApiError::RetriesExhausted { attempts: 3 })
            ),
            "expected RetriesExhausted {{ attempts: 3 }}, got {err:?}"
        );
        assert_eq!(
            client.clock.recorded(),
            vec![Duration::from_millis(10), Duration::from_millis(20)],
            "the injected policy's exponential backoff must drive the sleeps"
        );
    }

    /// A 5xx is retryable, so if every attempt in the policy's budget keeps
    /// failing, `sync` must exhaust the retry budget (`RetriesExhausted`),
    /// not surface the bare `Server` error from a single un-retried
    /// attempt, and it must sleep between attempts exactly like a
    /// transport failure would.
    #[tokio::test]
    async fn server_errors_are_retried_until_the_budget_is_exhausted() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(503))
            .mount(&server)
            .await;

        let clock = RecordingSleeper::new();
        let client = SyncClient::with_clock(fixture_token(), &server.uri(), clock)
            .expect("loopback base url is accepted in debug builds")
            .with_retry_policy(|| no_jitter_policy(2, Duration::from_millis(5)));

        let err = client
            .sync(&SyncRequest::full())
            .await
            .expect_err("503 must be an error");
        assert!(
            matches!(
                err,
                AppError::Api(ApiError::RetriesExhausted { attempts: 2 })
            ),
            "a retryable 5xx that never recovers must exhaust the retry budget; got {err:?}"
        );
        assert_eq!(
            client.clock.recorded(),
            vec![Duration::from_millis(5)],
            "a 5xx must sleep between attempts just like a transport failure"
        );
        let requests = server
            .received_requests()
            .await
            .expect("mock server records requests");
        assert_eq!(requests.len(), 2, "both budgeted attempts must be sent");
    }

    /// A 5xx is retryable, so two 500s followed by a 200 should succeed on
    /// the third attempt with the injected policy's backoff between
    /// attempts.
    #[tokio::test]
    async fn retries_transient_5xx_then_succeeds() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(500))
            .up_to_n_times(2)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"sync_token": "s3"}"#))
            .mount(&server)
            .await;

        let clock = RecordingSleeper::new();
        let client = SyncClient::with_clock(fixture_token(), &server.uri(), clock)
            .expect("loopback base url is accepted in debug builds")
            .with_retry_policy(|| no_jitter_policy(3, Duration::from_millis(10)));

        let response = client
            .sync(&SyncRequest::full())
            .await
            .expect("third attempt succeeds");
        assert_eq!(response.sync_token, "s3");
        assert_eq!(
            client.clock.recorded(),
            vec![Duration::from_millis(10), Duration::from_millis(20)]
        );
    }

    /// T-206's `Retry-After` override: a 429 with `Retry-After: 5` must make
    /// the next wait 5s even though the policy's backoff is 1ms.
    #[tokio::test]
    async fn retry_after_hint_raises_the_injected_backoff() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(429).insert_header("retry-after", "5"))
            .up_to_n_times(1)
            .mount(&server)
            .await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(200).set_body_string(r#"{"sync_token": "s4"}"#))
            .mount(&server)
            .await;

        let clock = RecordingSleeper::new();
        let client = SyncClient::with_clock(fixture_token(), &server.uri(), clock)
            .expect("loopback base url is accepted in debug builds")
            .with_retry_policy(|| no_jitter_policy(2, Duration::from_millis(1)));

        let response = client
            .sync(&SyncRequest::full())
            .await
            .expect("retry after the rate-limit hint succeeds");
        assert_eq!(response.sync_token, "s4");
        assert_eq!(
            client.clock.recorded(),
            vec![Duration::from_secs(5)],
            "the Retry-After delta-seconds hint must override the 1ms backoff"
        );
    }

    // --- Scheme / https_only edge cases -----------------------------------

    #[test]
    fn scheme_gate_accepts_https_variants_and_rejects_lookalike_schemes() {
        for good in [
            "https://api.todoist.com",
            "https://api.todoist.com/",
            "https://api.todoist.com:8443",
            "https://",
        ] {
            assert!(
                is_acceptable_base_url_scheme(good),
                "https base URL {good:?} must be accepted"
            );
        }

        // Mixed/upper-case scheme: URL schemes are case-insensitive per
        // RFC 3986, so this is a (documented, conservative) rejection rather
        // than a security hole. Pinned here so a future change is deliberate.
        for bad in [
            "HTTPS://api.todoist.com",
            "Https://api.todoist.com",
            "http://api.todoist.com",
            "ws://api.todoist.com",
            "https:/api.todoist.com",
            " https://api.todoist.com",
        ] {
            assert!(
                !is_acceptable_base_url_scheme(bad),
                "non-https base URL {bad:?} must be rejected"
            );
        }
    }

    /// GAP-2 (reported, not fixed): the debug-only loopback exception is a
    /// plain `starts_with` *prefix* check, not a host-boundary check, so
    /// `http://localhost.evil.example` and `http://127.0.0.1.evil.example`
    /// are treated as loopback and accepted — meaning a debug build would
    /// send the bearer token over plaintext HTTP to a remote host. Release
    /// builds are unaffected (the whole branch is `cfg(debug_assertions)`),
    /// and the identical shape exists in `src/auth/validate.rs`, so this is
    /// pre-existing rather than new to T-207. This test pins the current
    /// permissive behaviour so a fix is a deliberate, visible change.
    #[test]
    fn gap2_loopback_exception_is_a_prefix_match_not_a_host_match() {
        assert!(
            is_acceptable_base_url_scheme("http://localhost.evil.example"),
            "current behaviour: prefix match accepts this remote host"
        );
        assert!(
            is_acceptable_base_url_scheme("http://127.0.0.1.evil.example"),
            "current behaviour: prefix match accepts this remote host"
        );
        assert!(
            !https_only_for("http://localhost.evil.example"),
            "current behaviour: https_only is relaxed for this remote host"
        );
    }

    #[test]
    fn rejections_happen_before_any_client_is_built() {
        // `InsecureBaseUrl` is the only error variant reachable here, which
        // proves the gate short-circuits before `reqwest::Client::builder()`
        // (a build failure would have produced `ApiError::Transport`).
        for bad in ["http://api.todoist.com", "file:///etc/passwd", ""] {
            let err = SyncClient::with_clock(fixture_token(), bad, RecordingSleeper::new())
                .expect_err("non-https base URL must be rejected");
            assert!(
                matches!(err, AppError::Api(ApiError::InsecureBaseUrl)),
                "{bad:?} must yield InsecureBaseUrl, got {err:?}"
            );
        }
    }

    #[tokio::test]
    async fn https_only_is_relaxed_only_for_the_documented_loopback_hosts() {
        // Debug builds: strict for any real https base URL, relaxed only for
        // the three loopback spellings the scheme gate also accepts.
        assert!(https_only_for("https://api.todoist.com"));
        assert!(https_only_for("https://api.todoist.com/"));
        assert!(!https_only_for("http://127.0.0.1:8080"));
        assert!(!https_only_for("http://localhost:5000"));
        assert!(!https_only_for("http://[::1]:9"));

        // A client built for a strict base URL rejects plain http before any
        // socket is opened, even in a debug build.
        let err = Client::builder()
            .https_only(true)
            .build()
            .expect("client builds")
            .get("http://127.0.0.1:1/api/v1/sync")
            .send()
            .await
            .expect_err("https_only client must refuse plain http");
        assert!(
            err.is_builder() || err.is_request(),
            "expected a scheme rejection, got {err:?}"
        );
    }

    #[test]
    fn token_absent_from_client_debug_for_an_injected_retry_policy() {
        let client = SyncClient::with_clock(
            fixture_token(),
            "https://api.todoist.com",
            RecordingSleeper::new(),
        )
        .expect("valid https base url builds a client")
        .with_retry_policy(|| no_jitter_policy(2, Duration::from_millis(1)));

        let rendered = format!("{client:?}");
        assert!(
            !rendered.contains(FIXTURE_TOKEN),
            "client Debug leaked the token: {rendered}"
        );
        // The base URL is still shown (useful diagnostics) and the token is
        // rendered through `Token`'s redacting `Debug` impl, which is exactly
        // `log::REDACTED`.
        assert!(rendered.contains("https://api.todoist.com"));
        assert!(
            rendered.contains(crate::log::REDACTED),
            "token field must render as the redaction marker: {rendered}"
        );
    }
}
