//! Offline command queue (T-402, EPIC-05).
//!
//! T-402 landed the error vocabulary (`QueueError` in `src/error.rs`) and
//! this module scaffold; T-403 adds the [`command`] submodule, modelling a
//! single queued command and its Sync API JSON projection. Persistence and
//! the replay loop are not wired up yet, so items here have no production
//! consumer until later EPIC-05 tasks land — hence the module-scope
//! `#![allow(dead_code, unused_imports)]`, same pattern as `error.rs`
//! (T-003) and `src/api/mod.rs` (T-201).
#![allow(dead_code, unused_imports)]

pub mod command;
pub mod enqueue;
pub mod flush;
pub mod recovery;
pub mod schema;
pub mod status;
pub mod store;
pub mod temp_ids;

pub use command::{CommandState, QueuedCommand};
pub use enqueue::{enqueue, enqueue_many};
pub use flush::{build_default_flush_batch, build_flush_batch, DEFAULT_MAX_BATCH};
pub use recovery::{recover_on_start, MAX_ATTEMPTS, MAX_ATTEMPTS_ERROR};
pub use status::{
    apply_sync_status, classify, failed_commands, ErrorClass, FailedCommand, StatusOutcome,
};
pub use store::{CommandStateCounts, CommandStore};
pub use temp_ids::{
    resolve_command_temp_ids, resolve_temp_ids, rewrite_json_value, rewrite_sqlite_cache_temp_ids,
    verify_no_temp_ids,
};
