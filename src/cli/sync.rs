//! `todoist-tui sync` subcommand (`T-1002`, `T-1004`, EPIC-11).
//!
//! Flushes any queued offline quick-add entries (`quick_add` kind) via
//! `POST /api/v1/tasks/quick`, then executes one full/incremental [`SyncCycle`],
//! reporting `synced: <pulled> pulled, <flushed> flushed, <pending> pending`
//! to stdout. On network outage, writes `sync failed: offline` to stderr
//! (keeping stdout empty) and exits with [`crate::error::CliError::Offline`].

#![allow(dead_code)]

use std::io::Write;
use std::path::Path;
use std::sync::Arc;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::api::client::SyncClient;
use crate::auth::{TokenStore, DEFAULT_BASE_URL};
use crate::cli::add::{is_offline_error, QUICK_ADD_CMD_KIND};
use crate::cli::output::Output;
use crate::db::{migrations, open, resources};
use crate::error::{CliError, Result};
use crate::queue::store::CommandStore;
use crate::sync::cycle::SyncCycle;
use crate::sync::SqliteSyncStore;

/// Summary counts produced by one `todoist-tui sync` execution.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SyncSummary {
    /// Number of resources pulled from the Sync API response.
    pub pulled: usize,
    /// Total commands flushed (quick-add + sync commands).
    pub flushed: usize,
    /// Commands still pending after this run.
    pub pending: usize,
}

/// Service abstraction for running a foreground CLI sync (injectable for tests).
pub trait SyncService {
    /// Performs a flush-then-pull sync, returning [`SyncSummary`] on success.
    ///
    /// # Errors
    ///
    /// Propagates offline/transport, auth, API, or storage errors.
    fn sync_once(&mut self) -> Result<SyncSummary>;

    /// Returns the current number of pending commands remaining in the queue.
    fn pending_count(&self) -> usize;
}

/// Production [`SyncService`] backed by SQLite + [`SyncClient`].
pub struct LiveSyncService {
    store: Arc<SqliteSyncStore>,
    client: Arc<SyncClient>,
}

impl LiveSyncService {
    /// Wraps a migrated [`Connection`] and authenticated [`SyncClient`].
    #[must_use]
    pub fn new(conn: Connection, client: SyncClient) -> Self {
        Self {
            store: Arc::new(SqliteSyncStore::new(conn)),
            client: Arc::new(client),
        }
    }

    fn flush_queued_quick_adds(&self, rt: &tokio::runtime::Runtime) -> Result<usize> {
        let mut flushed = 0usize;

        loop {
            let next_qa = {
                let conn_arc = self.store.shared_connection();
                let conn = conn_arc
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                let pending = CommandStore::new(&conn).pending(100)?;
                pending
                    .into_iter()
                    .find(|cmd| cmd.kind == QUICK_ADD_CMD_KIND)
            };

            let Some(cmd) = next_qa else {
                break;
            };

            let text = cmd
                .args
                .get("text")
                .and_then(serde_json::Value::as_str)
                .unwrap_or_default()
                .to_string();

            let item = rt.block_on(self.client.quick_add(&text))?;

            {
                let conn_arc = self.store.shared_connection();
                let conn = conn_arc
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner);
                resources::item::upsert(&conn, &item)?;
                CommandStore::new(&conn).remove(cmd.uuid)?;
            }
            flushed += 1;
        }

        Ok(flushed)
    }
}

impl SyncService for LiveSyncService {
    fn sync_once(&mut self) -> Result<SyncSummary> {
        let rt = tokio::runtime::Runtime::new().map_err(|source| CliError::Io { source })?;

        let qa_flushed = self.flush_queued_quick_adds(&rt)?;

        let cycle = SyncCycle::new(
            Arc::clone(&self.client),
            Arc::clone(&self.store),
            Arc::clone(&self.store),
        );
        let now_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

        let outcome = rt.block_on(cycle.run_once(now_secs))?;
        let pending = self.pending_count();

        Ok(SyncSummary {
            pulled: outcome.pulled_resources,
            flushed: qa_flushed + outcome.flushed_commands,
            pending,
        })
    }

    fn pending_count(&self) -> usize {
        let conn_arc = self.store.shared_connection();
        let conn = conn_arc
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        CommandStore::new(&conn)
            .count_by_state()
            .map_or(0, |c| usize::try_from(c.pending).unwrap_or(usize::MAX))
    }
}

/// Runs `todoist-tui sync` against an injected [`SyncService`] and [`Output`].
///
/// # Errors
///
/// Emits a concise offline diagnostic to stderr and returns [`CliError::Offline`]
/// on connectivity errors, leaving stdout completely empty.
pub fn run_sync_with<S, W, E>(service: &mut S, out: &mut Output<W, E>) -> Result<()>
where
    S: SyncService,
    W: Write,
    E: Write,
{
    match service.sync_once() {
        Ok(summary) => {
            let line = format!(
                "synced: {} pulled, {} flushed, {} pending",
                summary.pulled, summary.flushed, summary.pending
            );
            out.write_line(&line)
                .map_err(|source| CliError::Io { source })?;
            Ok(())
        }
        Err(err) if is_offline_error(&err) => {
            let pending = service.pending_count();
            let _ = out.write_error_line(&format!("sync failed: offline ({pending} pending)"));
            Err(CliError::Offline.into())
        }
        Err(err) => Err(err),
    }
}

fn resolve_base_url() -> String {
    match std::env::var("TODOIST_API_BASE_URL") {
        Ok(val) if !val.trim().is_empty() => val,
        _ => DEFAULT_BASE_URL.to_string(),
    }
}

/// Production entry point for `todoist-tui sync`.
///
/// # Errors
///
/// Propagates authentication, offline, API, or storage errors.
pub fn run_sync(_config_override: Option<&Path>, no_color: bool) -> Result<()> {
    let token = TokenStore::resolve()?.require()?;
    let mut conn = open::open_default()?;
    migrations::run(&mut conn)?;

    let client = SyncClient::new(token, &resolve_base_url())?;
    let mut service = LiveSyncService::new(conn, client);
    let mut out = Output::for_process(no_color);

    run_sync_with(&mut service, &mut out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::ApiError;

    struct FakeSyncService {
        result: Option<Result<SyncSummary>>,
        pending: usize,
    }

    impl SyncService for FakeSyncService {
        fn sync_once(&mut self) -> Result<SyncSummary> {
            self.result.take().unwrap_or(Err(CliError::Offline.into()))
        }

        fn pending_count(&self) -> usize {
            self.pending
        }
    }

    #[test]
    fn online_sync_prints_summary_line_and_exits_ok() {
        let mut svc = FakeSyncService {
            result: Some(Ok(SyncSummary {
                pulled: 12,
                flushed: 3,
                pending: 0,
            })),
            pending: 0,
        };
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut out = Output::new(&mut stdout_buf, &mut stderr_buf, false);

        run_sync_with(&mut svc, &mut out).unwrap();

        let stdout = String::from_utf8(stdout_buf).unwrap();
        assert_eq!(stdout, "synced: 12 pulled, 3 flushed, 0 pending\n");
    }

    #[test]
    fn offline_sync_writes_stderr_with_empty_stdout_and_offline_exit_code() {
        let mut svc = FakeSyncService {
            result: Some(Err(ApiError::RetriesExhausted { attempts: 4 }.into())),
            pending: 5,
        };
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut out = Output::new(&mut stdout_buf, &mut stderr_buf, false);

        let err = run_sync_with(&mut svc, &mut out).unwrap_err();
        assert_eq!(crate::error::exit_code(&err), 4);
        assert!(stdout_buf.is_empty(), "stdout must be empty when offline");
        let stderr = String::from_utf8(stderr_buf).unwrap();
        assert!(stderr.contains("sync failed: offline (5 pending)"));
    }
}
