//! Token validation against the real Todoist API (T-106, EPIC-02).
//!
//! [`check`] calls a lightweight authenticated Todoist Sync API endpoint
//! (`POST /api/v1/sync` with `sync_token="*"` and `resource_types=["user"]`,
//! which is the cheapest authenticated sync call that still requires a
//! valid token) over HTTPS-only `reqwest` (rustls, see `Cargo.toml` — no
//! native-tls/OpenSSL, REQ-001) and maps the response to
//! [`crate::error::AuthError`]: a 2xx response means the token is accepted,
//! 401/403 means it was rejected, and anything else (network failure,
//! unexpected status) is surfaced as a distinct error so callers can tell
//! "your token is wrong" apart from "we could not reach Todoist".
//!
//! This module is intentionally minimal: a single request, no retries, no
//! rate-limit handling, no parsing of the sync response body, no domain
//! model. **EPIC-03 owns the real typed Sync API client** (with retries,
//! backoff, and full resource parsing) and supersedes this validator for
//! anything beyond "is this token accepted".
//!
//! The token is only ever read through [`Token::expose`] when building the
//! `Authorization` header; it is never formatted into a log line, an error
//! message, or the request URL (query strings/paths end up in error/debug
//! output far more easily than headers).
//!
//! The base URL is an injectable parameter (not a constant) so tests can
//! point at a local mock server instead of the real Todoist API — this
//! module's own `tests`/`unit_tests` modules make no real network calls,
//! they only exercise the pure HTTPS-enforcement / URL-building logic and
//! transport-level failure modes that a mock HTTP server cannot simulate
//! (TLS downgrade attempts); the `wiremock_tests` module below covers the
//! HTTP status-mapping contract with a local mock server.

use reqwest::{Client, StatusCode};

use crate::auth::Token;
use crate::error::{AuthError, Result};

/// Default Todoist API base URL used by [`check`].
///
/// `POST {base}/api/v1/sync` is the cheapest authenticated Sync API call:
/// requesting only the `user` resource with no incremental state (`"*"`)
/// returns a small payload for a valid token and 401/403 for an invalid
/// one. (The older `/sync/v9/*` and `/rest/v2/*` endpoints are deprecated
/// and return HTTP 410 Gone for every request, valid token or not.)
pub const DEFAULT_BASE_URL: &str = "https://api.todoist.com";

/// Request timeout applied to the single validation request (T-106 MAJOR
/// finding: named instead of a bare literal so the budget is documented and
/// reusable).
const REQUEST_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

/// Validates `token` against the Todoist API reachable at `base_url`.
///
/// `base_url` must use the `https://` scheme; any other scheme (including
/// plain `http://`) is rejected before any network activity happens, so a
/// misconfigured or attacker-controlled base URL can never cause the token
/// to be sent in the clear. Tests inject a local mock server's URL here
/// instead of [`DEFAULT_BASE_URL`].
///
/// **Debug-only loopback exception (scope widened for T-110, human-approved):**
/// in a `debug_assertions` build only, `http://127.0.0.1`, `http://localhost`
/// and `http://[::1]` are also accepted (see [`is_acceptable_base_url_scheme`]
/// below) so the T-110 end-to-end test harness can point this function at a
/// local `wiremock` server without TLS. This can never be reached in a
/// release binary (the check is behind `cfg(not(debug_assertions))` in that
/// profile, see [`is_acceptable_base_url_scheme`]), so it does not widen the
/// production attack surface described above or in
/// [`AuthError::InsecureBaseUrl`]'s own docs: a shipped binary still rejects
/// every non-`https://` base URL with no exceptions whatsoever.
///
/// # Errors
///
/// - [`AuthError::InsecureBaseUrl`] if `base_url`'s scheme is not accepted
///   (see [`is_acceptable_base_url_scheme`] for exactly which schemes that
///   is, per build profile).
/// - [`AuthError::Request`] if the HTTPS client could not be built, or the
///   request failed at the transport level (DNS, TLS, timeout, connection
///   refused, ...).
/// - [`AuthError::InvalidToken`] if the API reported the token as
///   unauthorized (HTTP 401 or 403).
/// - [`AuthError::UnexpectedStatus`] if the API returned any other
///   non-success status.
pub async fn check(token: &Token, base_url: &str) -> Result<()> {
    if !is_acceptable_base_url_scheme(base_url) {
        return Err(AuthError::InsecureBaseUrl.into());
    }

    let client = Client::builder()
        .https_only(https_only_for(base_url))
        .timeout(REQUEST_TIMEOUT)
        .build()
        .map_err(|source| AuthError::Request { source })?;

    send_and_map(&client, token, base_url).await
}

/// Returns whether the `reqwest` client built for `base_url` should enforce
/// `https_only` in a release build: always `true`, no exceptions — exactly
/// as strict as [`is_acceptable_base_url_scheme`] itself, byte-identical to
/// pre-T-110 behaviour.
#[cfg(not(debug_assertions))]
fn https_only_for(_base_url: &str) -> bool {
    true
}

/// Debug-build-only variant of [`https_only_for`] (fixes a T-110 seam
/// defect found by the integration tester): [`is_acceptable_base_url_scheme`]
/// already accepts plain `http://` for the loopback exceptions below, but
/// unconditionally building the client with `.https_only(true)` meant
/// `reqwest` itself rejected the very request the scheme gate had just
/// approved — the loopback exception could never actually reach a server.
/// This mirrors the gate's own cfg-split: only relax `https_only` when
/// `base_url` was accepted via the `http://` branch specifically (not
/// `https://`), so an `https://` base URL in a debug build still gets the
/// same strict client as release.
#[cfg(debug_assertions)]
fn https_only_for(base_url: &str) -> bool {
    !(base_url.starts_with("http://127.0.0.1")
        || base_url.starts_with("http://localhost")
        || base_url.starts_with("http://[::1]"))
}

/// Returns whether `base_url`'s scheme is acceptable to [`check`] in a
/// release build: `https://` only, no exceptions whatsoever. This is the
/// production security boundary described in [`AuthError::InsecureBaseUrl`]'s
/// docs and is byte-identical to the pre-T-110 behaviour.
#[cfg(not(debug_assertions))]
fn is_acceptable_base_url_scheme(base_url: &str) -> bool {
    base_url.starts_with("https://")
}

/// Debug-build-only variant of [`is_acceptable_base_url_scheme`] (scope
/// widened for T-110, human-approved): in addition to `https://`, also
/// accepts plain `http://` when the host is a loopback address —
/// `http://127.0.0.1`, `http://localhost` or `http://[::1]` — so the T-110
/// end-to-end test harness can validate against a local `wiremock` server
/// with no TLS. This function only exists in `debug_assertions` builds (see
/// the `cfg(not(debug_assertions))` twin above), so it can never be compiled
/// into, let alone reached in, a release binary: the loopback relaxation
/// does not widen the production attack surface at all.
#[cfg(debug_assertions)]
fn is_acceptable_base_url_scheme(base_url: &str) -> bool {
    base_url.starts_with("https://")
        || base_url.starts_with("http://127.0.0.1")
        || base_url.starts_with("http://localhost")
        || base_url.starts_with("http://[::1]")
}

/// Sends the single sync-API validation request and maps its outcome to
/// [`Result<()>`], without enforcing the `https://` scheme gate or building
/// the client — that is [`check`]'s job.
///
/// Split out purely so the `wiremock_tests` module below can exercise the
/// HTTP status-mapping contract (2xx/401/403/other) against a real local
/// mock server: `wiremock` has no TLS support (it only ever serves plain
/// `http://`), so a test going through the public, https-only [`check`]
/// would always fail at the scheme gate before reaching this logic. The
/// scheme gate itself is unit-tested separately (see `tests` and
/// `unit_tests` above) and is completely unaffected by this split — `check`
/// still runs it unconditionally before any client is built or request
/// sent.
async fn send_and_map(client: &Client, token: &Token, base_url: &str) -> Result<()> {
    let url = format!("{base_url}/api/v1/sync");

    let response = client
        .post(url)
        .bearer_auth(token.expose())
        .form(&[("sync_token", "*"), ("resource_types", r#"["user"]"#)])
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

/// Blocking wrapper around [`check`] for sync call sites (the T-107 CLI
/// path) that do not already run inside a Tokio runtime.
///
/// Builds a dedicated current-thread runtime for the single request and
/// tears it down afterwards; it does not reuse or require an ambient
/// runtime.
///
/// # Errors
///
/// - [`AuthError::Runtime`] if the current-thread Tokio runtime could not
///   be built.
/// - Any error [`check`] can return (see its docs).
pub fn check_blocking(token: &Token, base_url: &str) -> Result<()> {
    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .map_err(|source| AuthError::Runtime { source })?;
    runtime.block_on(check(token, base_url))
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
        let err = check(&token, "http://api.todoist.com")
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
            let err = check(&token, bad)
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
/// HTTP status mapping (2xx → Ok, 401/403 → `InvalidToken`, other →
/// `UnexpectedStatus`) is covered separately in the `wiremock_tests` module
/// below, which runs a local plaintext HTTP mock server via the `wiremock`
/// dev-dependency rather than the raw-TCP/TLS fixtures used here.
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
        //
        // `http://127.0.0.1` itself is a debug-build-only loopback
        // exception added for T-110 (see `is_acceptable_base_url_scheme`),
        // so this test pins the always-on, non-loopback scheme gate against
        // a host the T-110 exception never covers. The gate must reject
        // (and the mock listener must never see a connection) before
        // `check()` ever tries to resolve or dial the host, so a
        // non-resolvable placeholder host is fine here — the port still
        // names the live listener purely for readability.
        let token = fixture_token();
        let server = spawn_server(ServerScript::Close, Duration::from_millis(300));
        let port = server
            .base_url
            .rsplit(':')
            .next()
            .expect("base_url has a port");
        let http_base = format!("http://not-a-loopback-host.invalid:{port}");

        let err = check(&token, &http_base)
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
            "check() must not open a socket for a non-https base URL"
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
            match check(&token, bad).await {
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
            match check(&token, bad).await {
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

        let err = check(&token, &server.base_url)
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
        // plaintext "HTTP/1.1 200 OK". Because check() is pinned to
        // https (scheme gate + https_only client + TLS), those bytes must
        // fail TLS record parsing — the result is a transport error, never
        // Ok(()). A plaintext mock can never impersonate the API.
        let token = fixture_token();
        let server = spawn_server(ServerScript::Plaintext200, Duration::from_secs(5));

        let err = check(&token, &server.base_url)
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

        let err = check(&token, &collapsed)
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
    async fn request_url_targets_the_live_sync_endpoint_and_carries_no_token() {
        // reqwest error renderings embed the request URL, so this asserts
        // two things at once:
        //
        // 1. SECURITY: the token is nowhere in the URL — it travels only in
        //    the Authorization header.
        // 2. CORRECTNESS: the requested path is the live, auth-gated
        //    `/api/v1/sync` endpoint. Verified against the real API
        //    (`curl -X POST https://api.todoist.com/api/v1/sync`): no auth
        //    returns 403, an invalid `Authorization: Bearer` token returns
        //    401 with a JSON `AUTH_INVALID_TOKEN` body — the older
        //    `/sync/v9/*` and `/rest/v2/*` surfaces return 410 Gone instead.
        let token = fixture_token();
        let server = spawn_server(ServerScript::Close, Duration::from_secs(5));

        let err = check(&token, &server.base_url)
            .await
            .expect_err("transport error expected");
        server.handle.join().expect("server thread must finish");

        let renderings = format!("{err}\n{err:?}\n{err:#?}");
        assert!(
            renderings.contains("127.0.0.1"),
            "mock host missing from error renderings: {renderings}"
        );
        assert!(
            renderings.contains("/api/v1/sync"),
            "check() must target the live /api/v1/sync endpoint: {renderings}"
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
        // trailing slash would join into `//api/v1/sync` (double slash),
        // so the produced URL is exactly
        // https://api.todoist.com/api/v1/sync.
        assert_eq!(DEFAULT_BASE_URL, "https://api.todoist.com");
        assert!(!DEFAULT_BASE_URL.ends_with('/'));
    }

    #[tokio::test]
    async fn mod_reexport_check_shares_the_https_gate() {
        // The re-export from `auth` must behave identically (the name the
        // CLI/T-107 will actually call).
        let token = fixture_token();
        assert!(matches!(
            crate::auth::check(&token, "gopher://api.todoist.com").await,
            Err(AppError::Auth(AuthError::InsecureBaseUrl))
        ));
    }
}

/// HTTP status-mapping coverage (T-106) using `wiremock` for a real local
/// HTTP mock server, complementing the TLS/transport-level fixtures in
/// `unit_tests` above. Minimal sanity coverage only; broader adversarial
/// cases are left to the unit-tester.
///
/// These tests call the private [`send_and_map`] helper directly against
/// `mock_server.uri()` rather than the public, https-only [`check`]:
/// `wiremock` has no TLS support (it only serves plain `http://`), so
/// `check`'s scheme gate would reject every wiremock URL before the request
/// was ever sent, making the HTTP status-mapping contract impossible to
/// exercise through the public entry point. This is a `wiremock` tooling
/// limitation, not a gap in the real security gate — that gate has its own
/// dedicated, always-passing coverage in `tests`/`unit_tests` above, and
/// `check` itself is unchanged: it still runs the gate unconditionally
/// before delegating to `send_and_map`.
#[cfg(test)]
mod wiremock_tests {
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    use super::*;
    use crate::error::AppError;

    /// Fake, never-real token fixture used for the Authorization-header
    /// assertion (per the acceptance wording, the literal `tok_fake_abc123`
    /// value).
    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    fn fixture_token() -> Token {
        Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty fake token")
    }

    #[tokio::test]
    async fn success_status_maps_to_ok() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let token = fixture_token();
        let result = send_and_map(&Client::new(), &token, &server.uri()).await;
        assert!(result.is_ok(), "expected Ok, got {result:?}");
    }

    #[tokio::test]
    async fn unauthorized_status_maps_to_invalid_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(401))
            .mount(&server)
            .await;

        let token = fixture_token();
        let err = send_and_map(&Client::new(), &token, &server.uri())
            .await
            .expect_err("401 must be rejected");
        assert!(
            matches!(err, AppError::Auth(AuthError::InvalidToken)),
            "got {err:?}"
        );
    }

    #[tokio::test]
    async fn server_error_status_does_not_map_to_invalid_token() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let token = fixture_token();
        let err = send_and_map(&Client::new(), &token, &server.uri())
            .await
            .expect_err("500 must be an error");
        assert!(
            !matches!(err, AppError::Auth(AuthError::InvalidToken)),
            "a 5xx backend failure must not be reported as an invalid token, got {err:?}"
        );
        assert!(
            matches!(
                err,
                AppError::Auth(AuthError::UnexpectedStatus { status: 500 })
            ),
            "expected UnexpectedStatus{{status: 500}}, got {err:?}"
        );
    }

    #[tokio::test]
    async fn request_carries_exactly_one_bearer_authorization_header() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .and(header(
                "Authorization",
                format!("Bearer {FIXTURE_TOKEN}").as_str(),
            ))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let token = fixture_token();
        send_and_map(&Client::new(), &token, &server.uri())
            .await
            .expect("request matching the expected Authorization header must succeed");

        let requests = server.received_requests().await.expect("requests recorded");
        assert_eq!(requests.len(), 1, "exactly one request must be sent");
        let auth_headers: Vec<_> = requests[0]
            .headers
            .get_all("Authorization")
            .iter()
            .collect();
        assert_eq!(
            auth_headers.len(),
            1,
            "exactly one Authorization header must be sent, got {auth_headers:?}"
        );
        assert_eq!(auth_headers[0], format!("Bearer {FIXTURE_TOKEN}").as_str());
    }

    #[tokio::test]
    async fn request_carries_sync_token_and_resource_types_form_fields() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(200))
            .mount(&server)
            .await;

        let token = fixture_token();
        send_and_map(&Client::new(), &token, &server.uri())
            .await
            .expect("mocked 200 must succeed");

        let requests = server.received_requests().await.expect("requests recorded");
        assert_eq!(requests.len(), 1);
        let body = String::from_utf8(requests[0].body.clone()).expect("utf8 form body");
        assert!(
            body.contains("sync_token=*") || body.contains("sync_token=%2A"),
            "form body must carry sync_token=*: {body}"
        );
        assert!(
            body.contains("resource_types"),
            "form body must carry resource_types: {body}"
        );
        assert!(
            body.contains("user"),
            "form body must request the user resource: {body}"
        );
    }

    #[tokio::test]
    async fn no_token_material_leaks_into_error_debug_or_display() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(ResponseTemplate::new(500))
            .mount(&server)
            .await;

        let token = fixture_token();
        let err = send_and_map(&Client::new(), &token, &server.uri())
            .await
            .expect_err("500 must be an error");

        let renderings = format!("{err}\n{err:?}\n{err:#?}");
        assert!(
            !renderings.contains(FIXTURE_TOKEN),
            "token leaked into an error rendering: {renderings}"
        );
    }
}
