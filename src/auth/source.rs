//! The [`TokenSource`] seam (T-102, EPIC-02).
//!
//! `TokenSource` is the storage-agnostic boundary between the rest of the
//! app and wherever the Todoist API token actually lives. T-103 lands an OS
//! keyring-backed implementation and T-104 lands a file-backed fallback;
//! both plug into this same trait. It is also the seam a future OAuth token
//! source would implement (REQ-001 open question 7: should auth move from a
//! static API token to OAuth?) without changing any caller.
//!
//! Implementations MUST NOT log token material. `load`/`store` move
//! [`Token`] values, which already redact `Display`/`Debug` via
//! [`crate::log::Secret`]; backends must not bypass that by formatting the
//! exposed string, writing it to a log sink, or embedding it in an error
//! variant.

use std::cell::RefCell;

use crate::auth::Token;
use crate::error::Result;
use crate::log::Secret;

/// Storage-agnostic seam for reading/writing the Todoist API token.
///
/// See the module docs for the OAuth-seam rationale and the "never log
/// token material" requirement that binds every implementation.
pub trait TokenSource {
    /// Reads the currently stored token, if any.
    ///
    /// Returns `Ok(None)` when no token has been stored (or it has been
    /// [`clear`](TokenSource::clear)ed), never an error for that case.
    fn load(&self) -> Result<Option<Token>>;

    /// Persists `token`, replacing any previously stored token.
    fn store(&self, token: &Token) -> Result<()>;

    /// Removes the stored token, if any.
    ///
    /// MUST be idempotent: calling `clear` when nothing is stored returns
    /// `Ok(())`, it is not an error (`logout` calls this unconditionally on
    /// every backend regardless of whether a token is present).
    fn clear(&self) -> Result<()>;

    /// Human-readable backend name for user-visible notices (e.g. "OS
    /// keyring", "token file"). Never includes token material.
    fn describe(&self) -> &'static str;
}

/// In-memory [`TokenSource`] for tests and local development.
///
/// Stores the token's raw bytes behind a [`RefCell`] so `&self` methods can
/// mutate it; nothing here is persisted or shared outside the process. The
/// stored value is wrapped in [`Secret`] so derived/automatic `Debug`
/// rendering (e.g. `tracing`'s `?backend`) never leaks token material, per
/// the module docs' no-bypass contract.
#[derive(Debug, Default)]
pub struct MemoryTokenSource {
    stored: RefCell<Option<Secret<String>>>,
}

impl MemoryTokenSource {
    /// Builds an empty in-memory token source.
    #[must_use]
    pub fn new() -> Self {
        Self {
            stored: RefCell::new(None),
        }
    }
}

impl TokenSource for MemoryTokenSource {
    fn load(&self) -> Result<Option<Token>> {
        self.stored
            .borrow()
            .as_ref()
            .map(|raw| Token::new(raw.expose().clone()))
            .transpose()
    }

    fn store(&self, token: &Token) -> Result<()> {
        *self.stored.borrow_mut() = Some(Secret::new(token.expose().to_string()));
        Ok(())
    }

    fn clear(&self) -> Result<()> {
        *self.stored.borrow_mut() = None;
        Ok(())
    }

    fn describe(&self) -> &'static str {
        "in-memory (test/dev)"
    }
}

#[cfg(test)]
mod tests {
    //! Sanity checks proving the T-102 design constraints hold; the
    //! unit-tester adds the full suite separately.
    use super::*;

    const FIXTURE_TOKEN: &str = "tok_fake_source_abc123";

    #[test]
    fn trait_is_object_safe_and_callable_through_a_box() {
        let source: Box<dyn TokenSource> = Box::new(MemoryTokenSource::new());
        assert!(source.load().expect("load ok").is_none());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        assert!(source.load().expect("load ok").is_some());
        source.clear().expect("clear ok");
        assert_eq!(source.describe(), "in-memory (test/dev)");
    }

    #[test]
    fn store_then_load_round_trips_an_equal_token() {
        let source = MemoryTokenSource::new();
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        let loaded = source.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), token.expose());
    }

    #[test]
    fn clear_then_load_returns_none() {
        let source = MemoryTokenSource::new();
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        source.clear().expect("clear ok");
        assert!(source.load().expect("load ok").is_none());
    }

    #[test]
    fn clear_on_empty_storage_is_a_no_op() {
        let source = MemoryTokenSource::new();
        assert!(source.load().expect("load ok").is_none());
        source.clear().expect("clear on empty is Ok(())");
        assert!(source.load().expect("load ok").is_none());
    }
}

/// Extended T-102 coverage (unit-tester, different model family from the
/// coder). Complements — does not duplicate — the sanity `tests` module
/// above: object safety exercised at collection scale (the T-103/T-104
/// fallback-chain shape), interior mutability through aliased `&dyn`
/// references, store-replaces semantics, clear idempotency loops,
/// store→clear→store cycles, cross-instance isolation, `Default`
/// equivalence, non-destructive `load`, byte-exact round-trips for
/// adversarial values, `describe` stability, white-box defensive
/// re-validation in `load`, and the never-leak `Debug` contract the trait
/// docs impose on every backend.
#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::error::{AppError, AuthError};

    /// Fakes only — never real credentials (repo rule 7).
    const FAKE_A: &str = "tok_fake_source_aaa111";
    const FAKE_B: &str = "tok_fake_source_bbb222";

    fn token(value: &str) -> Token {
        Token::new(value.to_string()).expect("non-empty fake token")
    }

    // ---- Acceptance 2: object safety beyond a single Box ----

    #[test]
    fn all_four_methods_work_through_box_dyn_ref_dyn_and_a_chain() {
        let boxed: Box<dyn TokenSource> = Box::new(MemoryTokenSource::new());
        assert!(boxed.load().expect("load ok").is_none());
        boxed.store(&token(FAKE_A)).expect("store ok");
        assert_eq!(
            boxed
                .load()
                .expect("load ok")
                .expect("token present")
                .expose(),
            FAKE_A
        );
        boxed.clear().expect("clear ok");
        assert_ne!(boxed.describe(), "");

        // Aliased shared references are the shape T-103/T-104 app state
        // will use once concrete backends sit behind the same trait object.
        let concrete = MemoryTokenSource::new();
        let r1: &dyn TokenSource = &concrete;
        let r2: &dyn TokenSource = &concrete;
        r1.store(&token(FAKE_B)).expect("store via &dyn");
        assert_eq!(
            r2.load().expect("load via &dyn").expect("present").expose(),
            FAKE_B
        );

        // A Vec of trait objects (the backend fallback-chain shape) stays
        // usable end to end.
        let chain: Vec<Box<dyn TokenSource>> = vec![
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        ];
        for (i, src) in chain.iter().enumerate() {
            let v = format!("tok_fake_chain_{i}");
            src.store(&token(&v)).expect("store ok");
            assert_eq!(src.load().expect("load ok").expect("present").expose(), v);
            src.clear().expect("clear ok");
            assert_ne!(src.describe(), "");
        }
    }

    // ---- Acceptance 3: MemoryTokenSource semantics ----

    #[test]
    fn interior_mutability_mutates_through_shared_references() {
        // Every trait method takes `&self`; the RefCell exists so mutation
        // works behind a shared — even aliased — reference, with no `&mut`
        // escape hatch anywhere on the type.
        let source = MemoryTokenSource::new();
        let r1: &dyn TokenSource = &source;
        let r2: &dyn TokenSource = &source;
        r1.store(&token(FAKE_A)).expect("store via alias 1");
        assert_eq!(
            r2.load()
                .expect("load via alias 2")
                .expect("present")
                .expose(),
            FAKE_A
        );
        r2.store(&token(FAKE_B)).expect("store via alias 2");
        assert_eq!(
            r1.load()
                .expect("load via alias 1")
                .expect("present")
                .expose(),
            FAKE_B
        );
        r1.clear().expect("clear via alias 1");
        assert!(r2.load().expect("load via alias 2").is_none());
    }

    #[test]
    fn store_replaces_the_previously_stored_token() {
        // Trait doc: "Persists `token`, replacing any previously stored
        // token" — the second store must win, not append and not no-op.
        let source = MemoryTokenSource::new();
        source.store(&token(FAKE_A)).expect("store A");
        source.store(&token(FAKE_B)).expect("store B");
        let loaded = source.load().expect("load").expect("present");
        assert_eq!(loaded.expose(), FAKE_B, "latest store must win");
        assert_ne!(loaded.expose(), FAKE_A, "stale token must not linger");
    }

    #[test]
    fn round_trip_preserves_exact_bytes_for_adversarial_values() {
        // Marker-colliding, whitespace-padded (valid per `Token::new`:
        // validation trims but never normalises), unicode,
        // newline-bearing, single-char boundary and 1 KiB values must all
        // survive store→load byte-for-byte.
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

        let source = MemoryTokenSource::new();
        for v in &values {
            source.store(&token(v)).expect("store ok");
            let loaded = source.load().expect("load ok").expect("present");
            assert_eq!(loaded.expose(), v, "round-trip must be byte-exact");
            assert_eq!(loaded.expose().len(), v.len());
        }
    }

    #[test]
    fn clear_is_idempotent_in_both_empty_and_stored_states() {
        let source = MemoryTokenSource::new();
        for _ in 0..3 {
            source.clear().expect("clear on empty is Ok(())");
        }
        assert!(source.load().expect("load").is_none());
        source.store(&token(FAKE_A)).expect("store");
        for _ in 0..3 {
            source.clear().expect("clear on stored is Ok(())");
        }
        assert!(source.load().expect("load").is_none());
    }

    #[test]
    fn store_clear_store_cycles_keep_the_source_usable() {
        // Logout/login loops must not wedge the backend.
        let source = MemoryTokenSource::new();
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

    #[test]
    fn distinct_instances_do_not_share_state() {
        let a = MemoryTokenSource::new();
        let b = MemoryTokenSource::new();
        a.store(&token(FAKE_A)).expect("store a");
        assert!(
            b.load().expect("load b").is_none(),
            "no shared/global state"
        );
        b.store(&token(FAKE_B)).expect("store b");
        assert_eq!(a.load().expect("load a").expect("present").expose(), FAKE_A);
        assert_eq!(b.load().expect("load b").expect("present").expose(), FAKE_B);
        a.clear().expect("clear a");
        assert!(a.load().expect("load a").is_none());
        assert!(
            b.load().expect("load b").is_some(),
            "clear(a) must not touch b"
        );
    }

    #[test]
    fn default_matches_new_for_a_fresh_instance() {
        let d = MemoryTokenSource::default();
        assert!(d.load().expect("load").is_none());
        d.store(&token(FAKE_A)).expect("store");
        assert!(d.load().expect("load").is_some());
        assert_ne!(d.describe(), "");
    }

    #[test]
    fn load_is_non_destructive_and_repeatable() {
        let source = MemoryTokenSource::new();
        source.store(&token(FAKE_A)).expect("store");
        for _ in 0..3 {
            let first = source.load().expect("load 1").expect("present");
            let second = source.load().expect("load 2").expect("present");
            assert_eq!(first.expose(), FAKE_A);
            assert_eq!(second.expose(), FAKE_A);
        }
    }

    // ---- Acceptance 1/4: describe() and the no-leak contract ----

    #[test]
    fn describe_is_stable_across_calls_instances_and_states() {
        let a = MemoryTokenSource::new();
        let before = a.describe();
        a.store(&token(FAKE_A)).expect("store");
        a.clear().expect("clear");
        let after = a.describe();
        assert_eq!(before, after, "describe must not depend on stored state");
        assert_eq!(
            MemoryTokenSource::new().describe(),
            before,
            "stable across instances"
        );
        assert!(!before.is_empty(), "user-visible notices need a real name");
        assert!(
            !before.contains(FAKE_A),
            "name must never carry token material"
        );
    }

    #[test]
    fn debug_rendering_never_exposes_stored_token_material() {
        // The trait docs bind every implementation: token material must
        // never be formattable into a log (`tracing`'s `?backend` uses
        // Debug; EPIC-12's log scan greps for exactly this leak class).
        // `Token` redacts Display/Debug via `Secret`; a backend holding a
        // raw copy must not undo that through derived formatting.
        let source = MemoryTokenSource::new();
        source.store(&token(FAKE_A)).expect("store ok");
        let dbg = format!("{source:?}");
        let pretty = format!("{source:#?}");
        assert!(
            !dbg.contains(FAKE_A) && !pretty.contains(FAKE_A),
            "Debug leaked stored token: {dbg}"
        );
        assert!(
            dbg.contains(crate::log::REDACTED),
            "no redaction marker in {dbg}"
        );
        assert!(
            pretty.contains(crate::log::REDACTED),
            "no redaction marker in {pretty}"
        );
    }

    // ---- Defensive edge (white-box) ----

    #[test]
    fn load_revalidates_and_errors_cleanly_on_corrupted_state() {
        // WHITE-BOX: unreachable through the public API (`store` only
        // accepts `Token` values, which `Token::new` already validated).
        // If the RefCell ever holds a whitespace-only string (e.g. via a
        // future serde/FromStr path), `load` must surface
        // `AuthError::EmptyToken` — not a bogus token, not a panic.
        let source = MemoryTokenSource::new();
        for corrupt in ["", " ", "\t\n"] {
            *source.stored.borrow_mut() = Some(Secret::new(corrupt.to_string()));
            match source.load() {
                Err(AppError::Auth(AuthError::EmptyToken)) => {}
                other => panic!("corrupt {corrupt:?}: expected EmptyToken, got {other:?}"),
            }
        }
    }

    #[test]
    fn memory_source_is_send_for_thread_moves() {
        // Documented reality for later wiring: `RefCell` keeps the backend
        // `Send` (movable into another thread) but NOT `Sync` (no `&self`
        // sharing across threads — negative bounds can't be asserted in a
        // positive test; the RefCell field is the structural proof).
        // T-103/T-104 must pick `Box<dyn TokenSource + Send + Sync>`
        // before app state crosses tokio task boundaries.
        fn assert_send<T: Send>() {}
        assert_send::<MemoryTokenSource>();
    }
}
