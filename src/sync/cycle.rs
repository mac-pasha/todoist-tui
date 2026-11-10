//! Sync cycle: flush-then-pull ordering, token management, and delta application (T-505, T-506, EPIC-06).

#![allow(dead_code)]

use std::sync::Arc;

use crate::api::sync::{SyncRequest, SyncResponse};
use crate::error::Result;
use crate::queue::QueuedCommand;
use crate::sync::conflict::replay_pending_conflicts;
use crate::sync::lag::{detect_lag, DEFAULT_LAG_THRESHOLD_SECS};
use crate::sync::ports::{Cache, SyncApi, WriteQueue};

/// Maximum batch size when draining pending commands for a cycle flush.
pub const CYCLE_FLUSH_BATCH_LIMIT: usize = 100;

/// Summary outcome of a single [`SyncCycle::run_once`] execution.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CycleOutcome {
    /// Incremental `sync_token` after the cycle.
    pub sync_token: String,
    /// Whether the server returned a full sync replacement.
    pub full_sync: bool,
    /// Number of write commands included in this cycle's flush.
    pub flushed_commands: usize,
    /// Number of resources pulled in this cycle.
    pub pulled_resources: usize,
    /// Whether a follow-up incremental sync was triggered to resolve full-sync lag.
    pub had_lag_followup: bool,
}

/// Orchestrator executing the atomic flush-then-pull sync sequence.
pub struct SyncCycle<A, C, Q> {
    api: Arc<A>,
    cache: Arc<C>,
    queue: Arc<Q>,
}

impl<A, C, Q> SyncCycle<A, C, Q>
where
    A: SyncApi,
    C: Cache,
    Q: WriteQueue,
{
    /// Creates a new [`SyncCycle`] around the provided port implementations.
    pub fn new(api: Arc<A>, cache: Arc<C>, queue: Arc<Q>) -> Self {
        Self { api, cache, queue }
    }

    /// Executes one full sync cycle:
    /// 1. Drains pending commands from [`WriteQueue`].
    /// 2. Reads `sync_token` from [`Cache`] (or `"*"` if absent).
    /// 3. Submits a combined `/sync` request.
    /// 4. If the call fails, requeues in-flight commands and aborts without updating token.
    /// 5. On success:
    ///    - resolves `temp_id_mapping` on the queue and cache;
    ///    - applies `sync_status` to in-flight commands;
    ///    - applies resource deltas to [`Cache`];
    ///    - stores the new `sync_token`;
    ///    - replays pending conflicts (FR-18 LWW);
    ///    - checks full-sync lag and triggers one follow-up incremental if lagged.
    pub async fn run_once(&self, now_epoch_secs: i64) -> Result<CycleOutcome> {
        self.run_cycle_internal(now_epoch_secs, true).await
    }

    async fn run_cycle_internal(
        &self,
        now_epoch_secs: i64,
        allow_lag_followup: bool,
    ) -> Result<CycleOutcome> {
        // 1. Drain pending batch
        let in_flight_batch: Option<Vec<QueuedCommand>> =
            self.queue.pending_batch(CYCLE_FLUSH_BATCH_LIMIT).await?;
        let flushed_count = in_flight_batch.as_ref().map_or(0, Vec::len);

        // 2. Determine sync token
        let token_opt = self.cache.sync_token().await?;
        let mut req = match token_opt.as_deref() {
            Some(tok) => SyncRequest::incremental(tok),
            None => SyncRequest::full(),
        };

        // 3. Attach commands in enqueue order
        if let Some(ref batch) = in_flight_batch {
            req.commands = batch.iter().map(QueuedCommand::to_sync_command).collect();
        }

        // 4. Submit to Sync API
        let response = match self.api.sync(&req).await {
            Ok(res) => res,
            Err(err) => {
                // On failure: requeue in-flight batch back to pending
                if let Some(ref batch) = in_flight_batch {
                    let _ = self.queue.requeue_in_flight(batch).await;
                }
                return Err(err);
            }
        };

        // 5. Sequence outcomes on success:
        // a. Resolve temp IDs
        if !response.temp_id_mapping.is_empty() {
            self.queue
                .resolve_temp_ids(&response.temp_id_mapping)
                .await?;
        }

        // b. Apply sync status to the submitted batch
        if let Some(ref batch) = in_flight_batch {
            self.queue
                .apply_sync_status(batch, &response.sync_status)
                .await?;
        }

        // c. Apply resource changes to cache
        self.cache.apply_response(&response).await?;

        // d. Update stored sync token
        self.cache.set_sync_token(&response.sync_token).await?;

        // e. Replay pending conflicts (FR-18)
        replay_pending_conflicts(&*self.cache, &*self.queue, &response).await?;

        let mut pulled_resources = response.items.len()
            + response.projects.len()
            + response.sections.len()
            + response.labels.len()
            + response.notes.len()
            + response.filters.len();

        // f. Check full-sync lag follow-up
        let mut had_lag_followup = false;
        if allow_lag_followup
            && detect_lag(
                response.full_sync,
                response.full_sync_date_utc.as_deref(),
                now_epoch_secs,
                DEFAULT_LAG_THRESHOLD_SECS,
            )
        {
            had_lag_followup = true;
            // Immediate incremental follow-up (non-recursive: allow_lag_followup = false)
            let followup = Box::pin(self.run_cycle_internal(now_epoch_secs, false)).await?;
            pulled_resources += followup.pulled_resources;
        }

        Ok(CycleOutcome {
            sync_token: response.sync_token,
            full_sync: response.full_sync,
            flushed_commands: flushed_count,
            pulled_resources,
            had_lag_followup,
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    use crate::api::sync::{CommandStatus, SyncResponse};
    use crate::error::ApiError;
    use crate::queue::command::QueuedCommand;
    use crate::sync::fakes::{fakes_with_log, PortCall};
    use serde_json::json;

    #[tokio::test]
    async fn empty_cache_issues_full_sync_with_star_and_persists_token() {
        let (api, cache, queue, _log) = fakes_with_log();
        let cycle = SyncCycle::new(
            Arc::new(api.clone()),
            Arc::new(cache.clone()),
            Arc::new(queue),
        );

        let scripted = SyncResponse {
            sync_token: "new_tok_1".to_string(),
            full_sync: true,
            ..SyncResponse::default()
        };
        api.push_response(scripted);

        assert_eq!(cache.current_token(), None);

        let outcome = cycle.run_once(1_700_000_000).await.unwrap();
        assert_eq!(outcome.sync_token, "new_tok_1");
        assert!(outcome.full_sync);

        // Assert request had sync_token: "*"
        let requests = api.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].sync_token, "*");
        assert_eq!(requests[0].commands.len(), 0);

        // Token is persisted in cache
        assert_eq!(cache.current_token().as_deref(), Some("new_tok_1"));

        // Second run uses the newly stored token
        let scripted_2 = SyncResponse {
            sync_token: "new_tok_2".to_string(),
            full_sync: false,
            ..SyncResponse::default()
        };
        api.push_response(scripted_2);

        let outcome_2 = cycle.run_once(1_700_000_000).await.unwrap();
        assert_eq!(outcome_2.sync_token, "new_tok_2");

        let requests_after = api.requests();
        assert_eq!(requests_after.len(), 2);
        assert_eq!(requests_after[1].sync_token, "new_tok_1");
    }

    #[tokio::test]
    async fn flush_then_pull_ordering_and_call_sequence() {
        let (api, cache, queue, log) = fakes_with_log();
        let cycle = SyncCycle::new(
            Arc::new(api.clone()),
            Arc::new(cache.clone()),
            Arc::new(queue.clone()),
        );

        let cmd1 = QueuedCommand::new("item_add", json!({"content": "one"}), None);
        let cmd2 = QueuedCommand::new("item_add", json!({"content": "two"}), None);
        let cmd3 = QueuedCommand::new("item_add", json!({"content": "three"}), None);

        let uuid1 = cmd1.uuid;
        let uuid2 = cmd2.uuid;
        let uuid3 = cmd3.uuid;

        queue.enqueue(cmd1);
        queue.enqueue(cmd2);
        queue.enqueue(cmd3);

        let mut sync_status = HashMap::new();
        sync_status.insert(uuid1.to_string(), CommandStatus::Ok("ok".to_string()));
        sync_status.insert(uuid2.to_string(), CommandStatus::Ok("ok".to_string()));
        sync_status.insert(uuid3.to_string(), CommandStatus::Ok("ok".to_string()));

        let scripted = SyncResponse {
            sync_token: "tok_flush_pull".to_string(),
            sync_status,
            ..SyncResponse::default()
        };
        api.push_response(scripted);

        let outcome = cycle.run_once(1_700_000_000).await.unwrap();
        assert_eq!(outcome.flushed_commands, 3);

        // Verify command order in request
        let requests = api.requests();
        assert_eq!(requests.len(), 1);
        assert_eq!(requests[0].commands.len(), 3);
        assert_eq!(requests[0].commands[0]["uuid"], uuid1.to_string());
        assert_eq!(requests[0].commands[1]["uuid"], uuid2.to_string());
        assert_eq!(requests[0].commands[2]["uuid"], uuid3.to_string());

        // Verify call sequence: resolve_temp_ids -> apply_sync_status -> apply_response
        let calls = log.calls();
        let status_idx = calls
            .iter()
            .position(|c| matches!(c, PortCall::QueueApplySyncStatus));
        let apply_idx = calls
            .iter()
            .position(|c| matches!(c, PortCall::CacheApplyResponse { .. }));

        assert!(status_idx.is_some());
        assert!(apply_idx.is_some());
        if let (Some(s), Some(a)) = (status_idx, apply_idx) {
            assert!(s < a, "apply_sync_status must run before apply_response");
        }
    }

    #[tokio::test]
    async fn api_error_leaves_token_unchanged_and_requeues_commands() {
        let (api, cache, queue, _log) = fakes_with_log();
        cache.set_token_direct(Some("unmodified_token".to_string()));

        let cmd = QueuedCommand::new("item_add", json!({"content": "fail"}), None);
        queue.enqueue(cmd);

        let cycle = SyncCycle::new(
            Arc::new(api.clone()),
            Arc::new(cache.clone()),
            Arc::new(queue.clone()),
        );

        api.push_sync_error(crate::error::SyncError::Offline);

        let res = cycle.run_once(1_700_000_000).await;
        assert!(res.is_err());

        // Stored token is still unmodified
        assert_eq!(cache.current_token().as_deref(), Some("unmodified_token"));

        // In-flight command was requeued
        assert_eq!(queue.commands().len(), 1);
    }
}
