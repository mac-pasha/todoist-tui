//! In-memory test doubles for [`SyncApi`], [`Cache`], and [`WriteQueue`] (T-503, EPIC-06).
//!
//! Designed for deterministic unit testing of the sync cycle (`T-505`..`T-508`),
//! offline backoff (`T-509`), and actor orchestration (`T-510`) without HTTP or
//! SQLite I/O. Every fake records its received arguments in order, supports
//! scripting per-call responses or errors, and can share a [`CallLog`] so tests
//! can assert cross-port ordering invariants (such as `resolve_temp_ids` ->
//! `apply_sync_status` -> `apply_response` in `T-506`).

#![allow(dead_code, clippy::unwrap_used, clippy::expect_used)]

use std::collections::{HashMap, VecDeque};
use std::sync::{Arc, Mutex};

use crate::api::model::{Id, Item};
use crate::api::sync::{CommandStatus, SyncRequest, SyncResponse};
use crate::db::apply::{ApplyStats, ResourceCounts};
use crate::error::{ApiError, AppError, Result, SyncError};
use crate::queue::command::{CommandState, QueuedCommand};
use crate::queue::temp_ids::rewrite_json_value;
use crate::sync::ports::{BoxFuture, Cache, QueueStatusSummary, SyncApi, WriteQueue};

/// Single recorded invocation across [`FakeApi`], [`FakeCache`], and [`FakeQueue`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PortCall {
    /// [`SyncApi::sync`] was called.
    ApiSync,
    /// [`Cache::sync_token`] was called.
    CacheGetSyncToken,
    /// [`Cache::set_sync_token`] was called with the given token.
    CacheSetSyncToken(String),
    /// [`Cache::full_sync_date_utc`] was called.
    CacheGetFullSyncDateUtc,
    /// [`Cache::apply_response`] was called with a response carrying `full_sync`.
    CacheApplyResponse {
        /// Value of `response.full_sync`.
        full_sync: bool,
    },
    /// [`Cache::item_by_id`] was called for `id`.
    CacheItemById(Id),
    /// [`Cache::upsert_item`] was called for `id`.
    CacheUpsertItem(Id),
    /// [`Cache::delete_item`] was called for `id`.
    CacheDeleteItem(Id),
    /// [`WriteQueue::pending_batch`] was called with `max_batch`.
    QueuePendingBatch {
        /// Requested maximum batch size.
        max_batch: usize,
    },
    /// [`WriteQueue::pending_commands`] was called.
    QueuePendingCommands,
    /// [`WriteQueue::pending_count`] was called.
    QueuePendingCount,
    /// [`WriteQueue::resolve_temp_ids`] was called.
    QueueResolveTempIds,
    /// [`WriteQueue::apply_sync_status`] was called.
    QueueApplySyncStatus,
    /// [`WriteQueue::requeue_in_flight`] was called.
    QueueRequeueInFlight,
}

/// Shared, thread-safe call log for asserting ordering across multiple port fakes.
#[derive(Debug, Default, Clone)]
pub struct CallLog {
    calls: Arc<Mutex<Vec<PortCall>>>,
}

impl CallLog {
    /// Creates an empty [`CallLog`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Appends `call` to the log.
    pub fn record(&self, call: PortCall) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .push(call);
    }

    /// Returns a snapshot of all recorded [`PortCall`] entries in invocation order.
    #[must_use]
    pub fn calls(&self) -> Vec<PortCall> {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }

    /// Clears all recorded entries.
    pub fn clear(&self) {
        self.calls
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clear();
    }
}

/// Convenience constructor returning a (`FakeApi`, `FakeCache`, `FakeQueue`, `CallLog`)
/// tuple wired to the same shared [`CallLog`].
#[must_use]
pub fn fakes_with_log() -> (FakeApi, FakeCache, FakeQueue, CallLog) {
    let log = CallLog::new();
    let api = FakeApi::new().with_call_log(log.clone());
    let cache = FakeCache::new().with_call_log(log.clone());
    let queue = FakeQueue::new().with_call_log(log.clone());
    (api, cache, queue, log)
}

#[derive(Debug, Default)]
struct FakeApiState {
    requests: Vec<SyncRequest>,
    scripted: VecDeque<Result<SyncResponse>>,
}

/// In-memory [`SyncApi`] test double that records every [`SyncRequest`] and
/// replays scripted [`SyncResponse`] or [`AppError`] outcomes in FIFO order.
#[derive(Debug, Default, Clone)]
pub struct FakeApi {
    state: Arc<Mutex<FakeApiState>>,
    log: Option<CallLog>,
}

impl FakeApi {
    /// Creates a new [`FakeApi`] with an empty request log and no scripted outcomes.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Attaches a shared [`CallLog`] to record [`PortCall::ApiSync`] events.
    #[must_use]
    pub fn with_call_log(mut self, log: CallLog) -> Self {
        self.log = Some(log);
        self
    }

    /// Enqueues a successful [`SyncResponse`] to return on the next [`SyncApi::sync`] call.
    pub fn push_response(&self, response: SyncResponse) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scripted
            .push_back(Ok(response));
    }

    /// Enqueues an [`AppError`] to return on the next [`SyncApi::sync`] call.
    pub fn push_error(&self, error: AppError) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .scripted
            .push_back(Err(error));
    }

    /// Enqueues a [`SyncError`] (wrapped into [`AppError::Sync`]) to return on
    /// the next [`SyncApi::sync`] call.
    pub fn push_sync_error(&self, error: SyncError) {
        self.push_error(AppError::Sync(error));
    }

    /// Enqueues an [`ApiError`] (wrapped into [`AppError::Api`]) to return on
    /// the next [`SyncApi::sync`] call.
    pub fn push_api_error(&self, error: ApiError) {
        self.push_error(AppError::Api(error));
    }

    /// Returns a clone of every [`SyncRequest`] received so far, in call order.
    #[must_use]
    pub fn requests(&self) -> Vec<SyncRequest> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .requests
            .clone()
    }

    /// Returns the total number of [`SyncApi::sync`] calls made so far.
    #[must_use]
    pub fn call_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .requests
            .len()
    }
}

impl SyncApi for FakeApi {
    fn sync<'a>(&'a self, req: &'a SyncRequest) -> BoxFuture<'a, Result<SyncResponse>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::ApiSync);
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.requests.push(req.clone());
        let outcome = guard.scripted.pop_front().unwrap_or_else(|| {
            Ok(SyncResponse {
                sync_token: "fake_default_sync_token".to_string(),
                ..SyncResponse::default()
            })
        });
        drop(guard);
        Box::pin(async move { outcome })
    }
}

#[derive(Debug, Default)]
struct FakeCacheState {
    sync_token: Option<String>,
    full_sync_date_utc: Option<String>,
    items: HashMap<Id, Item>,
    applied_responses: Vec<SyncResponse>,
    token_writes: Vec<String>,
    upserted_items: Vec<Item>,
    deleted_items: Vec<Id>,
    apply_errors: VecDeque<AppError>,
    set_token_errors: VecDeque<AppError>,
}

/// In-memory [`Cache`] test double.
///
/// Tracks `sync_token`, `full_sync_date_utc`, an in-memory `items` table with
/// full-vs-delta semantics, and logs every call for test assertions.
#[derive(Debug, Default, Clone)]
pub struct FakeCache {
    state: Arc<Mutex<FakeCacheState>>,
    log: Option<CallLog>,
}

impl FakeCache {
    /// Creates an empty [`FakeCache`] with no stored `sync_token`.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a [`FakeCache`] pre-seeded with `token`.
    #[must_use]
    pub fn with_token(token: impl Into<String>) -> Self {
        let cache = Self::new();
        cache.set_token_direct(Some(token.into()));
        cache
    }

    /// Attaches a shared [`CallLog`] to record cache port invocations.
    #[must_use]
    pub fn with_call_log(mut self, log: CallLog) -> Self {
        self.log = Some(log);
        self
    }

    /// Returns the currently stored `sync_token` without recording a port call.
    #[must_use]
    pub fn current_token(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sync_token
            .clone()
    }

    /// Directly sets `sync_token` for test setup without recording a port call.
    pub fn set_token_direct(&self, token: Option<String>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .sync_token = token;
    }

    /// Returns the currently stored `full_sync_date_utc` without recording a port call.
    #[must_use]
    pub fn current_full_sync_date_utc(&self) -> Option<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .full_sync_date_utc
            .clone()
    }

    /// Directly sets `full_sync_date_utc` for test setup.
    pub fn set_full_sync_date_utc_direct(&self, value: Option<String>) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .full_sync_date_utc = value;
    }

    /// Seeds an [`Item`] directly into the fake cache without recording a port call.
    pub fn seed_item(&self, item: Item) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .items
            .insert(item.id.clone(), item);
    }

    /// Reads an [`Item`] by `id` directly for test assertions without recording a port call.
    #[must_use]
    pub fn get_item(&self, id: &Id) -> Option<Item> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .items
            .get(id)
            .cloned()
    }

    /// Returns all currently stored items (unordered).
    #[must_use]
    pub fn all_items(&self) -> Vec<Item> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .items
            .values()
            .cloned()
            .collect()
    }

    /// Returns every [`SyncResponse`] passed to [`Cache::apply_response`], in call order.
    #[must_use]
    pub fn applied_responses(&self) -> Vec<SyncResponse> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .applied_responses
            .clone()
    }

    /// Returns every token string passed to [`Cache::set_sync_token`], in call order.
    #[must_use]
    pub fn token_writes(&self) -> Vec<String> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .token_writes
            .clone()
    }

    /// Returns every [`Item`] written via [`Cache::upsert_item`], in call order.
    #[must_use]
    pub fn upserted_items(&self) -> Vec<Item> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .upserted_items
            .clone()
    }

    /// Returns every [`Id`] deleted via [`Cache::delete_item`], in call order.
    #[must_use]
    pub fn deleted_items(&self) -> Vec<Id> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .deleted_items
            .clone()
    }

    /// Total number of item-level replay writes (`upsert_item` + `delete_item`).
    #[must_use]
    pub fn replay_write_count(&self) -> usize {
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.upserted_items.len() + guard.deleted_items.len()
    }

    /// Scripts an error for the next [`Cache::apply_response`] call.
    pub fn push_apply_error(&self, error: AppError) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .apply_errors
            .push_back(error);
    }

    /// Scripts an error for the next [`Cache::set_sync_token`] call.
    pub fn push_set_token_error(&self, error: AppError) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .set_token_errors
            .push_back(error);
    }
}

impl Cache for FakeCache {
    fn sync_token(&self) -> BoxFuture<'_, Result<Option<String>>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::CacheGetSyncToken);
        }
        let tok = self.current_token();
        Box::pin(async move { Ok(tok) })
    }

    fn set_sync_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<()>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::CacheSetSyncToken(token.to_string()));
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(err) = guard.set_token_errors.pop_front() {
            return Box::pin(async move { Err(err) });
        }
        guard.sync_token = Some(token.to_string());
        guard.token_writes.push(token.to_string());
        drop(guard);
        Box::pin(async { Ok(()) })
    }

    fn full_sync_date_utc(&self) -> BoxFuture<'_, Result<Option<String>>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::CacheGetFullSyncDateUtc);
        }
        let date = self.current_full_sync_date_utc();
        Box::pin(async move { Ok(date) })
    }

    fn apply_response<'a>(
        &'a self,
        response: &'a SyncResponse,
    ) -> BoxFuture<'a, Result<ApplyStats>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::CacheApplyResponse {
                full_sync: response.full_sync,
            });
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(err) = guard.apply_errors.pop_front() {
            return Box::pin(async move { Err(err) });
        }

        guard.applied_responses.push(response.clone());
        let is_full_sync = response.full_sync || response.full_sync_date_utc.is_some();
        if is_full_sync {
            guard.items.clear();
        }

        let mut item_counts = ResourceCounts::default();
        for item in &response.items {
            if item.is_deleted {
                guard.items.remove(&item.id);
                item_counts.deleted += 1;
            } else {
                guard.items.insert(item.id.clone(), item.clone());
                item_counts.upserted += 1;
            }
        }

        if !response.sync_token.is_empty() {
            guard.sync_token = Some(response.sync_token.clone());
        }
        if let Some(ref date) = response.full_sync_date_utc {
            guard.full_sync_date_utc = Some(date.clone());
        }

        let stats = ApplyStats {
            full_sync: is_full_sync,
            items: item_counts,
            user_upserted: response.user.is_some(),
            day_orders_replaced: if is_full_sync || !response.day_orders.is_empty() {
                response.day_orders.len()
            } else {
                0
            },
            ..ApplyStats::default()
        };
        drop(guard);

        Box::pin(async move { Ok(stats) })
    }

    fn item_by_id<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<Option<Item>>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::CacheItemById(id.clone()));
        }
        let found = self.get_item(id);
        Box::pin(async move { Ok(found) })
    }

    fn upsert_item<'a>(&'a self, item: &'a Item) -> BoxFuture<'a, Result<()>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::CacheUpsertItem(item.id.clone()));
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.items.insert(item.id.clone(), item.clone());
        guard.upserted_items.push(item.clone());
        drop(guard);
        Box::pin(async { Ok(()) })
    }

    fn delete_item<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<()>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::CacheDeleteItem(id.clone()));
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.items.remove(id);
        guard.deleted_items.push(id.clone());
        drop(guard);
        Box::pin(async { Ok(()) })
    }
}

#[derive(Debug, Default)]
struct FakeQueueState {
    commands: Vec<QueuedCommand>,
    resolved_mappings: Vec<HashMap<String, Id>>,
    applied_statuses: Vec<(Vec<QueuedCommand>, HashMap<String, CommandStatus>)>,
    requeued_batches: Vec<Vec<QueuedCommand>>,
    pending_batch_errors: VecDeque<AppError>,
    resolve_errors: VecDeque<AppError>,
    status_errors: VecDeque<AppError>,
}

/// In-memory [`WriteQueue`] test double.
#[derive(Debug, Default, Clone)]
pub struct FakeQueue {
    state: Arc<Mutex<FakeQueueState>>,
    log: Option<CallLog>,
}

impl FakeQueue {
    /// Creates an empty [`FakeQueue`].
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Creates a [`FakeQueue`] pre-populated with `commands`.
    #[must_use]
    pub fn with_commands(commands: Vec<QueuedCommand>) -> Self {
        let queue = Self::new();
        queue
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .commands = commands;
        queue
    }

    /// Attaches a shared [`CallLog`] to record queue port invocations.
    #[must_use]
    pub fn with_call_log(mut self, log: CallLog) -> Self {
        self.log = Some(log);
        self
    }

    /// Appends `command` to the in-memory queue (ignoring duplicate `uuid`s).
    pub fn enqueue(&self, command: QueuedCommand) {
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if !guard.commands.iter().any(|c| c.uuid == command.uuid) {
            guard.commands.push(command);
        }
    }

    /// Returns a snapshot of all commands currently in the queue across all states.
    #[must_use]
    pub fn commands(&self) -> Vec<QueuedCommand> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .commands
            .clone()
    }

    /// Returns every `temp_id_mapping` passed to [`WriteQueue::resolve_temp_ids`].
    #[must_use]
    pub fn resolved_mappings(&self) -> Vec<HashMap<String, Id>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resolved_mappings
            .clone()
    }

    /// Returns every `(batch, sync_status)` pair passed to [`WriteQueue::apply_sync_status`].
    #[must_use]
    pub fn applied_statuses(&self) -> Vec<(Vec<QueuedCommand>, HashMap<String, CommandStatus>)> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .applied_statuses
            .clone()
    }

    /// Returns every batch passed to [`WriteQueue::requeue_in_flight`].
    #[must_use]
    pub fn requeued_batches(&self) -> Vec<Vec<QueuedCommand>> {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .requeued_batches
            .clone()
    }

    /// Scripts an error for the next [`WriteQueue::pending_batch`] call.
    pub fn push_pending_batch_error(&self, error: AppError) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .pending_batch_errors
            .push_back(error);
    }

    /// Scripts an error for the next [`WriteQueue::resolve_temp_ids`] call.
    pub fn push_resolve_error(&self, error: AppError) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .resolve_errors
            .push_back(error);
    }

    /// Scripts an error for the next [`WriteQueue::apply_sync_status`] call.
    pub fn push_status_error(&self, error: AppError) {
        self.state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .status_errors
            .push_back(error);
    }
}

impl WriteQueue for FakeQueue {
    fn pending_batch(&self, max_batch: usize) -> BoxFuture<'_, Result<Option<Vec<QueuedCommand>>>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::QueuePendingBatch { max_batch });
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(err) = guard.pending_batch_errors.pop_front() {
            return Box::pin(async move { Err(err) });
        }
        if max_batch == 0 {
            return Box::pin(async { Ok(None) });
        }

        let mut batch = Vec::new();
        for cmd in &mut guard.commands {
            if cmd.state == CommandState::Pending && batch.len() < max_batch {
                cmd.state = CommandState::InFlight;
                cmd.attempts = cmd.attempts.saturating_add(1);
                batch.push(cmd.clone());
            }
        }
        drop(guard);

        let result = if batch.is_empty() { None } else { Some(batch) };
        Box::pin(async move { Ok(result) })
    }

    fn pending_commands(&self) -> BoxFuture<'_, Result<Vec<QueuedCommand>>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::QueuePendingCommands);
        }
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let pending: Vec<QueuedCommand> = guard
            .commands
            .iter()
            .filter(|cmd| cmd.state == CommandState::Pending)
            .cloned()
            .collect();
        drop(guard);
        Box::pin(async move { Ok(pending) })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::QueuePendingCount);
        }
        let guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let count = guard
            .commands
            .iter()
            .filter(|cmd| cmd.state == CommandState::Pending)
            .count();
        drop(guard);
        Box::pin(async move { Ok(count) })
    }

    fn resolve_temp_ids<'a>(
        &'a self,
        mapping: &'a HashMap<String, Id>,
    ) -> BoxFuture<'a, Result<usize>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::QueueResolveTempIds);
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(err) = guard.resolve_errors.pop_front() {
            return Box::pin(async move { Err(err) });
        }
        guard.resolved_mappings.push(mapping.clone());

        let mut updated = 0usize;
        if !mapping.is_empty() {
            for cmd in &mut guard.commands {
                let args_changed = rewrite_json_value(&mut cmd.args, mapping);
                let mut temp_changed = false;
                if let Some(ref tid) = cmd.temp_id {
                    if let Some(real_id) = mapping.get(tid.as_str()) {
                        cmd.temp_id = Some(real_id.as_str().to_owned());
                        temp_changed = true;
                    }
                }
                if args_changed || temp_changed {
                    updated = updated.saturating_add(1);
                }
            }
        }
        drop(guard);
        Box::pin(async move { Ok(updated) })
    }

    fn apply_sync_status<'a>(
        &'a self,
        batch: &'a [QueuedCommand],
        sync_status: &'a HashMap<String, CommandStatus>,
    ) -> BoxFuture<'a, Result<QueueStatusSummary>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::QueueApplySyncStatus);
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        if let Some(err) = guard.status_errors.pop_front() {
            return Box::pin(async move { Err(err) });
        }
        guard
            .applied_statuses
            .push((batch.to_vec(), sync_status.clone()));

        let mut summary = QueueStatusSummary::default();
        for submitted in batch {
            let key = submitted.uuid.to_string();
            match sync_status.get(&key) {
                Some(CommandStatus::Ok(_)) => {
                    guard.commands.retain(|c| c.uuid != submitted.uuid);
                    summary.succeeded = summary.succeeded.saturating_add(1);
                }
                Some(CommandStatus::Error(err)) => {
                    if let Some(existing) =
                        guard.commands.iter_mut().find(|c| c.uuid == submitted.uuid)
                    {
                        existing.state = CommandState::Failed;
                        existing.error = Some(
                            err.error
                                .clone()
                                .unwrap_or_else(|| "command failed".to_string()),
                        );
                    }
                    summary.failed = summary.failed.saturating_add(1);
                }
                None => {
                    if let Some(existing) =
                        guard.commands.iter_mut().find(|c| c.uuid == submitted.uuid)
                    {
                        existing.state = CommandState::Pending;
                    }
                    summary.requeued = summary.requeued.saturating_add(1);
                }
            }
        }
        drop(guard);

        Box::pin(async move { Ok(summary) })
    }

    fn requeue_in_flight<'a>(&'a self, batch: &'a [QueuedCommand]) -> BoxFuture<'a, Result<()>> {
        if let Some(ref log) = self.log {
            log.record(PortCall::QueueRequeueInFlight);
        }
        let mut guard = self
            .state
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        guard.requeued_batches.push(batch.to_vec());
        for submitted in batch {
            if let Some(existing) = guard.commands.iter_mut().find(|c| c.uuid == submitted.uuid) {
                if existing.state == CommandState::InFlight {
                    existing.state = CommandState::Pending;
                }
            }
        }
        drop(guard);
        Box::pin(async { Ok(()) })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use serde_json::json;

    use super::*;
    use crate::api::sync::CommandError;

    #[tokio::test]
    async fn fake_api_records_requests_through_one_success_and_one_transport_error() {
        let api = FakeApi::new();
        api.push_response(SyncResponse {
            sync_token: "tok_sync_001".to_string(),
            full_sync: true,
            ..SyncResponse::default()
        });
        api.push_sync_error(SyncError::Offline);

        let first_req = SyncRequest::full();
        let first_resp = api.sync(&first_req).await.expect("first call succeeds");
        assert_eq!(first_resp.sync_token, "tok_sync_001");
        assert!(first_resp.full_sync);

        let second_req = SyncRequest::incremental("tok_sync_001");
        let second_err = api
            .sync(&second_req)
            .await
            .expect_err("second call returns scripted transport/offline error");
        match second_err {
            AppError::Sync(ref err) => assert!(err.is_offline()),
            other => panic!("expected AppError::Sync(Offline), got {other:?}"),
        }

        let recorded = api.requests();
        assert_eq!(recorded.len(), 2);
        assert_eq!(recorded[0], first_req);
        assert_eq!(recorded[1], second_req);
    }

    #[tokio::test]
    async fn fake_cache_and_fake_queue_record_calls_in_shared_call_log() {
        let (api, cache, queue, log) = fakes_with_log();

        let cmd = QueuedCommand::new(
            "item_add",
            json!({"content": "Buy oat milk", "project_id": "temp_proj_1"}),
            Some("temp_item_1".to_string()),
        );
        let cmd_uuid = cmd.uuid.to_string();
        queue.enqueue(cmd);

        assert_eq!(queue.pending_count().await.unwrap(), 1);
        let batch = queue.pending_batch(10).await.unwrap().expect("one batch");
        assert_eq!(batch.len(), 1);
        assert_eq!(batch[0].state, CommandState::InFlight);

        api.push_response(SyncResponse {
            sync_token: "tok_next".to_string(),
            full_sync: false,
            ..SyncResponse::default()
        });
        let resp = api.sync(&SyncRequest::full()).await.unwrap();

        let mut mapping = HashMap::new();
        mapping.insert("temp_proj_1".to_string(), Id::new("real_proj_1"));
        assert_eq!(queue.resolve_temp_ids(&mapping).await.unwrap(), 1);

        let mut status = HashMap::new();
        status.insert(cmd_uuid, CommandStatus::Ok("ok".to_string()));
        let summary = queue.apply_sync_status(&batch, &status).await.unwrap();
        assert_eq!(
            summary,
            QueueStatusSummary {
                succeeded: 1,
                failed: 0,
                requeued: 0,
            }
        );

        cache.apply_response(&resp).await.unwrap();
        cache.set_sync_token(&resp.sync_token).await.unwrap();

        assert_eq!(
            log.calls(),
            vec![
                PortCall::QueuePendingCount,
                PortCall::QueuePendingBatch { max_batch: 10 },
                PortCall::ApiSync,
                PortCall::QueueResolveTempIds,
                PortCall::QueueApplySyncStatus,
                PortCall::CacheApplyResponse { full_sync: false },
                PortCall::CacheSetSyncToken("tok_next".to_string()),
            ]
        );
    }

    #[tokio::test]
    async fn fake_queue_handles_failed_and_missing_sync_status_and_requeue_in_flight() {
        let queue = FakeQueue::new();
        let c1 = QueuedCommand::new("item_add", json!({"content": "one"}), None);
        let c2 = QueuedCommand::new("item_add", json!({"content": "two"}), None);
        queue.enqueue(c1.clone());
        queue.enqueue(c2.clone());

        let batch = queue.pending_batch(10).await.unwrap().unwrap();
        assert_eq!(queue.pending_count().await.unwrap(), 0);

        // Simulate transport failure before response: requeue_in_flight restores Pending.
        queue.requeue_in_flight(&batch).await.unwrap();
        assert_eq!(queue.pending_count().await.unwrap(), 2);

        // Take batch again and apply mixed status (c1 error, c2 omitted -> requeued).
        let batch2 = queue.pending_batch(10).await.unwrap().unwrap();
        let mut status = HashMap::new();
        status.insert(
            c1.uuid.to_string(),
            CommandStatus::Error(CommandError {
                error_code: Some(40),
                error: Some("bad args".to_string()),
                extra: HashMap::new(),
            }),
        );
        let summary = queue.apply_sync_status(&batch2, &status).await.unwrap();
        assert_eq!(
            summary,
            QueueStatusSummary {
                succeeded: 0,
                failed: 1,
                requeued: 1,
            }
        );
        assert_eq!(queue.pending_count().await.unwrap(), 1);
        let remaining = queue.pending_commands().await.unwrap();
        assert_eq!(remaining.len(), 1);
        assert_eq!(remaining[0].uuid, c2.uuid);
    }
}
