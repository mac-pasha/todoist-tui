//! Authentication / API token handling (T-101, EPIC-02).
//!
//! [`token`] defines the [`Token`] newtype that keeps the Todoist API token
//! redacted everywhere except the single, greppable [`Token::expose`] read
//! path. Consumers (keyring storage, HTTP client wiring) land in later
//! EPIC-02 tasks.

// Consumers land in later EPIC-02 tasks (keyring storage, HTTP client
// wiring); `pub` alone does not silence `dead_code` in a binary crate, so
// allow it at module scope — same pattern as `error.rs` (T-003) and
// `config/paths.rs` (T-004).
#![allow(dead_code, unused_imports)]

pub mod token;

pub use token::Token;
