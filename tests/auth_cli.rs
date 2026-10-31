//! Process-level end-to-end tests for the EPIC-02 auth surface (T-110):
//! `auth` → `today`/`sync`/`add` guard → `logout`, driven through the real
//! binary via `CARGO_BIN_EXE_todoist-tui`.
//!
//! Every test sandboxes `HOME` and the `XDG_*` directories under
//! `CARGO_TARGET_TMPDIR` (same shape as `tests/cli.rs`'s helper — integration
//! test binaries do not share code by default in this crate, so the small
//! helper is duplicated here rather than inventing a second harness), and
//! sets `TODOIST_TUI_FORCE_FILE_TOKEN=1` in every child so the file-backed
//! token source is used deterministically: no real OS keyring entry is read,
//! written or probed, whatever machine the suite runs on.
//!
//! `auth`'s token validation is pointed at a local `wiremock` server through
//! `TODOIST_API_BASE_URL` (a debug-build-only seam, see `src/cli/auth.rs`
//! and `src/auth/validate.rs`). `wiremock` only serves plain HTTP, so this
//! only works because of the companion debug-only loopback exception in
//! `validate::check` — no request ever leaves 127.0.0.1 and the suite passes
//! with no network access.
//!
//! All tokens are fakes (`tok_fake_*`); no real credentials, NID/phone
//! numbers, accounts or prod data appear anywhere in this file.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::io::Write as _;
use std::path::{Path, PathBuf};
use std::process::{Command, Output, Stdio};

use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// Fixture token for every must-succeed path (matches the value T-111's docs
/// use). Fake, never a real credential.
const VALID_TOKEN: &str = "tok_fake_abc123";

/// Fixture token for the API-rejects-this case. Fake.
const REJECTED_TOKEN: &str = "tok_fake_rejected";

/// Exact prompt `auth` writes to stderr (no trailing newline in production).
const PROMPT: &str = "Todoist API token: ";

// ---------------------------------------------------------------------------
// sandbox + child-process helpers (mirrors tests/cli.rs)
// ---------------------------------------------------------------------------

/// A per-test sandbox: a private `HOME` with `cfg`/`state`/`data` subtrees
/// under `CARGO_TARGET_TMPDIR`. Nothing outside it is read or written.
struct Sandbox {
    root: PathBuf,
}

impl Sandbox {
    fn new(name: &str) -> Self {
        let root = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("auth-{name}"));
        // Start from a clean slate: a previous (failed) run must not leak a
        // stored token or a log file into this one.
        match std::fs::remove_dir_all(&root) {
            Ok(()) => {}
            Err(err) if err.kind() == std::io::ErrorKind::NotFound => {}
            Err(err) => panic!("could not reset sandbox {}: {err}", root.display()),
        }
        std::fs::create_dir_all(&root).unwrap();
        Self { root }
    }

    /// `<sandbox>/cfg/todoist-tui/token` — the file-fallback token path.
    fn token_file(&self) -> PathBuf {
        self.root.join("cfg").join("todoist-tui").join("token")
    }

    /// `<sandbox>/state/todoist-tui` — where the rotating log files live.
    fn state_dir(&self) -> PathBuf {
        self.root.join("state").join("todoist-tui")
    }

    /// A child process with every directory redirected into this sandbox and
    /// every inherited environment value that could change behaviour
    /// scrubbed (the same scrubbing `tests/cli.rs` does, plus the two
    /// debug-only seams under test, which are always set explicitly).
    fn command(&self) -> Command {
        let mut cmd = Command::new(env!("CARGO_BIN_EXE_todoist-tui"));
        cmd.env("HOME", &self.root)
            .env("XDG_CONFIG_HOME", self.root.join("cfg"))
            .env("XDG_STATE_HOME", self.root.join("state"))
            .env("XDG_DATA_HOME", self.root.join("data"))
            .env("APPDATA", self.root.join("appdata"))
            .env("LOCALAPPDATA", self.root.join("localappdata"))
            .env("TODOIST_TUI_FORCE_FILE_TOKEN", "1")
            .env_remove("TODOIST_API_BASE_URL")
            .env_remove("RUST_LOG")
            .env_remove("RUST_BACKTRACE")
            .env_remove("RUST_LIB_BACKTRACE");
        cmd
    }

    /// Runs the binary with the given args, optionally piping `stdin_data`
    /// (always the token for `auth`, never a CLI argument) and adding
    /// `extra_env` (the mock server's base URL).
    fn run(&self, args: &[&str], stdin_data: Option<&str>, extra_env: &[(&str, &str)]) -> Output {
        let mut cmd = self.command();
        cmd.args(args)
            .stdin(if stdin_data.is_some() {
                Stdio::piped()
            } else {
                Stdio::null()
            })
            .stdout(Stdio::piped())
            .stderr(Stdio::piped());
        for (key, value) in extra_env {
            cmd.env(key, value);
        }

        let mut child = cmd
            .spawn()
            .unwrap_or_else(|err| panic!("could not spawn the binary: {err}"));
        if let Some(data) = stdin_data {
            let mut stdin = child.stdin.take().expect("stdin was piped");
            // Close the pipe afterwards so the child sees EOF, exactly like
            // `echo tok | todoist-tui auth` in a shell.
            stdin.write_all(data.as_bytes()).expect("write stdin");
            drop(stdin);
        }
        child.wait_with_output().expect("child must not be killed")
    }

    /// Runs `auth` with `token` piped on stdin and validation pointed at
    /// `base_url`.
    fn run_auth(&self, token: &str, base_url: &str) -> Output {
        self.run(
            &["auth"],
            Some(&format!("{token}\n")),
            &[("TODOIST_API_BASE_URL", base_url)],
        )
    }

    /// Runs a guarded command (`today`/`sync`/`add`/`logout`) with no stdin.
    fn run_plain(&self, args: &[&str]) -> Output {
        self.run(args, None, &[])
    }

    /// Contents of every `todoist-tui.log*` file in the sandbox state dir,
    /// as `(path, contents)` pairs.
    fn log_files(&self) -> Vec<(PathBuf, String)> {
        let dir = self.state_dir();
        let Ok(entries) = std::fs::read_dir(&dir) else {
            return Vec::new();
        };
        let mut files: Vec<(PathBuf, String)> = entries
            .map(|entry| entry.expect("state dir entry").path())
            .filter(|path| {
                path.file_name()
                    .and_then(|name| name.to_str())
                    .is_some_and(|name| name.starts_with("todoist-tui.log"))
            })
            .map(|path| {
                let contents = std::fs::read_to_string(&path)
                    .unwrap_or_else(|err| panic!("could not read {}: {err}", path.display()));
                (path, contents)
            })
            .collect();
        files.sort();
        files
    }
}

fn stdout(output: &Output) -> String {
    String::from_utf8_lossy(&output.stdout).into_owned()
}

fn stderr(output: &Output) -> String {
    String::from_utf8_lossy(&output.stderr).into_owned()
}

// ---------------------------------------------------------------------------
// wiremock helper: a local mock API kept alive for the whole test
// ---------------------------------------------------------------------------

/// A `wiremock` server answering `POST /api/v1/sync` with a fixed status,
/// kept alive by a background thread that drives its own multi-threaded
/// Tokio runtime.
///
/// The runtime has to keep being polled for the mock to answer: the tests
/// themselves are ordinary (blocking) `#[test]` functions that spawn a child
/// process, so `block_on`-ing only for setup would leave the server task
/// unpolled and every request would hang until the client's 10 s timeout.
/// The driver thread therefore blocks on a never-ready future, which polls
/// the runtime forever; the mock server handle stays in the test thread so
/// received requests can still be inspected through [`MockApi::requests`].
struct MockApi {
    server: MockServer,
    runtime: tokio::runtime::Handle,
}

impl MockApi {
    fn start(status: u16) -> Self {
        let runtime = tokio::runtime::Builder::new_multi_thread()
            .worker_threads(2)
            .enable_all()
            .build()
            .expect("build a Tokio runtime for the mock server");
        let handle = runtime.handle().clone();
        let server = handle.block_on(async {
            let server = MockServer::start().await;
            Mock::given(method("POST"))
                .and(path("/api/v1/sync"))
                .respond_with(ResponseTemplate::new(status))
                .mount(&server)
                .await;
            server
        });

        // Keep the runtime polled for the rest of the test (leaked on
        // purpose: the process exits when the test binary finishes).
        std::thread::spawn(move || runtime.block_on(std::future::pending::<()>()));

        Self {
            server,
            runtime: handle,
        }
    }

    /// `http://127.0.0.1:<port>` — plain HTTP on loopback, which is exactly
    /// what the debug-only loopback exception in `validate::check` allows.
    fn base_url(&self) -> String {
        self.server.uri()
    }

    /// Every request the child process actually sent to this mock.
    fn requests(&self) -> Vec<wiremock::Request> {
        self.runtime
            .block_on(self.server.received_requests())
            .expect("recorded requests")
    }
}

/// `wiremock` on loopback only: proves the suite never needs the network.
#[test]
fn mock_server_binds_loopback_only() {
    let api = MockApi::start(200);
    let base = api.base_url();
    assert!(
        base.starts_with("http://127.0.0.1:") || base.starts_with("http://localhost:"),
        "the mock must be a plain-HTTP loopback URL, got {base}"
    );
}

// ---------------------------------------------------------------------------
// acceptance 6: the auth guard blocks every guarded command with no token
// ---------------------------------------------------------------------------

#[test]
fn guarded_commands_exit_nonzero_with_not_authenticated_when_no_token_is_stored() {
    for (index, args) in [
        vec!["sync"],
        vec!["add", "x"],
        vec!["today"],
        vec!["today", "--json"],
    ]
    .iter()
    .enumerate()
    {
        // One sandbox per command so no command can see another's writes.
        let sandbox = Sandbox::new(&format!("guard-{index}"));
        let output = sandbox.run_plain(args);
        let err = stderr(&output);

        // T-1001: the auth guard's exit code is now its own dedicated
        // code (3), distinct from the generic failure code (1).
        assert_eq!(
            output.status.code(),
            Some(3),
            "args {args:?}: expected exit 3, stderr: {err}"
        );
        assert!(
            err.contains("not authenticated"),
            "args {args:?}: stderr must say `not authenticated`: {err}"
        );
        assert!(
            err.contains("run `todoist-tui auth`"),
            "args {args:?}: stderr must point at `todoist-tui auth`: {err}"
        );
        assert!(
            !err.contains("not implemented"),
            "args {args:?}: the guard must run before the unimplemented body: {err}"
        );
        assert!(
            !err.contains(PROMPT.trim()),
            "args {args:?}: a guarded command must never prompt: {err}"
        );
        assert!(stdout(&output).is_empty(), "args {args:?}: no stdout");
        assert!(
            !sandbox.token_file().exists(),
            "args {args:?}: no token file may be conjured by a read-only guard"
        );
        // Nothing may reach a log file on the guard path except the app's own
        // startup lines — but never the prompt or a token.
        for (path, contents) in sandbox.log_files() {
            assert!(
                !contents.contains(PROMPT.trim()),
                "{}: log file captured the prompt: {contents}",
                path.display()
            );
        }
    }
}

// ---------------------------------------------------------------------------
// acceptance 2: happy path — auth 200 stores the token, a second process run
// of a guarded command finds it and does not prompt
// ---------------------------------------------------------------------------

#[test]
fn auth_against_accepting_mock_exits_zero_and_a_second_run_finds_the_token() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("happy");

    let auth = sandbox.run_auth(VALID_TOKEN, &api.base_url());
    let auth_err = stderr(&auth);
    assert!(
        auth.status.success(),
        "auth against a 200 mock must exit 0, stderr: {auth_err}"
    );
    assert!(
        stdout(&auth).contains("Token saved."),
        "auth must confirm the save on stdout: {}",
        stdout(&auth)
    );
    assert!(
        !auth_err.contains("invalid token"),
        "a 200 must not be reported as an invalid token: {auth_err}"
    );
    assert!(
        sandbox.token_file().exists(),
        "the fallback token file must exist after a successful auth: {}",
        sandbox.token_file().display()
    );

    // The mock must have been the thing that answered, carrying the token.
    let requests = api.requests();
    assert_eq!(requests.len(), 1, "exactly one validation request");
    assert_eq!(
        requests[0]
            .headers
            .get("Authorization")
            .expect("an Authorization header must be sent")
            .to_str()
            .expect("ascii header"),
        format!("Bearer {VALID_TOKEN}"),
        "the piped token must be the bearer credential"
    );

    // Second process: the guard must find the stored token in the file
    // fallback and never prompt. `today` is still a stub (T-109 keeps it
    // NotImplemented), so success here means "the guard passed", proven by
    // the guard's own error being absent.
    let today = sandbox.run_plain(&["today"]);
    let today_err = stderr(&today);
    assert!(
        !today_err.contains("not authenticated"),
        "the stored token must satisfy the guard in a fresh process: {today_err}"
    );
    assert!(
        !today_err.contains(PROMPT.trim()),
        "a guarded command must never re-prompt for the token: {today_err}"
    );
    assert!(
        today_err.contains("not implemented: today"),
        "with a token stored, `today` reaches its stub body: {today_err}"
    );
    assert_eq!(
        today.status.code(),
        Some(1),
        "the stub body still exits 1: {today_err}"
    );
    assert!(stdout(&today).is_empty(), "the stub writes no stdout");
}

// ---------------------------------------------------------------------------
// acceptance 3: a 401 mock rejects the token and nothing is stored
// ---------------------------------------------------------------------------

#[test]
fn auth_against_401_mock_exits_nonzero_names_invalid_token_and_stores_nothing() {
    let api = MockApi::start(401);
    let sandbox = Sandbox::new("rejected");

    let auth = sandbox.run_auth(REJECTED_TOKEN, &api.base_url());
    let err = stderr(&auth);

    assert!(!auth.status.success(), "a 401 mock must exit non-zero");
    assert!(
        err.contains("invalid token"),
        "stderr must contain `invalid token`: {err}"
    );
    assert!(
        !stdout(&auth).contains("Token saved."),
        "a rejected token must never be confirmed: {}",
        stdout(&auth)
    );
    assert!(
        !sandbox.token_file().exists(),
        "no token file may exist after a rejected token: {}",
        sandbox.token_file().display()
    );
    // The rejection message must not echo the rejected credential.
    assert!(
        !err.contains(REJECTED_TOKEN),
        "stderr leaked the rejected token: {err}"
    );

    // Idempotent retry: a second identical attempt also fails and still
    // stores nothing.
    let retry = sandbox.run_auth(REJECTED_TOKEN, &api.base_url());
    assert!(!retry.status.success(), "the retry must fail too");
    assert!(
        stderr(&retry).contains("invalid token"),
        "{}",
        stderr(&retry)
    );
    assert!(
        !sandbox.token_file().exists(),
        "a retried rejection must still store nothing"
    );
}

// ---------------------------------------------------------------------------
// acceptance 4: fallback path — file written 0600, notice names the path
// ---------------------------------------------------------------------------

#[test]
fn fallback_auth_writes_a_0600_token_file_and_the_notice_names_its_path() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("fallback");
    let token_file = sandbox.token_file();

    let auth = sandbox.run_auth(VALID_TOKEN, &api.base_url());
    let err = stderr(&auth);
    assert!(auth.status.success(), "auth must succeed: {err}");

    assert!(
        err.contains("OS keyring unavailable"),
        "stderr must carry the fallback notice: {err}"
    );
    assert!(
        err.contains(&token_file.display().to_string()),
        "the fallback notice must name the token file path {}: {err}",
        token_file.display()
    );
    assert!(
        err.contains("0600"),
        "the fallback notice must name the 0600 mode: {err}"
    );

    assert!(token_file.exists(), "the fallback token file must exist");
    let stored = std::fs::read_to_string(&token_file).expect("token file is readable");
    assert_eq!(
        stored, VALID_TOKEN,
        "the file must hold exactly the validated token (no newline, no reformatting)"
    );

    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt as _;
        let mode = std::fs::metadata(&token_file)
            .expect("token file metadata")
            .permissions()
            .mode();
        assert_eq!(
            mode & 0o777,
            0o600,
            "the token file must be owner-only, mode was {:o}",
            mode & 0o777
        );
    }

    // The token must not be echoed on either stream.
    assert!(
        !stdout(&auth).contains(VALID_TOKEN) && !err.contains(VALID_TOKEN),
        "auth leaked the token into its output"
    );
}

// ---------------------------------------------------------------------------
// acceptance 5: logout clears the file, blocks the guard again, is idempotent
// ---------------------------------------------------------------------------

#[test]
fn logout_removes_the_token_file_blocks_the_guard_and_is_idempotent() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("logout");

    // Log in first, so there is something to clear.
    let auth = sandbox.run_auth(VALID_TOKEN, &api.base_url());
    assert!(
        auth.status.success(),
        "auth must succeed before logout can be exercised: {}",
        stderr(&auth)
    );
    assert!(
        sandbox.token_file().exists(),
        "precondition: the token file must exist after auth"
    );

    let logout = sandbox.run_plain(&["logout"]);
    let logout_err = stderr(&logout);
    assert!(
        logout.status.success(),
        "logout after a successful auth must exit 0, stderr: {logout_err}"
    );
    assert!(
        stdout(&logout).contains("Cleared stored API token."),
        "logout must confirm the clear: {}",
        stdout(&logout)
    );
    assert!(
        !sandbox.token_file().exists(),
        "the fallback token file must be gone after logout: {}",
        sandbox.token_file().display()
    );

    // The guard must block again in a fresh process.
    let today = sandbox.run_plain(&["today"]);
    let today_err = stderr(&today);
    assert!(!today.status.success(), "the guard must fail after logout");
    assert!(
        today_err.contains("not authenticated"),
        "stderr must say `not authenticated` after logout: {today_err}"
    );

    // Idempotent: a second consecutive logout is still a success.
    let second = sandbox.run_plain(&["logout"]);
    assert!(
        second.status.success(),
        "a second consecutive logout must also exit 0, stderr: {}",
        stderr(&second)
    );
    assert!(
        stdout(&second).contains("Cleared stored API token."),
        "the second logout must still report the (idempotent) clear: {}",
        stdout(&second)
    );
}

// ---------------------------------------------------------------------------
// external service down: no reachable mock, auth fails cleanly
// ---------------------------------------------------------------------------

#[test]
fn auth_against_an_unreachable_validation_endpoint_fails_cleanly() {
    // Reserve a free loopback port, then close it: nothing listens there, so
    // the child gets "connection refused" (a transport failure, not a
    // rejected token). Dropping a wiremock `MockServer` is not usable for
    // this: its listener task lives on the leaked runtime and keeps serving.
    let listener = std::net::TcpListener::bind("127.0.0.1:0").expect("reserve a loopback port");
    let port = listener.local_addr().expect("local addr").port();
    drop(listener);
    let base_url = format!("http://127.0.0.1:{port}");

    let sandbox = Sandbox::new("service-down");
    let auth = sandbox.run_auth(VALID_TOKEN, &base_url);
    let err = stderr(&auth);

    assert!(!auth.status.success(), "an unreachable API must fail");
    assert!(
        err.contains("request to the Todoist API failed"),
        "stderr must report the transport failure: {err}"
    );
    assert!(
        !err.contains("invalid token"),
        "a network failure must not be reported as an invalid token: {err}"
    );
    assert!(
        !sandbox.token_file().exists(),
        "nothing may be stored when validation could not run"
    );
}

// ---------------------------------------------------------------------------
// acceptance 7: no fixture token ever reaches a log file
// ---------------------------------------------------------------------------

/// Log-leak scan over the real rotating log files after a full
/// `auth` → `logout` sequence.
///
/// **Acceptance 7 relaxation (human decision, T-110):** the original wording
/// also required at least one `***` redaction marker in the logs. No current
/// log line formats a redacted `Secret`, so that clause is unsatisfiable
/// without inventing a log line in `src/` (out of this task's scope) and is
/// dropped here on purpose. What remains — and what actually matters for the
/// leak contract — is asserted: zero occurrences of the fixture token across
/// every `todoist-tui.log*` file, while the scan itself is proven meaningful
/// by requiring the log files to exist and carry the auth flow's own
/// (token-free) events.
#[test]
fn no_fixture_token_reaches_any_log_file_across_auth_and_logout() {
    let api = MockApi::start(200);
    let sandbox = Sandbox::new("log-leak");

    let auth = sandbox.run_auth(VALID_TOKEN, &api.base_url());
    assert!(
        auth.status.success(),
        "auth must succeed: {}",
        stderr(&auth)
    );
    let logout = sandbox.run_plain(&["logout"]);
    assert!(
        logout.status.success(),
        "logout must succeed: {}",
        stderr(&logout)
    );

    let logs = sandbox.log_files();
    assert!(
        !logs.is_empty(),
        "expected at least one todoist-tui.log* file under {}",
        sandbox.state_dir().display()
    );

    let mut all_contents = String::new();
    for (path, contents) in &logs {
        assert!(
            !contents.contains(VALID_TOKEN),
            "{}: the fixture token leaked into a log file:\n{contents}",
            path.display()
        );
        assert!(
            !contents.contains(REJECTED_TOKEN),
            "{}: the rejected fixture token leaked into a log file:\n{contents}",
            path.display()
        );
        all_contents.push_str(contents);
    }
    // The scan must be scanning real output, not empty files: the auth and
    // logout flows both emit token-free `info!` events.
    assert!(
        all_contents.contains("auth command finished"),
        "expected the auth flow's own log event to be present: {all_contents}"
    );
    assert!(
        all_contents.contains("logout command finished"),
        "expected the logout flow's own log event to be present: {all_contents}"
    );
    assert!(
        !auth_status_contains_token(&auth) && !auth_status_contains_token(&logout),
        "no stream may have carried the token either"
    );
}

/// True when either captured stream of `output` echoes the fixture token.
fn auth_status_contains_token(output: &Output) -> bool {
    stdout(output).contains(VALID_TOKEN) || stderr(output).contains(VALID_TOKEN)
}

/// A missing token file is the "not authenticated" state, not corruption:
/// asserted directly so a sandbox-layout regression (wrong XDG path) fails
/// here rather than silently turning every guard test into a pass.
#[test]
fn sandbox_paths_match_the_documented_layout() {
    let sandbox = Sandbox::new("paths");
    assert_eq!(
        sandbox.token_file(),
        Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("auth-paths")
            .join("cfg")
            .join("todoist-tui")
            .join("token"),
        "the token file must live at <XDG_CONFIG_HOME>/todoist-tui/token"
    );
    assert_eq!(
        sandbox.state_dir(),
        Path::new(env!("CARGO_TARGET_TMPDIR"))
            .join("auth-paths")
            .join("state")
            .join("todoist-tui"),
        "logs must live at <XDG_STATE_HOME>/todoist-tui"
    );
    assert!(!sandbox.token_file().exists());
}
