//! Subcommand dispatch (T-007, EPIC-01; auth wired in T-107, logout in
//! T-108; `sync`/`add`/`today` gated on authentication in T-109).
//!
//! Each variant of [`crate::cli::Commands`] routes to its implementation:
//! `auth` is real (T-107: prompt / validate / store, see [`crate::cli::auth`]),
//! `logout` is real (T-108: clear both token backends plus the cache-wipe
//! hook, see [`crate::cli::logout`]). `sync`, `add` and `today` are still
//! stubs that return [`AppError::NotImplemented`] carrying their name until
//! their epics land (sync: EPIC-02/03; add: EPIC-11 quick-add parsing;
//! today: EPIC-05 task list) — but T-109 makes even the stub path first
//! call [`require_auth`], so each of those three commands fails with
//! [`crate::error::AuthError::NotAuthenticated`] before anything else when
//! no token is stored, instead of silently "succeeding" at not being
//! implemented. Kept free of network and UI code on purpose — those live
//! behind the routed modules.

use crate::auth::TokenStore;
use crate::cli::auth::run_auth;
use crate::cli::logout::run_logout;
use crate::cli::today::run_today_default;
use crate::cli::Commands;
#[allow(unused_imports)]
use crate::error::{AppError, Result};

/// Routes a parsed subcommand to its implementation.
///
/// # Errors
///
/// Returns whatever the routed implementation returns for `auth`/`logout`/`today`;
/// `sync`/`add` return [`crate::error::AuthError::NotAuthenticated`]
/// (via [`require_auth`]) when no token is stored, otherwise
/// [`AppError::NotImplemented`] with the subcommand's name.
pub fn dispatch(command: &Commands) -> Result<()> {
    match command {
        Commands::Auth => run_auth(),
        Commands::Logout => run_logout(),
        Commands::Sync => {
            require_auth(&TokenStore::resolve()?)?;
            crate::cli::sync::run_sync(None, false)
        }
        Commands::Add { text } => {
            require_auth(&TokenStore::resolve()?)?;
            crate::cli::add::run_add(None, text, false)
        }
        Commands::Today { json } => {
            require_auth(&TokenStore::resolve()?)?;
            run_today_default(*json)
        }
        Commands::Tui => {
            let config = crate::config::Config::load()?;
            crate::cli::tui::run_tui(&config)
        }
    }
}

/// Authentication guard shared by every not-yet-implemented command: fails
/// with [`crate::error::AuthError::NotAuthenticated`] (via
/// [`TokenStore::require`]) when `store` holds no token in either backend,
/// otherwise succeeds without exposing the token itself to the caller.
///
/// Takes an injected `&TokenStore` (rather than resolving one itself) so
/// tests can pass [`TokenStore::with_sources`] fakes instead of touching a
/// real OS keyring or config directory; production call sites build the
/// store via [`TokenStore::resolve`].
///
/// # Errors
///
/// Returns whatever [`TokenStore::require`] returns.
fn require_auth(store: &TokenStore) -> Result<()> {
    store.require()?;
    Ok(())
}

/// A not-yet-implemented command's full behaviour: run [`require_auth`]
/// first, and only on success fail with [`AppError::NotImplemented(name)`] —
/// `sync` with `NotImplemented("sync")`, `add` with `NotImplemented("add")`,
/// `today` with `NotImplemented("today")`. Single shared body for all three
/// so the guard is never copy-pasted per command.
///
/// # Errors
///
/// Returns [`require_auth`]'s error when no token is stored, otherwise
/// [`AppError::NotImplemented(name)`].
#[cfg(test)]
fn guarded_stub(store: &TokenStore, name: &'static str) -> Result<()> {
    require_auth(store)?;
    Err(AppError::NotImplemented(name))
}

#[cfg(test)]
mod tests {
    //! T-109 acceptance coverage. Both branches use an injected
    //! [`TokenStore::with_sources`] fake (in-memory backends only) — no
    //! real OS keyring, no real config directory.
    use super::*;
    use crate::auth::{MemoryTokenSource, Token, TokenSource};

    const FIXTURE_TOKEN: &str = "tok_fake_t109_abc123";

    fn empty_store() -> TokenStore {
        TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        )
    }

    fn seeded_store() -> TokenStore {
        let primary = MemoryTokenSource::new();
        primary
            .store(&Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty fake token"))
            .expect("seed primary");
        TokenStore::with_sources(Box::new(primary), Box::new(MemoryTokenSource::new()))
    }

    #[test]
    fn require_auth_fails_with_not_authenticated_hint_when_no_token_is_stored() {
        let store = empty_store();

        let error = require_auth(&store).expect_err("no token must fail the guard");
        let message = error.to_string();

        assert!(
            message.contains("not authenticated"),
            "error message missing the expected phrase: {message}"
        );
        assert!(
            message.contains("run `todoist-tui auth`"),
            "error message missing the expected hint: {message}"
        );
        assert!(
            !message.contains(FIXTURE_TOKEN),
            "no token material may ever appear in the guard's error message: {message}"
        );
    }

    #[test]
    fn require_auth_succeeds_when_a_token_is_stored() {
        let store = seeded_store();

        require_auth(&store).expect("a stored token must pass the guard");
    }

    #[test]
    fn guarded_stub_propagates_the_guard_error_before_reaching_not_implemented() {
        let store = empty_store();

        let error = guarded_stub(&store, "sync").expect_err("no token must short-circuit");

        assert!(
            matches!(error, AppError::Auth(_)),
            "guard failure must surface as an auth error, not NotImplemented: {error:?}"
        );
        assert!(
            !matches!(error, AppError::NotImplemented(_)),
            "the guard must run before the stub body: {error:?}"
        );
    }

    #[test]
    fn guarded_stub_still_returns_not_implemented_once_authenticated() {
        let store = seeded_store();

        for name in ["sync", "add", "today"] {
            let error = guarded_stub(&store, name)
                .expect_err("the authenticated path still ends in NotImplemented");
            match error {
                AppError::NotImplemented(got) => assert_eq!(got, name),
                other => panic!("expected NotImplemented({name:?}), got {other:?}"),
            }
        }
    }

    #[test]
    fn no_token_material_reaches_the_rendered_error_in_either_branch() {
        for store in [empty_store(), seeded_store()] {
            let result = require_auth(&store);
            let rendered = match &result {
                Ok(()) => String::new(),
                Err(error) => format!("{error}{error:?}"),
            };
            assert!(
                !rendered.contains(FIXTURE_TOKEN),
                "token material leaked into the guard's output: {rendered}"
            );
        }
    }
}

// ---------------------------------------------------------------------------
// T-109 adversarial suite (sdlc-unit-tester, different model family from the
// coder). Deliberately separate from `tests` above and complementary to it:
//   - the guard's *ordering* is proven with call-counting backends (exactly
//     one read per invocation, zero writes), not merely by its outcome,
//   - the three command names are exercised against the SAME store instance
//     in sequence, so no hidden state can mask a later guard,
//   - the no-token failure is pinned to the exact
//     `AppError::Auth(AuthError::NotAuthenticated)` variant (a different
//     auth variant would still fail a substring-only assertion) *and* to the
//     byte-exact rendered message, so wording drift is a test failure,
//   - the gating path is proven never to mutate a backend (`store()`/
//     `clear()` panic if reached), and never to leak token material in
//     either `Display` or `Debug`, raw or `escape_debug`-escaped,
//   - `dispatch()`'s own three arms are checked against the source text so
//     an arm cannot be silently reverted to a bare `NotImplemented`.
// Fakes only: in-memory `TokenSource` implementations. No OS keyring, no
// config directory, no network.
// ---------------------------------------------------------------------------
#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::auth::{MemoryTokenSource, Token, TokenSource};
    use crate::error::AuthError;
    use std::cell::{Cell, RefCell};
    use std::rc::Rc;

    /// Distinctive fake token, deliberately different from the coder's
    /// `FIXTURE_TOKEN` so a leak assertion cannot pass by accident.
    const PROBE_TOKEN: &str = "tok_fake_t109_probe_Qq7_\"'\\}";

    /// Exact text `AuthError::NotAuthenticated` must render (wrapped by
    /// `AppError::Auth`'s `auth: ` context). Pinned as a literal on purpose:
    /// the acceptance requires the phrase "not authenticated" *and* the
    /// `todoist-tui auth` hint, and users are told to run that command.
    const EXPECTED_GUARD_MESSAGE: &str =
        "auth: not authenticated: no API token is stored; run `todoist-tui auth` to add one";

    // ---- fake: counts every backend call, can explode on writes -------

    #[derive(Default)]
    struct CountingState {
        load_calls: Cell<usize>,
        store_calls: Cell<usize>,
        clear_calls: Cell<usize>,
        stored: RefCell<Option<String>>,
    }

    /// Inspectable, programmatically hostile [`TokenSource`]: counts
    /// `load`/`store`/`clear` calls (the handle survives boxing) and can be
    /// armed to panic on any write, to prove the auth guard is read-only.
    #[derive(Clone, Default)]
    struct CountingSource {
        state: Rc<CountingState>,
        panic_on_write: bool,
    }

    impl CountingSource {
        fn new() -> Self {
            Self::default()
        }

        fn read_only() -> Self {
            Self {
                panic_on_write: true,
                ..Self::default()
            }
        }

        fn seeded(value: &str) -> Self {
            let source = Self::new();
            *source.state.stored.borrow_mut() = Some(value.to_string());
            source
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
    }

    impl TokenSource for CountingSource {
        fn load(&self) -> Result<Option<Token>> {
            self.state.load_calls.set(self.load_calls() + 1);
            self.state
                .stored
                .borrow()
                .clone()
                .map(Token::new)
                .transpose()
        }

        fn store(&self, token: &Token) -> Result<()> {
            assert!(
                !self.panic_on_write,
                "the auth guard must never call store() on a backend"
            );
            self.state.store_calls.set(self.store_calls() + 1);
            *self.state.stored.borrow_mut() = Some(token.expose().to_string());
            Ok(())
        }

        fn clear(&self) -> Result<()> {
            assert!(
                !self.panic_on_write,
                "the auth guard must never call clear() on a backend"
            );
            self.state.clear_calls.set(self.clear_calls() + 1);
            *self.state.stored.borrow_mut() = None;
            Ok(())
        }

        fn describe(&self) -> &'static str {
            "counting (unit_tests)"
        }
    }

    /// The two entry points the three stub commands share; both must behave
    /// identically on the guard path.
    const GUARDS: [&str; 2] = ["require_auth", "guarded_stub"];

    fn run_guard(guard: &str, store: &TokenStore) -> Result<()> {
        match guard {
            "require_auth" => require_auth(store),
            "guarded_stub" => guarded_stub(store, "sync"),
            other => panic!("unknown guard under test: {other}"),
        }
    }

    fn counting_store(primary: &CountingSource, secondary: &CountingSource) -> TokenStore {
        TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary.clone()))
    }

    // ---- 1. ordering proof: one read per invocation, zero writes --------

    #[test]
    fn every_guard_invocation_reads_each_needed_backend_exactly_once() {
        for guard in GUARDS {
            // Unauthenticated: primary is empty, so exactly one fallback
            // read of the secondary is expected — never a second attempt.
            let primary = CountingSource::new();
            let secondary = CountingSource::new();
            let store = counting_store(&primary, &secondary);

            assert!(run_guard(guard, &store).is_err(), "{guard}: no token");
            assert_eq!(
                primary.load_calls(),
                1,
                "{guard}: primary read exactly once"
            );
            assert_eq!(
                secondary.load_calls(),
                1,
                "{guard}: the empty primary must fall through exactly once"
            );
            assert_eq!(
                (primary.store_calls(), secondary.store_calls()),
                (0, 0),
                "{guard}: the guard must not write"
            );
            assert_eq!(
                (primary.clear_calls(), secondary.clear_calls()),
                (0, 0),
                "{guard}: the guard must not clear"
            );

            // Repeat on the SAME store: one extra read per invocation, never
            // a hidden retry loop, and still zero writes.
            assert!(run_guard(guard, &store).is_err(), "{guard}: still no token");
            assert_eq!(primary.load_calls(), 2, "{guard}: no extra reads per call");
            assert_eq!(
                secondary.load_calls(),
                2,
                "{guard}: no extra fallback reads"
            );

            // Authenticated: the primary hit must short-circuit the second
            // backend entirely, again with no write.
            let primary = CountingSource::seeded(PROBE_TOKEN);
            let secondary = CountingSource::new();
            let store = counting_store(&primary, &secondary);
            match run_guard(guard, &store) {
                // `require_auth` succeeds; `guarded_stub` clears the guard and
                // then still reports its own unimplemented body.
                Ok(()) => assert_eq!(guard, "require_auth", "{guard}: unexpected Ok"),
                Err(AppError::NotImplemented("sync")) => {
                    assert_eq!(guard, "guarded_stub", "{guard}: unexpected NotImplemented");
                }
                Err(other) => panic!("{guard}: a stored token must pass the guard, got {other:?}"),
            }
            assert_eq!(primary.load_calls(), 1, "{guard}: one primary read");
            assert_eq!(
                secondary.load_calls(),
                0,
                "{guard}: a primary hit must not touch the secondary"
            );
            assert_eq!(
                (
                    primary.store_calls(),
                    secondary.store_calls(),
                    primary.clear_calls()
                ),
                (0, 0, 0),
                "{guard}: the authenticated path must not mutate either backend"
            );
        }
    }

    // ---- 2. independence across the three names, same store instance ----

    #[test]
    fn the_three_names_each_guard_independently_against_one_shared_store() {
        let primary = CountingSource::new();
        let secondary = CountingSource::new();
        let store = counting_store(&primary, &secondary);

        for (index, name) in ["sync", "add", "today"].iter().enumerate() {
            let error = guarded_stub(&store, name)
                .expect_err("every stub must hit the guard on the same empty store");
            assert_eq!(
                error.to_string(),
                EXPECTED_GUARD_MESSAGE,
                "{name} (call {index}) must fail with the auth guard, not skip it"
            );
            assert!(
                matches!(error, AppError::Auth(AuthError::NotAuthenticated)),
                "{name} (call {index}) produced the wrong error: {error:?}"
            );
        }

        // Three guarded invocations, three reads of each backend: no call
        // cached/reused another's verdict and none mutated the store.
        assert_eq!(primary.load_calls(), 3);
        assert_eq!(secondary.load_calls(), 3);
        assert_eq!((primary.store_calls(), primary.clear_calls()), (0, 0));
    }

    // ---- 3. error-variant precision -------------------------------------

    #[test]
    fn the_no_token_error_is_exactly_the_not_authenticated_variant() {
        let store = TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        );

        // Exact-variant matches (not "some Auth error"): a future change to
        // `InvalidToken`, `EmptyToken`, `Backend`, ... would still contain
        // "auth" but must fail here.
        match require_auth(&store) {
            Err(AppError::Auth(AuthError::NotAuthenticated)) => {}
            other => panic!("require_auth: expected Auth(NotAuthenticated), got {other:?}"),
        }
        for name in ["sync", "add", "today"] {
            match guarded_stub(&store, name) {
                Err(AppError::Auth(AuthError::NotAuthenticated)) => {}
                other => {
                    panic!("guarded_stub({name}): expected Auth(NotAuthenticated), got {other:?}")
                }
            }
        }

        let debug = format!("{:?}", require_auth(&store).expect_err("no token"));
        assert_eq!(
            debug, "Auth(NotAuthenticated)",
            "the guard's error must be the NotAuthenticated variant, not a lookalike: {debug}"
        );
        // Sanity: the siblings this must not be confused with render
        // differently, so the pin above is meaningful.
        assert_ne!(
            format!("{:?}", AppError::from(AuthError::InvalidToken)),
            debug
        );
        assert_ne!(
            format!("{:?}", AppError::from(AuthError::EmptyToken)),
            debug
        );
    }

    // ---- 4. byte-exact message pin --------------------------------------

    #[test]
    fn the_guard_message_is_pinned_byte_for_byte_on_both_entry_points() {
        let store = TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        );

        let from_require = require_auth(&store).expect_err("no token");
        assert_eq!(
            from_require.to_string(),
            EXPECTED_GUARD_MESSAGE,
            "guard message drifted from the documented wording"
        );
        for name in ["sync", "add", "today"] {
            let from_stub = guarded_stub(&store, name).expect_err("no token");
            assert_eq!(
                from_stub.to_string(),
                EXPECTED_GUARD_MESSAGE,
                "guarded_stub({name}) message drifted"
            );
            assert!(
                from_stub.to_string().contains("run `todoist-tui auth`"),
                "the hint must survive: {from_stub}"
            );
        }

        // The underlying `AuthError` renders the message without the
        // `AppError::Auth` context prefix — both levels are pinned so a
        // change in either layer is caught.
        assert_eq!(
            AuthError::NotAuthenticated.to_string(),
            "not authenticated: no API token is stored; run `todoist-tui auth` to add one"
        );
    }

    // ---- 5. the guard path is strictly read-only ------------------------

    #[test]
    fn the_gate_never_calls_store_or_clear_on_any_backend() {
        // Both backends panic if written to; the guard must only ever read.
        let primary = CountingSource::read_only();
        let secondary = CountingSource::read_only();
        let store = counting_store(&primary, &secondary);

        for name in ["sync", "add", "today"] {
            let error =
                guarded_stub(&store, name).expect_err("no token stored: every stub must be gated");
            assert_eq!(error.to_string(), EXPECTED_GUARD_MESSAGE);
        }
        assert!(require_auth(&store).is_err(), "still unauthenticated");
        // Reaching this line at all proves no `store()`/`clear()` ran.
        assert_eq!(
            (primary.store_calls(), primary.clear_calls()),
            (0, 0),
            "write counters stayed at zero"
        );
        assert_eq!((secondary.store_calls(), secondary.clear_calls()), (0, 0));
        assert_eq!(primary.load_calls(), 4, "one read per guard invocation");

        // The authenticated branch must be equally read-only.
        let primary = CountingSource::seeded(PROBE_TOKEN);
        let secondary = CountingSource::read_only();
        let store = TokenStore::with_sources(Box::new(primary.clone()), Box::new(secondary));
        require_auth(&store).expect("a stored token passes the guard read-only");
        assert_eq!(primary.load_calls(), 1);
        assert_eq!((primary.store_calls(), primary.clear_calls()), (0, 0));
    }

    // ---- 6. dispatch() arms route through the shared guard --------------

    #[test]
    fn dispatch_routes_all_three_stub_commands_through_the_shared_guard() {
        // `dispatch` resolves the real `TokenStore`, so routing is asserted
        // against the source of `dispatch` itself: no arm may construct a
        // bare `NotImplemented` and thereby bypass the guard.
        const SOURCE: &str = include_str!("commands.rs");
        const ARMS: [&str; 3] = ["Commands::Sync", "Commands::Add", "Commands::Today"];

        let start = SOURCE
            .find("pub fn dispatch")
            .expect("dispatch must exist in this file");
        let body = &SOURCE[start..];
        let body = &body[..body.find("\n}\n").expect("dispatch body must end")];

        assert!(
            !body.contains("AppError::NotImplemented"),
            "dispatch must not construct NotImplemented directly (guard bypass): {body}"
        );
        for arm in ARMS {
            let at = body
                .find(arm)
                .unwrap_or_else(|| panic!("dispatch lost the {arm} match arm"));
            let rest = &body[at + arm.len()..];
            let route_end = ARMS
                .iter()
                .filter_map(|other| rest.find(other))
                .min()
                .unwrap_or(rest.len());
            let route = &rest[..route_end];
            assert!(
                route.contains("guarded_stub(") || route.contains("require_auth("),
                "{arm} must route through guarded_stub or require_auth, got: {route:?}"
            );
            assert!(
                route.contains("TokenStore::resolve()"),
                "{arm} must resolve a TokenStore before guarding, got: {route:?}"
            );
        }
        assert_eq!(
            body.matches("guarded_stub(").count() + body.matches("require_auth(").count(),
            ARMS.len(),
            "exactly one guard call per guarded arm: {body}"
        );
    }

    // ---- 7. no token material in either branch, raw or escaped ----------

    #[test]
    fn no_token_material_reaches_any_rendering_in_either_branch() {
        let escaped = PROBE_TOKEN.escape_debug().to_string();
        let assert_clean = |rendered: &str, branch: &str| {
            assert!(
                !rendered.contains(PROBE_TOKEN),
                "{branch}: raw token material leaked: {rendered}"
            );
            assert!(
                !rendered.contains(&escaped),
                "{branch}: escaped token material leaked: {rendered}"
            );
        };

        // Unauthenticated branch: the message must not mention any token.
        let store = TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        );
        let error = guarded_stub(&store, "add").expect_err("no token");
        assert_clean(&format!("{error}"), "guard Display");
        assert_clean(&format!("{error:?}"), "guard Debug");
        assert_clean(&format!("{error:#?}"), "guard pretty Debug");

        // Authenticated branch: `NotImplemented(name)` must carry only the
        // command name, even though a token was read to get here.
        let primary = MemoryTokenSource::new();
        primary
            .store(&Token::new(PROBE_TOKEN.to_string()).expect("non-empty fake token"))
            .expect("seed primary");
        let store = TokenStore::with_sources(Box::new(primary), Box::new(MemoryTokenSource::new()));
        let error = guarded_stub(&store, "today").expect_err("stub still unimplemented");
        let rendered = format!("{error}{error:?}");
        assert_clean(&rendered, "authenticated stub");
        assert!(
            rendered.contains("today"),
            "the stub must still name its command: {rendered}"
        );
    }
}
