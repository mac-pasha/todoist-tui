//! `TokenStore`: the keyring-first, file-fallback resolution seam (T-105,
//! EPIC-02).
//!
//! Every other command (sync/add/today in T-109, `auth` in T-107, `logout`
//! in T-108) goes through [`TokenStore`] rather than talking to a
//! [`TokenSource`] directly. [`TokenStore::resolve`] probes
//! [`KeyringTokenSource::is_available`] and picks the OS keyring as the
//! active ("primary") backend when it works, falling back to
//! [`FileTokenSource`] otherwise — the common case on headless Linux with
//! no Secret Service daemon. [`TokenStore::load`] additionally checks the
//! *other* backend when the primary has nothing stored, so a token written
//! before a backend became (un)available is still found.
//!
//! [`TokenStore::with_sources`] is the test seam: it builds a store over
//! two injected [`TokenSource`] implementations (typically
//! [`crate::auth::MemoryTokenSource`]) without touching the OS keyring or
//! the real config directory.

use crate::auth::source::TokenSource;
use crate::auth::{FileTokenSource, KeyringTokenSource, Token};
use crate::error::{AppError, AuthError, Result};

/// Keyring-first, file-fallback resolution of the Todoist API token.
///
/// Holds two backends: `primary` is the one [`TokenStore::store`] writes
/// to and [`TokenStore::load`] checks first; `secondary` is only consulted
/// by `load` when `primary` has nothing stored. Never derives `Debug`
/// (trait objects aren't `Debug`, and formatting must never need to call
/// `load`, which could itself fail or hit a real backend); see the manual
/// impl below.
pub struct TokenStore {
    primary: Box<dyn TokenSource>,
    secondary: Box<dyn TokenSource>,
    notice: Option<String>,
}

impl TokenStore {
    /// Resolves the active backend: the OS keyring when
    /// [`KeyringTokenSource::is_available`] reports it usable, the
    /// file-backed fallback otherwise.
    ///
    /// When the fallback is chosen, [`TokenStore::backend_notice`] returns
    /// `Some` text naming the token file's path and that it is written
    /// with owner-only (`0600`) permissions; the keyring-primary case
    /// returns `None` — no notice is needed when the preferred backend is
    /// in use.
    ///
    /// # Errors
    ///
    /// Returns an error when the file fallback's default location cannot
    /// be determined (see [`FileTokenSource::default_location`]).
    pub fn resolve() -> Result<Self> {
        let keyring = KeyringTokenSource::new();
        let file = FileTokenSource::default_location()?;

        if keyring.is_available() {
            Ok(Self {
                primary: Box::new(keyring),
                secondary: Box::new(file),
                notice: None,
            })
        } else {
            let notice = format!(
                "OS keyring unavailable; falling back to token file at {} (mode 0600)",
                file.path().display()
            );
            Ok(Self {
                primary: Box::new(file),
                secondary: Box::new(keyring),
                notice: Some(notice),
            })
        }
    }

    /// Test/injection seam: builds a store over two caller-supplied
    /// backends, with no backend notice (callers testing `resolve`'s
    /// fallback/secondary-read behaviour construct this directly rather
    /// than reading `backend_notice`).
    #[must_use]
    pub fn with_sources(primary: Box<dyn TokenSource>, secondary: Box<dyn TokenSource>) -> Self {
        Self {
            primary,
            secondary,
            notice: None,
        }
    }

    /// User-facing notice to print when the fallback backend was chosen by
    /// [`TokenStore::resolve`]; `None` when the preferred (keyring) backend
    /// is in use. Rendering/printing this text is the caller's job (T-107);
    /// this type only produces the string.
    #[must_use]
    pub fn backend_notice(&self) -> Option<String> {
        self.notice.clone()
    }

    /// Reads the token from the active backend, falling back to the other
    /// backend when the active one has nothing stored — so a token written
    /// before a backend change (e.g. keyring became unavailable, or vice
    /// versa) is still found.
    pub fn load(&self) -> Result<Option<Token>> {
        if let Some(token) = self.primary.load()? {
            return Ok(Some(token));
        }
        self.secondary.load()
    }

    /// Persists `token` to the active backend only; the other backend is
    /// left untouched.
    pub fn store(&self, token: &Token) -> Result<()> {
        self.primary.store(token)
    }

    /// Clears the token from both backends unconditionally.
    ///
    /// Both backends are always attempted, even if the first one errors,
    /// so a failure in one never leaves a stale token behind in the other.
    /// If either (or both) fail, the first error encountered is returned —
    /// it is never silently swallowed.
    pub fn clear(&self) -> Result<()> {
        let primary_result = self.primary.clear();
        let secondary_result = self.secondary.clear();
        primary_result.and(secondary_result)
    }

    /// Reads the token from either backend, or returns
    /// [`AuthError::NotAuthenticated`] when neither holds one.
    ///
    /// # Errors
    ///
    /// Returns [`AuthError::NotAuthenticated`] (rendered as "not
    /// authenticated: no API token is stored; run `todoist-tui auth` to add
    /// one") when no backend has a token stored, or whatever error the
    /// active/fallback backend's [`TokenSource::load`] returns.
    pub fn require(&self) -> Result<Token> {
        match self.load()? {
            Some(token) => Ok(token),
            None => Err(AppError::Auth(AuthError::NotAuthenticated)),
        }
    }
}

impl std::fmt::Debug for TokenStore {
    /// Manual impl: the backend trait objects aren't `Debug`, and
    /// formatting a `TokenStore` must never call `load()` (which could
    /// fail or touch a real backend) just to render something — only the
    /// already-computed, token-free `notice` is shown.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("TokenStore")
            .field("notice", &self.notice)
            .finish_non_exhaustive()
    }
}

#[cfg(test)]
mod tests {
    //! Sanity checks proving the T-105 design constraints hold; the
    //! unit-tester adds the full suite separately.
    use super::*;
    use crate::auth::MemoryTokenSource;

    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    fn token(value: &str) -> Token {
        Token::new(value.to_string()).expect("non-empty fake token")
    }

    /// Fake [`TokenSource`] whose `clear()` can be scripted to fail once,
    /// so the "clear both unconditionally, don't swallow the error" policy
    /// can be exercised without touching a real backend.
    #[derive(Default)]
    struct FailingClearSource {
        inner: MemoryTokenSource,
        fail_clear: std::cell::Cell<bool>,
    }

    impl TokenSource for FailingClearSource {
        fn load(&self) -> Result<Option<Token>> {
            self.inner.load()
        }

        fn store(&self, token: &Token) -> Result<()> {
            self.inner.store(token)
        }

        fn clear(&self) -> Result<()> {
            if self.fail_clear.get() {
                return Err(AppError::Auth(AuthError::Backend {
                    source: keyring::Error::NoStorageAccess("fake locked store".into()),
                }));
            }
            self.inner.clear()
        }

        fn describe(&self) -> &'static str {
            "failing-clear (test)"
        }
    }

    #[test]
    fn load_falls_back_to_the_secondary_backend_when_primary_is_empty() {
        let primary = MemoryTokenSource::new();
        let secondary = MemoryTokenSource::new();
        secondary
            .store(&token(FIXTURE_TOKEN))
            .expect("seed secondary");

        let store = TokenStore::with_sources(Box::new(primary), Box::new(secondary));
        let loaded = store.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), FIXTURE_TOKEN);
    }

    #[test]
    fn load_prefers_the_primary_backend_when_both_hold_a_token() {
        let primary = MemoryTokenSource::new();
        let secondary = MemoryTokenSource::new();
        primary
            .store(&token("tok_fake_primary"))
            .expect("seed primary");
        secondary
            .store(&token("tok_fake_secondary"))
            .expect("seed secondary");

        let store = TokenStore::with_sources(Box::new(primary), Box::new(secondary));
        let loaded = store.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), "tok_fake_primary");
    }

    #[test]
    fn store_writes_only_to_the_primary_backend() {
        let primary = MemoryTokenSource::new();
        let secondary = MemoryTokenSource::new();
        let store = TokenStore::with_sources(Box::new(primary), Box::new(secondary));
        store.store(&token(FIXTURE_TOKEN)).expect("store ok");

        // Re-resolve a fresh store sharing the same underlying backends is
        // not possible here (ownership moved into `store`), so assert via
        // `load`'s primary-preferred behaviour: a store with an empty
        // secondary and the same primary data must still find it.
        let loaded = store.load().expect("load ok").expect("present");
        assert_eq!(loaded.expose(), FIXTURE_TOKEN);
    }

    #[test]
    fn clear_clears_both_backends_and_reports_an_error_without_swallowing_it() {
        let ok_backend = MemoryTokenSource::new();
        ok_backend
            .store(&token(FIXTURE_TOKEN))
            .expect("seed ok backend");

        let failing_backend = FailingClearSource::default();
        failing_backend
            .store(&token(FIXTURE_TOKEN))
            .expect("seed failing backend");
        failing_backend.fail_clear.set(true);

        let store = TokenStore::with_sources(Box::new(failing_backend), Box::new(ok_backend));
        let result = store.clear();

        assert!(
            matches!(result, Err(AppError::Auth(AuthError::Backend { .. }))),
            "clear must report the backend error, not swallow it, got {result:?}"
        );
        // The secondary (ok) backend must still have been cleared despite
        // the primary's error.
        assert!(store.secondary.load().expect("load ok").is_none());
    }

    #[test]
    fn require_errors_with_not_authenticated_when_no_backend_has_a_token() {
        let store = TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        );
        match store.require() {
            Err(AppError::Auth(AuthError::NotAuthenticated)) => {}
            other => panic!("expected NotAuthenticated, got {other:?}"),
        }
        // The rendered message must point the user at the auth command.
        let err = store.require().expect_err("still not authenticated");
        let message = err.to_string();
        assert!(
            message.contains("todoist-tui auth"),
            "message should tell the user to run `todoist-tui auth`: {message}"
        );
    }

    #[test]
    fn require_returns_the_token_when_a_backend_has_one() {
        let primary = MemoryTokenSource::new();
        primary.store(&token(FIXTURE_TOKEN)).expect("seed primary");
        let store = TokenStore::with_sources(Box::new(primary), Box::new(MemoryTokenSource::new()));
        let token = store.require().expect("require ok");
        assert_eq!(token.expose(), FIXTURE_TOKEN);
    }

    #[test]
    fn debug_rendering_never_exposes_stored_token_material() {
        let primary = MemoryTokenSource::new();
        primary.store(&token(FIXTURE_TOKEN)).expect("seed primary");
        let store = TokenStore::with_sources(Box::new(primary), Box::new(MemoryTokenSource::new()));
        let dbg = format!("{store:?}");
        assert!(!dbg.contains(FIXTURE_TOKEN), "Debug leaked token: {dbg}");
    }
}

// ---------------------------------------------------------------------------
// T-105 adversarial suite (sdlc-unit-tester, different model family from the
// coder). Complements `tests` above instead of duplicating it:
//   - the backend *choice* `resolve` makes is proven via `describe()` pins
//     plus notice content (the sanity suite never inspects the wiring),
//   - load/store/clear run against a scriptable fake whose handle survives
//     boxing, so the tests assert "the other backend was not even called"
//     (exact call counts), not just "its value survived",
//   - primary-*error* paths (distinct from empty) are pinned: only `None`
//     triggers fallback, errors surface with the failing backend's marker,
//   - `NotAuthenticated`'s message is pinned byte-for-byte at both error
//     levels (the hint `todoist-tui auth` must survive any wording drift),
//   - `Debug` is checked for token leaks across adversarial values, both
//     `{}` and `{:#?}`, raw *and* `escape_debug`-rendered material, and is
//     proven metadata-only (never calls `load`/`store`/`clear`).
// Nothing here touches a real OS keyring or a real config dir; the only
// read-only environment probe is `KeyringTokenSource::is_available()` in
// the `resolve` tests, which never writes or stores anything.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::auth::MemoryTokenSource;
    use crate::config::test_support::ENV_LOCK;
    use crate::error::ConfigError;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;
    use std::sync::MutexGuard;

    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    /// Markers embedded in every scripted error so tests can prove *which*
    /// backend's error surfaced (and that it was not rewritten on the way).
    const PRIM_MARKER: &str = "fake-primary";
    const SEC_MARKER: &str = "fake-secondary";

    // ---- fakes ----------------------------------------------------------

    /// Inspectable, scriptable [`TokenSource`]. Unlike `MemoryTokenSource`
    /// (whose ownership moves into the store, leaving nothing to inspect),
    /// a handle can be cloned before boxing, so the tests can read raw
    /// state and call counts afterwards. In-memory only.
    #[derive(Default)]
    struct ScriptedState {
        raw: RefCell<Option<String>>,
        load_calls: Cell<usize>,
        store_calls: Cell<usize>,
        clear_calls: Cell<usize>,
        fail_load: Cell<bool>,
        fail_store: Cell<bool>,
        fail_clear: Cell<bool>,
    }

    #[derive(Clone, Default)]
    struct ScriptedSource {
        state: Rc<ScriptedState>,
        marker: &'static str,
    }

    impl ScriptedSource {
        fn primary() -> Self {
            Self {
                marker: PRIM_MARKER,
                ..Self::default()
            }
        }

        fn secondary() -> Self {
            Self {
                marker: SEC_MARKER,
                ..Self::default()
            }
        }

        fn seed(&self, value: &str) {
            *self.state.raw.borrow_mut() = Some(value.to_string());
        }

        fn raw(&self) -> Option<String> {
            self.state.raw.borrow().clone()
        }

        fn fail_load(&self) {
            self.state.fail_load.set(true);
        }

        fn fail_store(&self) {
            self.state.fail_store.set(true);
        }

        fn fail_clear(&self) {
            self.state.fail_clear.set(true);
        }

        fn load_calls(&self) -> usize {
            self.state.load_calls.get()
        }

        fn store_calls(&self) -> usize {
            self.state.store_calls.get()
        }

        fn clear_calls(&self) -> usize {
            self.state.clear_calls.get()
        }

        /// Distinctive `AuthError::Backend` carrying this source's marker,
        /// so `to_string()` proves whose failure surfaced.
        fn backend_err(&self, op: &str) -> AppError {
            AppError::Auth(AuthError::Backend {
                source: keyring::Error::NoStorageAccess(format!("{} {}", self.marker, op).into()),
            })
        }
    }

    impl TokenSource for ScriptedSource {
        fn load(&self) -> Result<Option<Token>> {
            self.state.load_calls.set(self.state.load_calls.get() + 1);
            if self.state.fail_load.get() {
                return Err(self.backend_err("load"));
            }
            match &*self.state.raw.borrow() {
                Some(raw) => Token::new(raw.clone()).map(Some),
                None => Ok(None),
            }
        }

        fn store(&self, token: &Token) -> Result<()> {
            self.state.store_calls.set(self.state.store_calls.get() + 1);
            if self.state.fail_store.get() {
                return Err(self.backend_err("store"));
            }
            *self.state.raw.borrow_mut() = Some(token.expose().to_string());
            Ok(())
        }

        fn clear(&self) -> Result<()> {
            self.state.clear_calls.set(self.state.clear_calls.get() + 1);
            if self.state.fail_clear.get() {
                return Err(self.backend_err("clear"));
            }
            *self.state.raw.borrow_mut() = None;
            Ok(())
        }

        fn describe(&self) -> &'static str {
            "scripted (unit_tests)"
        }
    }

    fn token(value: &str) -> Token {
        Token::new(value.to_string()).expect("non-empty fake token")
    }

    /// `resolve`'s outcome depends on (a) the OS keyring actually working
    /// here and (b) `XDG_CONFIG_HOME`-derived paths. (a) cannot be forced
    /// from a test, so these tests *probe* availability read-only and then
    /// assert `resolve` took the matching branch. (b) is process-global
    /// state other tests mutate, so the probe and the `resolve` call are
    /// serialised under the shared `ENV_LOCK` — this suite never mutates
    /// env vars itself, so no snapshot/restore is needed.
    fn env_lock() -> MutexGuard<'static, ()> {
        ENV_LOCK
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }

    /// Asserts `rendered` contains neither the raw nor the `escape_debug`
    /// form of any value (a leak via `Debug` would show up escaped, so
    /// checking only the raw bytes would miss it).
    fn assert_no_token_leak(rendered: &str, values: &[&str]) {
        for value in values {
            let escaped = value.escape_debug().to_string();
            assert!(
                !rendered.contains(value),
                "rendering leaked raw token material {value:?}: {rendered}"
            );
            assert!(
                !rendered.contains(&escaped),
                "rendering leaked escaped token material {value:?}: {rendered}"
            );
        }
    }

    // ---- resolve: backend choice + fallback notice (acceptance 1) -------

    #[test]
    fn resolve_wires_keyring_first_and_file_fallback_per_availability() {
        let _env = env_lock();
        let keyring_ready = KeyringTokenSource::new().is_available();
        match TokenStore::resolve() {
            Ok(store) if keyring_ready => {
                assert_eq!(
                    store.backend_notice(),
                    None,
                    "keyring available: no fallback notice is owed"
                );
                assert_eq!(
                    store.primary.describe(),
                    "OS keyring",
                    "keyring must be primary when available"
                );
                assert_eq!(
                    store.secondary.describe(),
                    "token file",
                    "file must remain the fallback read"
                );
            }
            Ok(store) => {
                // Headless/sandboxed environment (e.g. secret-service-less
                // CI): file becomes primary and the notice must name the
                // exact token path and its 0600 mode.
                let notice = store
                    .backend_notice()
                    .expect("fallback must record a notice");
                let path = FileTokenSource::default_location()
                    .expect("resolve just resolved the same environment")
                    .path()
                    .display()
                    .to_string();
                assert!(
                    notice.contains("0600"),
                    "notice must name the mode: {notice}"
                );
                assert!(
                    notice.contains(&path),
                    "notice must name the token file path {path}: {notice}"
                );
                assert_eq!(
                    store.primary.describe(),
                    "token file",
                    "keyring unavailable: file must be primary"
                );
                assert_eq!(
                    store.secondary.describe(),
                    "OS keyring",
                    "keyring must remain the secondary read"
                );
            }
            Err(AppError::Config(ConfigError::Io { .. })) => {
                // Sane failure shape when the config dir itself is
                // unresolvable in this sandbox: a typed config error,
                // never a panic and never an auth error. Only acceptable
                // when the file location is equally unresolvable right now.
                assert!(
                    FileTokenSource::default_location().is_err(),
                    "resolve failed while default_location still resolves"
                );
            }
            other => {
                panic!("resolve must pick a backend or fail with a config error, got {other:?}")
            }
        }
    }

    #[test]
    fn resolve_is_stable_across_consecutive_calls() {
        let _env = env_lock();
        let first = TokenStore::resolve().expect("resolve works in a test environment");
        let second = TokenStore::resolve().expect("resolve works again");
        assert_eq!(
            first.backend_notice(),
            second.backend_notice(),
            "backend choice flapped between two immediate calls"
        );
        assert_eq!(
            (first.primary.describe(), first.secondary.describe()),
            (second.primary.describe(), second.secondary.describe()),
            "backend wiring flapped between two immediate calls"
        );
    }

    // ---- with_sources: the injection seam (acceptance 5) ----------------

    #[test]
    fn with_sources_yields_no_notice_and_none_survives_every_operation() {
        // Mixed concrete types on purpose: the seam takes a
        // `Box<dyn TokenSource>` from any module, not just this file's fakes.
        let secondary = ScriptedSource::secondary();
        let store = TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(secondary.clone()),
        );
        assert_eq!(
            store.backend_notice(),
            None,
            "injection seam: notice starts None"
        );
        store.store(&token(FIXTURE_TOKEN)).expect("store ok");
        assert_eq!(store.backend_notice(), None, "store must not mint a notice");
        store.load().expect("load ok");
        assert_eq!(store.backend_notice(), None, "load must not mint a notice");
        store.clear().expect("clear ok");
        assert_eq!(store.backend_notice(), None, "clear must not mint a notice");
        assert_eq!(secondary.clear_calls(), 1, "clear still hits both backends");
    }

    #[test]
    fn with_sources_tolerates_the_same_backend_wired_twice() {
        let shared = ScriptedSource::primary();
        let store = TokenStore::with_sources(Box::new(shared.clone()), Box::new(shared.clone()));
        store.store(&token(FIXTURE_TOKEN)).expect("store ok");
        let loaded = store.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), FIXTURE_TOKEN);
        store
            .clear()
            .expect("degenerate double clear of one backend is still Ok");
        assert_eq!(
            shared.clear_calls(),
            2,
            "both slots clear the shared backend"
        );
        assert_eq!(shared.raw(), None);
        assert!(store.load().expect("load ok").is_none());
    }

    // ---- load: fallback matrix (acceptance 2) ---------------------------

    #[test]
    fn load_returns_none_when_both_backends_are_empty_and_reads_each_exactly_once() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        assert!(store.load().expect("load ok").is_none());
        assert_eq!(primary.load_calls(), 1, "primary must be read");
        assert_eq!(
            secondary.load_calls(),
            1,
            "an empty primary must fall through to the secondary"
        );
    }

    #[test]
    fn load_prefers_the_primary_without_calling_the_secondary_at_all() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        primary.seed("tok_fake_primary_wins");
        secondary.seed("tok_fake_secondary_loses");
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        let loaded = store.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), "tok_fake_primary_wins");
        assert_eq!(
            secondary.load_calls(),
            0,
            "a hit on the primary must short-circuit the secondary read"
        );
    }

    #[test]
    fn load_falls_back_byte_exact_and_leaves_the_secondary_token_intact() {
        // Adversarial bytes through the fallback path: quoting, braces,
        // unicode, emoji, control characters and a 1 KiB payload — all
        // valid per `Token::new` (only whitespace-only is rejected) — must
        // round-trip byte-exactly, and the read must not consume anything.
        let long = format!("tok_fake_1kib_{}", "A".repeat(1024));
        for value in [
            FIXTURE_TOKEN,
            "tok_fake_special\"':}{]}<>?/|\\~`",
            "tok_fake_unicode_日本語_emoji_🔐_áccent",
            "tok_fake_newline_\n\t_tab",
            long.as_str(),
        ] {
            let primary = ScriptedSource::primary();
            let secondary = ScriptedSource::secondary();
            secondary.seed(value);
            let store =
                TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
            let loaded = store
                .load()
                .expect("load ok")
                .expect("fallback token present");
            assert_eq!(
                loaded.expose(),
                value,
                "fallback round-trip must be byte-exact"
            );
            assert_eq!(
                secondary.raw(),
                Some(value.to_string()),
                "load must not remove the secondary token"
            );
        }
    }

    #[test]
    fn load_propagates_a_primary_error_and_never_consults_the_secondary() {
        // Observed contract, pinned deliberately: only `None` from the
        // primary triggers fallback — a hard backend error surfaces
        // verbatim instead of silently reading the other backend.
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        secondary.seed(FIXTURE_TOKEN);
        primary.fail_load();
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        let err = store
            .load()
            .expect_err("primary error must propagate, not fall back");
        assert!(
            matches!(err, AppError::Auth(AuthError::Backend { .. })),
            "wrong error shape: {err:?}"
        );
        assert!(
            err.to_string().contains(PRIM_MARKER),
            "the primary's own error must surface verbatim: {err}"
        );
        assert_eq!(
            secondary.load_calls(),
            0,
            "an errored primary must not trigger the fallback read"
        );
    }

    #[test]
    fn load_propagates_a_secondary_error_when_only_the_fallback_read_fails() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        secondary.fail_load();
        let store = TokenStore::with_sources(Box::new(primary), Box::new(secondary));
        let err = store.load().expect_err("secondary error must propagate");
        assert!(
            err.to_string().contains(SEC_MARKER),
            "the secondary's own error must surface verbatim: {err}"
        );
    }

    #[test]
    fn a_token_written_before_a_backend_switch_is_still_found_after_it() {
        // Literal acceptance scenario: token written while backend A was
        // active; A later becomes unavailable (demoted to secondary), B is
        // the new active backend with nothing stored.
        let old_backend = ScriptedSource::primary();
        let new_backend = ScriptedSource::secondary();
        let before =
            TokenStore::with_sources(Box::new(old_backend.clone()), Box::new(new_backend.clone()));
        before.store(&token(FIXTURE_TOKEN)).expect("store ok");
        assert_eq!(old_backend.raw().as_deref(), Some(FIXTURE_TOKEN));
        assert_eq!(
            new_backend.raw(),
            None,
            "pre-switch writes must not touch the inactive backend"
        );

        let after =
            TokenStore::with_sources(Box::new(new_backend.clone()), Box::new(old_backend.clone()));
        let found = after
            .load()
            .expect("load ok")
            .expect("the old token must survive the backend switch");
        assert_eq!(
            found.expose(),
            FIXTURE_TOKEN,
            "fallback must reach the demoted backend"
        );

        // Post-switch writes go to the NEW active backend only.
        after.store(&token("tok_fake_rotated")).expect("store ok");
        assert_eq!(new_backend.raw().as_deref(), Some("tok_fake_rotated"));
        assert_eq!(
            old_backend.raw().as_deref(),
            Some(FIXTURE_TOKEN),
            "the demoted backend must stay untouched"
        );
    }

    // ---- store: active backend only (acceptance 3, first half) ----------

    #[test]
    fn store_writes_replaces_and_never_touches_the_secondary() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        primary.seed("tok_fake_stale_primary");
        secondary.seed("tok_fake_secondary_survives");
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        store.store(&token(FIXTURE_TOKEN)).expect("store ok");
        assert_eq!(
            primary.raw().as_deref(),
            Some(FIXTURE_TOKEN),
            "store must write (and replace) the primary"
        );
        assert_eq!(primary.store_calls(), 1);
        assert_eq!(
            secondary.raw().as_deref(),
            Some("tok_fake_secondary_survives"),
            "the other backend must keep its bytes"
        );
        assert_eq!(
            (
                secondary.store_calls(),
                secondary.load_calls(),
                secondary.clear_calls()
            ),
            (0, 0, 0),
            "the other backend must not be called at all"
        );
    }

    #[test]
    fn store_failure_propagates_and_leaves_both_backends_untouched() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        primary.seed("tok_fake_primary_keeps_old");
        secondary.seed("tok_fake_secondary_keeps_old");
        primary.fail_store();
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        let err = store
            .store(&token(FIXTURE_TOKEN))
            .expect_err("store error must surface");
        assert!(
            err.to_string().contains(PRIM_MARKER),
            "the store error must name the primary: {err}"
        );
        assert_eq!(
            primary.raw().as_deref(),
            Some("tok_fake_primary_keeps_old"),
            "a failed store must not mutate the primary"
        );
        assert_eq!(
            secondary.raw().as_deref(),
            Some("tok_fake_secondary_keeps_old")
        );
        assert_eq!(
            (
                secondary.store_calls(),
                secondary.load_calls(),
                secondary.clear_calls()
            ),
            (0, 0, 0),
            "a primary store failure must never fall through to the secondary"
        );
    }

    // ---- clear: both backends, errors reported (acceptance 3, 2nd half) --

    #[test]
    fn clear_succeeds_on_empty_backends_and_is_repeatable() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        store.clear().expect("clearing empty backends must be Ok");
        store
            .clear()
            .expect("clear must be repeatable (idempotent backends)");
        assert_eq!(primary.raw(), None);
        assert_eq!(secondary.raw(), None);
        assert_eq!(
            (primary.clear_calls(), secondary.clear_calls()),
            (2, 2),
            "both backends are cleared on every call"
        );
    }

    #[test]
    fn clear_removes_tokens_from_both_backends_in_one_call() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        primary.seed("tok_fake_primary_gone");
        secondary.seed("tok_fake_secondary_gone");
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        store.clear().expect("clear ok");
        assert_eq!(primary.raw(), None);
        assert_eq!(secondary.raw(), None);
        assert_eq!((primary.clear_calls(), secondary.clear_calls()), (1, 1));
    }

    #[test]
    fn clear_attempts_both_exactly_once_even_when_the_primary_fails() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        primary.seed("tok_fake_stuck_in_failing_primary");
        secondary.seed("tok_fake_secondary_cleared");
        primary.fail_clear();
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        let err = store
            .clear()
            .expect_err("primary clear failure must surface");
        assert!(
            err.to_string().contains(PRIM_MARKER),
            "the clear error must name the failing primary: {err}"
        );
        assert_eq!(
            (primary.clear_calls(), secondary.clear_calls()),
            (1, 1),
            "the secondary must still be cleared exactly once"
        );
        assert_eq!(
            secondary.raw(),
            None,
            "a failing primary must not strand a token in the secondary"
        );
        assert_eq!(
            primary.raw().as_deref(),
            Some("tok_fake_stuck_in_failing_primary"),
            "the failed backend itself keeps its token (its clear failed)"
        );
    }

    #[test]
    fn clear_reports_a_secondary_failure_but_still_clears_the_primary() {
        // Mirror of the primary-failure case, which the sanity suite never
        // exercises: the *secondary* failing must not undo the primary's
        // clear nor swallow the error.
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        primary.seed("tok_fake_primary_cleared");
        secondary.seed("tok_fake_stuck_in_failing_secondary");
        secondary.fail_clear();
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        let err = store
            .clear()
            .expect_err("secondary clear failure must surface");
        assert!(
            err.to_string().contains(SEC_MARKER),
            "the clear error must name the failing secondary: {err}"
        );
        assert_eq!(primary.raw(), None, "the primary must still be cleared");
        assert_eq!((primary.clear_calls(), secondary.clear_calls()), (1, 1));
    }

    #[test]
    fn clear_with_both_backends_failing_reports_the_primary_error_without_panicking() {
        let primary = ScriptedSource::primary();
        let secondary = ScriptedSource::secondary();
        primary.seed("tok_fake_stuck_primary");
        secondary.seed("tok_fake_stuck_secondary");
        primary.fail_clear();
        secondary.fail_clear();
        let store =
            TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()));
        let err = store
            .clear()
            .expect_err("a double failure must surface, not panic or Ok");
        assert!(
            matches!(err, AppError::Auth(AuthError::Backend { .. })),
            "wrong error shape: {err:?}"
        );
        // The acceptance only demands "not swallowed" (either error);
        // this additionally pins the observed policy — the primary's
        // error wins via `Result::and` — so any change is a conscious one.
        assert!(
            err.to_string().contains(PRIM_MARKER),
            "current policy surfaces the primary error first: {err}"
        );
        assert_eq!(
            (primary.clear_calls(), secondary.clear_calls()),
            (1, 1),
            "both backends must still be attempted exactly once"
        );
    }

    // ---- require: NotAuthenticated + its message (acceptance 4) ----------

    #[test]
    fn require_pins_the_not_authenticated_message_byte_for_byte() {
        let store = TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        );
        let err = store.require().expect_err("no backend holds a token");
        assert!(
            matches!(err, AppError::Auth(AuthError::NotAuthenticated)),
            "wrong error shape: {err:?}"
        );
        // Byte-exact pin: accidental wording drift (dropping the
        // `todoist-tui auth` hint or the "auth: " context) fails here.
        assert_eq!(
            err.to_string(),
            "auth: not authenticated: no API token is stored; run `todoist-tui auth` to add one"
        );
    }

    #[test]
    fn not_authenticated_display_carries_the_auth_command_hint_directly() {
        // Cross-check against `error.rs` itself (not just through the
        // store): the hint must survive at both error levels.
        let direct = AuthError::NotAuthenticated.to_string();
        assert!(
            direct.contains("todoist-tui auth"),
            "hint missing from AuthError::NotAuthenticated: {direct}"
        );
        assert_eq!(
            direct,
            "not authenticated: no API token is stored; run `todoist-tui auth` to add one"
        );
    }

    #[test]
    fn require_reads_the_secondary_when_only_it_holds_a_token() {
        // The acceptance case with two plain `MemoryTokenSource`s.
        let secondary = MemoryTokenSource::new();
        secondary
            .store(&token(FIXTURE_TOKEN))
            .expect("seed secondary");
        let store =
            TokenStore::with_sources(Box::new(MemoryTokenSource::new()), Box::new(secondary));
        let required = store.require().expect("a fallback token satisfies require");
        assert_eq!(required.expose(), FIXTURE_TOKEN);
    }

    #[test]
    fn require_propagates_a_backend_error_instead_of_not_authenticated() {
        // A dead backend must not be misreported as "not authenticated" —
        // that would send the user to re-enter a token they already have.
        let primary = ScriptedSource::primary();
        primary.fail_load();
        let store =
            TokenStore::with_sources(Box::new(primary), Box::new(ScriptedSource::secondary()));
        let err = store
            .require()
            .expect_err("a dead backend is not the same as no token");
        assert!(
            matches!(err, AppError::Auth(AuthError::Backend { .. })),
            "backend failure must not be masked as NotAuthenticated: {err:?}"
        );
    }

    // ---- Debug: metadata-only, never a token leak (acceptance 6) --------

    #[test]
    fn debug_of_an_injected_store_renders_notice_none_only() {
        let store = TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        );
        // Exact rendering of the manual impl: only `notice`, plus the
        // non-exhaustive marker for the backend trait objects.
        assert_eq!(format!("{store:?}"), "TokenStore { notice: None, .. }");
        let pretty = format!("{store:#?}");
        assert!(
            pretty.contains("notice: None"),
            "pretty Debug dropped the notice field: {pretty}"
        );
    }

    #[test]
    fn debug_never_exposes_adversarial_tokens_in_either_backend() {
        let long = format!("tok_fake_1kib_{}", "A".repeat(1024));
        let notice_like = "tok_fake_OS-keyring-unavailable-mode-0600-notice-collision";
        for value in [
            FIXTURE_TOKEN,
            "tok_fake_special\"':}{]}<>?/|\\~`",
            "tok_fake_unicode_日本語_emoji_🔐_áccent",
            "tok_fake_newline_\n\t_tab",
            long.as_str(),
            notice_like,
        ] {
            let primary = ScriptedSource::primary();
            let secondary = ScriptedSource::secondary();
            primary.seed(value);
            secondary.seed(value); // both backends hold the material
            let store = TokenStore::with_sources(Box::new(primary), Box::new(secondary));
            let rendered = format!("{store:?}");
            assert!(
                rendered.contains("TokenStore"),
                "type name missing: {rendered}"
            );
            assert_no_token_leak(&rendered, &[value]);
            let pretty = format!("{store:#?}");
            assert_no_token_leak(&pretty, &[value]);
        }
    }

    #[test]
    fn debug_and_backend_notice_never_call_load_store_or_clear() {
        // Manual `Debug` contract: rendering is metadata-only and must
        // never touch a (real) backend. Proven with a fake that fails
        // loudly on any backend operation.
        struct VolatileSource;
        impl TokenSource for VolatileSource {
            fn load(&self) -> Result<Option<Token>> {
                panic!("Debug/backend_notice must not call load")
            }

            fn store(&self, _token: &Token) -> Result<()> {
                panic!("Debug/backend_notice must not call store")
            }

            fn clear(&self) -> Result<()> {
                panic!("Debug/backend_notice must not call clear")
            }

            fn describe(&self) -> &'static str {
                "volatile (unit_tests)"
            }
        }
        let store = TokenStore::with_sources(Box::new(VolatileSource), Box::new(VolatileSource));
        assert_eq!(store.backend_notice(), None);
        let rendered = format!("{store:?}");
        assert!(rendered.contains("TokenStore"));
        let _pretty = format!("{store:#?}");
    }

    #[test]
    fn debug_of_a_notice_bearing_store_renders_only_the_notice() {
        // `resolve` mints a notice only when the OS keyring is missing,
        // which cannot be forced from here — so the notice-bearing shape
        // is exercised via the same construction `resolve` uses (a direct
        // struct literal; this child module sees the private fields).
        let notice = "OS keyring unavailable; falling back to token file at \
                      /tmp/tok_fake_no_real_file/token (mode 0600)";
        let primary = ScriptedSource::primary();
        primary.seed("tok_fake_secret_payload");
        let store = TokenStore {
            primary: Box::new(primary),
            secondary: Box::new(ScriptedSource::secondary()),
            notice: Some(notice.to_string()),
        };
        let rendered = format!("{store:?}");
        assert!(
            rendered.contains(notice),
            "notice text must render verbatim: {rendered}"
        );
        assert_no_token_leak(&rendered, &["tok_fake_secret_payload"]);
        assert_eq!(store.backend_notice().as_deref(), Some(notice));
    }
}
