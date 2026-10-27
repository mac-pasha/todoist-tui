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
