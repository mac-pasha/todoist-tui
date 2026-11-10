//! Terminal lifecycle guard: raw mode, alternate screen, and panic restore
//! (T-602, EPIC-07).
//!
//! [`TerminalGuard`] owns the terminal contract for the TUI:
//! * [`TerminalGuard::enter`] (and [`TerminalGuard::enter_with`] for headless
//!   tests) enables raw mode, enters the alternate screen, installs a panic
//!   hook that restores the terminal before delegating to the previously
//!   installed hook, and exposes a [`ratatui::Terminal<CrosstermBackend<W>>`]
//!   handle via [`Deref`] / [`DerefMut`] and [`TerminalGuard::terminal_mut`].
//! * [`TerminalGuard::restore`] and [`Drop`] leave the alternate screen, show
//!   the cursor, and disable raw mode idempotently (at most once across
//!   explicit `restore()`, panic-hook execution, and `Drop`).
//! * Terminal I/O and raw-mode syscalls are routed through an injectable
//!   [`Write`] sink and [`TerminalOps`] trait so all unit and integration
//!   tests run headlessly in CI without a real TTY.

#![allow(dead_code)]

use std::fmt;
use std::io::{self, Stdout, Write};
use std::ops::{Deref, DerefMut};
use std::panic::PanicHookInfo;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, PoisonError};

use crossterm::cursor::Show;
use crossterm::queue;
use crossterm::terminal::{self, EnterAlternateScreen, LeaveAlternateScreen};
use ratatui::backend::CrosstermBackend;
use ratatui::layout::Rect;
use ratatui::{Terminal, TerminalOptions, Viewport};

use crate::error::{Result, UiError};

/// Type alias for a [`ratatui::Terminal`] backed by [`CrosstermBackend`].
pub type CrosstermTerminal<W = Stdout> = Terminal<CrosstermBackend<W>>;

/// Shared writer sink used to restore the terminal on exit, drop, or panic.
pub type RestoreWriter = Arc<Mutex<Box<dyn Write + Send>>>;

/// Shared [`TerminalOps`] handle used to restore raw mode and the screen.
pub type SharedOps = Arc<Mutex<Box<dyn TerminalOps + Send>>>;

/// Boxed panic-hook callback matching [`std::panic::set_hook`].
pub type PanicHookFn = Box<dyn Fn(&PanicHookInfo<'_>) + Send + Sync + 'static>;

/// ANSI escape sequence written by [`EnterAlternateScreen`].
pub const ENTER_ALT_SCREEN_SEQ: &[u8] = b"\x1b[?1049h";

/// ANSI escape sequence written by [`LeaveAlternateScreen`].
pub const LEAVE_ALT_SCREEN_SEQ: &[u8] = b"\x1b[?1049l";

/// ANSI escape sequence written by [`Show`] (cursor visibility restore).
pub const SHOW_CURSOR_SEQ: &[u8] = b"\x1b[?25h";

/// Writes the alternate-screen entry sequence ([`EnterAlternateScreen`]) to
/// `writer` and flushes it.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if queuing or flushing fails.
pub fn write_enter_sequence<W: Write + ?Sized>(mut writer: &mut W) -> io::Result<()> {
    queue!(&mut writer, EnterAlternateScreen)?;
    writer.flush()
}

/// Writes the terminal restoration sequence ([`LeaveAlternateScreen`] followed
/// by [`Show`]) to `writer` and flushes it.
///
/// # Errors
///
/// Returns the underlying [`io::Error`] if queuing or flushing fails.
pub fn write_restore_sequence<W: Write + ?Sized>(mut writer: &mut W) -> io::Result<()> {
    queue!(&mut writer, LeaveAlternateScreen, Show)?;
    writer.flush()
}

/// Seam over OS-level terminal operations so tests can exercise setup,
/// teardown, and error propagation headlessly without a real TTY.
pub trait TerminalOps: Send {
    /// Enables raw mode on the active terminal.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the terminal cannot enter raw mode.
    fn enable_raw_mode(&mut self) -> io::Result<()>;

    /// Disables raw mode on the active terminal.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if the terminal cannot leave raw mode.
    fn disable_raw_mode(&mut self) -> io::Result<()>;

    /// Enters the alternate screen by writing the escape sequence to `writer`.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing or flushing `writer` fails.
    fn enter_alternate_screen(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        write_enter_sequence(writer)
    }

    /// Leaves the alternate screen and restores cursor visibility on `writer`.
    ///
    /// # Errors
    ///
    /// Returns an [`io::Error`] if writing or flushing `writer` fails.
    fn leave_alternate_screen_and_show_cursor(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        write_restore_sequence(writer)
    }

    /// Returns the [`Viewport`] used to initialise the [`ratatui::Terminal`].
    ///
    /// Production [`CrosstermOps`] returns [`Viewport::Fullscreen`] (which
    /// queries the OS terminal size); headless implementations default to a
    /// fixed `80x24` viewport so no TTY ioctl is required in CI.
    fn viewport(&self) -> Viewport {
        Viewport::Fixed(Rect::new(0, 0, 80, 24))
    }
}

/// Production [`TerminalOps`] implementation backed by [`crossterm`].
#[derive(Debug, Default, Clone, Copy)]
pub struct CrosstermOps;

impl TerminalOps for CrosstermOps {
    fn enable_raw_mode(&mut self) -> io::Result<()> {
        terminal::enable_raw_mode()
    }

    fn disable_raw_mode(&mut self) -> io::Result<()> {
        terminal::disable_raw_mode()
    }

    fn viewport(&self) -> Viewport {
        Viewport::Fullscreen
    }
}

/// Thread-safe, cloneable in-memory [`Write`] sink for headless terminal tests.
#[derive(Debug, Default, Clone)]
pub struct SharedWriter {
    buffer: Arc<Mutex<Vec<u8>>>,
    fail_on_write: Arc<Mutex<Option<io::ErrorKind>>>,
}

impl SharedWriter {
    /// Creates an empty [`SharedWriter`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures subsequent writes or flushes to fail with `kind`.
    pub fn set_write_error(&self, kind: Option<io::ErrorKind>) {
        *self
            .fail_on_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = kind;
    }

    /// Returns a snapshot of all bytes written so far.
    #[must_use]
    pub fn bytes(&self) -> Vec<u8> {
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .clone()
    }

    /// Returns the written bytes decoded lossily as UTF-8.
    #[must_use]
    pub fn to_string_lossy(&self) -> String {
        String::from_utf8_lossy(&self.bytes()).into_owned()
    }
}

impl Write for SharedWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if let Some(kind) = *self
            .fail_on_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Err(io::Error::new(kind, "injected write failure"));
        }
        self.buffer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        if let Some(kind) = *self
            .fail_on_write
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
        {
            return Err(io::Error::new(kind, "injected flush failure"));
        }
        Ok(())
    }
}

#[derive(Debug, Default)]
struct RecordingState {
    enable_raw_count: usize,
    disable_raw_count: usize,
    enter_alt_count: usize,
    leave_alt_count: usize,
    raw_mode_enabled: bool,
    in_alternate_screen: bool,
    fail_enable_raw: Option<io::ErrorKind>,
    fail_disable_raw: Option<io::ErrorKind>,
    fail_enter_alt: Option<io::ErrorKind>,
    fail_leave_alt: Option<io::ErrorKind>,
}

/// Headless, recording [`TerminalOps`] double for unit and integration tests.
///
/// Tracks every raw-mode and alternate-screen transition in memory while still
/// writing the real [`crossterm`] escape sequences to the injected [`Write`]
/// sink.
#[derive(Debug, Default, Clone)]
pub struct RecordingOps {
    state: Arc<Mutex<RecordingState>>,
}

impl RecordingOps {
    /// Creates a fresh [`RecordingOps`] with all counters at zero.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Configures `enable_raw_mode` to fail with `kind`.
    #[must_use]
    pub fn with_enable_raw_error(self, kind: io::ErrorKind) -> Self {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fail_enable_raw = Some(kind);
        self
    }

    /// Configures `disable_raw_mode` to fail with `kind`.
    #[must_use]
    pub fn with_disable_raw_error(self, kind: io::ErrorKind) -> Self {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fail_disable_raw = Some(kind);
        self
    }

    /// Configures `enter_alternate_screen` to fail with `kind`.
    #[must_use]
    pub fn with_enter_alt_error(self, kind: io::ErrorKind) -> Self {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fail_enter_alt = Some(kind);
        self
    }

    /// Configures `leave_alternate_screen_and_show_cursor` to fail with `kind`.
    #[must_use]
    pub fn with_leave_alt_error(self, kind: io::ErrorKind) -> Self {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .fail_leave_alt = Some(kind);
        self
    }

    /// Number of times `enable_raw_mode` was called.
    #[must_use]
    pub fn enable_raw_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .enable_raw_count
    }

    /// Number of times `disable_raw_mode` was called.
    #[must_use]
    pub fn disable_raw_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .disable_raw_count
    }

    /// Number of times `enter_alternate_screen` was called.
    #[must_use]
    pub fn enter_alt_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .enter_alt_count
    }

    /// Number of times `leave_alternate_screen_and_show_cursor` was called.
    #[must_use]
    pub fn leave_alt_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .leave_alt_count
    }

    /// Whether raw mode is currently marked as enabled.
    #[must_use]
    pub fn is_raw_mode_enabled(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .raw_mode_enabled
    }

    /// Whether the terminal is currently marked as inside the alternate screen.
    #[must_use]
    pub fn is_in_alternate_screen(&self) -> bool {
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .in_alternate_screen
    }
}

impl TerminalOps for RecordingOps {
    fn enable_raw_mode(&mut self) -> io::Result<()> {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.enable_raw_count += 1;
        if let Some(kind) = guard.fail_enable_raw {
            return Err(io::Error::new(kind, "injected enable_raw_mode failure"));
        }
        guard.raw_mode_enabled = true;
        Ok(())
    }

    fn disable_raw_mode(&mut self) -> io::Result<()> {
        let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        guard.disable_raw_count += 1;
        if let Some(kind) = guard.fail_disable_raw {
            return Err(io::Error::new(kind, "injected disable_raw_mode failure"));
        }
        guard.raw_mode_enabled = false;
        Ok(())
    }

    fn enter_alternate_screen(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        {
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            guard.enter_alt_count += 1;
            if let Some(kind) = guard.fail_enter_alt {
                return Err(io::Error::new(
                    kind,
                    "injected enter_alternate_screen failure",
                ));
            }
        }
        write_enter_sequence(writer)?;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .in_alternate_screen = true;
        Ok(())
    }

    fn leave_alternate_screen_and_show_cursor(&mut self, writer: &mut dyn Write) -> io::Result<()> {
        {
            let mut guard = self.state.lock().unwrap_or_else(PoisonError::into_inner);
            guard.leave_alt_count += 1;
            if let Some(kind) = guard.fail_leave_alt {
                return Err(io::Error::new(
                    kind,
                    "injected leave_alternate_screen failure",
                ));
            }
        }
        write_restore_sequence(writer)?;
        self.state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .in_alternate_screen = false;
        Ok(())
    }
}

/// RAII guard that owns the terminal's raw-mode and alternate-screen state and
/// dereferences to [`ratatui::Terminal<CrosstermBackend<W>>`].
///
/// Leaving scope (or calling [`TerminalGuard::restore`] explicitly, or
/// panicking while the guard is active) restores the terminal at most once.
pub struct TerminalGuard<W: Write = Stdout> {
    terminal: CrosstermTerminal<W>,
    restore_writer: RestoreWriter,
    ops: SharedOps,
    restored: Arc<AtomicBool>,
}

impl<W: Write> fmt::Debug for TerminalGuard<W> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("TerminalGuard")
            .field("restored", &self.restored.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl TerminalGuard<Stdout> {
    /// Enables raw mode, enters the alternate screen on [`io::stdout`],
    /// installs a panic hook that restores the terminal before delegating to
    /// the previously installed hook, and returns a [`TerminalGuard`] wrapping
    /// [`ratatui::Terminal<CrosstermBackend<Stdout>>`].
    ///
    /// # Errors
    ///
    /// Returns [`UiError::TerminalInit`] (wrapped in [`crate::error::AppError::Ui`])
    /// if raw mode, alternate-screen entry, or terminal backend initialization
    /// fails.
    pub fn enter() -> Result<Self> {
        Self::enter_internal(io::stdout(), Box::new(io::stdout()), Box::new(CrosstermOps))
    }
}

impl<W: Write + Clone + Send + 'static> TerminalGuard<W> {
    /// Headless-friendly constructor that routes all terminal I/O through
    /// `writer` and all raw-mode/screen transitions through `ops`.
    ///
    /// Installs the same panic hook as [`TerminalGuard::enter`], bound to the
    /// injected `writer` and `ops`.
    ///
    /// # Errors
    ///
    /// Returns [`UiError::TerminalInit`] (wrapped in [`crate::error::AppError::Ui`])
    /// if raw mode, alternate-screen entry, or terminal initialization fails.
    pub fn enter_with<O>(writer: W, ops: O) -> Result<Self>
    where
        O: TerminalOps + 'static,
    {
        let restore_sink: Box<dyn Write + Send> = Box::new(writer.clone());
        Self::enter_internal(writer, restore_sink, Box::new(ops))
    }

    /// Enters the terminal guard using `writer` and a default headless
    /// [`RecordingOps`] implementation so no TTY is required.
    ///
    /// # Errors
    ///
    /// Returns [`UiError::TerminalInit`] if writing the initial escape sequence
    /// to `writer` fails.
    pub fn enter_headless(writer: W) -> Result<Self> {
        Self::enter_with(writer, RecordingOps::new())
    }
}

impl<W: Write> TerminalGuard<W> {
    fn enter_internal(
        backend_writer: W,
        mut restore_writer: Box<dyn Write + Send>,
        mut ops: Box<dyn TerminalOps + Send>,
    ) -> Result<Self> {
        ops.enable_raw_mode()
            .map_err(|source| UiError::TerminalInit { source })?;

        if let Err(source) = ops.enter_alternate_screen(&mut *restore_writer) {
            let _ = ops.disable_raw_mode();
            return Err(UiError::TerminalInit { source }.into());
        }

        let viewport = ops.viewport();
        let backend = CrosstermBackend::new(backend_writer);
        let terminal = match Terminal::with_options(backend, TerminalOptions { viewport }) {
            Ok(term) => term,
            Err(source) => {
                let _ = ops.leave_alternate_screen_and_show_cursor(&mut *restore_writer);
                let _ = ops.disable_raw_mode();
                return Err(UiError::TerminalInit { source }.into());
            }
        };

        let restore_writer = Arc::new(Mutex::new(restore_writer));
        let ops = Arc::new(Mutex::new(ops));
        let restored = Arc::new(AtomicBool::new(false));

        install_panic_hook(
            Arc::clone(&restore_writer),
            Arc::clone(&ops),
            Arc::clone(&restored),
        );

        Ok(Self {
            terminal,
            restore_writer,
            ops,
            restored,
        })
    }

    /// Borrows the underlying [`ratatui::Terminal<CrosstermBackend<W>>`].
    #[must_use]
    pub const fn terminal(&self) -> &CrosstermTerminal<W> {
        &self.terminal
    }

    /// Mutably borrows the underlying [`ratatui::Terminal<CrosstermBackend<W>>`].
    pub fn terminal_mut(&mut self) -> &mut CrosstermTerminal<W> {
        &mut self.terminal
    }

    /// Returns `true` if [`TerminalGuard::restore`] (or the panic hook) has
    /// already restored the terminal.
    #[must_use]
    pub fn is_restored(&self) -> bool {
        self.restored.load(Ordering::SeqCst)
    }

    /// Leaves the alternate screen, shows the cursor, and disables raw mode.
    ///
    /// Idempotent: calling `restore()` multiple times (or dropping the guard
    /// after calling `restore()`) performs the restoration sequence only once.
    ///
    /// # Errors
    ///
    /// Returns [`UiError::TerminalRestore`] (wrapped in
    /// [`crate::error::AppError::Ui`]) if leaving the alternate screen or
    /// disabling raw mode fails on the first restoration call.
    pub fn restore(&mut self) -> Result<()> {
        restore_once(&self.restore_writer, &self.ops, &self.restored)
    }
}

impl<W: Write> Deref for TerminalGuard<W> {
    type Target = CrosstermTerminal<W>;

    fn deref(&self) -> &Self::Target {
        &self.terminal
    }
}

impl<W: Write> DerefMut for TerminalGuard<W> {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.terminal
    }
}

impl<W: Write> Drop for TerminalGuard<W> {
    fn drop(&mut self) {
        let _ = self.restore();
    }
}

/// Executes the restoration sequence at most once for a shared `(writer, ops,
/// restored)` triple.
fn restore_once(
    restore_writer: &RestoreWriter,
    ops: &SharedOps,
    restored: &Arc<AtomicBool>,
) -> Result<()> {
    if restored.swap(true, Ordering::SeqCst) {
        return Ok(());
    }

    let mut ops_guard = ops.lock().unwrap_or_else(PoisonError::into_inner);
    let screen_result = {
        let mut writer_guard = restore_writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        ops_guard.leave_alternate_screen_and_show_cursor(&mut **writer_guard)
    };
    let raw_result = ops_guard.disable_raw_mode();

    if let Err(source) = screen_result {
        return Err(UiError::TerminalRestore { source }.into());
    }
    if let Err(source) = raw_result {
        return Err(UiError::TerminalRestore { source }.into());
    }
    Ok(())
}

/// Builds a panic-hook closure that restores the terminal via `restore_writer`
/// and `ops` before invoking `prev_hook`.
pub fn make_panic_hook<H>(
    restore_writer: RestoreWriter,
    ops: SharedOps,
    restored: Arc<AtomicBool>,
    prev_hook: H,
) -> PanicHookFn
where
    H: Fn(&PanicHookInfo<'_>) + Send + Sync + 'static,
{
    Box::new(move |panic_info: &PanicHookInfo<'_>| {
        let _ = restore_once(&restore_writer, &ops, &restored);
        prev_hook(panic_info);
    })
}

/// Installs a process-wide panic hook that restores the terminal before
/// delegating to the previously installed panic hook.
fn install_panic_hook(restore_writer: RestoreWriter, ops: SharedOps, restored: Arc<AtomicBool>) {
    let prev_hook = std::panic::take_hook();
    let hook = make_panic_hook(restore_writer, ops, restored, prev_hook);
    std::panic::set_hook(hook);
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use std::panic::{self, AssertUnwindSafe};

    use super::*;
    use crate::error::AppError;

    /// Serialises tests in this module so global `std::panic::set_hook` calls
    /// never interleave across threads.
    static HOOK_LOCK: Mutex<()> = Mutex::new(());

    /// Counts non-overlapping occurrences of `needle` in `haystack`.
    fn count_subsequence(haystack: &[u8], needle: &[u8]) -> usize {
        if needle.is_empty() || haystack.len() < needle.len() {
            return 0;
        }
        let mut count = 0;
        let mut idx = 0;
        while idx + needle.len() <= haystack.len() {
            if &haystack[idx..idx + needle.len()] == needle {
                count += 1;
                idx += needle.len();
            } else {
                idx += 1;
            }
        }
        count
    }

    #[test]
    fn enter_with_enables_raw_mode_enters_alternate_screen_and_returns_terminal_handle() {
        let _serial = HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let saved_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));

        let writer = SharedWriter::new();
        let ops = RecordingOps::new();

        let mut guard =
            TerminalGuard::enter_with(writer.clone(), ops.clone()).expect("enter_with succeeds");

        assert_eq!(ops.enable_raw_count(), 1);
        assert_eq!(ops.enter_alt_count(), 1);
        assert!(ops.is_raw_mode_enabled());
        assert!(ops.is_in_alternate_screen());
        assert!(!guard.is_restored());

        // EnterAlternateScreen sequence was written to the injected sink.
        assert_eq!(count_subsequence(&writer.bytes(), ENTER_ALT_SCREEN_SEQ), 1);

        // Guard dereferences to ratatui::Terminal<CrosstermBackend<SharedWriter>>
        // and supports drawing headlessly without a TTY.
        guard
            .draw(|frame| {
                let area = frame.area();
                assert_eq!(area.width, 80);
                assert_eq!(area.height, 24);
            })
            .expect("headless draw succeeds");

        drop(guard);
        panic::set_hook(saved_hook);
    }

    #[test]
    fn drop_restores_raw_mode_alternate_screen_and_cursor_exactly_once() {
        let _serial = HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let saved_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));

        let writer = SharedWriter::new();
        let ops = RecordingOps::new();

        {
            let _guard = TerminalGuard::enter_with(writer.clone(), ops.clone())
                .expect("enter_with succeeds");
            assert_eq!(ops.disable_raw_count(), 0);
            assert_eq!(ops.leave_alt_count(), 0);
        }

        // After Drop: raw mode disabled once, alternate screen left once, cursor shown once.
        assert_eq!(ops.disable_raw_count(), 1);
        assert_eq!(ops.leave_alt_count(), 1);
        assert!(!ops.is_raw_mode_enabled());
        assert!(!ops.is_in_alternate_screen());

        let bytes = writer.bytes();
        assert_eq!(count_subsequence(&bytes, LEAVE_ALT_SCREEN_SEQ), 1);
        assert_eq!(count_subsequence(&bytes, SHOW_CURSOR_SEQ), 1);

        panic::set_hook(saved_hook);
    }

    #[test]
    fn explicit_restore_followed_by_drop_is_idempotent_and_restores_only_once() {
        let _serial = HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let saved_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));

        let writer = SharedWriter::new();
        let ops = RecordingOps::new();

        {
            let mut guard = TerminalGuard::enter_with(writer.clone(), ops.clone())
                .expect("enter_with succeeds");

            // First explicit restore() performs the restoration.
            guard.restore().expect("first restore succeeds");
            assert!(guard.is_restored());
            assert_eq!(ops.disable_raw_count(), 1);
            assert_eq!(ops.leave_alt_count(), 1);

            // Second explicit restore() is a no-op.
            guard.restore().expect("second restore is a no-op");
            assert_eq!(ops.disable_raw_count(), 1);
            assert_eq!(ops.leave_alt_count(), 1);
        }

        // Subsequent Drop is also a no-op: total count remains 1.
        assert_eq!(ops.disable_raw_count(), 1);
        assert_eq!(ops.leave_alt_count(), 1);

        let bytes = writer.bytes();
        assert_eq!(count_subsequence(&bytes, LEAVE_ALT_SCREEN_SEQ), 1);
        assert_eq!(count_subsequence(&bytes, SHOW_CURSOR_SEQ), 1);

        panic::set_hook(saved_hook);
    }

    #[test]
    fn panic_hook_restores_terminal_before_delegating_to_previous_hook() {
        let _serial = HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let saved_hook = panic::take_hook();

        let writer = SharedWriter::new();
        let ops = RecordingOps::new();

        // Track whether the previously installed hook ran and capture the state
        // of `writer` and `ops` at the exact moment `prev_hook` is invoked.
        let prev_called = Arc::new(AtomicBool::new(false));
        let restored_before_prev = Arc::new(AtomicBool::new(false));

        let prev_called_clone = Arc::clone(&prev_called);
        let restored_before_prev_clone = Arc::clone(&restored_before_prev);
        let writer_for_prev = writer.clone();
        let ops_for_prev = ops.clone();

        panic::set_hook(Box::new(move |_info| {
            prev_called_clone.store(true, Ordering::SeqCst);
            let current_bytes = writer_for_prev.bytes();
            let has_leave = count_subsequence(&current_bytes, LEAVE_ALT_SCREEN_SEQ) == 1;
            let has_show = count_subsequence(&current_bytes, SHOW_CURSOR_SEQ) == 1;
            let raw_disabled = !ops_for_prev.is_raw_mode_enabled();
            if has_leave && has_show && raw_disabled {
                restored_before_prev_clone.store(true, Ordering::SeqCst);
            }
        }));

        let guard =
            TerminalGuard::enter_with(writer.clone(), ops.clone()).expect("enter_with succeeds");

        let unwind_result = panic::catch_unwind(AssertUnwindSafe(|| {
            panic!("simulated panic during frame draw");
        }));
        assert!(unwind_result.is_err(), "panic must unwind");

        // The previous hook was invoked AND saw the terminal already restored.
        assert!(
            prev_called.load(Ordering::SeqCst),
            "previously installed panic hook must be invoked"
        );
        assert!(
            restored_before_prev.load(Ordering::SeqCst),
            "terminal must be restored BEFORE delegating to the previous panic hook"
        );

        // Dropping the guard after the panic hook ran does not write a second
        // restore sequence.
        drop(guard);
        assert_eq!(ops.disable_raw_count(), 1);
        assert_eq!(ops.leave_alt_count(), 1);
        assert_eq!(count_subsequence(&writer.bytes(), LEAVE_ALT_SCREEN_SEQ), 1);
        assert_eq!(count_subsequence(&writer.bytes(), SHOW_CURSOR_SEQ), 1);

        panic::set_hook(saved_hook);
    }

    #[test]
    fn enable_raw_mode_error_maps_to_ui_error_terminal_init() {
        let writer = SharedWriter::new();
        let ops = RecordingOps::new().with_enable_raw_error(io::ErrorKind::PermissionDenied);

        let err = TerminalGuard::enter_with(writer.clone(), ops.clone())
            .expect_err("enable_raw_mode failure must return Err");

        assert!(matches!(
            err,
            AppError::Ui(UiError::TerminalInit { ref source })
                if source.kind() == io::ErrorKind::PermissionDenied
        ));
        assert_eq!(ops.enter_alt_count(), 0);
        assert_eq!(writer.bytes(), Vec::<u8>::new());
    }

    #[test]
    fn enter_alternate_screen_error_rolls_back_raw_mode_and_maps_to_terminal_init() {
        let writer = SharedWriter::new();
        let ops = RecordingOps::new().with_enter_alt_error(io::ErrorKind::BrokenPipe);

        let err = TerminalGuard::enter_with(writer, ops.clone())
            .expect_err("enter_alternate_screen failure must return Err");

        assert!(matches!(
            err,
            AppError::Ui(UiError::TerminalInit { ref source })
                if source.kind() == io::ErrorKind::BrokenPipe
        ));
        // Raw mode was enabled and then rolled back on failure.
        assert_eq!(ops.enable_raw_count(), 1);
        assert_eq!(ops.disable_raw_count(), 1);
        assert!(!ops.is_raw_mode_enabled());
    }

    #[test]
    fn restore_error_maps_to_ui_error_terminal_restore() {
        let _serial = HOOK_LOCK.lock().unwrap_or_else(PoisonError::into_inner);
        let saved_hook = panic::take_hook();
        panic::set_hook(Box::new(|_| {}));

        let writer = SharedWriter::new();
        let ops = RecordingOps::new().with_disable_raw_error(io::ErrorKind::Other);

        let mut guard =
            TerminalGuard::enter_with(writer, ops.clone()).expect("enter_with succeeds");
        let err = guard
            .restore()
            .expect_err("disable_raw_mode failure must return Err");

        assert!(matches!(err, AppError::Ui(UiError::TerminalRestore { .. })));
        // Leave alternate screen still ran even though disable_raw_mode failed.
        assert_eq!(ops.leave_alt_count(), 1);
        assert_eq!(ops.disable_raw_count(), 1);

        drop(guard);
        panic::set_hook(saved_hook);
    }
}
