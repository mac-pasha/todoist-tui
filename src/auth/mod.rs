//! Authentication / API token handling (T-101, T-102, T-103, EPIC-02).
//!
//! [`token`] defines the [`Token`] newtype that keeps the Todoist API token
//! redacted everywhere except the single, greppable [`Token::expose`] read
//! path. [`source`] defines the [`TokenSource`] storage seam; [`keyring_source`]
//! is the OS keyring-backed implementation and [`file_source`] is the
//! file-backed fallback (T-104, owner-only `0600` permissions) used when the
//! OS keyring (T-103) is unavailable. [`store`] is the keyring-first,
//! file-fallback [`TokenStore`] resolution every command goes through
//! (T-105). [`validate`] calls the real Todoist API over HTTPS-only
//! `reqwest` (rustls) to confirm a token is accepted (T-106). [`cache_wipe`]
//! defines the [`CacheWipe`] hook `logout` (T-108) calls to wipe the local
//! task cache; the real SQLite-backed implementation is EPIC-04's, so
//! [`NoopCacheWipe`] is wired in for now.

// Consumers land in later EPIC-02 tasks (keyring storage, HTTP client
// wiring); `pub` alone does not silence `dead_code` in a binary crate, so
// allow it at module scope — same pattern as `error.rs` (T-003) and
// `config/paths.rs` (T-004).
#![allow(dead_code, unused_imports)]

pub mod cache_wipe;
pub mod file_source;
pub mod keyring_source;
pub mod source;
pub mod store;
pub mod token;
pub mod validate;

pub use cache_wipe::{CacheWipe, NoopCacheWipe};
pub use file_source::FileTokenSource;
pub use keyring_source::KeyringTokenSource;
pub use source::{MemoryTokenSource, TokenSource};
pub use store::TokenStore;
pub use token::Token;
pub use validate::{check, check_blocking, DEFAULT_BASE_URL};
