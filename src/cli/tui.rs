//! TUI entrypoint (`todoist-tui` / `todoist-tui tui`) wiring UI, SQLite cache, and `SyncEngine` (`T-613`, EPIC-07).

#![allow(dead_code)]

use std::sync::Arc;

use crate::api::client::SyncClient;
use crate::auth::{TokenStore, DEFAULT_BASE_URL};
use crate::cli::today::model::{resolve_today_date, SystemClock};
use crate::config::Config;
use crate::db::{migrations, open};
use crate::error::{Result, UiError};
use crate::sync::{SqliteSyncStore, SyncEngine};
use crate::ui::data::FixtureData;
use crate::ui::run::{self, LiveContext};

fn resolve_base_url() -> String {
    match std::env::var("TODOIST_API_BASE_URL") {
        Ok(val) if !val.trim().is_empty() => val,
        _ => DEFAULT_BASE_URL.to_string(),
    }
}

/// Launches the interactive TUI using the loaded `config`.
///
/// When authenticated and able to open the local SQLite cache, connects
/// [`SqliteViewData`] and spawns the background [`SyncEngine`]. Otherwise
/// falls back cleanly to [`FixtureData::sample()`].
///
/// # Errors
///
/// Returns [`UiError`] or underlying storage/engine errors on failure.
pub fn run_tui(config: &Config) -> Result<()> {
    let rt = tokio::runtime::Runtime::new().map_err(|source| UiError::TerminalInit { source })?;
    rt.block_on(async {
        let token_opt = TokenStore::resolve()
            .ok()
            .and_then(|store| store.load().ok().flatten());

        if let Some(token) = token_opt {
            if let Ok(mut conn) = open::open_default() {
                if migrations::run(&mut conn).is_ok() {
                    let store = Arc::new(SqliteSyncStore::new(conn));
                    let base_url = resolve_base_url();
                    let client = Arc::new(SyncClient::new(token, &base_url)?);
                    let handle =
                        SyncEngine::spawn(client, Arc::clone(&store), Arc::clone(&store), config);
                    let _ = handle.trigger();

                    let mut view_conn = open::open_default()?;
                    let today = resolve_today_date(&view_conn, &SystemClock);
                    let mut ctx = LiveContext {
                        conn: &mut view_conn,
                        today_date: today,
                        authenticated: true,
                    };
                    let ui_res = run::run_live(config, &mut ctx, Some(&handle)).await;

                    handle.shutdown();
                    handle.stopped().await;
                    let _ = ui_res?;
                    return Ok(());
                }
            }
        }

        let data = FixtureData::sample();
        let _ = run::run(config, &data, None).await?;
        Ok(())
    })
}
