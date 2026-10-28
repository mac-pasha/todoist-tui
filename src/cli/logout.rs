//! The `logout` subcommand (T-108, EPIC-02).
//!
//! Clears the stored Todoist API token from **both** token backends via
//! [`TokenStore::clear`] (T-105, which already attempts the keyring and
//! the file fallback unconditionally — see its docs) and then runs the
//! injected [`CacheWipe`] hook (T-108; a no-op until EPIC-04 lands the real
//! cache database, see [`crate::auth::cache_wipe`]).
//!
//! # Idempotency and partial failure
//!
//! Both steps are attempted regardless of whether the other failed: a
//! locked keyring must not prevent the file fallback (or the cache) from
//! being cleared, and vice versa. Running `logout` with nothing stored at
//! all is not an error — `TokenStore::clear` and [`CacheWipe::wipe`] are
//! both documented as idempotent no-ops in that case — so the command
//! exits `0`. When either step fails, the command reports which one(s)
//! failed on stderr, still runs the remaining step, and returns an error
//! (propagated to a non-zero exit by `main`'s existing error-to-exit-code
//! mapping, the same path T-107's `auth` command uses).
//!
//! # Logging contract (T-108 acceptance 6)
//!
//! The only `tracing` events this path emits are the two
//! `tracing::info!(outcome = ..., "logout command finished")` calls in
//! [`run_logout_with`], whose fields are static outcome strings — never
//! token material, cache contents, or file paths. See `auth.rs`'s module
//! docs for why a runtime dispatcher-capture test is impossible in this
//! binary; this module's `tests` pins the event surface the same way, via
//! a source-scan assertion.

use std::io::{self, Write};

use crate::auth::{CacheWipe, NoopCacheWipe, TokenStore};
use crate::error::Result;

/// Runs the `logout` subcommand against the real token store, the
/// not-yet-implemented cache hook, and real stdout/stderr.
///
/// # Errors
///
/// Returns whatever [`TokenStore::resolve`] or [`run_logout_with`] return.
pub fn run_logout() -> Result<()> {
    let store = TokenStore::resolve()?;
    run_logout_with(&store, &NoopCacheWipe, &mut io::stdout(), &mut io::stderr())
}

/// Injectable seam: clears `store` (both backends) and runs `cache`'s
/// wipe hook, writing the user-facing summary to `out`/`err`.
///
/// Both steps are always attempted, even when the first one fails, so a
/// failure in one backend/hook never leaves the other one in a stale
/// state. On full success, `out` names each thing that was cleared; on any
/// failure, `err` names which step(s) failed (still followed by whatever
/// succeeded being named on `out`), and the first encountered error is
/// returned.
///
/// # Errors
///
/// Returns the error from [`TokenStore::clear`] if it fails, otherwise the
/// error from [`CacheWipe::wipe`] if that fails.
pub fn run_logout_with(
    store: &TokenStore,
    cache: &dyn CacheWipe,
    out: &mut dyn Write,
    err: &mut dyn Write,
) -> Result<()> {
    let token_result = store.clear();
    let cache_result = cache.wipe();

    match &token_result {
        Ok(()) => {
            let _ = writeln!(out, "Cleared stored API token.");
        }
        Err(error) => {
            let _ = writeln!(err, "failed to clear stored API token: {error}");
        }
    }
    match &cache_result {
        Ok(()) => {
            let _ = writeln!(out, "Cleared {}.", cache.describe());
        }
        Err(error) => {
            let _ = writeln!(err, "failed to clear {}: {error}", cache.describe());
        }
    }

    if token_result.is_ok() && cache_result.is_ok() {
        tracing::info!(outcome = "cleared", "logout command finished");
        Ok(())
    } else {
        tracing::info!(outcome = "failed", "logout command finished");
        token_result.and(cache_result)
    }
}

#[cfg(test)]
mod tests {
    //! T-108 acceptance coverage. Every seam is injected (writers, store,
    //! cache) so no test touches a real OS keyring, a real config
    //! directory, or a real cache database. The file-backed-source
    //! acceptance case uses a real temp-dir [`crate::auth::FileTokenSource`]
    //! (no `tempfile` dev-dependency: same `std::env::temp_dir()` + pid +
    //! counter pattern `file_source.rs`'s own tests use), the only real
    //! filesystem touch in this module.
    use std::env;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::auth::{FileTokenSource, MemoryTokenSource, Token, TokenSource};
    use crate::error::{AppError, AuthError};

    const FIXTURE_TOKEN: &str = "tok_fake_logout_abc123";

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    fn temp_token_path() -> std::path::PathBuf {
        env::temp_dir().join(format!(
            "todoist-tui-t108-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
    }

    fn memory_store() -> TokenStore {
        TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        )
    }

    fn token(value: &str) -> Token {
        Token::new(value.to_string()).expect("non-empty fake token")
    }

    fn utf8(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("captured output must be utf-8")
    }

    /// Fake [`CacheWipe`] that always succeeds, recording that it ran.
    #[derive(Default)]
    struct SpyCacheWipe {
        called: std::cell::Cell<bool>,
    }

    impl CacheWipe for SpyCacheWipe {
        fn wipe(&self) -> Result<()> {
            self.called.set(true);
            Ok(())
        }

        fn describe(&self) -> &'static str {
            "cache (test)"
        }
    }

    /// Fake [`CacheWipe`] that always fails, recording that it ran.
    #[derive(Default)]
    struct FailingCacheWipe {
        called: std::cell::Cell<bool>,
    }

    impl CacheWipe for FailingCacheWipe {
        fn wipe(&self) -> Result<()> {
            self.called.set(true);
            Err(AppError::Auth(AuthError::Backend {
                source: keyring::Error::NoStorageAccess("fake cache locked".into()),
            }))
        }

        fn describe(&self) -> &'static str {
            "cache (test)"
        }
    }

    /// Fake [`TokenSource`] whose `clear()` always fails, recording how
    /// many times it was called.
    #[derive(Default)]
    struct FailingClearSource {
        calls: std::cell::Cell<usize>,
    }

    impl TokenSource for FailingClearSource {
        fn load(&self) -> Result<Option<Token>> {
            Ok(None)
        }

        fn store(&self, _token: &Token) -> Result<()> {
            Ok(())
        }

        fn clear(&self) -> Result<()> {
            self.calls.set(self.calls.get() + 1);
            Err(AppError::Auth(AuthError::Backend {
                source: keyring::Error::NoStorageAccess("fake keyring locked".into()),
            }))
        }

        fn describe(&self) -> &'static str {
            "failing-clear (test)"
        }
    }

    #[test]
    fn idempotent_success_with_nothing_stored_prints_summary_and_exits_ok() {
        let store = memory_store();
        let cache = SpyCacheWipe::default();
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_logout_with(&store, &cache, &mut out, &mut err).expect("nothing stored is still ok");

        assert!(cache.called.get(), "cache wipe must still run");
        let summary = utf8(out);
        assert!(
            summary.contains("Cleared stored API token."),
            "summary missing token line: {summary}"
        );
        assert!(
            summary.contains("Cleared cache (test)."),
            "summary missing cache line: {summary}"
        );
        assert!(err.is_empty(), "no failures expected: {err:?}");
    }

    #[test]
    fn success_path_with_a_stored_token_clears_both_backends() {
        let primary = MemoryTokenSource::new();
        primary.store(&token(FIXTURE_TOKEN)).expect("seed primary");
        let store = TokenStore::with_sources(Box::new(primary), Box::new(MemoryTokenSource::new()));
        let cache = SpyCacheWipe::default();
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_logout_with(&store, &cache, &mut out, &mut err).expect("logout must succeed");

        assert!(
            store.load().expect("load ok").is_none(),
            "TokenStore::load must return Ok(None) after logout"
        );
        assert_eq!(err, Vec::<u8>::new(), "no failures expected");
        assert!(
            !utf8(out).contains(FIXTURE_TOKEN),
            "summary leaked the token"
        );
    }

    #[test]
    fn logout_removes_the_fallback_token_file_from_disk() {
        // Acceptance 3: asserted with an injected temp-dir `FileTokenSource`.
        let path = temp_token_path();
        let file_source = FileTokenSource::at(path.clone());
        file_source
            .store(&token(FIXTURE_TOKEN))
            .expect("seed the file backend");
        assert!(path.exists(), "fixture setup must have created the file");

        let store =
            TokenStore::with_sources(Box::new(file_source), Box::new(MemoryTokenSource::new()));
        let cache = SpyCacheWipe::default();
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_logout_with(&store, &cache, &mut out, &mut err).expect("logout must succeed");

        assert!(
            store.load().expect("load ok").is_none(),
            "TokenStore::load must return Ok(None) after logout"
        );
        assert!(
            !path.exists(),
            "the fallback token file must not exist on disk after logout"
        );
        let _ = std::fs::remove_file(&path);
    }

    #[test]
    fn failing_keyring_clear_still_clears_the_file_source_and_runs_the_cache_hook() {
        let failing_primary = FailingClearSource::default();
        let secondary = MemoryTokenSource::new();
        secondary
            .store(&token(FIXTURE_TOKEN))
            .expect("seed secondary");
        let store = TokenStore::with_sources(Box::new(failing_primary), Box::new(secondary));
        let cache = SpyCacheWipe::default();
        let mut out = Vec::new();
        let mut err = Vec::new();

        let result = run_logout_with(&store, &cache, &mut out, &mut err);

        assert!(
            result.is_err(),
            "a failing backend must surface as an error"
        );
        assert!(
            cache.called.get(),
            "the cache hook must still run after a token-clear failure"
        );
        let stderr_text = utf8(err);
        assert!(
            stderr_text.contains("failed to clear stored API token"),
            "stderr must name the failing step: {stderr_text}"
        );
        let stdout_text = utf8(out);
        assert!(
            stdout_text.contains("Cleared cache (test)."),
            "the cache step must still be reported as cleared: {stdout_text}"
        );
    }

    #[test]
    fn failing_cache_wipe_still_clears_the_token_store() {
        let primary = MemoryTokenSource::new();
        primary.store(&token(FIXTURE_TOKEN)).expect("seed primary");
        let store = TokenStore::with_sources(Box::new(primary), Box::new(MemoryTokenSource::new()));
        let cache = FailingCacheWipe::default();
        let mut out = Vec::new();
        let mut err = Vec::new();

        let result = run_logout_with(&store, &cache, &mut out, &mut err);

        assert!(
            result.is_err(),
            "a failing cache wipe must surface as an error"
        );
        assert!(cache.called.get());
        assert!(
            store.load().expect("load ok").is_none(),
            "the token store must still be cleared despite the cache failure"
        );
        let stderr_text = utf8(err);
        assert!(
            stderr_text.contains("failed to clear cache (test)"),
            "stderr must name the failing step: {stderr_text}"
        );
        let stdout_text = utf8(out);
        assert!(
            stdout_text.contains("Cleared stored API token."),
            "the token step must still be reported as cleared: {stdout_text}"
        );
    }

    #[test]
    fn user_facing_output_never_contains_token_material() {
        let primary = MemoryTokenSource::new();
        primary.store(&token(FIXTURE_TOKEN)).expect("seed primary");
        let store = TokenStore::with_sources(Box::new(primary), Box::new(MemoryTokenSource::new()));
        let cache = SpyCacheWipe::default();
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_logout_with(&store, &cache, &mut out, &mut err).expect("logout must succeed");

        let everything = utf8(out) + &utf8(err);
        assert!(
            !everything.contains(FIXTURE_TOKEN),
            "token material leaked into user-facing output: {everything}"
        );
    }

    // ---- no token material in any `tracing` event (acceptance 6) --------

    const TRACING_EVENT_LINES: &[&str] = &[
        r#"tracing::info!(outcome = "cleared", "logout command finished");"#,
        r#"tracing::info!(outcome = "failed", "logout command finished");"#,
    ];

    /// Documented log-scan assertion (see `auth.rs`'s equivalent test for
    /// why a runtime dispatcher-capture test is impossible in this
    /// binary): the `tracing::` event surface of the production half of
    /// this module must be exactly the static allowlist above.
    #[test]
    fn tracing_event_surface_is_static_and_token_free() {
        let source = include_str!("logout.rs");
        let production = source
            .split("#[cfg(test)]")
            .next()
            .expect("module source has a test section");
        let event_lines: Vec<&str> = production
            .lines()
            .map(str::trim)
            .filter(|line| line.starts_with("tracing::"))
            .collect();
        assert_eq!(
            event_lines, TRACING_EVENT_LINES,
            "the logout flow's tracing event surface changed; it must stay static \
             (outcome strings only) and token-free"
        );
    }
}

// ---------------------------------------------------------------------------
// T-108 adversarial suite (sdlc-unit-tester, different model family from the
// coder). Deliberately does NOT re-run the coder's `tests` cases; it attacks
// the seams those cases leave unproven:
//   - exact call counts per backend/cache step (attempted-once, never
//     retried) instead of "the other value survived",
//   - the double-failure path (both token backends *and* the cache hook
//     failing at once) must still surface an error and name both steps,
//   - byte-exact output shapes: the whole success summary in order, an
//     exactly-empty stderr, and a byte-exact failure-line prefix/newline,
//   - `describe()` is called at most once per logout and the value it
//     returned is the one printed (a side-effecting `describe()` cannot
//     desynchronise the summary),
//   - repeated invocations (true idempotency, not just first-call),
//   - leak scan over raw *and* `escape_debug` forms with a canary token
//     distinct from the coder's `FIXTURE_TOKEN`,
//   - a doc-comment drift guard on `NoopCacheWipe`'s EPIC-04 promise.
// Fakes only: no real keyring, no config dir, no cache database, no temp
// files (the coder already covers the on-disk case).
// ---------------------------------------------------------------------------
#[cfg(test)]
mod unit_tests {
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    use super::*;
    use crate::auth::{Token, TokenSource};
    use crate::error::{AppError, AuthError};

    /// Canary bearer material for this suite only — fake, never a real
    /// token, and deliberately different from the coder's `FIXTURE_TOKEN`
    /// so a copy-paste of their assertion cannot mask a leak here.
    const CANARY: &str = "tok_fake_t108_ut_LEAKCANARY_7f3";

    /// `describe()` names returned in call order by [`ProbeCache`], so a
    /// second `describe()` call becomes visible in the output.
    const DESCRIBE_NAMES: [&str; 3] = ["cache (ut #1)", "cache (ut #2)", "cache (ut #3)"];

    /// Inspectable, scriptable [`TokenSource`]: a handle survives boxing so
    /// the test can read exact call counts and raw stored bytes afterwards.
    #[derive(Default)]
    struct SourceState {
        stored: RefCell<Option<String>>,
        load_calls: Cell<usize>,
        clear_calls: Cell<usize>,
        fail_clear: Cell<bool>,
    }

    #[derive(Clone, Default)]
    struct ProbeSource {
        state: Rc<SourceState>,
        marker: &'static str,
    }

    impl ProbeSource {
        fn named(marker: &'static str) -> Self {
            Self {
                state: Rc::new(SourceState::default()),
                marker,
            }
        }

        fn seed(&self, value: &str) {
            *self.state.stored.borrow_mut() = Some(value.to_string());
        }

        fn raw(&self) -> Option<String> {
            self.state.stored.borrow().clone()
        }

        fn clear_calls(&self) -> usize {
            self.state.clear_calls.get()
        }

        fn fail_clear(&self) {
            self.state.fail_clear.set(true);
        }
    }

    impl TokenSource for ProbeSource {
        fn load(&self) -> Result<Option<Token>> {
            self.state.load_calls.set(self.state.load_calls.get() + 1);
            match &*self.state.stored.borrow() {
                Some(raw) => Token::new(raw.clone()).map(Some),
                None => Ok(None),
            }
        }

        fn store(&self, token: &Token) -> Result<()> {
            *self.state.stored.borrow_mut() = Some(token.expose().to_string());
            Ok(())
        }

        fn clear(&self) -> Result<()> {
            self.state.clear_calls.set(self.state.clear_calls.get() + 1);
            if self.state.fail_clear.get() {
                return Err(AppError::Auth(AuthError::Backend {
                    source: keyring::Error::NoStorageAccess(
                        format!("{} locked", self.marker).into(),
                    ),
                }));
            }
            *self.state.stored.borrow_mut() = None;
            Ok(())
        }

        fn describe(&self) -> &'static str {
            "probe (unit_tests)"
        }
    }

    /// Scriptable [`CacheWipe`] counting both `wipe()` and `describe()`
    /// calls, whose `describe()` result changes on every call — so
    /// "call it once and print that value" is provable from the output.
    struct ProbeCache {
        wipe_calls: Cell<usize>,
        describe_calls: Cell<usize>,
        fail: bool,
    }

    impl ProbeCache {
        fn ok() -> Self {
            Self {
                wipe_calls: Cell::new(0),
                describe_calls: Cell::new(0),
                fail: false,
            }
        }

        fn failing() -> Self {
            Self {
                fail: true,
                ..Self::ok()
            }
        }

        fn wipe_calls(&self) -> usize {
            self.wipe_calls.get()
        }

        fn describe_calls(&self) -> usize {
            self.describe_calls.get()
        }
    }

    impl CacheWipe for ProbeCache {
        fn wipe(&self) -> Result<()> {
            self.wipe_calls.set(self.wipe_calls.get() + 1);
            if self.fail {
                return Err(AppError::Auth(AuthError::Backend {
                    source: keyring::Error::NoStorageAccess("fake cache locked".into()),
                }));
            }
            Ok(())
        }

        fn describe(&self) -> &'static str {
            let n = self.describe_calls.get();
            self.describe_calls.set(n + 1);
            DESCRIBE_NAMES
                .get(n)
                .copied()
                .unwrap_or("cache (ut overflow)")
        }
    }

    fn utf8(bytes: Vec<u8>) -> String {
        String::from_utf8(bytes).expect("captured output must be utf-8")
    }

    fn probe_store(primary: &ProbeSource, secondary: &ProbeSource) -> TokenStore {
        TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()))
    }

    // ---- 1. attempted exactly once, never retried -----------------------

    #[test]
    fn a_failing_token_backend_is_attempted_exactly_once_and_is_not_retried() {
        let primary = ProbeSource::named("fake-keyring");
        let secondary = ProbeSource::named("fake-file");
        primary.seed("tok_fake_stuck_in_primary");
        secondary.seed(CANARY);
        primary.fail_clear();
        let store = probe_store(&primary, &secondary);
        let cache = ProbeCache::ok();
        let mut out = Vec::new();
        let mut err = Vec::new();

        let result = run_logout_with(&store, &cache, &mut out, &mut err);

        assert!(result.is_err(), "a failing backend must surface as Err");
        // "Attempted once, still runs the other step" is NOT "retried":
        // one call per backend slot, exactly.
        assert_eq!(
            primary.clear_calls(),
            1,
            "the failing backend must be cleared exactly once (no retry loop)"
        );
        assert_eq!(
            secondary.clear_calls(),
            1,
            "the remaining backend must still be cleared, exactly once"
        );
        assert_eq!(cache.wipe_calls(), 1, "the cache hook runs exactly once");
        assert_eq!(
            secondary.raw(),
            None,
            "a failing primary must not strand the fallback token"
        );
        assert_eq!(
            primary.raw().as_deref(),
            Some("tok_fake_stuck_in_primary"),
            "the failed backend keeps its token (its clear never succeeded)"
        );
        let err_text = utf8(err);
        assert_eq!(
            err_text.lines().count(),
            1,
            "exactly one failing step must be reported: {err_text:?}"
        );
    }

    // ---- 2. double failure: both backends AND the cache hook ------------

    #[test]
    fn simultaneous_token_and_cache_failures_still_error_and_name_both_steps() {
        let primary = ProbeSource::named("fake-keyring");
        let secondary = ProbeSource::named("fake-file");
        primary.fail_clear();
        secondary.fail_clear();
        let store = probe_store(&primary, &secondary);
        let cache = ProbeCache::failing();
        let mut out = Vec::new();
        let mut err = Vec::new();

        let result = run_logout_with(&store, &cache, &mut out, &mut err);

        assert!(
            matches!(result, Err(AppError::Auth(AuthError::Backend { .. }))),
            "a triple failure must not be swallowed or downgraded: {result:?}"
        );
        assert_eq!(
            (
                primary.clear_calls(),
                secondary.clear_calls(),
                cache.wipe_calls()
            ),
            (1, 1, 1),
            "every step is attempted exactly once even when all of them fail"
        );
        let stdout_text = utf8(out);
        assert_eq!(
            stdout_text, "",
            "no success line may be printed when every step failed"
        );
        let stderr_text = utf8(err);
        assert_eq!(
            stderr_text.lines().count(),
            2,
            "both failing steps must be reported (not garbled/dropped): {stderr_text:?}"
        );
        assert!(
            stderr_text.contains("failed to clear stored API token: "),
            "the token step must be named: {stderr_text:?}"
        );
        assert!(
            stderr_text.contains("failed to clear cache (ut #1): "),
            "the cache step must be named: {stderr_text:?}"
        );
    }

    // ---- 3. exact success-output shape ----------------------------------

    #[test]
    fn success_summary_is_byte_exact_ordered_and_stderr_stays_empty() {
        let primary = ProbeSource::named("fake-keyring");
        let secondary = ProbeSource::named("fake-file");
        let store = probe_store(&primary, &secondary);
        let cache = ProbeCache::ok();
        let mut out = Vec::new();
        let mut err = Vec::new();

        // Nothing stored at all: still a success (acceptance 2, idempotent).
        run_logout_with(&store, &cache, &mut out, &mut err).expect("nothing stored is still Ok");

        // Byte-exact: each cleared thing named exactly once, in step order,
        // one line each — no duplicates, no extra prose.
        assert_eq!(
            utf8(out),
            "Cleared stored API token.\nCleared cache (ut #1).\n"
        );
        assert_eq!(
            err,
            Vec::<u8>::new(),
            "success must write zero stderr bytes"
        );
        assert_eq!(cache.describe_calls(), 1);
    }

    // ---- 4. byte-exact failure-line pin ---------------------------------

    #[test]
    fn token_failure_line_is_byte_exact() {
        let primary = ProbeSource::named("fake-keyring");
        primary.fail_clear();
        let secondary = ProbeSource::named("fake-file");
        let store = probe_store(&primary, &secondary);
        let cache = ProbeCache::ok();
        let mut out = Vec::new();
        let mut err = Vec::new();

        let _ = run_logout_with(&store, &cache, &mut out, &mut err);

        // Wording drift (dropped prefix, missing/extra newline, re-wrapped
        // sentence) fails here; the inner text is derived, not retyped, so
        // only this module's own wording is pinned.
        let expected_inner = AppError::Auth(AuthError::Backend {
            source: keyring::Error::NoStorageAccess("fake-keyring locked".into()),
        })
        .to_string();
        let err_text = utf8(err);
        assert_eq!(
            err_text,
            format!("failed to clear stored API token: {expected_inner}\n")
        );
        // The greppable prefix on its own, byte-exact, in case the derived
        // tail ever drifts legitimately.
        assert!(
            err_text.starts_with("failed to clear stored API token: "),
            "greppable failure prefix drifted: {err_text:?}"
        );
    }

    // ---- 5. describe() called once per step, value reused ----------------

    #[test]
    fn describe_is_called_once_per_logout_and_the_printed_name_is_that_call() {
        // Success path.
        let primary = ProbeSource::named("fake-keyring");
        let store = probe_store(&primary, &ProbeSource::named("fake-file"));
        let cache = ProbeCache::ok();
        let mut out = Vec::new();
        let mut err = Vec::new();
        run_logout_with(&store, &cache, &mut out, &mut err).expect("logout ok");
        assert_eq!(
            cache.describe_calls(),
            1,
            "describe() must be called exactly once, not once per branch"
        );
        let success_out = utf8(out);
        assert!(
            success_out.contains("Cleared cache (ut #1)."),
            "the printed name must be the first describe() result: {success_out:?}"
        );
        assert!(
            !success_out.contains("cache (ut #2)"),
            "a second describe() call leaked into the output: {success_out:?}"
        );
        assert_eq!(err, Vec::<u8>::new());

        // Failure path: same single call, same printed value.
        let cache = ProbeCache::failing();
        let mut out = Vec::new();
        let mut err = Vec::new();
        let _ = run_logout_with(&store, &cache, &mut out, &mut err);
        assert_eq!(
            cache.describe_calls(),
            1,
            "the failure branch must reuse the same describe() result"
        );
        let failure_err = utf8(err);
        assert!(
            failure_err.contains("failed to clear cache (ut #1): "),
            "failure line must name the first describe() result: {failure_err:?}"
        );
        assert!(
            !failure_err.contains("cache (ut #2)"),
            "a second describe() call leaked into stderr: {failure_err:?}"
        );
    }

    // ---- 6. true re-entrancy over the same store + cache ----------------

    #[test]
    fn logout_is_reentrant_on_an_already_cleared_store_and_cache() {
        let primary = ProbeSource::named("fake-keyring");
        let secondary = ProbeSource::named("fake-file");
        primary.seed(CANARY);
        secondary.seed(CANARY);
        let store = probe_store(&primary, &secondary);
        // Wipe and describe both count, so the second run must repeat both
        // steps (idempotent, not short-circuited by stale state).
        let cache = ProbeCache::ok();
        let mut out = Vec::new();
        let mut err = Vec::new();

        run_logout_with(&store, &cache, &mut out, &mut err).expect("first logout ok");
        run_logout_with(&store, &cache, &mut out, &mut err).expect("second logout must also be Ok");

        assert!(
            store.load().expect("load ok").is_none(),
            "store must stay empty after repeated logout"
        );
        assert_eq!(
            (
                primary.clear_calls(),
                secondary.clear_calls(),
                cache.wipe_calls()
            ),
            (2, 2, 2),
            "each run must perform every step exactly once"
        );
        assert_eq!(
            err,
            Vec::<u8>::new(),
            "a repeated logout must not report failures"
        );
        let summary = utf8(out);
        assert_eq!(
            summary.matches("Cleared stored API token.").count(),
            2,
            "one token line per run, no duplicates: {summary:?}"
        );
        assert_eq!(summary.matches("Cleared cache").count(), 2);
    }

    // ---- 7. leak scan: raw *and* escaped forms, both streams -------------

    #[test]
    fn no_canary_token_material_reaches_output_in_raw_or_escaped_form() {
        // Include a control-character/escape-bearing value so the
        // `escape_debug` form differs from the raw bytes.
        let control_bearing = "tok_fake_t108_ut_\"ctl\\\u{7}_canary2";
        for canary in [CANARY, control_bearing] {
            // Failure path exercises the error interpolation too.
            for failing in [false, true] {
                let primary = ProbeSource::named("fake-keyring");
                let secondary = ProbeSource::named("fake-file");
                primary.seed(canary);
                secondary.seed(canary);
                if failing {
                    primary.fail_clear();
                }
                let store = probe_store(&primary, &secondary);
                let cache = if failing {
                    ProbeCache::failing()
                } else {
                    ProbeCache::ok()
                };
                let mut out = Vec::new();
                let mut err = Vec::new();
                let _ = run_logout_with(&store, &cache, &mut out, &mut err);

                let everything = utf8(out) + &utf8(err);
                assert!(
                    !everything.is_empty() || failing,
                    "output should not be silently empty"
                );
                let escaped = canary.escape_debug().to_string();
                assert!(
                    !everything.contains(canary),
                    "raw token material leaked (failing={failing}): {everything:?}"
                );
                assert!(
                    !everything.contains(&escaped),
                    "escaped token material leaked (failing={failing}): {everything:?}"
                );
            }
        }
    }

    // ---- 8. NoopCacheWipe doc-comment drift guard ------------------------

    #[test]
    fn noop_cache_wipe_doc_comment_keeps_the_epic_04_promise() {
        // Acceptance 1 requires the doc comment to state that EPIC-04
        // replaces this with the real SQLite `delete_all()` and that it
        // currently deletes nothing; `cache_wipe.rs`'s own tests never
        // assert that prose, so pin it here. The scan is scoped to
        // `NoopCacheWipe`'s own doc block (from its `pub struct` line
        // backwards to the preceding `/// Placeholder` line) so the same
        // phrases surviving in the *module* docs cannot mask a rewrite of
        // the type's doc.
        let source = include_str!("../auth/cache_wipe.rs");
        let start = source
            .find("/// Placeholder [`CacheWipe`]")
            .expect("NoopCacheWipe's doc block must still open with `/// Placeholder`");
        let end = source
            .find("pub struct NoopCacheWipe;")
            .expect("NoopCacheWipe must still exist");
        assert!(start < end, "doc block must precede the struct definition");
        let doc = &source[start..end];
        for phrase in ["EPIC-04", "delete_all", "deletes nothing"] {
            assert!(
                doc.contains(phrase),
                "NoopCacheWipe's own doc comment lost the phrase {phrase:?}: {doc:?}"
            );
        }
        // And the placeholder must stay inert + self-describing.
        assert!(NoopCacheWipe.wipe().is_ok(), "no-op wipe must stay Ok(())");
        assert!(
            NoopCacheWipe.describe().contains("not yet implemented"),
            "placeholder must advertise itself as unfinished: {:?}",
            NoopCacheWipe.describe()
        );
    }

    // ---- 9. dispatch routes Logout here, not to NotImplemented ----------

    #[test]
    fn dispatch_routes_logout_to_this_module_instead_of_not_implemented() {
        // Acceptance 5. A behavioral assertion is impossible here: the
        // real `run_logout()` resolves the OS keyring and the real config
        // dir, which a unit test must not touch. So pin the wiring at the
        // source level and prove the route is not the stub arm.
        let commands = include_str!("commands.rs");
        assert!(
            commands.contains("Commands::Logout => run_logout()"),
            "dispatch must route Commands::Logout to run_logout()"
        );
        assert!(
            !commands.contains("NotImplemented(\"logout\")"),
            "logout must not still be a NotImplemented stub"
        );
        // Every remaining NotImplemented arm is a *different* subcommand,
        // so the stub list cannot silently grow `logout` back.
        for stub in ["sync", "add", "today"] {
            assert!(
                commands.contains(&format!("NotImplemented(\"{stub}\")")),
                "{stub} should still be a stub in this task's scope"
            );
        }
        assert!(
            commands.contains("use crate::cli::logout::run_logout;"),
            "dispatch must import this module's run_logout"
        );
    }
}
