//! Typed configuration model (T-005, EPIC-01).
//!
//! [`Config`] is the deserialized shape of the TOML file located by
//! [`paths::config_file`](crate::config::paths::config_file); the loader
//! itself lives in [`crate::config::load`]. Every field is backed by
//! `#[serde(default = "...")]`, so a missing key — or a missing file, which
//! the loader maps to [`Config::default`] — always yields a usable [`Config`].
//!
//! `sync_interval` is stored as a [`Duration`]: the TOML representation stays
//! a plain integer number of seconds (`sync_interval = 60`), deserialized by
//! [`de_secs_to_duration`]. `keymap` is a free-form string map — EPIC-07
//! interprets the bindings, so no key or value validation happens here.

// The model ships ahead of its consumer: nothing calls `Config::load` until a
// later task wires the binary. In a binary crate `pub` alone does not silence
// `dead_code`, so allow it at module scope — same pattern as error.rs (T-003)
// and paths.rs (T-004). Remove once a consumer exists.
#![allow(dead_code)]

use std::collections::HashMap;
use std::time::Duration;

use serde::Deserialize;

/// Default sync interval in seconds: one minute.
const DEFAULT_SYNC_INTERVAL_SECS: u64 = 60;

/// Name of the built-in theme selected when `config.toml` does not name one.
/// Theme assets arrive with the UI epic; this name is the stable contract.
const DEFAULT_THEME: &str = "default";

/// Top-level shape of `config.toml`.
///
/// # Unknown keys: rejected, not ignored (T-005 acceptance item 6)
///
/// The struct carries `#[serde(deny_unknown_fields)]`: a mistyped key makes
/// the load fail with a precise `ConfigError::Parse` (unknown-field message
/// naming the key, at the key's line) instead of being silently dropped. For
/// a single-user, hand-edited config, a setting that "saves" but never takes
/// effect is worse than a loud startup failure; and since the binary and its
/// config belong to the same user and upgrade in lockstep, forward
/// compatibility with newer config files does not justify silent ignores.
/// The check applies to top-level keys only — entries inside the `[keymap]`
/// table stay free-form.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Config {
    /// How often the sync engine polls for remote changes.
    ///
    /// TOML form is an integer number of seconds (`sync_interval = 60`);
    /// stored as a [`Duration`] so consumers (e.g. the EPIC-02 tokio
    /// interval) need no conversion. Defaults to one minute.
    #[serde(
        default = "default_sync_interval",
        deserialize_with = "de_secs_to_duration"
    )]
    pub sync_interval: Duration,

    /// Name of the active UI theme. Defaults to the built-in
    /// [`DEFAULT_THEME`] theme.
    #[serde(default = "default_theme")]
    pub theme: String,

    /// Keybinding overrides: action or key name → the key the user wants.
    ///
    /// Free-form on purpose: EPIC-07 interprets the entries, so neither keys
    /// nor values are validated at load time (per the T-005 acceptance). An
    /// empty map means "use the built-in bindings".
    #[serde(default)]
    pub keymap: HashMap<String, String>,

    /// View shown when the app starts. Defaults to [`DefaultView::Inbox`].
    #[serde(default)]
    pub default_view: DefaultView,
}

impl Default for Config {
    /// The all-defaults configuration: one-minute sync, built-in theme, no
    /// key overrides, Inbox at startup. Identical to what the loader yields
    /// for a missing file or an empty `config.toml`.
    fn default() -> Self {
        Self {
            sync_interval: default_sync_interval(),
            theme: default_theme(),
            keymap: HashMap::new(),
            default_view: DefaultView::default(),
        }
    }
}

/// The view shown when the app starts.
///
/// TOML form is a lowercase string (`default_view = "today"`); any other
/// value is a load-time `ConfigError::Parse` naming the expected variants.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum DefaultView {
    /// The Inbox (default).
    #[default]
    Inbox,
    /// Tasks due today.
    Today,
    /// Tasks due in the future.
    Upcoming,
}

/// Default [`Config::sync_interval`]: [`DEFAULT_SYNC_INTERVAL_SECS`] seconds.
fn default_sync_interval() -> Duration {
    Duration::from_secs(DEFAULT_SYNC_INTERVAL_SECS)
}

/// Default [`Config::theme`]: the [`DEFAULT_THEME`] theme.
fn default_theme() -> String {
    String::from(DEFAULT_THEME)
}

/// Deserializes an integer number of seconds into a [`Duration`].
///
/// The on-disk form stays a plain integer (`sync_interval = 60`) rather than
/// an ISO-8601 or humantime string; negative or non-integer values are
/// rejected by serde with the offending key named in the message.
fn de_secs_to_duration<'de, D>(deserializer: D) -> std::result::Result<Duration, D::Error>
where
    D: serde::Deserializer<'de>,
{
    u64::deserialize(deserializer).map(Duration::from_secs)
}
