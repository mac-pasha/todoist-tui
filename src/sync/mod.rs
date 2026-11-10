//! Sync engine orchestration (background, offline, conflict policy) (EPIC-06).

#![allow(dead_code, unused_imports)]

pub mod conflict;
pub mod cycle;
pub mod engine;
pub mod fakes;
pub mod handle;
pub mod lag;
pub mod offline;
pub mod ports;
pub mod state;

pub use cycle::{CycleOutcome, SyncCycle, CYCLE_FLUSH_BATCH_LIMIT};
pub use engine::SyncEngine;
pub use handle::{Receiver, SyncDriver, SyncHandle, SyncStateReceiver};
pub use offline::{OfflineTracker, DEFAULT_MAX_OFFLINE_BACKOFF};
pub use ports::{BoxFuture, Cache, QueueStatusSummary, SqliteSyncStore, SyncApi, WriteQueue};
pub use state::{SyncState, SyncStatus, SyncTimestamp};
