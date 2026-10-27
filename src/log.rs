//! Logging subsystem (T-006, EPIC-01).
//!
//! Installs a process-wide `tracing` subscriber that writes events to a
//! daily-rotating file under the platform state directory (T-004), with
//! redaction helpers for secrets (see [`Secret`]). This module is the single
//! place where the process decides where log output goes: T-007 wires it to
//! the CLI (`--verbose`), EPIC-02 wraps the Todoist API token in [`Secret`],
//! and EPIC-12 scans these logs for leaked secrets.
//!
//! # Output contract
//!
//! * **TUI mode (default):** events go only to the log file — nothing is
//!   written to stdout or stderr by the subscriber, so terminal drawing is
//!   never corrupted. [`LogOptions::with_stderr`] opts into an additional
//!   plain stderr layer for non-TUI / `--verbose` runs.
//! * **Rotation:** log files are named `todoist-tui.log.YYYY-MM-DD` inside
//!   the state directory (glob `todoist-tui.log*`); a new file starts every
//!   day. ANSI styling is disabled on every layer so lines stay greppable
//!   for EPIC-12's automated log scan.
//! * **Filtering:** the `RUST_LOG` environment variable drives a
//!   `tracing_subscriber::EnvFilter`; when `RUST_LOG` is unset *or
//!   malformed*, the documented default `info` applies.
//!
//! # Init contract
//!
//! [`init`] installs a *global default* subscriber, so it succeeds at most
//! once per process; a second call returns [`LogError::OpenFile`] whose
//! source explains that a global subscriber already exists. [`init`]
//! returns a [`WorkerGuard`] the caller MUST keep alive: dropping it flushes
//! buffered records and stops the writer thread (`WorkerGuard` is itself
//! `#[must_use]`).
//!
//! # Testing
//!
//! [`init`] reads its location at call time, so redirect output either by
//! passing a directory with [`LogOptions::with_state_dir`] or by setting
//! `XDG_STATE_HOME` first (resolved per call, T-004). Because the
//! subscriber is process-global, tests must install it once and share it:
//!
//! 1. Pick a fresh, not-yet-existing tempdir path `D` — `init` creates the
//!    directory (covering the "state dir is created when missing"
//!    assertion) and the appender eagerly creates the first dated file, so
//!    directory/file existence assertions are deterministic.
//! 2. Call `init(LogOptions::new().with_state_dir(D))` exactly once per test
//!    process (e.g. behind `std::sync::OnceLock`) and leak the returned
//!    guard (`std::mem::forget`) so the writer thread outlives all tests.
//! 3. To assert on file *content* (e.g. the redaction marker), emit the
//!    event, then poll the log file with a bounded retry loop: the worker
//!    writes each line to the file immediately upon receipt, but guard drop
//!    is the only flush hook, so give the worker thread a scheduling
//!    quantum before reading.
//!
//! If a test redirects via `XDG_STATE_HOME` instead of `with_state_dir`,
//! serialize env access exactly like the `config::paths` tests do (env is
//! process-global).

// Consumers land in T-007 (CLI wiring: `log::init` + `--verbose`) and
// EPIC-02 (`Secret` around the API token); `pub` alone does not silence
// `dead_code` in a binary crate, so allow it at module scope — same pattern
// as `error.rs` (T-003) and `config/paths.rs` (T-004).
#![allow(dead_code)]

use std::fmt;
use std::io;
use std::path::PathBuf;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_appender::rolling::{RollingFileAppender, Rotation};
use tracing_subscriber::layer::SubscriberExt;
use tracing_subscriber::util::SubscriberInitExt;
use tracing_subscriber::EnvFilter;

use crate::config::paths;
use crate::error::{LogError, Result};

/// Log file name prefix; the daily rotation appends `.YYYY-MM-DD`, so
/// produced files are `todoist-tui.log.2025-01-31` and friends. Keep in
/// sync with `LOG_FILE_NAME` in `config::paths` (T-004).
const LOG_FILE_PREFIX: &str = "todoist-tui.log";

/// Documented default for `RUST_LOG` when the variable is unset or holds a
/// malformed filter directive.
const DEFAULT_FILTER: &str = "info";

/// Marker rendered by [`Secret`]'s `Display` and `Debug`. EPIC-12's
/// automated log scan greps for this constant.
pub const REDACTED: &str = "***";

/// Options controlling [`init`].
///
/// Construct with [`LogOptions::new`] (or `Default`), then override:
/// `LogOptions::new()` alone gives TUI mode — the platform state dir, file
/// output only. `.with_stderr(true)` adds the stderr layer for non-TUI /
/// `--verbose` runs; `.with_state_dir(dir)` redirects the log directory
/// (test seam, also useful for portable runs).
#[derive(Debug, Clone, Default)]
pub struct LogOptions {
    /// Explicit log directory; `None` resolves via `config::paths::state_dir`.
    state_dir: Option<PathBuf>,
    /// When `true`, additionally write formatted events to stderr.
    stderr: bool,
}

impl LogOptions {
    /// TUI-mode defaults: the platform state dir, file output only.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Overrides the log directory; created recursively when missing. When
    /// unset, [`config::paths::state_dir`] resolves it (`XDG_STATE_HOME`
    /// first, platform default otherwise).
    #[must_use]
    pub fn with_state_dir(mut self, dir: impl Into<PathBuf>) -> Self {
        self.state_dir = Some(dir.into());
        self
    }

    /// When `true`, adds a plain (no ANSI) formatted stderr layer on top of
    /// the file layer, for non-TUI / `--verbose` runs. Default `false`: the
    /// subscriber stays silent on stdout/stderr so TUI drawing is not
    /// corrupted.
    #[must_use]
    pub fn with_stderr(mut self, on: bool) -> Self {
        self.stderr = on;
        self
    }
}

/// Installs a global `tracing` subscriber writing to a daily-rotating file
/// appender under `config::paths::state_dir()` (or
/// [`LogOptions::with_state_dir`]) and returns the guard that keeps the
/// non-blocking writer alive.
///
/// Behavior at a glance:
///
/// * The state directory is created recursively when missing.
/// * The rotating appender eagerly creates the first dated file
///   (`<dir>/todoist-tui.log.YYYY-MM-DD`) before this function returns.
/// * Filtering comes from `RUST_LOG` via [`EnvFilter`], defaulting to
///   `info` when `RUST_LOG` is unset or malformed.
/// * In the default (TUI) mode nothing is written to stdout/stderr; with
///   [`LogOptions::with_stderr`] a plain stderr layer is added.
/// * **Call at most once per process** and keep the returned
///   [`WorkerGuard`] alive for as long as records should be written;
///   dropping it flushes buffered records and stops the writer thread.
///
/// # Errors
///
/// Returns the crate-wide [`Result`]: `AppError::Config` when the state
/// directory cannot be resolved, `AppError::Log(LogError::CreateDir)` when
/// the directory cannot be created, and `AppError::Log(LogError::OpenFile)`
/// when the appender cannot be constructed or a global tracing subscriber
/// is already installed (see the module-level init contract).
pub fn init(opts: LogOptions) -> Result<WorkerGuard> {
    let dir = match opts.state_dir {
        Some(dir) => dir,
        None => paths::state_dir()?,
    };

    // Fail fast with a clear message instead of tracing-subscriber's more
    // opaque `TryInitError`; the `try_init` below still backstops the
    // parallel-init race (two threads passing this check simultaneously).
    if tracing::dispatcher::has_been_set() {
        return Err(LogError::OpenFile {
            path: dir.join(LOG_FILE_PREFIX),
            source: io::Error::other(
                "a global tracing subscriber is already installed; \
                 `log::init` must be called at most once per process",
            ),
        }
        .into());
    }

    std::fs::create_dir_all(&dir).map_err(|source| LogError::CreateDir {
        path: dir.clone(),
        source,
    })?;

    let appender = RollingFileAppender::builder()
        .rotation(Rotation::DAILY)
        .filename_prefix(LOG_FILE_PREFIX)
        .build(&dir)
        .map_err(|source| LogError::OpenFile {
            path: dir.join(LOG_FILE_PREFIX),
            source: io::Error::other(source),
        })?;
    let (writer, guard) = tracing_appender::non_blocking(appender);

    // `RUST_LOG` wins; an unset *or malformed* value falls back to the
    // documented default instead of failing startup.
    let filter =
        EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new(DEFAULT_FILTER));

    // TUI default: file layer only — never a stdout/stderr destination.
    let file_layer = tracing_subscriber::fmt::layer()
        .with_writer(writer)
        .with_ansi(false);
    let subscriber = tracing_subscriber::registry().with(filter).with(file_layer);

    let installed = if opts.stderr {
        let stderr_layer = tracing_subscriber::fmt::layer()
            .with_writer(io::stderr)
            .with_ansi(false);
        subscriber.with(stderr_layer).try_init()
    } else {
        subscriber.try_init()
    };
    installed.map_err(|source| LogError::OpenFile {
        path: dir.join(LOG_FILE_PREFIX),
        source: io::Error::other(source),
    })?;

    Ok(guard)
}

/// A wrapper that redacts a sensitive value from every automatic rendering.
///
/// Both `Display` and `Debug` render exactly [`REDACTED`] (`***`); the inner
/// value is reachable only through the explicit, greppable
/// [`Secret::expose`]. This is the seam the crate uses for anything that
/// must never reach the log file — log it by value/reference and it stays
/// redacted even if a caller forgets and formats with `{:?}`:
///
/// ```text
/// tracing::info!(token = %secret);              // Display: `***`
/// tracing::info!("api token: {:?}", secret);    // Debug:   `***`
/// let raw: &String = secret.expose();           // the audited escape hatch
/// ```
///
/// EPIC-02 wraps the Todoist API token in this type; EPIC-12's automated
/// log scan asserts raw secrets never appear while `***` does.
pub struct Secret<T>(T);

impl<T> Secret<T> {
    /// Wraps `value`, hiding it from `Display`/`Debug` renderings.
    pub fn new(value: T) -> Self {
        Self(value)
    }

    /// Explicit, greppable access to the wrapped value. Call sites handling
    /// the raw secret stay auditable by searching for `expose`.
    #[must_use]
    pub fn expose(&self) -> &T {
        &self.0
    }
}

impl<T> fmt::Display for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

impl<T> fmt::Debug for Secret<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(REDACTED)
    }
}

#[cfg(test)]
mod tests {
    //! Unit tests (unit-tester). The tracing subscriber is process-global,
    //! so `init` runs exactly once via [`fixture`] and its guard is leaked.
    use super::*;
    use crate::error::AppError;
    use std::fs;
    use std::path::Path;
    use std::sync::{Mutex, OnceLock};
    use std::time::{Duration, Instant};

    struct Opaque {
        _a: u8,
    }

    #[derive(Debug)]
    struct Creds {
        _user: &'static str,
    }

    struct Fixture {
        base: PathBuf,
        dir: PathBuf,
        /// Whether the first-ever `init` call returned `Ok`.
        first_ok: bool,
    }

    static FIXTURE: OnceLock<Fixture> = OnceLock::new();
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    fn fixture() -> &'static Fixture {
        FIXTURE.get_or_init(|| {
            let _g = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let base =
                std::env::temp_dir().join(format!("todoist-tui-logtest-{}", std::process::id()));
            let _ = fs::remove_dir_all(&base);
            // Nested + nonexistent: init must create it recursively.
            let dir = base.join("a").join("b").join("state");
            assert!(!dir.exists());
            let saved = std::env::var_os("RUST_LOG");
            std::env::remove_var("RUST_LOG");
            let res = init(LogOptions::new().with_state_dir(&dir));
            if let Some(v) = saved {
                std::env::set_var("RUST_LOG", v);
            }
            let first_ok = match res {
                Ok(guard) => {
                    std::mem::forget(guard);
                    true
                }
                Err(_) => false,
            };
            Fixture {
                base,
                dir,
                first_ok,
            }
        })
    }

    fn log_files(dir: &Path) -> Vec<PathBuf> {
        fs::read_dir(dir)
            .map(|rd| {
                rd.filter_map(|e| e.ok().map(|e| e.path()))
                    .filter(|p| {
                        p.file_name()
                            .and_then(|n| n.to_str())
                            .is_some_and(|n| n.starts_with("todoist-tui.log"))
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    fn read_all(dir: &Path) -> String {
        log_files(dir)
            .iter()
            .map(|p| fs::read_to_string(p).unwrap_or_default())
            .collect::<Vec<_>>()
            .join("\n")
    }

    /// Polls until `pred(content)` holds or ~5s elapse; returns final content.
    fn wait_for(dir: &Path, pred: impl Fn(&str) -> bool) -> String {
        let end = Instant::now() + Duration::from_secs(5);
        loop {
            let c = read_all(dir);
            if pred(&c) || Instant::now() > end {
                return c;
            }
            std::thread::sleep(Duration::from_millis(25));
        }
    }

    #[test]
    fn init_creates_missing_nested_dir_and_log_file() {
        let f = fixture();
        assert!(f.first_ok, "first init must succeed");
        assert!(f.dir.is_dir(), "nested state dir must be created");
        let files = log_files(&f.dir);
        assert!(!files.is_empty(), "todoist-tui.log* must exist");
        let name = files[0].file_name().and_then(|n| n.to_str()).unwrap_or("");
        assert!(name.starts_with("todoist-tui.log"), "got {name}");
    }

    #[test]
    fn info_events_are_written_to_file() {
        let f = fixture();
        tracing::info!("marker-plain-info-7f3a");
        let c = wait_for(&f.dir, |c| c.contains("marker-plain-info-7f3a"));
        assert!(c.contains("marker-plain-info-7f3a"), "log content: {c:?}");
        assert!(!c.contains("\u{1b}["), "ANSI escapes must be disabled");
    }

    #[test]
    fn secret_is_redacted_in_log_file() {
        let f = fixture();
        let secret = Secret::new("tok_abc123".to_string());
        tracing::info!(token = %secret, "display-field-marker");
        tracing::info!(token = ?secret, "debug-field-marker");
        tracing::info!("interpolated {} and {:?} secret-fmt-marker", secret, secret);
        // Sentinel logged after: once it is on disk, earlier lines are too.
        tracing::info!("sentinel-end-marker");
        let c = wait_for(&f.dir, |c| c.contains("sentinel-end-marker"));
        assert!(c.contains("sentinel-end-marker"), "log content: {c:?}");
        assert!(c.contains("display-field-marker"));
        assert!(c.contains("debug-field-marker"));
        assert!(c.contains("secret-fmt-marker"));
        assert!(c.contains("***"), "redaction marker missing: {c:?}");
        assert_eq!(c.matches("tok_abc123").count(), 0, "secret leaked: {c:?}");
        // Every one of the three events carries the marker.
        assert!(c.matches("***").count() >= 4);
    }

    #[test]
    fn default_level_is_info_when_rust_log_unset() {
        // Fixture was initialised with RUST_LOG removed.
        let f = fixture();
        tracing::debug!("debug-should-be-filtered-91c2");
        tracing::trace!("trace-should-be-filtered-91c2");
        tracing::warn!("warn-passes-91c2");
        let c = wait_for(&f.dir, |c| c.contains("warn-passes-91c2"));
        assert!(c.contains("warn-passes-91c2"), "log content: {c:?}");
        assert!(!c.contains("debug-should-be-filtered-91c2"));
        assert!(!c.contains("trace-should-be-filtered-91c2"));
        assert_eq!(DEFAULT_FILTER, "info");
    }

    #[test]
    fn second_init_fails_cleanly_without_panic_or_side_effects() {
        let f = fixture();
        let other = f.base.join("second-init-dir");
        let res = init(LogOptions::new().with_state_dir(&other).with_stderr(true));
        match res {
            Err(AppError::Log(LogError::OpenFile { .. })) => {}
            Err(e) => panic!("unexpected error variant: {e:?}"),
            Ok(_) => panic!("second init must not succeed"),
        }
        assert!(!other.exists(), "failed init must not create its dir");
        // Concurrent calls also fail cleanly.
        let hs: Vec<_> = (0..4)
            .map(|_| {
                std::thread::spawn(|| {
                    init(LogOptions::new().with_state_dir(fixture().base.join("x"))).is_err()
                })
            })
            .collect();
        for h in hs {
            assert!(h.join().unwrap_or(false));
        }
    }

    #[test]
    fn secret_display_and_debug_are_exactly_redacted() {
        assert_eq!(format!("{}", Secret::new(42_i32)), "***");
        assert_eq!(format!("{:?}", Secret::new(42_i32)), "***");
        assert_eq!(format!("{}", Secret::new("tok_abc123".to_string())), "***");
        assert_eq!(
            format!("{:?}", Secret::new("tok_abc123".to_string())),
            "***"
        );
        assert_eq!(format!("{}", Secret::new(String::new())), "***");
        assert_eq!(format!("{:#?}", Secret::new("x")), "***");
        // Struct without Display/Debug: Secret<T> must not require bounds.
        let s = Secret::new(Opaque { _a: 1 });
        assert_eq!(format!("{s}"), "***");
        assert_eq!(format!("{s:?}"), "***");
        let s = Secret::new(Creds {
            _user: "tok_abc123",
        });
        assert_eq!(format!("{s:?}"), "***");
        // Nested inside another Debug-derived type stays redacted.
        let v = vec![Secret::new("tok_abc123")];
        assert_eq!(format!("{v:?}"), "[***]");
        assert_eq!(REDACTED, "***");
    }

    #[test]
    fn expose_returns_original_value_unchanged() {
        assert_eq!(*Secret::new(42_i32).expose(), 42);
        let s = Secret::new("tok_abc123".to_string());
        assert_eq!(s.expose(), "tok_abc123");
        // Exposing does not alter later rendering.
        assert_eq!(format!("{s}"), "***");
        assert_eq!(Secret::new(String::new()).expose(), "");
        assert_eq!(Secret::new(vec![1, 2, 3]).expose(), &vec![1, 2, 3]);
    }

    #[test]
    fn log_options_builders_set_fields() {
        let o = LogOptions::new();
        assert!(o.state_dir.is_none());
        assert!(!o.stderr, "TUI default must not log to stderr");
        let o = LogOptions::new().with_state_dir("/x/y").with_stderr(true);
        assert_eq!(o.state_dir.as_deref(), Some(Path::new("/x/y")));
        assert!(o.stderr);
    }
}
