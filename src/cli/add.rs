//! `todoist-tui add` subcommand (`T-1003`, `T-1004`, EPIC-11).
//!
//! Sends quick-add text verbatim to the server-side `POST /api/v1/tasks/quick`
//! endpoint (`REQ-001` D1 — no local NL parsing). When offline, durably
//! enqueues the raw text into the SQLite `commands` queue and exits `0` with
//! `queued (offline): <text>`.

#![allow(dead_code)]

use std::io::Write;
use std::path::Path;

use rusqlite::Connection;
use serde_json::json;
use uuid::Uuid;

use crate::api::client::SyncClient;
use crate::api::model::Item;
use crate::api::retry::Sleeper;
use crate::auth::{TokenStore, DEFAULT_BASE_URL};
use crate::cli::output::Output;
use crate::db::{migrations, open, resources};
use crate::error::{ApiError, AppError, CliError, Result};
use crate::queue::enqueue::enqueue;
use crate::queue::QueuedCommand;

/// Command kind stored in `commands.type` for offline unparsed quick-adds.
pub const QUICK_ADD_CMD_KIND: &str = "quick_add";

/// Minimal client trait for submitting quick-add text (enables headless unit tests).
pub trait QuickAddApi {
    /// Submits raw `text` verbatim to the Todoist quick-add endpoint.
    ///
    /// # Errors
    ///
    /// Propagates transport, auth, or API errors.
    fn quick_add(&self, text: &str) -> Result<Item>;
}

/// Minimal store trait for caching the added item and durably queuing offline adds.
pub trait AddStore {
    /// Persists a successfully quick-added [`Item`] in the local SQLite cache.
    fn upsert_added_item(&mut self, item: &Item) -> Result<()>;

    /// Durably enqueues an unparsed quick-add [`QueuedCommand`] for later flush.
    fn enqueue_offline_add(&mut self, cmd: &QueuedCommand) -> Result<()>;
}

/// Production [`QuickAddApi`] backed by a Tokio runtime and [`SyncClient`].
pub struct LiveQuickAddClient<C: Sleeper> {
    client: SyncClient<C>,
}

impl<C: Sleeper> LiveQuickAddClient<C> {
    /// Wraps a [`SyncClient`].
    #[must_use]
    pub const fn new(client: SyncClient<C>) -> Self {
        Self { client }
    }
}

impl<C: Sleeper> QuickAddApi for LiveQuickAddClient<C> {
    fn quick_add(&self, text: &str) -> Result<Item> {
        let rt = tokio::runtime::Runtime::new().map_err(|source| CliError::Io { source })?;
        rt.block_on(self.client.quick_add(text))
    }
}

/// Production [`AddStore`] backed by a [`rusqlite::Connection`].
pub struct SqliteAddStore<'a> {
    conn: &'a mut Connection,
}

impl<'a> SqliteAddStore<'a> {
    /// Wraps a migrated [`Connection`].
    pub fn new(conn: &'a mut Connection) -> Self {
        Self { conn }
    }
}

impl AddStore for SqliteAddStore<'_> {
    fn upsert_added_item(&mut self, item: &Item) -> Result<()> {
        resources::item::upsert(self.conn, item)
    }

    fn enqueue_offline_add(&mut self, cmd: &QueuedCommand) -> Result<()> {
        enqueue(self.conn, cmd, |_tx| Ok(()))
    }
}

/// Returns `true` if `err` represents a transport/connectivity outage (eligible for offline queue).
#[must_use]
pub fn is_offline_error(err: &AppError) -> bool {
    match err {
        AppError::Cli(CliError::Offline)
        | AppError::Api(ApiError::Transport { .. } | ApiError::RetriesExhausted { .. }) => true,
        AppError::Sync(sync_err) => sync_err.is_offline(),
        _ => false,
    }
}

/// Executes `todoist-tui add <text>` with injected dependencies.
///
/// # Errors
///
/// Returns [`CliError::Usage`] (exit code 2) for empty/whitespace-only `text`,
/// or non-transport API/storage errors. Transport errors fall back to durable
/// queueing and return `Ok(())`.
pub fn run_add_with<A, S, W, E>(
    text: &str,
    api: &A,
    store: &mut S,
    out: &mut Output<W, E>,
) -> Result<()>
where
    A: QuickAddApi,
    S: AddStore,
    W: Write,
    E: Write,
{
    if text.trim().is_empty() {
        return Err(CliError::Usage {
            message: "task text cannot be empty".to_string(),
        }
        .into());
    }

    match api.quick_add(text) {
        Ok(item) => {
            store.upsert_added_item(&item)?;
            let line = match &item.due {
                Some(due) => {
                    let due_label = due.string.as_deref().unwrap_or(&due.date);
                    format!("added: {} (due: {due_label})", item.content)
                }
                None => format!("added: {}", item.content),
            };
            out.write_line(&line)
                .map_err(|source| CliError::Io { source })?;
            Ok(())
        }
        Err(err) if is_offline_error(&err) => {
            let temp_id = format!("tmp_qa_{}", Uuid::new_v4());
            let cmd = QueuedCommand::new(
                QUICK_ADD_CMD_KIND,
                json!({
                    "text": text,
                    "unparsed": true,
                }),
                Some(temp_id),
            );
            store.enqueue_offline_add(&cmd)?;
            out.write_line(&format!("queued (offline): {text}"))
                .map_err(|source| CliError::Io { source })?;
            Ok(())
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

/// Production entry point for `todoist-tui add <text>`.
///
/// # Errors
///
/// Propagates authentication, usage, storage, or API errors.
pub fn run_add(_config_override: Option<&Path>, text: &str, no_color: bool) -> Result<()> {
    let token = TokenStore::resolve()?.require()?;
    let mut conn = open::open_default()?;
    migrations::run(&mut conn)?;

    let client = SyncClient::new(token, &resolve_base_url())?;
    let api = LiveQuickAddClient::new(client);
    let mut store = SqliteAddStore::new(&mut conn);
    let mut out = Output::for_process(no_color);

    run_add_with(text, &api, &mut store, &mut out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::RefCell;

    use crate::queue::store::CommandStore;

    struct FakeQuickAddApi {
        calls: RefCell<Vec<String>>,
        response: std::result::Result<Item, ApiError>,
    }

    impl QuickAddApi for FakeQuickAddApi {
        fn quick_add(&self, text: &str) -> Result<Item> {
            self.calls.borrow_mut().push(text.to_string());
            match &self.response {
                Ok(item) => Ok(item.clone()),
                Err(ApiError::Unauthorized) => Err(ApiError::Unauthorized.into()),
                Err(ApiError::Server { status }) => {
                    Err(ApiError::Server { status: *status }.into())
                }
                Err(ApiError::RetriesExhausted { attempts }) => Err(ApiError::RetriesExhausted {
                    attempts: *attempts,
                }
                .into()),
                Err(_) => Err(CliError::Offline.into()),
            }
        }
    }

    fn fake_item(content: &str) -> Item {
        serde_json::from_value(json!({
            "id": "6X4VfakeQA1",
            "user_id": "u1",
            "project_id": "inbox1",
            "content": content,
            "description": "",
            "priority": 1,
            "child_order": 1,
            "added_at": "2026-11-01T10:00:00Z"
        }))
        .unwrap()
    }

    #[test]
    fn empty_text_rejected_with_usage_error_and_zero_requests() {
        let api = FakeQuickAddApi {
            calls: RefCell::new(Vec::new()),
            response: Ok(fake_item("ignored")),
        };
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        let mut store = SqliteAddStore::new(&mut conn);
        let mut out = Output::new(Vec::new(), Vec::new(), false);

        let err = run_add_with("   ", &api, &mut store, &mut out).unwrap_err();
        assert_eq!(crate::error::exit_code(&err), 2);
        assert!(api.calls.borrow().is_empty());
    }

    #[test]
    fn online_add_forwards_verbatim_caches_and_prints_added() {
        let raw = "Buy milk tomorrow 5pm #Work";
        let api = FakeQuickAddApi {
            calls: RefCell::new(Vec::new()),
            response: Ok(fake_item("Buy milk")),
        };
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        let mut store = SqliteAddStore::new(&mut conn);
        let mut stdout_buf = Vec::new();
        let mut stderr_buf = Vec::new();
        let mut out = Output::new(&mut stdout_buf, &mut stderr_buf, false);

        run_add_with(raw, &api, &mut store, &mut out).unwrap();

        assert_eq!(api.calls.borrow().as_slice(), &[raw.to_string()]);
        let stdout = String::from_utf8(stdout_buf).unwrap();
        assert_eq!(stdout, "added: Buy milk\n");
    }

    #[test]
    fn offline_add_queues_verbatim_and_exits_ok() {
        let raw = "Offline task tomorrow #Home";
        let api = FakeQuickAddApi {
            calls: RefCell::new(Vec::new()),
            response: Err(ApiError::RetriesExhausted { attempts: 1 }),
        };
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();

        {
            let mut store = SqliteAddStore::new(&mut conn);
            let mut stdout_buf = Vec::new();
            let mut stderr_buf = Vec::new();
            let mut out = Output::new(&mut stdout_buf, &mut stderr_buf, false);
            run_add_with(raw, &api, &mut store, &mut out).unwrap();
            let stdout = String::from_utf8(stdout_buf).unwrap();
            assert_eq!(stdout, format!("queued (offline): {raw}\n"));
        }

        let pending = CommandStore::new(&conn).pending(10).unwrap();
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].kind, QUICK_ADD_CMD_KIND);
        assert_eq!(pending[0].args["text"], raw);
    }

    #[test]
    fn unauthorized_or_4xx_error_is_not_queued() {
        for err in [ApiError::Unauthorized, ApiError::Server { status: 400 }] {
            let api = FakeQuickAddApi {
                calls: RefCell::new(Vec::new()),
                response: Err(err),
            };
            let mut conn = Connection::open_in_memory().unwrap();
            migrations::run(&mut conn).unwrap();

            {
                let mut store = SqliteAddStore::new(&mut conn);
                let mut out = Output::new(Vec::new(), Vec::new(), false);
                let res = run_add_with("Task", &api, &mut store, &mut out);
                assert!(res.is_err());
            }

            let pending = CommandStore::new(&conn).pending(10).unwrap();
            assert!(pending.is_empty(), "401/4xx must never be queued");
        }
    }
}
