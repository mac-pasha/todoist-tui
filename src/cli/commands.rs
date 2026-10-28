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
use crate::cli::Commands;
use crate::error::{AppError, Result};

/// Routes a parsed subcommand to its implementation.
///
/// # Errors
///
/// Returns whatever the routed implementation returns for `auth`/`logout`;
/// `sync`/`add`/`today` return [`crate::error::AuthError::NotAuthenticated`]
/// (via [`require_auth`]) when no token is stored, otherwise
/// [`AppError::NotImplemented`] with the subcommand's name.
pub fn dispatch(command: &Commands) -> Result<()> {
    match command {
        Commands::Auth => run_auth(),
        Commands::Logout => run_logout(),
        Commands::Sync => guarded_stub(&TokenStore::resolve()?, "sync"),
        // `text` is intentionally unused: this task only captures the quick-add
        // argument on the CLI surface, EPIC-11 parses it.
        Commands::Add { text: _ } => guarded_stub(&TokenStore::resolve()?, "add"),
        Commands::Today { json: _ } => guarded_stub(&TokenStore::resolve()?, "today"),
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
