//! Background sync engine actor (T-510, EPIC-06).

#![allow(dead_code)]

use std::sync::Arc;
use std::time::{Duration, SystemTime, UNIX_EPOCH};

use tokio::select;

use crate::config::Config;
use crate::error::{AppError, SyncError};
use crate::sync::cycle::SyncCycle;
use crate::sync::handle::{SyncDriver, SyncHandle};
use crate::sync::offline::{OfflineTracker, DEFAULT_MAX_OFFLINE_BACKOFF};
use crate::sync::ports::{Cache, SyncApi, WriteQueue};
use crate::sync::state::SyncState;

/// Actor orchestrating the periodic and on-demand sync loop.
pub struct SyncEngine;

impl SyncEngine {
    /// Spawns the sync actor as a background `tokio` task and returns a [`SyncHandle`].
    pub fn spawn<A, C, Q>(api: Arc<A>, cache: Arc<C>, queue: Arc<Q>, config: &Config) -> SyncHandle
    where
        A: SyncApi + 'static,
        C: Cache + 'static,
        Q: WriteQueue + 'static,
    {
        let (handle, driver) = SyncHandle::channel();
        let interval = if config.sync_interval.is_zero() {
            Duration::from_secs(60)
        } else {
            config.sync_interval
        };

        tokio::spawn(async move {
            run_engine_loop(api, cache, queue, driver, interval).await;
        });

        handle
    }
}

async fn run_engine_loop<A, C, Q>(
    api: Arc<A>,
    cache: Arc<C>,
    queue: Arc<Q>,
    mut driver: SyncDriver,
    base_interval: Duration,
) where
    A: SyncApi + 'static,
    C: Cache + 'static,
    Q: WriteQueue + 'static,
{
    let cycle = SyncCycle::new(Arc::clone(&api), Arc::clone(&cache), Arc::clone(&queue));
    let mut tracker = OfflineTracker::new(base_interval, DEFAULT_MAX_OFFLINE_BACKOFF);

    // Initial pending count update
    if let Ok(count) = queue.pending_count().await {
        if count > 0 {
            driver.emit(SyncState::Pending { count });
        }
    }

    while !driver.is_shutdown() {
        let delay = tracker.next_delay();

        select! {
            biased;

            () = driver.wait_for_shutdown() => {
                break;
            }
            trigger_opt = driver.wait_for_trigger() => {
                if trigger_opt.is_none() {
                    break;
                }
                tracker.on_manual_trigger();
            }
            () = tokio::time::sleep(delay) => {}
        }

        if driver.is_shutdown() {
            break;
        }

        // Emit Syncing
        driver.emit(SyncState::Syncing);

        let now = SystemTime::now();
        let now_secs = now
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

        match cycle.run_once(now_secs).await {
            Ok(_outcome) => {
                let state = tracker.on_success(now);
                driver.emit(state);
            }
            Err(err) => {
                let sync_err = match err {
                    AppError::Sync(s) => s,
                    AppError::Api(crate::error::ApiError::RetriesExhausted { .. }) => {
                        SyncError::Offline
                    }
                    AppError::Api(a) => SyncError::Api { source: a },
                    other => SyncError::Store {
                        message: other.to_string(),
                    },
                };
                let state = tracker.on_error(&sync_err);
                driver.emit(state);
            }
        }

        // Update pending count
        if let Ok(count) = queue.pending_count().await {
            driver.emit(SyncState::Pending { count });
        }
    }

    driver.mark_stopped();
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};
    use std::time::Duration;

    use crate::api::sync::SyncResponse;
    use crate::error::SyncError;
    use crate::sync::fakes::fakes_with_log;
    use crate::sync::ports::BoxFuture;

    struct HangingApi;
    impl SyncApi for HangingApi {
        fn sync<'a>(
            &'a self,
            _req: &'a crate::api::sync::SyncRequest,
        ) -> BoxFuture<'a, crate::error::Result<SyncResponse>> {
            Box::pin(async {
                tokio::time::sleep(Duration::from_secs(3600)).await;
                Ok(SyncResponse::default())
            })
        }
    }

    #[tokio::test]
    async fn trigger_does_not_block_caller() {
        let (_api, cache, queue, _log) = fakes_with_log();
        let config = Config {
            sync_interval: Duration::from_millis(100),
            ..Config::default()
        };

        let handle = SyncEngine::spawn(
            Arc::new(HangingApi),
            Arc::new(cache),
            Arc::new(queue),
            &config,
        );

        // trigger() returns immediately without waiting for API
        let start = std::time::Instant::now();
        let res = handle.trigger();
        assert!(res.is_ok());
        assert!(start.elapsed() < Duration::from_millis(50));

        handle.shutdown();
    }

    #[tokio::test]
    async fn shutdown_stops_engine_cleanly() {
        let (api, cache, queue, _log) = fakes_with_log();
        let config = Config {
            sync_interval: Duration::from_millis(50),
            ..Config::default()
        };

        let handle = SyncEngine::spawn(Arc::new(api), Arc::new(cache), Arc::new(queue), &config);

        assert!(!handle.is_shutdown());
        handle.shutdown();
        assert!(handle.is_shutdown());

        // Stopped resolves
        tokio::time::timeout(Duration::from_millis(500), handle.stopped())
            .await
            .expect("engine must stop");
    }

    #[tokio::test]
    async fn emits_syncing_then_synced_event() {
        let (api, cache, queue, _log) = fakes_with_log();
        let config = Config {
            sync_interval: Duration::from_millis(100),
            ..Config::default()
        };

        let handle = SyncEngine::spawn(Arc::new(api), Arc::new(cache), Arc::new(queue), &config);

        let mut sub = handle.subscribe();
        handle.trigger().unwrap();

        // Expect Syncing then Synced
        let event1 = tokio::time::timeout(Duration::from_millis(200), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert_eq!(event1, SyncState::Syncing);

        let event2 = tokio::time::timeout(Duration::from_millis(200), sub.recv())
            .await
            .unwrap()
            .unwrap();
        assert!(matches!(event2, SyncState::Synced { .. }));

        handle.shutdown();
    }
}
