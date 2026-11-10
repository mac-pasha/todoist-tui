//! Shared integration-test harness (T-1101, EPIC-12).
//!
//! Cargo only treats `tests/*.rs` as test targets, so this directory is not a
//! test binary: each integration test includes it with `mod support;` and
//! uses the two helpers re-exported here.
//!
//! * [`MockSyncApi`] — a `wiremock` Todoist Sync API server with one named
//!   constructor per scenario (full sync, incremental, 401, 429, 5xx,
//!   transport drop) and `received_commands()` for exactly-once assertions.
//! * [`TestHome`] — an RAII isolated HOME/XDG sandbox holding a file-backed
//!   fake token, with a ready-made `Command` for the real binary.
//!
//! The harness deliberately depends on no crate-internal types, so it needs
//! no `#[path]` includes of `src/` and compiles into any test binary. Tests
//! that want crate-internal types (e.g. `SyncClient`) add the `#[path]`
//! includes themselves, as `tests/sync_client.rs` does.
//!
//! Not every test binary uses every helper, and `dead_code` is per-crate, so
//! it is allowed here rather than failing `clippy -D warnings` per binary.

#![allow(dead_code, unused_imports)]

pub mod dataset;
pub mod env;
pub mod mock_api;
pub mod sync_fixtures;

pub use env::{TestHome, FAKE_TOKEN};
pub use mock_api::MockSyncApi;
