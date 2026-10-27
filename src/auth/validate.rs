//! Token validation against the real Todoist API (T-106, EPIC-02).
//!
//! [`validate`] calls a lightweight authenticated Todoist REST endpoint
//! (`GET /api/v1/user`, which echoes the current user for a valid token)
//! over HTTPS-only `reqwest` (rustls, see `Cargo.toml` — no native-tls/
//! OpenSSL, REQ-001) and maps the response to [`crate::error::AuthError`]:
//! a 2xx response means the token is accepted, 401/403 means it was
//! rejected, and anything else (network failure, unexpected status) is
//! surfaced as a distinct error so callers can tell "your token is wrong"
//! apart from "we could not reach Todoist".
//!
//! The token is only ever read through [`Token::expose`] when building the
//! `Authorization` header; it is never formatted into a log line, an error
//! message, or the request URL (query strings/paths end up in error/debug
//! output far more easily than headers).
//!
//! The base URL is an injectable parameter (not a constant) so tests can
//! point at a local mock server instead of the real Todoist API — this
//! module itself makes no real network calls in its own tests, it only
//! exercises the pure HTTPS-enforcement / URL-building logic.

use reqwest::{Client, StatusCode};

use crate::auth::Token;
use crate::error::{AuthError, Result};

/// Default Todoist API base URL used by [`validate`].
///
/// A lightweight authenticated endpoint: `GET {base}/api/v1/user` returns
/// the current user for a valid token and 401 for an invalid one, without
/// requiring any request body or pulling the user's full task list. (The
/// older `/sync/v9/user` and `/rest/v2/*` endpoints are deprecated and now
/// return HTTP 410 Gone for every request, valid token or not.)
pub const DEFAULT_BASE_URL: &str = "https://api.todoist.com";

/// Validates `token` against the Todoist API reachable at `base_url`.
///
/// `base_url` must use the `https://` scheme; any other scheme (including
/// plain `http://`) is rejected before any network activity happens, so a
/// misconfigured or attacker-controlled base URL can never cause the token
/// to be sent in the clear. Tests inject a local HTTPS mock server's URL
/// here instead of [`DEFAULT_BASE_URL`].
///
/// # Errors
///
/// - [`AuthError::InsecureBaseUrl`] if `base_url` is not `https://`.
/// - [`AuthError::Request`] if the HTTPS client could not be built, or the
///   request failed at the transport level (DNS, TLS, timeout, connection
///   refused, ...).
/// - [`AuthError::InvalidToken`] if the API reported the token as
///   unauthorized (HTTP 401 or 403).
/// - [`AuthError::UnexpectedStatus`] if the API returned any other
///   non-success status.
pub async fn validate(token: &Token, base_url: &str) -> Result<()> {
    if !base_url.starts_with("https://") {
        return Err(AuthError::InsecureBaseUrl.into());
    }

    let client = Client::builder()
        .https_only(true)
        .timeout(std::time::Duration::from_secs(10))
        .build()
        .map_err(|source| AuthError::Request { source })?;

    let url = format!("{base_url}/api/v1/user");

    let response = client
        .get(url)
        .bearer_auth(token.expose())
        .send()
        .await
        .map_err(|source| AuthError::Request { source })?;

    match response.status() {
        status if status.is_success() => Ok(()),
        StatusCode::UNAUTHORIZED | StatusCode::FORBIDDEN => Err(AuthError::InvalidToken.into()),
        status => Err(AuthError::UnexpectedStatus {
            status: status.as_u16(),
        }
        .into()),
    }
}

#[cfg(test)]
mod tests {
    //! Sanity checks proving the T-106 design constraints hold without any
    //! real network activity: pure URL-scheme enforcement and the HTTPS
    //! client-build path. The unit-tester adds HTTP-mocking coverage
    //! separately (the acceptance task notes no mocking dev-dependency was
    //! available at implementation time — see the task report).
    use super::*;

    const FIXTURE_TOKEN: &str = "tok_fake_validate_abc123";

    #[tokio::test]
    async fn rejects_plain_http_base_url_without_any_network_call() {
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        let err = validate(&token, "http://api.todoist.com")
            .await
            .expect_err("http:// must be rejected");
        assert!(matches!(
            err,
            crate::error::AppError::Auth(AuthError::InsecureBaseUrl)
        ));
    }

    #[tokio::test]
    async fn rejects_non_https_schemes_and_scheme_less_hosts() {
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        for bad in ["ftp://api.todoist.com", "api.todoist.com", ""] {
            let err = validate(&token, bad)
                .await
                .expect_err("non-https base URL must be rejected");
            assert!(matches!(
                err,
                crate::error::AppError::Auth(AuthError::InsecureBaseUrl)
            ));
        }
    }

    #[test]
    fn default_base_url_uses_https() {
        assert!(DEFAULT_BASE_URL.starts_with("https://"));
    }
}

/// Extended T-106 coverage (unit-tester, different model family from the
/// coder). Complements the sanity `tests` module above with: proof that the
/// HTTPS gate runs *before* any socket is opened (a live loopback listener
/// sees zero connections for rejected schemes), proof that a plaintext
/// response cannot impersonate a 200 over `https://` (TLS + `https_only`
/// enforcement, the MITM/downgrade scenario), transport-failure mapping to
/// `AuthError::Request`, URL-shape and token-absence checks via reqwest's
/// error renderings, malformed-`https://`-prefixed inputs, and the
/// Display/Debug contract of the new error variants.
///
/// NOT coverable here without a new dev-dependency: HTTP status mapping
/// (2xx → Ok, 401/403 → `InvalidToken`, other → `UnexpectedStatus`). Feeding
/// real responses into `validate` requires terminating TLS, and the crate
/// has no TLS server implementation available (no `[dev-dependencies]`
/// exist; rustls is only a transitive dep and a from-scratch TLS server in
/// std is infeasible). Wiring a `wiremock`/`httpmock` dev-dep or injecting
/// the `reqwest::Client` is left to a follow-up task — the mapping itself
/// is verified by code review against the task spec.
#[cfg(test)]
mod unit_tests {
    use std::io::{self, Write};
    use std::net::TcpListener;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::sync::Arc;
    use std::time::{Duration, Instant};

    use super::*;
    use crate::error::AppError;

    /// Fake, never-real token fixture (all tests assert it never leaks).
    const FAKE_TOKEN: &str = "tok_fake_t106_9d4e1f";

    /// What the mock loopback server does with the single connection it
    /// serves.
    enum ServerScript {
        /// Drop the connection immediately — the TLS handshake sees EOF.
        Close,
        /// Answer with a syntactically valid *plaintext* HTTP 200 response
        /// and close. Only a client willing to downgrade to plaintext
        /// could ever parse this as success.
        Plaintext200,
    }

    impl ServerScript {
        fn run(self, mut stream: std::net::TcpStream) {
            match self {
                ServerScript::Close => drop(stream),
                ServerScript::Plaintext200 => {
                    let _ = stream.write_all(
                        b"HTTP/1.1 200 OK\r\nContent-Length: 13\r\n\r\n{\"ok\": true}\n",
                    );
                    let _ = stream.flush();
                    let _ = stream.shutdown(std::net::Shutdown::Both);
                }
            }
        }
    }

    struct MockServer {
        /// `https://127.0.0.1:<ephemeral port>` base URL.
        base_url: String,
        /// Set once any connection has been accepted.
        accepted: Arc<AtomicBool>,
        handle: std::thread::JoinHandle<()>,
    }

    /// Spawns a loopback TCP server that polls for a single connection for
    /// up to `serve_for`, then terminates (so joining never hangs). Loopback
    /// only: no external network is ever touched.
    fn spawn_server(script: ServerScript, serve_for: Duration) -> MockServer {
        let listener = TcpListener::bind(("127.0.0.1", 0)).expect("bind ephemeral loopback port");
        let port = listener.local_addr().expect("resolve local addr").port();
        let accepted = Arc::new(AtomicBool::new(false));
        let flag = Arc::clone(&accepted);
        let handle = std::thread::spawn(move || {
            listener
                .set_nonblocking(true)
                .expect("nonblocking listener");
            let deadline = Instant::now() + serve_for;
            while Instant::now() < deadline {
                match listener.accept() {
                    Ok((stream, _addr)) => {
                        flag.store(true, Ordering::SeqCst);
                        script.run(stream);
                        break;
                    }
                    Err(e) if e.kind() == io::ErrorKind::WouldBlock => {
                        std::thread::sleep(Duration::from_millis(2));
                    }
                    Err(_) => break,
                }
            }
        });
        MockServer {
            base_url: format!("https://127.0.0.1:{port}"),
            accepted,
            handle,
        }
    }

    fn fixture_token() -> Token {
        Token::new(FAKE_TOKEN.to_string()).expect("non-empty fake token")
    }

    #[tokio::test]
    async fn https_gate_rejects_before_any_connection_attempt() {
        // A live listener is up and would accept a plaintext connection,
        // yet an http:// base URL pointing straight at it must be refused
        // with InsecureBaseUrl and zero dials — the scheme gate has to run
        // before any socket activity.
        let token = fixture_token();
        let server = spawn_server(ServerScript::Close, Duration::from_millis(300));
        let http_base = server.base_url.replacen("https://", "http://", 1);

        let err = validate(&token, &http_base)
            .await
            .expect_err("http:// base URL must be rejected");
        assert!(
            matches!(err, AppError::Auth(AuthError::InsecureBaseUrl)),
            "got {err:?}"
        );

        // Grace period for any (buggy) connection attempt to land, then
        // prove none did. If a dial had happened, the OS would have queued
        // it into the listener backlog and the polling thread would have
        // set the flag within a couple of milliseconds.
        std::thread::sleep(Duration::from_millis(150));
        assert!(
            !server.accepted.load(Ordering::SeqCst),
            "validate() must not open a socket for a non-https base URL"
        );
        server.handle.join().expect("server thread must finish");
    }

    #[tokio::test]
    async fn scheme_gate_is_exact_lowercase_https_prefix() {
        // Uppercase scheme, missing/extra slashes, lookalike schemes,
        // scheme-less hosts, empty string: all rejected as InsecureBaseUrl.
        // (Rejecting the RFC-valid uppercase "HTTPS://" is conservative by
        // design — observed behaviour pinned here.)
        let token = fixture_token();
        for bad in [
            "HTTPS://api.todoist.com",
            "https:/api.todoist.com",
            "https:api.todoist.com",
            " https://api.todoist.com",
            "xhttps://api.todoist.com",
            "http://api.todoist.com",
            "ftp://api.todoist.com",
            "gopher://api.todoist.com",
            "//api.todoist.com",
            "api.todoist.com",
            "",
        ] {
            match validate(&token, bad).await {
                Err(AppError::Auth(AuthError::InsecureBaseUrl)) => {}
                other => panic!("base URL {bad:?} should be InsecureBaseUrl, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn https_prefixed_but_malformed_urls_fail_as_request_not_panic() {
        // These pass the cheap starts_with("https://") gate but are not
        // valid URLs. They must surface as AuthError::Request (parse
        // failure) — never a panic, never Ok, and without touching the
        // network (no host in any of them resolves anywhere).
        //
        // NOTE deliberately absent: "https:////host" — the WHATWG URL
        // parser *collapses* extra slashes after the scheme for special
        // schemes, so that input is NOT malformed: it dials `host` over
        // TLS (observed live). That behaviour is pinned separately in
        // `extra_slashes_after_scheme_still_enforce_tls` below.
        let token = fixture_token();
        for bad in [
            "https://",
            "https:///",
            "https://[::1",
            "https:// api.todoist.com",
            "https://api.todoist.com:<not-a-port>",
        ] {
            match validate(&token, bad).await {
                Err(AppError::Auth(AuthError::Request { .. })) => {}
                other => panic!("malformed base {bad:?} should be Request, got {other:?}"),
            }
        }
    }

    #[tokio::test]
    async fn closed_tls_connection_maps_to_request_error() {
        // Server accepts then drops mid-TLS-handshake: a transport-level
        // failure that must map to AuthError::Request, and the client must
        // actually have dialled the mock.
        let token = fixture_token();
        let server = spawn_server(ServerScript::Close, Duration::from_secs(5));

        let err = validate(&token, &server.base_url)
            .await
            .expect_err("connection closed mid-handshake must fail");
        server.handle.join().expect("server thread must finish");

        assert!(
            matches!(err, AppError::Auth(AuthError::Request { .. })),
            "transport failure should map to Request, got {err:?}"
        );
        assert!(
            server.accepted.load(Ordering::SeqCst),
            "client must have dialled the mock server"
        );
    }

    #[tokio::test]
    async fn plaintext_http_response_cannot_fake_a_200_over_https() {
        // Downgrade/MITM scenario: the server answers with a fully valid
        // plaintext "HTTP/1.1 200 OK". Because validate() is pinned to
        // https (scheme gate + https_only client + TLS), those bytes must
        // fail TLS record parsing — the result is a transport error, never
        // Ok(()). A plaintext mock can never impersonate the API.
        let token = fixture_token();
        let server = spawn_server(ServerScript::Plaintext200, Duration::from_secs(5));

        let err = validate(&token, &server.base_url)
            .await
            .expect_err("plaintext response must fail the TLS layer");
        server.handle.join().expect("server thread must finish");

        assert!(
            matches!(err, AppError::Auth(AuthError::Request { .. })),
            "plaintext 200 must be a TLS/transport error, got {err:?}"
        );
        assert!(
            server.accepted.load(Ordering::SeqCst),
            "client must have dialled the mock server"
        );
    }

    #[tokio::test]
    async fn extra_slashes_after_scheme_still_enforce_tls() {
        // Observed WHATWG behaviour (found while testing): extra slashes
        // after the scheme collapse, so "https:////127.0.0.1:P" dials
        // 127.0.0.1:P over TLS. Pin that this laxness can never become a
        // downgrade: even with a slash-collapsed URL the plaintext-200
        // server must still fail the TLS layer instead of validating the
        // token.
        let token = fixture_token();
        let server = spawn_server(ServerScript::Plaintext200, Duration::from_secs(5));
        let collapsed = format!(
            "https:////{}",
            server.base_url.trim_start_matches("https://")
        );

        let err = validate(&token, &collapsed)
            .await
            .expect_err("slash-collapsed https URL must still require TLS");
        server.handle.join().expect("server thread must finish");

        assert!(
            matches!(err, AppError::Auth(AuthError::Request { .. })),
            "slash-collapsed URL must not downgrade to plaintext, got {err:?}"
        );
        assert!(
            server.accepted.load(Ordering::SeqCst),
            "client must have dialled the mock server"
        );
    }

    #[tokio::test]
    async fn request_url_targets_a_live_todoist_endpoint_and_carries_no_token() {
        // BUG-DEMONSTRATING TEST (expected to fail until the endpoint is
        // fixed — see the unit-test report). reqwest error renderings embed
        // the request URL, so this asserts two things at once:
        //
        // 1. SECURITY (passes today): the token is nowhere in the URL — it
        //    travels only in the Authorization header.
        // 2. CORRECTNESS (fails today): the requested path must be a
        //    *live* Todoist endpoint. Observed 2026-10-28 against
        //    https://api.todoist.com: GET /sync/v9/user returns
        //    "410 This endpoint is deprecated" for every caller, valid
        //    token or not, so `validate` can never return Ok against the
        //    real API. The current API surface is /api/v1/ (GET
        //    /api/v1/user returns 401 without auth — exists and is
        //    auth-gated). Until the hardcoded path moves to /api/v1/user,
        //    this assertion keeps failing.
        let token = fixture_token();
        let server = spawn_server(ServerScript::Close, Duration::from_secs(5));

        let err = validate(&token, &server.base_url)
            .await
            .expect_err("transport error expected");
        server.handle.join().expect("server thread must finish");

        let renderings = format!("{err}\n{err:?}\n{err:#?}");
        assert!(
            renderings.contains("127.0.0.1"),
            "mock host missing from error renderings: {renderings}"
        );
        assert!(
            renderings.contains("/api/v1/user"),
            "validate() must target the live /api/v1/user endpoint, but the \
             requested path is the 410-deprecated /sync/v9 one: {renderings}"
        );
        assert!(
            !renderings.contains(FAKE_TOKEN),
            "token leaked into an error rendering: {renderings}"
        );
    }

    #[test]
    fn new_error_variants_have_stable_token_free_display_strings() {
        // CLI-facing contract (T-107 wires user-facing messages): exact
        // Display strings for the three T-106 error paths, wrapped and raw.
        assert_eq!(
            AuthError::InsecureBaseUrl.to_string(),
            "refusing to validate the API token over a non-HTTPS URL"
        );
        assert_eq!(
            AppError::Auth(AuthError::InsecureBaseUrl).to_string(),
            "auth: refusing to validate the API token over a non-HTTPS URL"
        );
        assert_eq!(
            AuthError::UnexpectedStatus { status: 503 }.to_string(),
            "unexpected response from the Todoist API: HTTP 503"
        );
        assert_eq!(
            AppError::Auth(AuthError::UnexpectedStatus { status: 429 }).to_string(),
            "auth: unexpected response from the Todoist API: HTTP 429"
        );
        assert_eq!(
            AuthError::InvalidToken.to_string(),
            "the API token is invalid"
        );

        // No rendering of any new variant may carry token material: the
        // variants only ever hold unit, u16 status, or opaque source data
        // (the AuthError doc contract).
        for err in [
            AppError::Auth(AuthError::InsecureBaseUrl),
            AppError::Auth(AuthError::InvalidToken),
            AppError::Auth(AuthError::UnexpectedStatus { status: 404 }),
        ] {
            for rendered in [format!("{err}"), format!("{err:?}"), format!("{err:#?}")] {
                assert!(
                    !rendered.contains(FAKE_TOKEN),
                    "error rendering leaked token material: {rendered}"
                );
            }
        }
    }

    #[test]
    fn default_base_url_is_exact_todoist_endpoint_no_trailing_slash() {
        // Exact pin: scheme https, official host, no trailing slash — a
        // trailing slash would join into `//api/v1/user` (double slash),
        // so the produced URL is exactly
        // https://api.todoist.com/api/v1/user.
        assert_eq!(DEFAULT_BASE_URL, "https://api.todoist.com");
        assert!(!DEFAULT_BASE_URL.ends_with('/'));
    }

    #[tokio::test]
    async fn mod_reexport_validate_token_shares_the_https_gate() {
        // The re-export from `auth` must behave identically (the name the
        // CLI/T-107 will actually call).
        let token = fixture_token();
        assert!(matches!(
            crate::auth::validate_token(&token, "gopher://api.todoist.com").await,
            Err(AppError::Auth(AuthError::InsecureBaseUrl))
        ));
    }
}
