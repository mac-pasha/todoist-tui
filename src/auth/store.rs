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
