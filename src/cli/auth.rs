//! The `auth` subcommand (T-107, EPIC-02).
//!
//! Prompts for the Todoist API token on **stderr**, reads one line from
//! stdin with terminal echo disabled (while stdin is a TTY), validates the
//! token against the Todoist API ([`check_blocking`], T-106) and stores it
//! through the keyring-first / file-fallback [`TokenStore`] (T-105).
//!
//! # Echo control
//!
//! crossterm 0.29 has no echo-only toggle: [`enable_raw_mode`] disables
//! echo *and* canonical line buffering together, so on a TTY the read
//! consumes per-key events ([`crossterm::event::read`]) and accumulates
//! characters until `Enter`. The terminal is restored by [`RawModeGuard`],
//! whose `Drop` re-enables echo on **every** return path — normal submit,
//! empty input, read error, Ctrl-D/EOF and the Ctrl-C key event — including
//! early returns via `?`. Two deliberate caveats:
//!
//! * In raw mode the terminal does not turn Ctrl-C into SIGINT (ISIG is
//!   off): it arrives as a key event, which cancels the prompt (mapped to
//!   [`AuthError::EmptyToken`], i.e. "no token was supplied"). A SIGINT
//!   delivered externally (`kill -INT`) still kills the process without
//!   running `Drop`; restoring the terminal there would need a
//!   signal-handler crate this task does not add. Manual verification of
//!   the terminal echo behaviour is T-111's job.
//! * If raw mode cannot be entered at all, the command falls back to a
//!   plain line read with echo left on (after warning the user) rather
//!   than failing: no existing `AppError` variant describes terminal-I/O
//!   failures, and `error.rs` is out of this task's scope.
//!
//! # Token hygiene
//!
//! The entered line is wrapped in [`Token`] immediately after the read
//! (see [`token_from_line`]); no bare `String` token escapes the read call,
//! and the token is never passed to a log macro or interpolated into a
//! user-facing string. All user-facing text goes to injectable writers so
//! the `tests` module can pin the exact wording.
//!
//! # Logging contract (T-107 acceptance 7)
//!
//! The only `tracing` events this path emits are the two
//! `tracing::info!(outcome = ..., "auth command finished")` calls in
//! [`validate_and_store`], whose fields are static outcome strings — never
//! the token, never runtime-interpolated material. A runtime capture test
//! is impossible in this crate's unit-test binary: installing any scoped or
//! global `tracing` dispatcher flips `tracing::dispatcher::has_been_set()`
//! for the rest of the process and breaks the once-per-process subscriber
//! contract `log::init` relies on (see `log::tests`). The `tests` module
//! therefore pins the event surface with a source-scan assertion (the
//! documented log-scan alternative the acceptance allows); the end-to-end
//! runtime scan over the real log file is EPIC-12's job.

use std::io::{self, IsTerminal, Write};

use crossterm::event::{self, Event, KeyCode, KeyEventKind, KeyModifiers};
use crossterm::terminal::{disable_raw_mode, enable_raw_mode};

use crate::auth::{check_blocking, Token, TokenStore, DEFAULT_BASE_URL};
use crate::error::{AppError, AuthError, Result};

/// Exact prompt text: written to stderr with no trailing newline so the
/// cursor stays on the prompt line.
const PROMPT: &str = "Todoist API token: ";

/// Runs the `auth` subcommand: resolve the store, prompt for the token,
/// validate it against the Todoist API, then store it.
///
/// # Errors
///
/// - [`AuthError::EmptyToken`] when the submitted line is empty (also used
///   for a cancelled prompt and for unrecoverable terminal-read failures,
///   both of which are explained on stderr first).
/// - Whatever [`TokenStore::resolve`], [`check_blocking`] and
///   [`TokenStore::store`] return: [`AuthError::InvalidToken`] covers the
///   API-rejected (401/403) case; network/unexpected-status failures keep
///   their T-106 variants.
pub fn run_auth() -> Result<()> {
    // Resolve before prompting: if no backend is usable there is no point
    // asking for a token we could not store anyway.
    let store = TokenStore::resolve()?;
    let token = prompt_and_read_token()?;
    validate_and_store(
        &token,
        &store,
        |token| check_blocking(token, DEFAULT_BASE_URL),
        &mut io::stdout(),
        &mut io::stderr(),
    )
}

/// Writes the prompt to stderr and reads one line of input, returning the
/// wrapped [`Token`]. This is the production seam over real stdin/stderr;
/// everything it delegates to ([`token_from_line`], [`validate_and_store`],
/// [`print_backend_notice`]) takes injectable writers or fakes so tests
/// never touch a real terminal, the real environment, or the network.
fn prompt_and_read_token() -> Result<Token> {
    let stderr = io::stderr();
    let mut err = stderr.lock();
    let _ = write!(err, "{PROMPT}");
    let _ = err.flush();

    let stdin = io::stdin();
    let read = if stdin.is_terminal() {
        read_tty_line()
    } else {
        // Piped or redirected stdin: no terminal attached, so no echo
        // control — this is what makes the command scriptable and testable.
        let mut lock = stdin.lock();
        read_piped_line(&mut lock).map(Some)
    };

    match read {
        Ok(Some(raw)) => token_from_line(&raw, &mut err),
        // Cancelled prompt (Ctrl-C / Ctrl-D on an empty line): no token was
        // supplied. The guard has already restored the terminal.
        Ok(None) => Err(AuthError::EmptyToken.into()),
        Err(source) => {
            let _ = writeln!(err, "failed to read the token: {source}");
            Err(AuthError::EmptyToken.into())
        }
    }
}

/// Reads one line from a TTY with echo disabled, restoring the terminal
/// before returning on every path (the [`RawModeGuard`] drop runs even on
/// early `?` returns and panics).
fn read_tty_line() -> io::Result<Option<String>> {
    if let Ok(guard) = RawModeGuard::enter() {
        let read = read_keys_until_submit();
        drop(guard); // restore echo/line discipline before any further output
        eprintln!(); // raw mode swallowed the Enter echo; keep the line tidy
        read
    } else {
        // Without raw mode we cannot suppress echo; stay functional and
        // warn instead of failing (no AppError variant fits).
        eprintln!(
            "warning: could not disable terminal echo; \
             the token will be visible as you type"
        );
        let stdin = io::stdin();
        let mut lock = stdin.lock();
        read_piped_line(&mut lock).map(Some)
    }
}

/// Accumulates key events until the user submits (`Enter`, or `Ctrl-D`
/// with a non-empty line) or cancels (`Ctrl-C`, or `Ctrl-D` on an empty
/// line, both mapping to `None`).
///
/// Must only run while raw mode is active: with canonical mode off, the CR
/// an `Enter` key sends is no longer translated to LF, so a plain line read
/// would never terminate.
fn read_keys_until_submit() -> io::Result<Option<String>> {
    let mut line = String::new();
    loop {
        let Event::Key(key) = event::read()? else {
            continue; // resize/mouse/paste events: ignore
        };
        if key.kind != KeyEventKind::Press {
            continue; // only act on presses, never releases/repeats
        }
        match key.code {
            KeyCode::Enter => return Ok(Some(line)),
            KeyCode::Char('c') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                return Ok(None); // cancelled: no token supplied
            }
            KeyCode::Char('d') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                // EOF semantics: submit what was typed, cancel if empty.
                return Ok(if line.is_empty() { None } else { Some(line) });
            }
            KeyCode::Char(character) => line.push(character),
            KeyCode::Backspace => {
                line.pop();
            }
            _ => {}
        }
    }
}

/// Reads a single line (trailing newline included) from a non-TTY reader
/// without any echo control: canonical mode already provides line
/// buffering, and a terminal echoes input by itself. Immediate EOF yields
/// an empty string, which [`token_from_line`] rejects with the
/// user-facing "empty token" message.
fn read_piped_line(reader: &mut dyn io::BufRead) -> io::Result<String> {
    let mut line = String::new();
    reader.read_line(&mut line)?;
    Ok(line)
}

/// RAII guard restoring the terminal's echo and line discipline on drop.
///
/// `enable_raw_mode`/`disable_raw_mode` are process-global in crossterm;
/// this one-shot command is the only raw-mode user, so pairing one enter
/// with one disable (on drop) is exact.
struct RawModeGuard;

impl RawModeGuard {
    fn enter() -> io::Result<Self> {
        enable_raw_mode().map(|()| Self)
    }
}

impl Drop for RawModeGuard {
    fn drop(&mut self) {
        // Best effort on purpose: there is no `AppError` variant for a
        // terminal-restore failure, and panicking in `drop` would abort.
        let _ = disable_raw_mode();
    }
}

/// Trims the entered line and wraps it in a [`Token`], or rejects an
/// empty/whitespace-only line with the user-facing "empty token" message
/// plus the existing [`AuthError::EmptyToken`].
fn token_from_line(line: &str, err: &mut dyn Write) -> Result<Token> {
    let trimmed = line.trim();
    if trimmed.is_empty() {
        // `AuthError::EmptyToken`'s own Display ("the API token must not be
        // empty") never contains the words "empty token", so the literal
        // phrase the CLI contract asks for is printed here.
        let _ = writeln!(err, "empty token: no token was entered");
        return Err(AuthError::EmptyToken.into());
    }
    // Only reachable with non-empty `trimmed`, where `Token::new` cannot
    // fail (it rejects only empty/whitespace-only input); propagating the
    // `Result` keeps this free of unwrap/expect.
    Token::new(trimmed.to_string())
}

/// Validates `token`, then — only on success — stores it and prints the
/// fallback-backend notice (when any) and the confirmation.
///
/// `validate` is injectable so tests can stand in for the real HTTPS call
/// ([`check_blocking`]); `out`/`err` carry every user-facing string so the
/// wording is assertable. The token is never written anywhere except the
/// store.
fn validate_and_store(
    token: &Token,
    store: &TokenStore,
    validate: impl FnOnce(&Token) -> Result<()>,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    if let Err(error) = validate(token) {
        // The error's own Display ("the API token is invalid") lacks the
        // literal words "invalid token"; print the contract phrase here.
        if matches!(error, AppError::Auth(AuthError::InvalidToken)) {
            let _ = writeln!(err, "invalid token: the Todoist API rejected it");
        }
        tracing::info!(outcome = "rejected", "auth command finished");
        return Err(error);
    }
    store.store(token)?;
    print_backend_notice(store.backend_notice().as_deref(), err);
    let _ = writeln!(out, "Token saved.");
    tracing::info!(outcome = "stored", "auth command finished");
    Ok(())
}

/// Writes the keyring-unavailable fallback notice (token-file path + mode
/// `0600`, produced by [`TokenStore::resolve`] in T-105) to stderr; a no-op
/// for the keyring-primary store.
fn print_backend_notice(notice: Option<&str>, err: &mut dyn Write) {
    if let Some(notice) = notice {
        let _ = writeln!(err, "{notice}");
    }
}

#[cfg(test)]
mod tests {
    //! T-107 acceptance coverage. Every seam is injected (writers, store,
    //! validate closure) so no test reads real stdin, touches the real
    //! environment, or makes a network call. The end-to-end exit-code and
    //! terminal-echo verification is T-110/T-111's job.
    use super::*;
    use crate::auth::MemoryTokenSource;

    /// Fake tokens standing in for the mock-API 200/401 outcomes (the real
    /// HTTP status mapping is T-106's wiremock suite).
    const FAKE_VALID: &str = "tok_fake_valid_6f2c";
    const FAKE_INVALID: &str = "tok_fake_invalid_9d1b";

    fn memory_store() -> TokenStore {
        TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        )
    }

    fn fake_api(token: &Token) -> Result<()> {
        if token.expose() == FAKE_VALID {
            Ok(())
        } else {
            Err(AuthError::InvalidToken.into())
        }
    }

    fn utf8(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("captured output must be utf-8")
    }

    #[test]
    fn prompt_text_is_exact() {
        // Pins the exact prompt: trailing space, no newline (production
        // writes it with `write!`, not `writeln!`).
        assert_eq!(PROMPT, "Todoist API token: ");
    }

    #[test]
    fn empty_or_whitespace_line_reports_empty_token() {
        let mut err = Vec::new();
        let result = token_from_line("   \n", &mut err);
        assert!(
            matches!(result, Err(AppError::Auth(AuthError::EmptyToken))),
            "got {result:?}"
        );
        let message = utf8(err);
        assert!(
            message.contains("empty token"),
            "stderr must contain the words `empty token`: {message}"
        );
    }

    #[test]
    fn piped_line_reading_consumes_exactly_one_line() {
        let mut input = io::Cursor::new(b"first\nsecond\n".to_vec());
        assert_eq!(read_piped_line(&mut input).expect("read"), "first\n");
        assert_eq!(read_piped_line(&mut input).expect("read"), "second\n");
        // Immediate EOF (the acceptance's "empty stdin") reads as "".
        assert_eq!(read_piped_line(&mut input).expect("read"), "");
    }

    #[test]
    fn piped_token_is_trimmed_so_the_newline_is_never_stored() {
        let mut err = Vec::new();
        let token = token_from_line(&format!(" {FAKE_VALID} \n"), &mut err).expect("token ok");
        assert_eq!(token.expose(), FAKE_VALID);
        assert!(err.is_empty(), "no message expected on success: {err:?}");
    }

    #[test]
    fn valid_token_is_stored_and_confirmed_without_its_characters() {
        let store = memory_store();
        let token = Token::new(FAKE_VALID.to_string()).expect("fake token");
        let mut out = Vec::new();
        let mut err = Vec::new();

        validate_and_store(&token, &store, fake_api, &mut out, &mut err)
            .expect("valid token must succeed");

        let confirmation = utf8(out);
        assert!(
            confirmation.contains("Token saved."),
            "stdout must confirm: {confirmation}"
        );
        assert!(
            !confirmation.contains(FAKE_VALID),
            "confirmation leaked the token: {confirmation}"
        );
        assert!(err.is_empty(), "no fallback notice for an injected store");
        let stored = store.load().expect("load ok").expect("token stored");
        assert_eq!(stored.expose(), FAKE_VALID);
    }

    #[test]
    fn invalid_token_rejected_with_message_and_nothing_stored() {
        let store = memory_store();
        let token = Token::new(FAKE_INVALID.to_string()).expect("fake token");
        let mut out = Vec::new();
        let mut err = Vec::new();

        let error = validate_and_store(&token, &store, fake_api, &mut out, &mut err)
            .expect_err("invalid token must fail");

        assert!(
            matches!(error, AppError::Auth(AuthError::InvalidToken)),
            "got {error:?}"
        );
        let message = utf8(err);
        assert!(
            message.contains("invalid token"),
            "stderr must contain the words `invalid token`: {message}"
        );
        assert!(
            !message.contains(FAKE_INVALID),
            "message leaked the token: {message}"
        );
        assert!(
            store.load().expect("load ok").is_none(),
            "nothing may be stored when validation fails"
        );
        assert!(
            !utf8(out).contains("Token saved."),
            "no confirmation on the rejected path"
        );
    }

    #[test]
    fn fallback_notice_prints_to_stderr_only_when_present() {
        let mut err = Vec::new();
        print_backend_notice(None, &mut err);
        assert!(err.is_empty(), "no notice, no output");

        let notice = "OS keyring unavailable; falling back to token file at \
                      /tmp/tok_fake/token (mode 0600)";
        print_backend_notice(Some(notice), &mut err);
        let printed = utf8(err);
        assert!(
            printed.contains(notice),
            "notice must be printed verbatim: {printed}"
        );
        assert!(printed.contains("0600"), "notice names the mode: {printed}");
    }

    // ---- no token material in any `tracing` event (acceptance 7) --------

    /// The exact `tracing` event calls the production code above may
    /// contain — every one uses only static outcome strings, never token
    /// material. Adding or changing an event in `auth.rs` requires a
    /// conscious update here.
    const TRACING_EVENT_LINES: &[&str] = &[
        r#"tracing::info!(outcome = "rejected", "auth command finished");"#,
        r#"tracing::info!(outcome = "stored", "auth command finished");"#,
    ];

    /// Documented log-scan assertion for acceptance 7 (runtime capture is
    /// impossible in this binary: any installed `tracing` dispatcher flips
    /// `has_been_set()` and breaks `log::init`'s once-per-process contract
    /// that `log::tests` depends on — observed and pinned by T-107).
    ///
    /// This scans the production half of this module and asserts its
    /// `tracing::` event surface is EXACTLY the static allowlist above: no
    /// event may interpolate runtime values (`{...}`/`format!`), so no code
    /// path can place token bytes into a log event. The runtime scan over
    /// the real log file is EPIC-12's job.
    #[test]
    fn tracing_event_surface_is_static_and_token_free() {
        let source = include_str!("auth.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("module source has a test section");
        // Only real statements count: `//!`/`//` lines (like this doc)
        // mention `tracing::` too but are not events.
        let event_lines: Vec<&str> = production
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("tracing::"))
            .collect();
        assert_eq!(
            event_lines, TRACING_EVENT_LINES,
            "the auth flow's tracing event surface changed; it must stay static \
             (outcome strings only) and token-free"
        );
    }

    #[test]
    fn user_facing_output_never_contains_token_material() {
        // Behavioral half of the leak contract: run both outcome paths and
        // scan every byte written to the injected writers for fake tokens.
        let store = memory_store();
        let valid = Token::new(FAKE_VALID.to_string()).expect("fake token");
        let invalid = Token::new(FAKE_INVALID.to_string()).expect("fake token");
        let mut out = Vec::new();
        let mut err = Vec::new();

        validate_and_store(&valid, &store, |_| Ok(()), &mut out, &mut err)
            .expect("stored path succeeds");
        let _ = validate_and_store(
            &invalid,
            &store,
            |_| Err::<(), _>(AuthError::InvalidToken.into()),
            &mut out,
            &mut err,
        );

        let everything = utf8(out) + &utf8(err);
        assert!(
            !everything.contains(FAKE_VALID),
            "valid token material leaked into user-facing output: {everything}"
        );
        assert!(
            !everything.contains(FAKE_INVALID),
            "invalid token material leaked into user-facing output: {everything}"
        );
    }
}
