//! Event loop and `ui::run` entry points wiring terminal I/O, input, and sync events (`T-613`, EPIC-07).

#![allow(dead_code)]

use std::io;
use std::time::SystemTime;

use crossterm::event::{Event, KeyCode, KeyEventKind};
use ratatui::backend::Backend;
use ratatui::Terminal;
use rusqlite::Connection;
use tokio::select;
use tokio::sync::mpsc;

use crate::config::Config;
use crate::error::{Result, UiError};
use crate::sync::SyncHandle;
use crate::ui::app::App;
use crate::ui::data::{SqliteViewData, ViewData};
use crate::ui::keymap::Keymap;
use crate::ui::session::{handle_live_key, refresh_queue_counts, InteractiveState};
use crate::ui::status::{StatusModel, SyncEvent};
use crate::ui::terminal::TerminalGuard;
use crate::ui::theme::{ColorDepth, Theme};
use crate::ui::view;

/// Live SQLite backing context for interactive TUI sessions (`EPIC-08`..`10`).
pub struct LiveContext<'a> {
    pub conn: &'a mut Connection,
    pub today_date: String,
    pub authenticated: bool,
}

/// Runs the TUI event loop on any [`Backend`] (real TTY or headless [`ratatui::backend::TestBackend`]).
///
/// Draws the initial frame, then selects over terminal input events and incoming
/// [`SyncEvent`]s, redrawing only when state changes or a resize arrives.
///
/// # Errors
///
/// Returns [`UiError`] or underlying data errors when rendering or data loading fails.
pub async fn run_with_backend<B>(
    terminal: &mut Terminal<B>,
    config: &Config,
    data: &dyn ViewData,
    input_rx: &mut mpsc::UnboundedReceiver<Event>,
    sync_rx: &mut mpsc::UnboundedReceiver<SyncEvent>,
    sync_trigger_tx: Option<&mpsc::UnboundedSender<()>>,
) -> Result<App>
where
    B: Backend + Send,
    B::Error: Send + Sync + 'static,
{
    let keymap = Keymap::from_config(&config.keymap)?;
    let depth = ColorDepth::from_env();
    let theme = Theme::by_name_for_depth(&config.theme, depth);

    let mut app = App::with_default_view(data, config.default_view)?;
    let mut status = StatusModel::new();

    // Initial draw
    terminal
        .draw(|f| view::render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme))
        .map_err(|e| UiError::Draw {
            source: io::Error::other(e.to_string()),
        })?;

    loop {
        if app.should_quit {
            break;
        }

        let mut needs_redraw = false;

        select! {
            maybe_ev = input_rx.recv() => {
                let Some(ev) = maybe_ev else {
                    break;
                };
                match ev {
                    Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                        if app.show_help {
                            // Any key or Esc closes the help overlay
                            app.dismiss_help();
                            needs_redraw = true;
                        } else if app.search_active {
                            if key.code == KeyCode::Esc {
                                app.search_active = false;
                                needs_redraw = true;
                            }
                        } else if let Some(action) = keymap.lookup(key) {
                            app.apply(action, data)?;
                            if app.take_sync_triggered() {
                                if let Some(tx) = sync_trigger_tx {
                                    let _ = tx.send(());
                                }
                            }
                            needs_redraw = true;
                        }
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
            }
            maybe_sync = sync_rx.recv() => {
                if let Some(sync_ev) = maybe_sync {
                    let is_synced = matches!(sync_ev, SyncEvent::Synced { .. });
                    status.apply(sync_ev, SystemTime::now());
                    if is_synced {
                        let _ = app.refresh(data);
                    }
                    needs_redraw = true;
                }
            }
        }

        if app.should_quit {
            break;
        }

        if needs_redraw {
            terminal
                .draw(|f| view::render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme))
                .map_err(|e| UiError::Draw {
                    source: io::Error::other(e.to_string()),
                })?;
        }
    }

    Ok(app)
}

/// Interactive entry point entering the [`TerminalGuard`], running the event loop,
/// and restoring the terminal before returning.
///
/// # Errors
///
/// Propagates terminal setup/teardown errors and UI/data errors.
pub async fn run(
    config: &Config,
    data: &dyn ViewData,
    sync_handle: Option<&SyncHandle>,
) -> Result<App> {
    let mut guard = TerminalGuard::enter()?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (sync_tx, mut sync_rx) = mpsc::unbounded_channel();
    let (trig_tx, mut trig_rx) = mpsc::unbounded_channel();

    // Forward engine SyncState events to UI SyncEvent channel
    if let Some(handle) = sync_handle {
        let mut engine_sub = handle.subscribe();
        let sync_tx_clone = sync_tx.clone();
        tokio::spawn(async move {
            while let Some(st) = engine_sub.recv().await {
                if sync_tx_clone.send(SyncEvent::from(st)).is_err() {
                    break;
                }
            }
        });

        let handle_clone = handle.clone();
        tokio::spawn(async move {
            while trig_rx.recv().await.is_some() {
                let _ = handle_clone.trigger();
            }
        });
    }

    // Spawn blocking reader thread for crossterm events
    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let res = run_with_backend(
        guard.terminal_mut(),
        config,
        data,
        &mut input_rx,
        &mut sync_rx,
        Some(&trig_tx),
    )
    .await;

    guard.restore()?;
    res
}

/// Runs the live interactive TUI event loop on any [`Backend`] backed by a writable SQLite
/// [`Connection`] so task/structure edits, `/` search, and read-only filter guards execute end-to-end.
///
/// # Errors
///
/// Returns [`UiError`] or underlying data errors when rendering or persistence fails.
#[allow(clippy::too_many_lines)]
pub async fn run_live_with_backend<B>(
    terminal: &mut Terminal<B>,
    config: &Config,
    ctx: &mut LiveContext<'_>,
    input_rx: &mut mpsc::UnboundedReceiver<Event>,
    sync_rx: &mut mpsc::UnboundedReceiver<SyncEvent>,
    sync_trigger_tx: Option<&mpsc::UnboundedSender<()>>,
) -> Result<App>
where
    B: Backend + Send,
    B::Error: Send + Sync + 'static,
{
    let keymap = Keymap::from_config(&config.keymap)?;
    let depth = ColorDepth::from_env();
    let theme = Theme::by_name_for_depth(&config.theme, depth);

    let mut app = {
        let data = SqliteViewData::new(ctx.conn, &ctx.today_date);
        App::with_default_view(&data, config.default_view)?
    };
    let mut status = StatusModel::new();
    refresh_queue_counts(ctx.conn, &mut status);
    let mut session = InteractiveState::default();

    terminal
        .draw(|f| session.render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme))
        .map_err(|e| UiError::Draw {
            source: io::Error::other(e.to_string()),
        })?;

    loop {
        if app.should_quit {
            break;
        }
        let mut needs_redraw = false;

        select! {
            maybe_ev = input_rx.recv() => {
                let Some(ev) = maybe_ev else {
                    break;
                };
                match ev {
                    Event::Key(key) if matches!(key.kind, KeyEventKind::Press | KeyEventKind::Repeat) => {
                        let (redraw, trigger) = handle_live_key(
                            key,
                            ctx.conn,
                            &ctx.today_date,
                            ctx.authenticated,
                            &mut app,
                            &mut session,
                            &mut status,
                            &keymap,
                        )?;
                        if trigger {
                            if let Some(tx) = sync_trigger_tx {
                                let _ = tx.send(());
                            }
                        }
                        needs_redraw = redraw;
                    }
                    Event::Resize(_, _) => {
                        needs_redraw = true;
                    }
                    _ => {}
                }
            }
            maybe_sync = sync_rx.recv() => {
                if let Some(sync_ev) = maybe_sync {
                    let is_synced = matches!(sync_ev, SyncEvent::Synced { .. });
                    status.apply(sync_ev, SystemTime::now());
                    refresh_queue_counts(ctx.conn, &mut status);
                    if is_synced {
                        let data = SqliteViewData::new(ctx.conn, &ctx.today_date);
                        let _ = app.refresh(&data);
                    }
                    needs_redraw = true;
                }
            }
        }

        if app.should_quit {
            break;
        }

        if needs_redraw {
            terminal
                .draw(|f| {
                    session.render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme);
                })
                .map_err(|e| UiError::Draw {
                    source: io::Error::other(e.to_string()),
                })?;
        }
    }

    Ok(app)
}

/// Interactive production entry point wired to a writable SQLite [`Connection`] and
/// optional [`SyncHandle`].
///
/// # Errors
///
/// Propagates terminal setup/teardown errors and UI/storage errors.
pub async fn run_live(
    config: &Config,
    ctx: &mut LiveContext<'_>,
    sync_handle: Option<&SyncHandle>,
) -> Result<App> {
    let mut guard = TerminalGuard::enter()?;

    let (input_tx, mut input_rx) = mpsc::unbounded_channel();
    let (sync_tx, mut sync_rx) = mpsc::unbounded_channel();
    let (trig_tx, mut trig_rx) = mpsc::unbounded_channel();

    if let Some(handle) = sync_handle {
        let mut engine_sub = handle.subscribe();
        let sync_tx_clone = sync_tx.clone();
        tokio::spawn(async move {
            while let Some(st) = engine_sub.recv().await {
                if sync_tx_clone.send(SyncEvent::from(st)).is_err() {
                    break;
                }
            }
        });

        let handle_clone = handle.clone();
        tokio::spawn(async move {
            while trig_rx.recv().await.is_some() {
                let _ = handle_clone.trigger();
            }
        });
    }

    std::thread::spawn(move || {
        while let Ok(ev) = crossterm::event::read() {
            if input_tx.send(ev).is_err() {
                break;
            }
        }
    });

    let res = run_live_with_backend(
        guard.terminal_mut(),
        config,
        ctx,
        &mut input_rx,
        &mut sync_rx,
        Some(&trig_tx),
    )
    .await;

    guard.restore()?;
    res
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::data::FixtureData;
    use crossterm::event::{KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;

    #[tokio::test]
    async fn headless_event_loop_processes_navigation_and_exits_on_q() {
        let config = Config::default();
        let data = FixtureData::sample();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        let (input_tx, mut input_rx) = mpsc::unbounded_channel();
        let (_sync_tx, mut sync_rx) = mpsc::unbounded_channel();
        let (trig_tx, mut trig_rx) = mpsc::unbounded_channel();

        // Feed scripted keys: j (move down), r (trigger sync), ? (help), Esc (close help), q (quit)
        let keys = [
            KeyCode::Char('j'),
            KeyCode::Char('r'),
            KeyCode::Char('?'),
            KeyCode::Esc,
            KeyCode::Char('q'),
        ];
        for code in keys {
            input_tx
                .send(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                .unwrap();
        }

        let app = run_with_backend(
            &mut terminal,
            &config,
            &data,
            &mut input_rx,
            &mut sync_rx,
            Some(&trig_tx),
        )
        .await
        .unwrap();

        assert!(app.should_quit);
        assert_eq!(app.sidebar_index(), 1);
        assert!(!app.show_help);
        assert!(trig_rx.try_recv().is_ok());
    }

    #[tokio::test]
    async fn live_event_loop_wires_task_add_structure_and_search() {
        use crate::api::model::{Id, Project};
        use crate::db::{migrations, queries, resources};
        use crate::queue::store::CommandStore;

        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        resources::project::upsert(
            &conn,
            &Project {
                id: Id::new("inbox"),
                name: "Inbox".to_string(),
                parent_id: None,
                child_order: 0,
                color: "grey".to_string(),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: "list".to_string(),
            },
        )
        .unwrap();

        let config = Config::default();
        let backend = TestBackend::new(100, 30);
        let mut terminal = Terminal::new(backend).unwrap();

        let (input_tx, mut input_rx) = mpsc::unbounded_channel();
        let (_sync_tx, mut sync_rx) = mpsc::unbounded_channel();
        let (trig_tx, mut trig_rx) = mpsc::unbounded_channel();

        let send_key = |code: KeyCode| {
            input_tx
                .send(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
                .unwrap();
        };

        // 1. Add task via 'a', type "Live Task", Ctrl+s to submit TaskForm
        send_key(KeyCode::Char('a'));
        for c in "Live Task".chars() {
            send_key(KeyCode::Char(c));
        }
        input_tx
            .send(Event::Key(KeyEvent::new(
                KeyCode::Char('s'),
                KeyModifiers::CONTROL,
            )))
            .unwrap();

        // 2. Add project via 'N' on Inbox, type "LiveProj", Enter
        send_key(KeyCode::Char('N'));
        for c in "LiveProj".chars() {
            send_key(KeyCode::Char(c));
        }
        send_key(KeyCode::Enter);

        // 3. Open search via '/', type "Live", Esc restore
        send_key(KeyCode::Char('/'));
        for c in "Live".chars() {
            send_key(KeyCode::Char(c));
        }
        send_key(KeyCode::Esc);

        // 4. Quit via 'q'
        send_key(KeyCode::Char('q'));

        let mut ctx = LiveContext {
            conn: &mut conn,
            today_date: "2026-11-10".to_string(),
            authenticated: true,
        };
        let app = run_live_with_backend(
            &mut terminal,
            &config,
            &mut ctx,
            &mut input_rx,
            &mut sync_rx,
            Some(&trig_tx),
        )
        .await
        .unwrap();

        assert!(app.should_quit);
        assert!(!app.search_active);
        assert!(trig_rx.try_recv().is_ok());
        assert_eq!(queries::all_active_items(&conn).unwrap().len(), 1);
        assert!(queries::all_projects(&conn)
            .unwrap()
            .iter()
            .any(|p| p.name == "LiveProj"));
        let pending = CommandStore::new(&conn).pending(10).unwrap();
        let kinds: Vec<&str> = pending.iter().map(|c| c.kind.as_str()).collect();
        assert_eq!(kinds, vec!["item_add", "project_add"]);
    }
}
