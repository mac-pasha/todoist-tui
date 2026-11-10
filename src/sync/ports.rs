//! Port traits decoupling the sync engine from HTTP, SQLite cache, and write queue (T-503, EPIC-06).
//!
//! The sync engine (`SyncCycle` and `SyncEngine`) interacts with external I/O
//! exclusively through three object-safe async traits defined here:
//! - [`SyncApi`]: submits a [`SyncRequest`] to `/api/v1/sync` and returns a [`SyncResponse`].
//! - [`Cache`]: reads/writes the incremental `sync_token`, `full_sync_date_utc`,
//!   applies full or delta [`SyncResponse`] payloads, and provides item-level
//!   accessors used by FR-18 conflict replay (`T-507`).
//! - [`WriteQueue`]: drains pending [`QueuedCommand`] batches, resolves
//!   `temp_id_mapping`, applies per-command `sync_status` outcomes, and
//!   requeues in-flight batches when a transport failure aborts a flush.
//!
//! Because `Cargo.toml` does not pull in `async-trait`, async trait methods
//! return [`BoxFuture`], making all three traits `Send + Sync` and object-safe
//! (`dyn SyncApi`, `dyn Cache`, `dyn WriteQueue`) with zero additional crate
//! dependencies.
//!
//! Production adapters also live here:
//! - [`SyncClient`] implements [`SyncApi`] via a thin forwarding implementation.
//! - [`SqliteSyncStore`] wraps a shared [`rusqlite::Connection`] and implements
//!   both [`Cache`] and [`WriteQueue`].

#![allow(dead_code)]

use std::collections::HashMap;
use std::future::Future;
use std::pin::Pin;
use std::sync::{Arc, Mutex};

use rusqlite::{params, Connection};

use crate::api::client::SyncClient;
use crate::api::model::{Id, Item};
use crate::api::retry::Sleeper;
use crate::api::sync::{CommandStatus, SyncRequest, SyncResponse};
use crate::db::apply::{apply_sync, ApplyStats};
use crate::db::resources::item as item_resource;
use crate::db::{meta, queries};
use crate::error::{AppError, QueueError, Result, SyncError};
use crate::queue::command::{CommandState, QueuedCommand};
use crate::queue::flush::build_flush_batch;
use crate::queue::store::CommandStore;
use crate::queue::temp_ids::{resolve_temp_ids, rewrite_sqlite_cache_temp_ids};

/// Object-safe boxed future returned by async port methods.
pub type BoxFuture<'a, T> = Pin<Box<dyn Future<Output = T> + Send + 'a>>;

/// Summary of applying a Sync API `sync_status` map to an in-flight command batch.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct QueueStatusSummary {
    /// Number of commands acknowledged `"ok"` and removed from the queue.
    pub succeeded: usize,
    /// Number of commands rejected with an error and marked [`CommandState::Failed`].
    pub failed: usize,
    /// Number of commands absent from `sync_status` and returned to [`CommandState::Pending`].
    pub requeued: usize,
}

/// Port for submitting a `/sync` request to the Todoist Sync API.
pub trait SyncApi: Send + Sync {
    /// Submits `req` to `/api/v1/sync` and returns the parsed [`SyncResponse`].
    fn sync<'a>(&'a self, req: &'a SyncRequest) -> BoxFuture<'a, Result<SyncResponse>>;
}

impl<C: Sleeper> SyncApi for SyncClient<C> {
    fn sync<'a>(&'a self, req: &'a SyncRequest) -> BoxFuture<'a, Result<SyncResponse>> {
        Box::pin(async move { SyncClient::sync(self, req).await })
    }
}

impl<T: SyncApi + ?Sized> SyncApi for &T {
    fn sync<'a>(&'a self, req: &'a SyncRequest) -> BoxFuture<'a, Result<SyncResponse>> {
        (**self).sync(req)
    }
}

impl<T: SyncApi + ?Sized> SyncApi for Arc<T> {
    fn sync<'a>(&'a self, req: &'a SyncRequest) -> BoxFuture<'a, Result<SyncResponse>> {
        (**self).sync(req)
    }
}

impl<T: SyncApi + ?Sized> SyncApi for Box<T> {
    fn sync<'a>(&'a self, req: &'a SyncRequest) -> BoxFuture<'a, Result<SyncResponse>> {
        (**self).sync(req)
    }
}

/// Port for reading and updating the local SQLite cache during a sync cycle.
pub trait Cache: Send + Sync {
    /// Reads the stored incremental `sync_token`, or `None` if no sync has ever
    /// completed.
    fn sync_token(&self) -> BoxFuture<'_, Result<Option<String>>>;

    /// Persists the incremental `sync_token` returned by the server.
    fn set_sync_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<()>>;

    /// Reads the UTC timestamp of the last full sync (`full_sync_date_utc`),
    /// if any.
    fn full_sync_date_utc(&self) -> BoxFuture<'_, Result<Option<String>>>;

    /// Applies `response` (full or incremental delta) atomically to the cache.
    fn apply_response<'a>(
        &'a self,
        response: &'a SyncResponse,
    ) -> BoxFuture<'a, Result<ApplyStats>>;

    /// Fetches a single cached [`Item`] by `id` (used by FR-18 conflict replay
    /// in `T-507`).
    fn item_by_id<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<Option<Item>>> {
        let _ = id;
        Box::pin(async { Ok(None) })
    }

    /// Upserts a single [`Item`] into the cache (used by FR-18 conflict replay
    /// in `T-507` to re-apply pending local mutations on top of server state).
    fn upsert_item<'a>(&'a self, item: &'a Item) -> BoxFuture<'a, Result<()>> {
        let _ = item;
        Box::pin(async { Ok(()) })
    }

    /// Deletes a single [`Item`] from the cache by `id` (used by FR-18 conflict
    /// replay when a pending `item_delete` is replayed).
    fn delete_item<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<()>> {
        let _ = id;
        Box::pin(async { Ok(()) })
    }
}

impl<T: Cache + ?Sized> Cache for &T {
    fn sync_token(&self) -> BoxFuture<'_, Result<Option<String>>> {
        (**self).sync_token()
    }

    fn set_sync_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<()>> {
        (**self).set_sync_token(token)
    }

    fn full_sync_date_utc(&self) -> BoxFuture<'_, Result<Option<String>>> {
        (**self).full_sync_date_utc()
    }

    fn apply_response<'a>(
        &'a self,
        response: &'a SyncResponse,
    ) -> BoxFuture<'a, Result<ApplyStats>> {
        (**self).apply_response(response)
    }

    fn item_by_id<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<Option<Item>>> {
        (**self).item_by_id(id)
    }

    fn upsert_item<'a>(&'a self, item: &'a Item) -> BoxFuture<'a, Result<()>> {
        (**self).upsert_item(item)
    }

    fn delete_item<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<()>> {
        (**self).delete_item(id)
    }
}

impl<T: Cache + ?Sized> Cache for Arc<T> {
    fn sync_token(&self) -> BoxFuture<'_, Result<Option<String>>> {
        (**self).sync_token()
    }

    fn set_sync_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<()>> {
        (**self).set_sync_token(token)
    }

    fn full_sync_date_utc(&self) -> BoxFuture<'_, Result<Option<String>>> {
        (**self).full_sync_date_utc()
    }

    fn apply_response<'a>(
        &'a self,
        response: &'a SyncResponse,
    ) -> BoxFuture<'a, Result<ApplyStats>> {
        (**self).apply_response(response)
    }

    fn item_by_id<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<Option<Item>>> {
        (**self).item_by_id(id)
    }

    fn upsert_item<'a>(&'a self, item: &'a Item) -> BoxFuture<'a, Result<()>> {
        (**self).upsert_item(item)
    }

    fn delete_item<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<()>> {
        (**self).delete_item(id)
    }
}

impl<T: Cache + ?Sized> Cache for Box<T> {
    fn sync_token(&self) -> BoxFuture<'_, Result<Option<String>>> {
        (**self).sync_token()
    }

    fn set_sync_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<()>> {
        (**self).set_sync_token(token)
    }

    fn full_sync_date_utc(&self) -> BoxFuture<'_, Result<Option<String>>> {
        (**self).full_sync_date_utc()
    }

    fn apply_response<'a>(
        &'a self,
        response: &'a SyncResponse,
    ) -> BoxFuture<'a, Result<ApplyStats>> {
        (**self).apply_response(response)
    }

    fn item_by_id<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<Option<Item>>> {
        (**self).item_by_id(id)
    }

    fn upsert_item<'a>(&'a self, item: &'a Item) -> BoxFuture<'a, Result<()>> {
        (**self).upsert_item(item)
    }

    fn delete_item<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<()>> {
        (**self).delete_item(id)
    }
}

/// Port for interacting with the durable offline write queue during a sync cycle.
pub trait WriteQueue: Send + Sync {
    /// Selects up to `max_batch` [`CommandState::Pending`] commands in enqueue
    /// order and marks them [`CommandState::InFlight`]. Returns `Ok(None)` when
    /// no pending commands exist.
    fn pending_batch(&self, max_batch: usize) -> BoxFuture<'_, Result<Option<Vec<QueuedCommand>>>>;

    /// Returns all currently [`CommandState::Pending`] commands in enqueue order
    /// without mutating their state (used by FR-18 conflict replay in `T-507`).
    fn pending_commands(&self) -> BoxFuture<'_, Result<Vec<QueuedCommand>>> {
        Box::pin(async { Ok(Vec::new()) })
    }

    /// Returns the number of commands currently in [`CommandState::Pending`].
    fn pending_count(&self) -> BoxFuture<'_, Result<usize>>;

    /// Rewrites every occurrence of each mapped temporary ID across queued
    /// commands (and associated cached rows) to its permanent [`Id`].
    fn resolve_temp_ids<'a>(
        &'a self,
        mapping: &'a HashMap<String, Id>,
    ) -> BoxFuture<'a, Result<usize>>;

    /// Applies the per-command `sync_status` outcomes from a [`SyncResponse`]
    /// to the `batch` that was submitted in the cycle.
    fn apply_sync_status<'a>(
        &'a self,
        batch: &'a [QueuedCommand],
        sync_status: &'a HashMap<String, CommandStatus>,
    ) -> BoxFuture<'a, Result<QueueStatusSummary>>;

    /// Returns `batch` commands from [`CommandState::InFlight`] back to
    /// [`CommandState::Pending`] with their original `uuid` intact after a
    /// failed `/sync` HTTP call.
    fn requeue_in_flight<'a>(&'a self, batch: &'a [QueuedCommand]) -> BoxFuture<'a, Result<()>> {
        let _ = batch;
        Box::pin(async { Ok(()) })
    }
}

impl<T: WriteQueue + ?Sized> WriteQueue for &T {
    fn pending_batch(&self, max_batch: usize) -> BoxFuture<'_, Result<Option<Vec<QueuedCommand>>>> {
        (**self).pending_batch(max_batch)
    }

    fn pending_commands(&self) -> BoxFuture<'_, Result<Vec<QueuedCommand>>> {
        (**self).pending_commands()
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize>> {
        (**self).pending_count()
    }

    fn resolve_temp_ids<'a>(
        &'a self,
        mapping: &'a HashMap<String, Id>,
    ) -> BoxFuture<'a, Result<usize>> {
        (**self).resolve_temp_ids(mapping)
    }

    fn apply_sync_status<'a>(
        &'a self,
        batch: &'a [QueuedCommand],
        sync_status: &'a HashMap<String, CommandStatus>,
    ) -> BoxFuture<'a, Result<QueueStatusSummary>> {
        (**self).apply_sync_status(batch, sync_status)
    }

    fn requeue_in_flight<'a>(&'a self, batch: &'a [QueuedCommand]) -> BoxFuture<'a, Result<()>> {
        (**self).requeue_in_flight(batch)
    }
}

impl<T: WriteQueue + ?Sized> WriteQueue for Arc<T> {
    fn pending_batch(&self, max_batch: usize) -> BoxFuture<'_, Result<Option<Vec<QueuedCommand>>>> {
        (**self).pending_batch(max_batch)
    }

    fn pending_commands(&self) -> BoxFuture<'_, Result<Vec<QueuedCommand>>> {
        (**self).pending_commands()
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize>> {
        (**self).pending_count()
    }

    fn resolve_temp_ids<'a>(
        &'a self,
        mapping: &'a HashMap<String, Id>,
    ) -> BoxFuture<'a, Result<usize>> {
        (**self).resolve_temp_ids(mapping)
    }

    fn apply_sync_status<'a>(
        &'a self,
        batch: &'a [QueuedCommand],
        sync_status: &'a HashMap<String, CommandStatus>,
    ) -> BoxFuture<'a, Result<QueueStatusSummary>> {
        (**self).apply_sync_status(batch, sync_status)
    }

    fn requeue_in_flight<'a>(&'a self, batch: &'a [QueuedCommand]) -> BoxFuture<'a, Result<()>> {
        (**self).requeue_in_flight(batch)
    }
}

impl<T: WriteQueue + ?Sized> WriteQueue for Box<T> {
    fn pending_batch(&self, max_batch: usize) -> BoxFuture<'_, Result<Option<Vec<QueuedCommand>>>> {
        (**self).pending_batch(max_batch)
    }

    fn pending_commands(&self) -> BoxFuture<'_, Result<Vec<QueuedCommand>>> {
        (**self).pending_commands()
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize>> {
        (**self).pending_count()
    }

    fn resolve_temp_ids<'a>(
        &'a self,
        mapping: &'a HashMap<String, Id>,
    ) -> BoxFuture<'a, Result<usize>> {
        (**self).resolve_temp_ids(mapping)
    }

    fn apply_sync_status<'a>(
        &'a self,
        batch: &'a [QueuedCommand],
        sync_status: &'a HashMap<String, CommandStatus>,
    ) -> BoxFuture<'a, Result<QueueStatusSummary>> {
        (**self).apply_sync_status(batch, sync_status)
    }

    fn requeue_in_flight<'a>(&'a self, batch: &'a [QueuedCommand]) -> BoxFuture<'a, Result<()>> {
        (**self).requeue_in_flight(batch)
    }
}

/// Production SQLite-backed adapter implementing both [`Cache`] and [`WriteQueue`].
///
/// Wraps a shared `Arc<Mutex<Connection>>` so the same underlying database
/// connection can serve both the `Cache` and `WriteQueue` ports in
/// `SyncCycle` / `SyncEngine`.
#[derive(Clone)]
pub struct SqliteSyncStore {
    conn: Arc<Mutex<Connection>>,
}

impl std::fmt::Debug for SqliteSyncStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SqliteSyncStore").finish_non_exhaustive()
    }
}

impl SqliteSyncStore {
    /// Wraps an owned [`Connection`] in a new [`SqliteSyncStore`].
    #[must_use]
    pub fn new(conn: Connection) -> Self {
        Self {
            conn: Arc::new(Mutex::new(conn)),
        }
    }

    /// Wraps an existing `Arc<Mutex<Connection>>`.
    #[must_use]
    pub const fn from_shared(conn: Arc<Mutex<Connection>>) -> Self {
        Self { conn }
    }

    /// Returns a clone of the inner shared connection handle.
    #[must_use]
    pub fn shared_connection(&self) -> Arc<Mutex<Connection>> {
        Arc::clone(&self.conn)
    }

    fn lock_conn(&self) -> std::sync::MutexGuard<'_, Connection> {
        self.conn
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
    }
}

impl Cache for SqliteSyncStore {
    fn sync_token(&self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            let conn = self.lock_conn();
            meta::sync_token(&conn).map_err(|err| {
                SyncError::Store {
                    message: err.to_string(),
                }
                .into()
            })
        })
    }

    fn set_sync_token<'a>(&'a self, token: &'a str) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let conn = self.lock_conn();
            meta::set_sync_token(&conn, token).map_err(|err| {
                SyncError::Store {
                    message: err.to_string(),
                }
                .into()
            })
        })
    }

    fn full_sync_date_utc(&self) -> BoxFuture<'_, Result<Option<String>>> {
        Box::pin(async move {
            let conn = self.lock_conn();
            meta::full_sync_date_utc(&conn).map_err(|err| {
                SyncError::Store {
                    message: err.to_string(),
                }
                .into()
            })
        })
    }

    fn apply_response<'a>(
        &'a self,
        response: &'a SyncResponse,
    ) -> BoxFuture<'a, Result<ApplyStats>> {
        Box::pin(async move {
            let mut conn = self.lock_conn();
            apply_sync(&mut conn, response).map_err(|err| {
                SyncError::Store {
                    message: err.to_string(),
                }
                .into()
            })
        })
    }

    fn item_by_id<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<Option<Item>>> {
        Box::pin(async move {
            let conn = self.lock_conn();
            queries::item_by_id(&conn, id).map_err(|err| {
                SyncError::Store {
                    message: err.to_string(),
                }
                .into()
            })
        })
    }

    fn upsert_item<'a>(&'a self, item: &'a Item) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let conn = self.lock_conn();
            item_resource::upsert(&conn, item).map_err(|err| {
                SyncError::Store {
                    message: err.to_string(),
                }
                .into()
            })
        })
    }

    fn delete_item<'a>(&'a self, id: &'a Id) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            let conn = self.lock_conn();
            item_resource::delete(&conn, id.as_str()).map_err(|err| {
                SyncError::Store {
                    message: err.to_string(),
                }
                .into()
            })
        })
    }
}

impl WriteQueue for SqliteSyncStore {
    fn pending_batch(&self, max_batch: usize) -> BoxFuture<'_, Result<Option<Vec<QueuedCommand>>>> {
        Box::pin(async move {
            let mut conn = self.lock_conn();
            let batch = build_flush_batch(&mut conn, max_batch).map_err(|err| {
                AppError::from(SyncError::Queue {
                    message: err.to_string(),
                })
            })?;
            Ok(batch.map(|(commands, _request)| commands))
        })
    }

    fn pending_commands(&self) -> BoxFuture<'_, Result<Vec<QueuedCommand>>> {
        Box::pin(async move {
            let conn = self.lock_conn();
            CommandStore::new(&conn).pending(u32::MAX).map_err(|err| {
                AppError::from(SyncError::Queue {
                    message: err.to_string(),
                })
            })
        })
    }

    fn pending_count(&self) -> BoxFuture<'_, Result<usize>> {
        Box::pin(async move {
            let conn = self.lock_conn();
            let counts = CommandStore::new(&conn).count_by_state().map_err(|err| {
                AppError::from(SyncError::Queue {
                    message: err.to_string(),
                })
            })?;
            Ok(usize::try_from(counts.pending).unwrap_or(usize::MAX))
        })
    }

    fn resolve_temp_ids<'a>(
        &'a self,
        mapping: &'a HashMap<String, Id>,
    ) -> BoxFuture<'a, Result<usize>> {
        Box::pin(async move {
            let mut conn = self.lock_conn();
            let tx = conn
                .transaction()
                .map_err(|source| QueueError::Storage { source })
                .map_err(|err| SyncError::Queue {
                    message: err.to_string(),
                })?;
            let updated =
                resolve_temp_ids(&tx, mapping, rewrite_sqlite_cache_temp_ids).map_err(|err| {
                    SyncError::Queue {
                        message: err.to_string(),
                    }
                })?;
            tx.commit()
                .map_err(|source| QueueError::Storage { source })
                .map_err(|err| SyncError::Queue {
                    message: err.to_string(),
                })?;
            Ok(updated)
        })
    }

    fn apply_sync_status<'a>(
        &'a self,
        batch: &'a [QueuedCommand],
        sync_status: &'a HashMap<String, CommandStatus>,
    ) -> BoxFuture<'a, Result<QueueStatusSummary>> {
        Box::pin(async move {
            let mut conn = self.lock_conn();
            let tx = conn
                .transaction()
                .map_err(|source| QueueError::Storage { source })
                .map_err(|err| SyncError::Queue {
                    message: err.to_string(),
                })?;
            let store = CommandStore::new(&tx);
            let mut summary = QueueStatusSummary::default();

            for cmd in batch {
                let key = cmd.uuid.to_string();
                match sync_status.get(&key) {
                    Some(CommandStatus::Ok(_)) => {
                        store.remove(cmd.uuid).map_err(|err| SyncError::Queue {
                            message: err.to_string(),
                        })?;
                        summary.succeeded = summary.succeeded.saturating_add(1);
                    }
                    Some(CommandStatus::Error(err)) => {
                        let message = err.error.as_deref().unwrap_or("command rejected by server");
                        store
                            .mark_failed(cmd.uuid, message)
                            .map_err(|e| SyncError::Queue {
                                message: e.to_string(),
                            })?;
                        summary.failed = summary.failed.saturating_add(1);
                    }
                    None => {
                        tx.execute(
                            "UPDATE commands SET state = ?1 WHERE uuid = ?2",
                            params![CommandState::Pending.as_str(), key],
                        )
                        .map_err(|source| QueueError::Storage { source })
                        .map_err(|e| SyncError::Queue {
                            message: e.to_string(),
                        })?;
                        summary.requeued = summary.requeued.saturating_add(1);
                    }
                }
            }

            tx.commit()
                .map_err(|source| QueueError::Storage { source })
                .map_err(|err| SyncError::Queue {
                    message: err.to_string(),
                })?;
            Ok(summary)
        })
    }

    fn requeue_in_flight<'a>(&'a self, batch: &'a [QueuedCommand]) -> BoxFuture<'a, Result<()>> {
        Box::pin(async move {
            if batch.is_empty() {
                return Ok(());
            }
            let mut conn = self.lock_conn();
            let tx = conn
                .transaction()
                .map_err(|source| QueueError::Storage { source })
                .map_err(|err| SyncError::Queue {
                    message: err.to_string(),
                })?;
            for cmd in batch {
                tx.execute(
                    "UPDATE commands SET state = ?1 WHERE uuid = ?2 AND state = ?3",
                    params![
                        CommandState::Pending.as_str(),
                        cmd.uuid.to_string(),
                        CommandState::InFlight.as_str()
                    ],
                )
                .map_err(|source| QueueError::Storage { source })
                .map_err(|err| SyncError::Queue {
                    message: err.to_string(),
                })?;
            }
            tx.commit()
                .map_err(|source| QueueError::Storage { source })
                .map_err(|err| SyncError::Queue {
                    message: err.to_string(),
                })?;
            Ok(())
        })
    }
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;
    use crate::api::retry::RecordingSleeper;
    use crate::auth::Token;
    use crate::db::migrations;
    use crate::queue::schema;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    const FIXTURE_TOKEN: &str = "tok_fake_abc123";

    fn assert_object_safe(_api: &dyn SyncApi, _cache: &dyn Cache, _queue: &dyn WriteQueue) {}

    #[tokio::test]
    async fn sync_client_implements_sync_api_and_forwards_calls() {
        let server = MockServer::start().await;
        Mock::given(method("POST"))
            .and(path("/api/v1/sync"))
            .respond_with(
                ResponseTemplate::new(200).set_body_string(r#"{"sync_token": "tok_forwarded_1"}"#),
            )
            .mount(&server)
            .await;

        let token = Token::new(FIXTURE_TOKEN.to_string()).unwrap();
        let client = SyncClient::with_clock(token, &server.uri(), RecordingSleeper::new()).unwrap();

        let api_obj: &dyn SyncApi = &client;
        let resp = api_obj.sync(&SyncRequest::full()).await.unwrap();
        assert_eq!(resp.sync_token, "tok_forwarded_1");
    }

    #[tokio::test]
    async fn sqlite_sync_store_implements_cache_and_write_queue_and_is_object_safe() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        schema::apply(&conn).unwrap();

        let store = SqliteSyncStore::new(conn);
        let dummy_token = Token::new(FIXTURE_TOKEN.to_string()).unwrap();
        let client = SyncClient::with_clock(
            dummy_token,
            "https://api.todoist.com",
            RecordingSleeper::new(),
        )
        .unwrap();

        assert_object_safe(&client, &store, &store);

        assert_eq!(store.sync_token().await.unwrap(), None);
        store.set_sync_token("cursor_001").await.unwrap();
        assert_eq!(
            store.sync_token().await.unwrap().as_deref(),
            Some("cursor_001")
        );
        assert_eq!(store.pending_count().await.unwrap(), 0);
        assert_eq!(store.pending_batch(10).await.unwrap(), None);
    }
}
