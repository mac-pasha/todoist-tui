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
