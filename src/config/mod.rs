//! Configuration subsystem (EPIC-01).
//!
//! T-004 lands platform directory resolution in [`paths`]; T-005 layers the
//! TOML config [`model`] and [`load`]er on top of it.
//!
//! # Unknown keys are rejected, not ignored (T-005 decision)
//!
//! [`Config`] is `#[serde(deny_unknown_fields)]`: a mistyped key in
//! `config.toml` fails the load with a precise parse error (unknown-field
//! message naming the key, at the key's line) instead of being silently
//! dropped. Rationale: for a single-user, hand-edited config, a setting that
//! "saves" but never takes effect is worse than a loud failure at startup,
//! and the binary plus its config belong to the same user, upgraded in
//! lockstep, so forward compatibility with newer config files does not
//! justify silent ignores. The check applies to top-level keys only — the
//! `keymap` table stays free-form (EPIC-07 interprets the bindings; T-005
//! does not validate key names).

pub mod load;
pub mod model;
pub mod paths;

// Re-exported for later tasks' consumers; in a binary crate an unreachable
// `pub use` warns as unused until something references it (same timing as
// the module-level `dead_code` allows above).
#[allow(unused_imports)]
pub use model::{Config, DefaultView};

/// Shared test-only support for `config::paths` and `config::load`.
///
/// Both modules are compiled into the *same* test binary (both are `mod`s of
/// this binary crate), so their `#[cfg(test)]` modules run as separate
/// threads of one process, not separate processes. Any test that mutates
/// `XDG_CONFIG_HOME` / `XDG_STATE_HOME` / `HOME` (process-global state) must
/// therefore serialise against *every other such test in the binary*, not
/// just the ones in its own module. A single shared [`Mutex`] here — rather
/// than one `static ENV_LOCK` per module — is what gives that guarantee
/// (T-104 follow-up: two independent per-module mutexes provided no real
/// mutual exclusion between `paths` and `load` tests, causing an
/// intermittent env-leak flake).
#[cfg(test)]
pub(crate) mod test_support {
    use std::sync::Mutex;

    /// Guards every env-var-mutating test in `config::paths` and
    /// `config::load`. A plain `Mutex` is used instead of a `serial_test`
    /// dependency (no new crates; `Cargo.toml` is out of scope).
    pub(crate) static ENV_LOCK: Mutex<()> = Mutex::new(());
}
