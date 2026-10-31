//! Sync engine scaffold (T-501, EPIC-06).
//!
//! This task only lands the error vocabulary (`SyncError` in
//! `src/error.rs`) and this module scaffold; the engine itself (sync-token
//! bookkeeping, queue draining, reconciliation with the local store) arrives
//! in later EPIC-06 tasks so the rest of the epic can land in parallel.

// No consumers yet: later EPIC-06 tasks add the sync engine here. `pub`
// alone does not silence `dead_code` in a binary crate, so allow it at
// module scope — same pattern as `error.rs` (T-003) and `src/api/mod.rs`
// (T-201).
#![allow(dead_code)]
