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

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::Mutex;

    /// Serialises tests in this module that mutate the process-global
    /// `NO_COLOR` environment variable, matching the shared-lock pattern
    /// used by `config::load`/`config::paths` tests for the same reason:
    /// env vars are global state shared by every test thread in this
    /// binary's test process.
    static ENV_LOCK: Mutex<()> = Mutex::new(());

    /// RAII guard that removes `NO_COLOR` on drop, restoring whatever value
    /// (present or absent) it had before the test ran.
    struct NoColorGuard {
        previous: Option<std::ffi::OsString>,
        _lock: std::sync::MutexGuard<'static, ()>,
    }

    impl NoColorGuard {
        fn set(value: &str) -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = env::var_os("NO_COLOR");
            env::set_var("NO_COLOR", value);
            Self {
                previous,
                _lock: lock,
            }
        }

        fn unset() -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let previous = env::var_os("NO_COLOR");
            env::remove_var("NO_COLOR");
            Self {
                previous,
                _lock: lock,
            }
        }
    }

    impl Drop for NoColorGuard {
        fn drop(&mut self) {
            match &self.previous {
                Some(value) => env::set_var("NO_COLOR", value),
                None => env::remove_var("NO_COLOR"),
            }
        }
    }

    /// `is_color_enabled` false and one-shot printing through `Output`
    /// never emits the ANSI escape byte (`0x1b`) anywhere in the captured
    /// buffers, matching the colour-disabled acceptance criterion.
    #[test]
    fn no_ansi_escape_bytes_when_color_disabled() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut output = Output::new(&mut stdout, &mut stderr, false);

        assert!(!output.is_color_enabled());
        output.write_line("hello world").unwrap();
        output.write_error_line("oh no").unwrap();
        output.flush().unwrap();

        assert!(
            !stdout.contains(&0x1b),
            "stdout must not contain the ESC byte when colour is disabled"
        );
        assert!(
            !stderr.contains(&0x1b),
            "stderr must not contain the ESC byte when colour is disabled"
        );
    }

    /// The rendered bytes for a sample line equal the expected plain-text
    /// bytes exactly: no hidden formatting, ANSI, or control bytes, and no
    /// alternate-screen / raw-mode / `/dev/tty` interaction.
    #[test]
    fn write_line_renders_exact_plain_text_bytes() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut output = Output::new(&mut stdout, &mut stderr, false);

        output.write_line("hello world").unwrap();

        assert_eq!(stdout, b"hello world\n");
        assert_eq!(stderr, [] as [u8; 0]);
    }

    /// Same exact-bytes guarantee for `write_error_line` against the
    /// stderr sink.
    #[test]
    fn write_error_line_renders_exact_plain_text_bytes() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut output = Output::new(&mut stdout, &mut stderr, false);

        output.write_error_line("oh no").unwrap();

        assert_eq!(stderr, b"oh no\n");
        assert_eq!(stdout, [] as [u8; 0]);
    }

    /// Even when `color_enabled` is true, `Output` itself never injects
    /// ANSI escapes on its own: it only ever forwards exactly what the
    /// caller passed in via `write!`/`writeln!`.
    #[test]
    fn write_line_with_color_enabled_still_writes_plain_bytes_only() {
        let mut stdout = Vec::new();
        let mut stderr = Vec::new();
        let mut output = Output::new(&mut stdout, &mut stderr, true);

        assert!(output.is_color_enabled());
        output.write_line("hello world").unwrap();

        assert_eq!(stdout, b"hello world\n");
        assert!(!stdout.contains(&0x1b));
    }

    #[test]
    fn color_enabled_false_when_not_a_terminal() {
        let _guard = NoColorGuard::unset();
        assert!(!color_enabled(false, false));
    }

    #[test]
    fn color_enabled_false_when_no_color_flag_set() {
        let _guard = NoColorGuard::unset();
        assert!(!color_enabled(true, true));
    }

    #[test]
    fn color_enabled_false_when_no_color_env_var_set() {
        let _guard = NoColorGuard::set("1");
        assert!(!color_enabled(true, false));
    }

    #[test]
    fn color_enabled_true_when_terminal_and_no_overrides() {
        let _guard = NoColorGuard::unset();
        assert!(color_enabled(true, false));
    }
}
