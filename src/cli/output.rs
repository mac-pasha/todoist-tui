//! Non-interactive output channel for one-shot CLI commands (T-1001,
//! EPIC-11).
//!
//! [`Output`] is the single place one-shot subcommand code writes to
//! stdout/stderr through. It is generic over [`std::io::Write`] so tests
//! capture output in an in-memory buffer instead of spawning a process, and
//! it decides whether colour/ANSI escapes are allowed via
//! [`Output::is_color_enabled`] — false whenever stdout is not a TTY,
//! `NO_COLOR` is set, or `--no-color` was passed. `Output` only ever calls
//! `write!`/`writeln!` on its two sinks: it never enters the alternate
//! screen, never installs a `crossterm` raw-mode guard, and never opens
//! `/dev/tty` — those belong to the (separate, interactive) TUI path, not
//! to one-shot commands.

#![allow(dead_code)]

use std::env;
use std::io::{self, IsTerminal, Write};

/// Writes one-shot CLI command output to separate stdout/stderr sinks.
///
/// Generic over the sink type so production code uses [`Output::for_process`]
/// (real [`io::Stdout`] / [`io::Stderr`]) while tests build an
/// [`Output::new`] over `Vec<u8>` buffers and assert on the captured bytes
/// without spawning a process.
pub struct Output<O, E> {
    stdout: O,
    stderr: E,
    color_enabled: bool,
}

impl<O: Write, E: Write> Output<O, E> {
    /// Builds an `Output` over arbitrary sinks with an already-decided
    /// colour setting. Use [`color_enabled`] to compute `color_enabled`
    /// from the TTY/`NO_COLOR`/`--no-color` rules.
    pub fn new(stdout: O, stderr: E, color_enabled: bool) -> Self {
        Self {
            stdout,
            stderr,
            color_enabled,
        }
    }

    /// Whether ANSI colour escapes may be emitted: false whenever stdout is
    /// not a TTY, `NO_COLOR` is set, or `--no-color` was passed (see
    /// [`color_enabled`]).
    #[must_use]
    pub fn is_color_enabled(&self) -> bool {
        self.color_enabled
    }

    /// Writes `line` followed by a newline to stdout.
    ///
    /// # Errors
    ///
    /// Returns the underlying sink's I/O error.
    pub fn write_line(&mut self, line: &str) -> io::Result<()> {
        writeln!(self.stdout, "{line}")
    }

    /// Writes `line` followed by a newline to stderr.
    ///
    /// # Errors
    ///
    /// Returns the underlying sink's I/O error.
    pub fn write_error_line(&mut self, line: &str) -> io::Result<()> {
        writeln!(self.stderr, "{line}")
    }

    /// Flushes both sinks.
    ///
    /// # Errors
    ///
    /// Returns the underlying sink's I/O error.
    pub fn flush(&mut self) -> io::Result<()> {
        self.stdout.flush()?;
        self.stderr.flush()
    }
}

impl Output<io::Stdout, io::Stderr> {
    /// Builds the real process-facing `Output`: real stdout/stderr, with
    /// [`color_enabled`] deciding colour from the real stdout TTY check,
    /// `NO_COLOR` and `no_color_flag` (the CLI's `--no-color`).
    #[must_use]
    pub fn for_process(no_color_flag: bool) -> Self {
        let enabled = color_enabled(io::stdout().is_terminal(), no_color_flag);
        Self::new(io::stdout(), io::stderr(), enabled)
    }
}

/// Decides whether colour output is allowed.
///
/// False whenever `stdout_is_terminal` is false, the `NO_COLOR` environment
/// variable is set (to any value, per <https://no-color.org>), or
/// `no_color_flag` (the CLI's `--no-color`) is true; true otherwise.
#[must_use]
pub fn color_enabled(stdout_is_terminal: bool, no_color_flag: bool) -> bool {
    if no_color_flag {
        return false;
    }
    if env::var_os("NO_COLOR").is_some() {
        return false;
    }
    stdout_is_terminal
}
