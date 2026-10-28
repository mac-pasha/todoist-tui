//! OS keyring-backed [`TokenSource`] (T-103, EPIC-02).
//!
//! Stores the Todoist API token under a single, fixed service/account pair
//! in the platform credential store — macOS Keychain, Windows Credential
//! Manager, or the Secret Service (via zbus) on other Unix-likes, per the
//! `keyring` crate's `v1` default backend. T-104 lands the file-backed
//! fallback for platforms where this backend is unavailable (e.g. headless
//! Linux without a Secret Service daemon); [`KeyringTokenSource::is_available`]
//! is the probe T-105's fallback chain uses to choose between them.
//!
//! Like every [`TokenSource`] implementation, this one must never log token
//! material: errors here carry only the opaque [`keyring::Error`] (mapped to
//! [`AuthError::Backend`]), never the token bytes passed to
//! [`TokenSource::store`].

use crate::auth::source::TokenSource;
use crate::auth::Token;
use crate::error::{AuthError, Result};

/// Service name the Todoist API token entry is stored under.
///
/// Exposed as `pub const` so other tasks (T-108 logout, tests) reference it
/// instead of re-typing the literal.
pub const SERVICE_NAME: &str = "todoist-tui";

/// Account/user name for the single API token entry this app stores.
pub const ACCOUNT_NAME: &str = "api-token";

/// Minimal seam over the keyring operations [`KeyringTokenSource`] needs.
///
/// Exists purely for testability: CI runners have no Secret Service/dbus
/// daemon (Linux) and no interactive Keychain (macOS/Windows), so unit
/// tests inject a fake in-process implementation via
/// [`KeyringTokenSource::with_backend`] instead of exercising a real OS
/// credential store. [`keyring::Entry`]'s own methods already match this
/// signature, so the blanket impl below is a pure delegation.
trait Backend: Send + Sync {
    fn get_password(&self) -> keyring::Result<String>;
    fn set_password(&self, password: &str) -> keyring::Result<()>;
    fn delete_credential(&self) -> keyring::Result<()>;
}

impl Backend for keyring::Entry {
    fn get_password(&self) -> keyring::Result<String> {
        Self::get_password(self)
    }

    fn set_password(&self, password: &str) -> keyring::Result<()> {
        Self::set_password(self, password)
    }

    fn delete_credential(&self) -> keyring::Result<()> {
        Self::delete_credential(self)
    }
}

/// Builds a fresh handle to the real OS keyring entry for
/// `(SERVICE_NAME, ACCOUNT_NAME)`.
fn real_backend() -> keyring::Result<Box<dyn Backend>> {
    keyring::Entry::new(SERVICE_NAME, ACCOUNT_NAME).map(|entry| Box::new(entry) as Box<dyn Backend>)
}

/// Factory invoked on every operation to obtain a fresh [`Backend`] handle.
///
/// Rebuilding the entry per call (rather than caching one) mirrors how
/// `keyring::Entry::new` is documented to behave: cheap, and the only way
/// to pick up the one-time platform store initialisation result.
type BackendFactory = dyn Fn() -> keyring::Result<Box<dyn Backend>> + Send + Sync;

/// [`TokenSource`] backed by the platform credential store.
///
/// Holds no token material itself — every byte lives in the OS keyring, not
/// in this struct — so its manual [`std::fmt::Debug`] impl has nothing to
/// redact; it exists only because the backend factory closure can't derive
/// `Debug`.
pub struct KeyringTokenSource {
    factory: Box<BackendFactory>,
}

impl KeyringTokenSource {
    /// Builds a `KeyringTokenSource` that talks to the real OS keyring.
    #[must_use]
    pub fn new() -> Self {
        Self {
            factory: Box::new(real_backend),
        }
    }

    /// Test seam: builds a `KeyringTokenSource` over an injected backend
    /// factory instead of the real OS keyring.
    ///
    /// Not part of the public API surface (private to this module); the
    /// `tests` module below uses it to simulate `NoEntry`,
    /// `PlatformFailure`, `NoStorageAccess` and success without touching a
    /// real credential store.
    fn with_backend<F>(factory: F) -> Self
    where
        F: Fn() -> keyring::Result<Box<dyn Backend>> + Send + Sync + 'static,
    {
        Self {
            factory: Box::new(factory),
        }
    }

    /// Probes whether the platform keyring backend is actually usable.
    ///
    /// Returns `false` on [`keyring::Error::PlatformFailure`] or
    /// [`keyring::Error::NoStorageAccess`] — e.g. headless Linux with no
    /// Secret Service daemon — so T-105's fallback chain can skip straight
    /// to the file-backed source instead of failing every call. Entry
    /// construction itself failing (e.g. `NoDefaultStore` on an
    /// unsupported platform) is treated the same way: not available. A
    /// missing entry (`NoEntry`) counts as *available* — the backend
    /// works, there's just nothing stored yet.
    #[must_use]
    pub fn is_available(&self) -> bool {
        let Ok(entry) = (self.factory)() else {
            return false;
        };
        !matches!(
            entry.get_password(),
            Err(keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_))
        )
    }
}

impl Default for KeyringTokenSource {
    fn default() -> Self {
        Self::new()
    }
}

impl std::fmt::Debug for KeyringTokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("KeyringTokenSource").finish_non_exhaustive()
    }
}

impl TokenSource for KeyringTokenSource {
    /// Reads the stored token, or `Ok(None)` when there is nothing to read
    /// *or* the backend itself is unreachable.
    ///
    /// Pre-existing-bug fix (found while implementing T-109, not part of
    /// its scope): [`keyring::Error::PlatformFailure`] and
    /// [`keyring::Error::NoStorageAccess`] are treated the same way
    /// [`KeyringTokenSource::is_available`] already treats them — as "the
    /// keyring backend is unusable," not as a hard failure — so `load`
    /// agrees with `is_available`'s truth table. Without this, a caller
    /// like [`crate::auth::TokenStore::load`] that falls back to reading
    /// this backend as the *secondary* source (after `resolve` already
    /// chose the file backend as primary because the keyring was
    /// unavailable) would still hit a hard error here instead of
    /// transparently finding "no token," turning every no-token command on
    /// a keyring-less machine into a confusing backend error instead of
    /// the intended "not authenticated" message.
    fn load(&self) -> Result<Option<Token>> {
        let entry = (self.factory)().map_err(|source| AuthError::Backend { source })?;
        match entry.get_password() {
            Ok(raw) => Token::new(raw).map(Some),
            Err(keyring::Error::NoEntry) => Ok(None),
            Err(keyring::Error::PlatformFailure(_) | keyring::Error::NoStorageAccess(_)) => {
                Ok(None)
            }
            Err(source) => Err(AuthError::Backend { source }.into()),
        }
    }

    fn store(&self, token: &Token) -> Result<()> {
        let entry = (self.factory)().map_err(|source| AuthError::Backend { source })?;
        entry
            .set_password(token.expose())
            .map_err(|source| AuthError::Backend { source }.into())
    }

    /// Deletes the stored token, or `Ok(())` when there is nothing to
    /// delete *or* the backend itself is unreachable.
    ///
    /// Pre-existing-bug fix (found via T-110's E2E test, the first thing to
    /// exercise `logout` on a machine with no reachable OS keychain; not
    /// part of T-110's own scope): mirrors [`KeyringTokenSource::load`]'s
    /// fix for T-109 — [`keyring::Error::PlatformFailure`] and
    /// [`keyring::Error::NoStorageAccess`] are treated as "nothing to
    /// clear in an unreachable backend," not as a hard failure, the same
    /// way `load` treats them as "no token in an unusable backend."
    /// Without this, `logout` failed outright on any machine with no
    /// reachable OS keychain (headless Linux with no Secret Service
    /// daemon, a CI box, a dev machine with the Keychain daemon down)
    /// instead of succeeding.
    fn clear(&self) -> Result<()> {
        let entry = (self.factory)().map_err(|source| AuthError::Backend { source })?;
        match entry.delete_credential() {
            Ok(())
            | Err(
                keyring::Error::NoEntry
                | keyring::Error::PlatformFailure(_)
                | keyring::Error::NoStorageAccess(_),
            ) => Ok(()),
            Err(source) => Err(AuthError::Backend { source }.into()),
        }
    }

    fn describe(&self) -> &'static str {
        "OS keyring"
    }
}

#[cfg(test)]
mod tests {
    //! Sanity checks proving the T-103 design constraints hold against an
    //! injected fake backend (never a real OS credential store, so these
    //! pass in CI with no Secret Service/dbus daemon present); the
    //! unit-tester adds the full suite separately.
    use std::sync::{Arc, Mutex, PoisonError};

    use super::*;
    use crate::error::AppError;

    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    /// In-process fake standing in for the real OS keyring. `Backend`
    /// requires `Send + Sync` (the real `keyring::Entry` must be movable
    /// across the tokio task boundary later wiring needs), so the fake uses
    /// a `Mutex` rather than a `RefCell`.
    #[derive(Default)]
    struct FakeBackend {
        stored: Mutex<Option<String>>,
    }

    impl Backend for FakeBackend {
        fn get_password(&self) -> keyring::Result<String> {
            self.stored
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .clone()
                .ok_or(keyring::Error::NoEntry)
        }

        fn set_password(&self, password: &str) -> keyring::Result<()> {
            *self.stored.lock().unwrap_or_else(PoisonError::into_inner) =
                Some(password.to_string());
            Ok(())
        }

        fn delete_credential(&self) -> keyring::Result<()> {
            let mut guard = self.stored.lock().unwrap_or_else(PoisonError::into_inner);
            if guard.take().is_none() {
                return Err(keyring::Error::NoEntry);
            }
            Ok(())
        }
    }

    /// Builds a `KeyringTokenSource` over a shared `FakeBackend`, so the
    /// test can both drive the source and inspect/mutate backend state.
    fn fake_source() -> (KeyringTokenSource, Arc<FakeBackend>) {
        let backend = Arc::new(FakeBackend::default());
        let shared = Arc::clone(&backend);
        let source = KeyringTokenSource::with_backend(move || {
            Ok(Box::new(ArcBackend(Arc::clone(&shared))) as Box<dyn Backend>)
        });
        (source, backend)
    }

    /// `Arc<FakeBackend>` wrapper so the factory closure can hand out a
    /// `Box<dyn Backend>` per call while all calls share one `FakeBackend`.
    struct ArcBackend(Arc<FakeBackend>);

    impl Backend for ArcBackend {
        fn get_password(&self) -> keyring::Result<String> {
            self.0.get_password()
        }

        fn set_password(&self, password: &str) -> keyring::Result<()> {
            self.0.set_password(password)
        }

        fn delete_credential(&self) -> keyring::Result<()> {
            self.0.delete_credential()
        }
    }

    #[test]
    fn load_on_empty_backend_is_ok_none() {
        let (source, _backend) = fake_source();
        assert!(source.load().expect("load ok").is_none());
    }

    #[test]
    fn store_then_load_round_trips_an_equal_token() {
        let (source, _backend) = fake_source();
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        let loaded = source.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), token.expose());
    }

    #[test]
    fn clear_on_empty_backend_is_a_no_op() {
        let (source, _backend) = fake_source();
        source.clear().expect("clear on empty is Ok(())");
        assert!(source.load().expect("load ok").is_none());
    }

    #[test]
    fn clear_then_load_returns_none() {
        let (source, _backend) = fake_source();
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        source.clear().expect("clear ok");
        assert!(source.load().expect("load ok").is_none());
    }

    #[test]
    fn load_maps_other_backend_errors_to_auth_backend() {
        let source = KeyringTokenSource::with_backend(|| {
            Err(keyring::Error::Invalid(
                "account".to_string(),
                "fake failure".to_string(),
            ))
        });
        match source.load() {
            Err(AppError::Auth(AuthError::Backend { .. })) => {}
            other => panic!("expected AuthError::Backend, got {other:?}"),
        }
    }

    /// Pre-existing-bug fix, discovered while implementing T-109 (not a
    /// T-109 feature itself): `load` must agree with `is_available` that
    /// `PlatformFailure`/`NoStorageAccess` mean "the keyring is
    /// unreachable," returning `Ok(None)` (same as `NoEntry`) instead of a
    /// hard `AuthError::Backend`. Without this, [`crate::auth::TokenStore`]
    /// reading this backend as its secondary source (after `resolve`
    /// already chose the file backend as primary for the same reason)
    /// would surface a confusing backend error instead of "no token".
    /// Fake [`Backend`] whose `get_password` always fails with a scripted
    /// error, to exercise `load`'s mapping of read-time backend failures
    /// (as opposed to the entry-construction failure the factory-closure
    /// tests above cover).
    struct FailingGetBackend(fn() -> keyring::Error);

    impl Backend for FailingGetBackend {
        fn get_password(&self) -> keyring::Result<String> {
            Err((self.0)())
        }

        fn set_password(&self, _password: &str) -> keyring::Result<()> {
            Ok(())
        }

        fn delete_credential(&self) -> keyring::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn load_treats_platform_failure_as_ok_none() {
        let source = KeyringTokenSource::with_backend(|| {
            Ok(Box::new(FailingGetBackend(|| {
                keyring::Error::PlatformFailure(
                    std::io::Error::other("fake platform failure").into(),
                )
            })) as Box<dyn Backend>)
        });
        assert!(
            source.load().expect("must not be an error").is_none(),
            "an unreachable keyring (platform failure) must read as no token, not an error"
        );
    }

    #[test]
    fn load_treats_no_storage_access_as_ok_none() {
        let source = KeyringTokenSource::with_backend(|| {
            Ok(Box::new(FailingGetBackend(|| {
                keyring::Error::NoStorageAccess("fake locked store".into())
            })) as Box<dyn Backend>)
        });
        assert!(
            source.load().expect("must not be an error").is_none(),
            "an unreachable keyring (no storage access) must read as no token, not an error"
        );
    }

    #[test]
    fn is_available_is_true_for_a_healthy_backend() {
        let (source, _backend) = fake_source();
        assert!(source.is_available());
    }

    #[test]
    fn is_available_is_false_on_platform_failure() {
        let source = KeyringTokenSource::with_backend(|| {
            Err(keyring::Error::PlatformFailure(
                "fake platform failure".into(),
            ))
        });
        assert!(!source.is_available());
    }

    #[test]
    fn is_available_is_false_on_no_storage_access() {
        let source = KeyringTokenSource::with_backend(|| {
            Err(keyring::Error::NoStorageAccess("fake locked store".into()))
        });
        assert!(!source.is_available());
    }

    #[test]
    fn describe_names_the_backend_without_leaking_tokens() {
        let (source, _backend) = fake_source();
        assert_eq!(source.describe(), "OS keyring");
        assert!(!source.describe().contains(FIXTURE_TOKEN));
    }

    #[test]
    fn debug_rendering_never_exposes_stored_token_material() {
        let (source, _backend) = fake_source();
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        let dbg = format!("{source:?}");
        assert!(
            !dbg.contains(FIXTURE_TOKEN),
            "Debug leaked stored token: {dbg}"
        );
    }
}

/// Extended T-103 coverage (unit-tester, different model family from the
/// coder). Complements — does not duplicate — the sanity `tests` module
/// above: the full `keyring::Error` matrix at every call site (entry
/// construction vs `get_password` vs `set_password` vs `delete_credential`),
/// transient-failure recovery across the fresh-entry-per-operation design,
/// defensive re-validation of corrupt backend state, store-replaces and
/// byte-exact round-trip semantics, idempotent `clear` loops, the
/// documented `is_available` truth table (including the "other errors mean
/// available" rows), error-rendering leak checks, object safety through
/// `dyn TokenSource + Send + Sync` (the T-105 fallback-chain shape), and
/// concurrent use of one source across threads.
#[cfg(test)]
mod unit_tests {
    use std::sync::{Arc, Mutex};
    use std::thread;

    use super::*;
    use crate::error::AppError;

    /// Fakes only — never real credentials (repo rule 7).
    const FAKE_A: &str = "tok_fake_keyring_aaa111";
    const FAKE_B: &str = "tok_fake_keyring_bbb222";

    fn token(value: &str) -> Token {
        Token::new(value.to_string()).expect("non-empty fake token")
    }

    /// Which `keyring::Error` a scripted failure surfaces. Every variant is
    /// exercised at least once below, keeping `dead_code` (deny-in-CI) quiet.
    #[derive(Clone, Copy, Debug)]
    enum ErrKind {
        PlatformFailure,
        NoStorageAccess,
        Invalid,
        BadEncoding,
        BadStoreFormat,
        TooLong,
        NotSupportedByStore,
        NoDefaultStore,
    }

    /// Builds a fresh fake `keyring::Error` of the requested shape. Errors
    /// are reconstructed per call (the enum is not `Clone`), mirroring how a
    /// real store would surface a new error object per operation.
    fn kr_err(kind: ErrKind) -> keyring::Error {
        match kind {
            ErrKind::PlatformFailure => keyring::Error::PlatformFailure(
                std::io::Error::other("fake platform failure").into(),
            ),
            ErrKind::NoStorageAccess => keyring::Error::NoStorageAccess("fake locked store".into()),
            ErrKind::Invalid => keyring::Error::Invalid("account".into(), "fake invalid".into()),
            ErrKind::BadEncoding => keyring::Error::BadEncoding(vec![0xff, 0xfe]),
            ErrKind::BadStoreFormat => {
                keyring::Error::BadStoreFormat("fake malformed store".into())
            }
            ErrKind::TooLong => keyring::Error::TooLong("password".into(), 42),
            ErrKind::NotSupportedByStore => {
                keyring::Error::NotSupportedByStore("fake unsupported operation".into())
            }
            ErrKind::NoDefaultStore => keyring::Error::NoDefaultStore,
        }
    }

    /// Fake keyring with an inspectable/seedable store plus optional
    /// one-shot failures per operation, so tests can script any
    /// `keyring::Error` at any call site without a real OS credential
    /// store (CI has no Secret Service daemon / interactive Keychain).
    /// One-shot (not sticky) failures model transient backend errors and
    /// let the same test observe recovery on the next call.
    #[derive(Default)]
    struct ScriptedBackend {
        stored: Mutex<Option<String>>,
        fail_get: Mutex<Option<ErrKind>>,
        fail_set: Mutex<Option<ErrKind>>,
        fail_delete: Mutex<Option<ErrKind>>,
    }

    impl ScriptedBackend {
        fn seed(&self, value: &str) {
            *self.stored.lock().unwrap() = Some(value.to_string());
        }

        fn raw(&self) -> Option<String> {
            self.stored.lock().unwrap().clone()
        }
    }

    /// Arms a one-shot failure for a single operation slot.
    fn fail_once(op: &Mutex<Option<ErrKind>>, kind: ErrKind) {
        *op.lock().unwrap() = Some(kind);
    }

    // `Arc<ScriptedBackend>` implements the seam directly, so the factory
    // can hand out a `Box<dyn Backend>` per operation while every handle
    // shares one fake store (same trick as the real `Arc`-free coder fake,
    // minus the wrapper struct).
    impl Backend for Arc<ScriptedBackend> {
        fn get_password(&self) -> keyring::Result<String> {
            if let Some(kind) = self.fail_get.lock().unwrap().take() {
                return Err(kr_err(kind));
            }
            self.raw().ok_or(keyring::Error::NoEntry)
        }

        fn set_password(&self, password: &str) -> keyring::Result<()> {
            if let Some(kind) = self.fail_set.lock().unwrap().take() {
                return Err(kr_err(kind));
            }
            self.seed(password);
            Ok(())
        }

        fn delete_credential(&self) -> keyring::Result<()> {
            if let Some(kind) = self.fail_delete.lock().unwrap().take() {
                return Err(kr_err(kind));
            }
            self.stored
                .lock()
                .unwrap()
                .take()
                .map_or(Err(keyring::Error::NoEntry), |_| Ok(()))
        }
    }

    /// Builds a `KeyringTokenSource` over a shared `ScriptedBackend`, so a
    /// test can both drive the source and script/inspect backend state.
    fn scripted_source() -> (KeyringTokenSource, Arc<ScriptedBackend>) {
        let backend = Arc::new(ScriptedBackend::default());
        let shared = Arc::clone(&backend);
        let source = KeyringTokenSource::with_backend(move || {
            Ok(Box::new(Arc::clone(&shared)) as Box<dyn Backend>)
        });
        (source, backend)
    }

    /// Builds a source whose *entry construction* fails with `kind` on
    /// every call (the `keyring::Entry::new` failure path).
    fn failing_factory_source(kind: ErrKind) -> KeyringTokenSource {
        KeyringTokenSource::with_backend(move || Err(kr_err(kind)))
    }

    /// Asserts the crate-wide error contract: any backend failure surfaces
    /// as `AppError::Auth(AuthError::Backend { .. })` — never a panic, never
    /// a bare `keyring::Error` escaping the `Result` alias. Takes the result
    /// by reference so callers can keep inspecting the error afterwards.
    fn assert_backend_error<T>(res: &Result<T>) {
        match res {
            Err(AppError::Auth(AuthError::Backend { .. })) => {}
            Err(other) => panic!("expected AppError::Auth(AuthError::Backend), got {other:?}"),
            Ok(_) => panic!("expected AppError::Auth(AuthError::Backend), got Ok"),
        }
    }

    // ---- load: the full backend-error matrix ----

    #[test]
    fn load_maps_every_remaining_backend_error_kind_to_auth_backend_without_leaking() {
        // Read failures other than `NoEntry`, `PlatformFailure` and
        // `NoStorageAccess` must surface as `AuthError::Backend`, and
        // neither Display nor Debug of the error may echo the stored token
        // (the module's no-leak contract). `PlatformFailure` and
        // `NoStorageAccess` are excluded from this loop: a pre-existing-bug
        // fix (found while implementing T-109) now maps both to `Ok(None)`
        // instead, matching `is_available`'s truth table — see
        // `load_treats_platform_failure_as_ok_none` /
        // `load_treats_no_storage_access_as_ok_none` in the `tests` module
        // above and `is_available_is_false_when_the_probe_read_fails_at_platform_level`
        // below.
        for kind in [
            ErrKind::Invalid,
            ErrKind::BadEncoding,
            ErrKind::BadStoreFormat,
            ErrKind::TooLong,
            ErrKind::NotSupportedByStore,
        ] {
            let (source, backend) = scripted_source();
            backend.seed(FAKE_A);
            fail_once(&backend.fail_get, kind);
            let err = source.load().expect_err("scripted read failure");
            // Capture renderings first: `err` moves into the variant check.
            let display = err.to_string();
            let debug = format!("{err:?}");
            assert_backend_error(&Err::<(), _>(err));
            assert!(
                !display.contains(FAKE_A),
                "error Display leaked stored token ({kind:?}): {display}"
            );
            assert!(
                !debug.contains(FAKE_A),
                "error Debug leaked stored token ({kind:?}): {debug}"
            );
        }
    }

    #[test]
    fn load_maps_entry_construction_failures_to_auth_backend() {
        // `keyring::Entry::new` failing (e.g. `NoDefaultStore` on a platform
        // with no default store) is a backend failure too, not a panic.
        for kind in [
            ErrKind::PlatformFailure,
            ErrKind::NoStorageAccess,
            ErrKind::NoDefaultStore,
            ErrKind::Invalid,
        ] {
            let source = failing_factory_source(kind);
            assert_backend_error(&source.load());
        }
    }

    #[test]
    fn load_recovers_after_a_transient_backend_failure() {
        // The source rebuilds its entry handle per operation, so a retry on
        // the SAME source must succeed once the transient failure clears —
        // no poisoned state, no need to reconstruct the source.
        // `ErrKind::Invalid` (not `PlatformFailure`): the pre-existing-bug
        // fix found while implementing T-109 now maps `PlatformFailure` /
        // `NoStorageAccess` to `Ok(None)` instead of a hard error, so a
        // still-error kind is needed here to exercise "errors then
        // recovers" rather than "immediately Ok(None)".
        let (source, backend) = scripted_source();
        backend.seed(FAKE_A);
        fail_once(&backend.fail_get, ErrKind::Invalid);
        assert_backend_error(&source.load());
        let loaded = source
            .load()
            .expect("retry load ok")
            .expect("token present");
        assert_eq!(loaded.expose(), FAKE_A);
    }

    #[test]
    fn load_revalidates_whitespace_only_backend_state_to_empty_token() {
        // WHITE-BOX: unreachable through this app's own `store` (a `Token`
        // is never whitespace-only), but another tool could have written
        // garbage under the same service/account pair. `load` must surface
        // `EmptyToken` — not a bogus token, not a panic — mirroring the
        // defensive behaviour of `MemoryTokenSource`.
        for corrupt in ["", " ", " \t\r\n "] {
            let (source, backend) = scripted_source();
            backend.seed(corrupt);
            match source.load() {
                Err(AppError::Auth(AuthError::EmptyToken)) => {}
                other => panic!("corrupt {corrupt:?}: expected EmptyToken, got {other:?}"),
            }
        }
    }

    // ---- store: error matrix and semantics ----

    #[test]
    fn store_maps_every_write_error_kind_to_auth_backend_without_leaking() {
        // Write failures must surface as `AuthError::Backend`, never echo
        // the token being stored, and must not leave a partial write.
        for kind in [
            ErrKind::PlatformFailure,
            ErrKind::NoStorageAccess,
            ErrKind::TooLong,
            ErrKind::Invalid,
            ErrKind::NotSupportedByStore,
        ] {
            let (source, backend) = scripted_source();
            fail_once(&backend.fail_set, kind);
            let err = source
                .store(&token(FAKE_B))
                .expect_err("scripted write failure");
            let display = err.to_string();
            let debug = format!("{err:?}");
            assert_backend_error(&Err::<(), _>(err));
            assert!(
                !display.contains(FAKE_B),
                "error Display leaked stored token ({kind:?}): {display}"
            );
            assert!(
                !debug.contains(FAKE_B),
                "error Debug leaked stored token ({kind:?}): {debug}"
            );
            assert!(backend.raw().is_none(), "failed store must not write");
        }
    }

    #[test]
    fn store_maps_entry_construction_failures_to_auth_backend() {
        for kind in [ErrKind::NoDefaultStore, ErrKind::PlatformFailure] {
            let source = failing_factory_source(kind);
            assert_backend_error(&source.store(&token(FAKE_A)));
        }
    }

    #[test]
    fn store_replaces_the_previous_entry_value_in_the_backend() {
        // Trait doc: "Persists `token`, replacing any previously stored
        // token" — the second store must win, in the backend itself, not
        // just at the `load` seam; and re-storing the same value is stable.
        let (source, backend) = scripted_source();
        source.store(&token(FAKE_A)).expect("store A");
        source.store(&token(FAKE_B)).expect("store B");
        assert_eq!(backend.raw().as_deref(), Some(FAKE_B), "latest store wins");
        source.store(&token(FAKE_B)).expect("re-store B");
        assert_eq!(backend.raw().as_deref(), Some(FAKE_B));
        assert_eq!(
            source.load().expect("load ok").expect("present").expose(),
            FAKE_B
        );
    }

    #[test]
    fn round_trips_preserve_exact_bytes_for_adversarial_values() {
        // Same value matrix as the T-102 suite: single-char boundary,
        // marker-colliding, padded (valid per `Token::new`: validation trims
        // but never normalises), unicode, newline-bearing, and 1 KiB values
        // must survive store→load byte-for-byte, and the backend must hold
        // exactly those bytes.
        let mut values: Vec<String> = [
            "x",
            "tok***fake",
            "***",
            "  tok_fake_pad_42  ",
            "токе́н_fäke",
            "a\nb\tc",
        ]
        .iter()
        .map(|s| (*s).to_string())
        .collect();
        values.push(format!("tok_fake_{}", "z".repeat(1024)));

        let (source, backend) = scripted_source();
        for v in &values {
            source.store(&token(v)).expect("store ok");
            assert_eq!(backend.raw().as_deref(), Some(v.as_str()), "backend bytes");
            let loaded = source.load().expect("load ok").expect("present");
            assert_eq!(loaded.expose(), v, "round-trip must be byte-exact");
        }
    }

    #[test]
    fn load_is_non_destructive_and_repeatable() {
        let (source, backend) = scripted_source();
        backend.seed(FAKE_A);
        for _ in 0..3 {
            let first = source.load().expect("load 1").expect("present");
            let second = source.load().expect("load 2").expect("present");
            assert_eq!(first.expose(), FAKE_A);
            assert_eq!(second.expose(), FAKE_A);
        }
        assert_eq!(
            backend.raw().as_deref(),
            Some(FAKE_A),
            "load must not consume"
        );
    }

    // ---- clear: error matrix, idempotency, state ----

    #[test]
    fn clear_maps_backend_delete_errors_to_auth_backend_and_keeps_entry() {
        // `PlatformFailure`/`NoStorageAccess` are intentionally excluded:
        // mirroring the pre-existing-bug fix applied to `load` for T-109,
        // `clear` now treats those two kinds as "nothing to clear in an
        // unreachable backend" (`Ok(())`), not a hard error; they are
        // covered by `clear_treats_platform_failure_and_no_storage_access_as_ok`
        // below instead.
        let kind = ErrKind::NotSupportedByStore;
        let (source, backend) = scripted_source();
        backend.seed(FAKE_A);
        fail_once(&backend.fail_delete, kind);
        assert_backend_error(&source.clear());
        assert_eq!(
            backend.raw().as_deref(),
            Some(FAKE_A),
            "failed delete must leave the entry in place"
        );
    }

    #[test]
    fn clear_treats_platform_failure_and_no_storage_access_as_ok() {
        // Pre-existing-bug fix (found via T-110's E2E test, not part of its
        // scope): mirrors `load`'s T-109 fix — an unreachable backend has
        // nothing that can be cleared, so `clear` must succeed instead of
        // erroring, the same way `load` returns `Ok(None)` instead of
        // erroring for these same two kinds.
        for kind in [ErrKind::PlatformFailure, ErrKind::NoStorageAccess] {
            let (source, backend) = scripted_source();
            backend.seed(FAKE_A);
            fail_once(&backend.fail_delete, kind);
            source
                .clear()
                .expect("unreachable backend: clear is Ok(())");
            assert_eq!(
                backend.raw().as_deref(),
                Some(FAKE_A),
                "a scripted delete failure must leave the fake entry in place"
            );
        }
    }

    #[test]
    fn clear_maps_entry_construction_failures_to_auth_backend() {
        let source = failing_factory_source(ErrKind::NoDefaultStore);
        assert_backend_error(&source.clear());
    }

    #[test]
    fn clear_removes_the_backend_entry_and_is_idempotent() {
        // Trait contract: `clear` when nothing is stored is `Ok(())`
        // (logout calls it unconditionally), and a successful clear must
        // actually delete the backend entry.
        let (source, backend) = scripted_source();
        for _ in 0..3 {
            source.clear().expect("clear on empty is Ok(())");
        }
        backend.seed(FAKE_A);
        source.clear().expect("clear ok");
        assert!(backend.raw().is_none(), "entry must be deleted");
        for _ in 0..3 {
            source.clear().expect("clear on empty again is Ok(())");
        }
        assert!(source.load().expect("load ok").is_none());
    }

    #[test]
    fn store_clear_store_cycles_keep_the_backend_usable() {
        // Logout/login loops must not wedge the backend or the source.
        let (source, _backend) = scripted_source();
        for (i, v) in [FAKE_A, FAKE_B, FAKE_A].iter().enumerate() {
            source.store(&token(v)).expect("store ok");
            assert_eq!(
                source.load().expect("load ok").expect("present").expose(),
                *v,
                "cycle {i}: token must be readable after re-store"
            );
            source.clear().expect("clear ok");
            assert!(source.load().expect("load ok").is_none(), "cycle {i}");
        }
    }

    // ---- is_available: the documented truth table ----

    #[test]
    fn is_available_is_true_when_no_entry_exists_and_after_clear() {
        // Documented: `NoEntry` means the backend WORKS, there is just
        // nothing stored yet — the fresh-install case T-105 must handle by
        // treating the keyring as usable.
        let (source, backend) = scripted_source();
        assert!(source.is_available(), "fresh install (NoEntry)");
        backend.seed(FAKE_A);
        assert!(source.is_available(), "healthy with a stored token");
        source.clear().expect("clear ok");
        assert!(source.is_available(), "healthy after clear (NoEntry)");
    }

    #[test]
    fn is_available_is_false_when_the_probe_read_fails_at_platform_level() {
        // Distinct from the coder's factory-level tests: the entry is
        // constructed fine, but `get_password` itself reports the store is
        // broken/locked (headless Linux, locked Keychain).
        for kind in [ErrKind::PlatformFailure, ErrKind::NoStorageAccess] {
            let (source, backend) = scripted_source();
            fail_once(&backend.fail_get, kind);
            assert!(!source.is_available(), "{kind:?} must mean unavailable");
        }
    }

    #[test]
    fn is_available_is_true_for_data_shape_errors_per_documented_contract() {
        // Documented contract: ONLY PlatformFailure / NoStorageAccess (or
        // entry-construction failure) mean unavailable. Data-shape errors
        // (Invalid, BadEncoding, BadStoreFormat, TooLong) prove the store
        // is reachable, so it stays "available". `NotSupportedByStore` is
        // asserted here as the current documented behaviour; flagged for
        // T-105 review (a store that rejects the operation would fail every
        // later load/store — see the unit-tester report).
        for kind in [
            ErrKind::Invalid,
            ErrKind::BadEncoding,
            ErrKind::BadStoreFormat,
            ErrKind::TooLong,
            ErrKind::NotSupportedByStore,
        ] {
            let (source, backend) = scripted_source();
            fail_once(&backend.fail_get, kind);
            assert!(source.is_available(), "{kind:?} must mean available");
        }
    }

    #[test]
    fn is_available_is_false_when_entry_construction_fails() {
        // Entry construction failing (e.g. NoDefaultStore on an
        // unsupported platform) is treated as not available — the coder's
        // Platform/NoStorageAccess factory tests generalised.
        for kind in [
            ErrKind::NoDefaultStore,
            ErrKind::PlatformFailure,
            ErrKind::NoStorageAccess,
            ErrKind::Invalid,
        ] {
            assert!(
                !failing_factory_source(kind).is_available(),
                "{kind:?} at construction must mean unavailable"
            );
        }
    }

    // ---- describe / Debug / constants: the no-leak contract ----

    #[test]
    fn describe_is_the_documented_name_and_independent_of_state() {
        let (source, backend) = scripted_source();
        assert_eq!(source.describe(), "OS keyring");
        backend.seed(FAKE_A);
        assert_eq!(source.describe(), "OS keyring");
        source.clear().expect("clear ok");
        assert_eq!(source.describe(), "OS keyring");
        assert!(!source.describe().contains(FAKE_A));
        assert_ne!(source.describe(), "");
    }

    #[test]
    fn debug_and_pretty_debug_render_no_token_or_factory_internals() {
        // `tracing`'s `?backend` uses Debug; the manual impl must render
        // only the type name with no fields — in both Debug flavours —
        // regardless of what the backend holds.
        let (source, backend) = scripted_source();
        backend.seed(FAKE_A);
        source.store(&token(FAKE_B)).expect("store ok");
        for dbg in [format!("{source:?}"), format!("{source:#?}")] {
            assert!(dbg.contains("KeyringTokenSource"), "got {dbg}");
            assert!(!dbg.contains(FAKE_A), "Debug leaked seeded token: {dbg}");
            assert!(!dbg.contains(FAKE_B), "Debug leaked stored token: {dbg}");
            assert!(!dbg.contains("tok_fake"), "Debug leaked any fixture: {dbg}");
        }
    }

    #[test]
    fn new_and_default_render_and_describe_without_touching_the_os_keyring() {
        // `describe`/`Debug` never invoke the backend factory, so these are
        // safe on CI machines with no Secret Service / Keychain. The data
        // operations are deliberately NOT called here: they would hit the
        // real OS credential store and are covered via `with_backend` above.
        let real = KeyringTokenSource::new();
        let defaulted = KeyringTokenSource::default();
        for s in [&real, &defaulted] {
            assert_eq!(s.describe(), "OS keyring");
            let dbg = format!("{s:?}");
            assert!(dbg.contains("KeyringTokenSource"), "got {dbg}");
            assert!(!dbg.contains("tok_fake"), "Debug leaked fixture: {dbg}");
        }
    }

    #[test]
    fn storage_location_constants_pin_the_documented_entry() {
        // T-105 (fallback chain) and T-108 (logout) key off this exact
        // service/account pair; silently changing it would orphan every
        // previously stored token.
        assert_eq!(SERVICE_NAME, "todoist-tui");
        assert_eq!(ACCOUNT_NAME, "api-token");
    }

    // ---- object safety, Send/Sync, concurrency ----

    #[test]
    fn works_through_dyn_token_source_and_is_send_sync_for_async_wiring() {
        // The T-102 suite called out `Box<dyn TokenSource + Send + Sync>` as
        // the shape T-103/T-104 app state needs across tokio task
        // boundaries; this backend must satisfy it.
        fn assert_send_sync<T: Send + Sync>() {}
        assert_send_sync::<KeyringTokenSource>();

        // Construct-only: proves the coercion works, but deliberately does
        // NOT invoke any data operation (that would hit the real OS keyring
        // — the injected-backend cases below cover behaviour).
        let _boxed: Box<dyn TokenSource + Send + Sync> = Box::new(KeyringTokenSource::new());

        let (concrete, backend) = scripted_source();
        let r1: &dyn TokenSource = &concrete;
        let r2: &dyn TokenSource = &concrete;
        r1.store(&token(FAKE_A)).expect("store via &dyn");
        assert_eq!(
            r2.load()
                .expect("load via aliased &dyn")
                .expect("present")
                .expose(),
            FAKE_A
        );
        r2.clear().expect("clear via aliased &dyn");
        assert!(r1.load().expect("load ok").is_none());
        assert!(backend.raw().is_none());

        // The fallback-chain shape: a collection of backend trait objects.
        let chain: Vec<Box<dyn TokenSource + Send + Sync>> =
            vec![Box::new(scripted_source().0), Box::new(scripted_source().0)];
        for (i, src) in chain.iter().enumerate() {
            let v = format!("tok_fake_chain_{i}");
            src.store(&token(&v)).expect("store ok");
            assert_eq!(src.load().expect("load ok").expect("present").expose(), v);
            src.clear().expect("clear ok");
            assert_ne!(src.describe(), "");
        }
    }

    #[test]
    fn concurrent_operations_through_one_source_are_sound() {
        // The factory hands out a fresh entry handle per operation and the
        // shared fake serialises on a Mutex, so hammering one source from
        // several threads must never panic, deadlock, or corrupt state.
        let (source, _backend) = scripted_source();
        let shared = Arc::new(source);
        let handles: Vec<_> = (0..4)
            .map(|i| {
                let src = Arc::clone(&shared);
                thread::spawn(move || {
                    for j in 0..25_u32 {
                        let v = format!("tok_fake_conc_{i}_{j}");
                        src.store(&token(&v)).expect("store ok");
                        let _ = src.load().expect("load ok");
                        src.clear().expect("clear ok");
                    }
                })
            })
            .collect();
        for h in handles {
            h.join().expect("worker thread must not panic");
        }
        assert!(shared.load().expect("final load ok").is_none());
    }
}
