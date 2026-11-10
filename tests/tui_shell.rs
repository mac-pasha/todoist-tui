//! Integration tests for TUI shell: navigation, snapshots, status states, resize guard, panic restore (`T-614`, EPIC-07).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

#[path = "../src/error.rs"]
mod error;

#[path = "../src/config/mod.rs"]
mod config;

#[path = "../src/log.rs"]
mod log;

#[path = "../src/auth/mod.rs"]
mod auth;

#[path = "../src/api/mod.rs"]
mod api;

#[path = "../src/db/mod.rs"]
mod db;

#[path = "../src/queue/mod.rs"]
mod queue;

#[path = "../src/sync/mod.rs"]
mod sync;

#[path = "../src/search/mod.rs"]
mod search;

#[path = "../src/filters/mod.rs"]
mod filters;

#[path = "../src/structure/mod.rs"]
mod structure;

#[path = "../src/ui/mod.rs"]
mod ui;

use std::io;
use std::sync::{Arc, Mutex};
use std::time::SystemTime;

use config::Config;
use crossterm::event::{Event, KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use tempfile::TempDir;
use tokio::sync::mpsc;
use ui::keymap::Keymap;
use ui::run::run_with_backend;
use ui::status::SyncEvent;
use ui::terminal::{TerminalGuard, TerminalOps, LEAVE_ALT_SCREEN_SEQ, SHOW_CURSOR_SEQ};
use ui::{App, FixtureData, Pane, Theme};

fn buffer_text(backend: &TestBackend) -> String {
    let area = *backend.buffer().area();
    let buf = backend.buffer();
    let mut out = String::new();
    for y in 0..area.height {
        for x in 0..area.width {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push('\n');
    }
    out
}

#[tokio::test]
async fn navigation_sequence_drives_app_state_and_renders_three_panes() {
    let config = Config::default();
    let data = FixtureData::sample();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    let (sync_tx, mut sync_rx) = mpsc::unbounded_channel();

    // Navigate down, switch to list, jump bottom/top, back to sidebar, quit
    for code in [
        KeyCode::Char('j'), // sidebar down -> Today
        KeyCode::Char('j'), // sidebar down -> Upcoming
        KeyCode::Char('k'), // sidebar up -> Today
        KeyCode::Char('l'), // focus -> List
        KeyCode::Char('G'), // jump bottom in List
        KeyCode::Char('g'), // jump top in List
        KeyCode::Char('h'), // focus -> Sidebar
        KeyCode::Char('q'), // quit
    ] {
        in_tx
            .send(Event::Key(KeyEvent::new(code, KeyModifiers::NONE)))
            .unwrap();
    }
    sync_tx
        .send(SyncEvent::Synced {
            at: Some(SystemTime::now()),
        })
        .unwrap();

    let app = run_with_backend(
        &mut terminal,
        &config,
        &data,
        &mut in_rx,
        &mut sync_rx,
        None,
    )
    .await
    .unwrap();

    assert!(app.should_quit);
    assert_eq!(app.focus(), Pane::Sidebar);
    assert_eq!(app.sidebar_index(), 1);
    assert_eq!(app.list_index(), 0);

    let screen = buffer_text(terminal.backend());
    assert!(screen.contains("Views"));
    assert!(screen.contains("Today"));
    assert!(screen.contains("Detail"));
}

#[tokio::test]
async fn resize_below_80x24_shows_guard_and_restores_above() {
    let config = Config::default();
    let data = FixtureData::sample();
    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();

    // Verify draw at 60x20 shows guard
    terminal.backend_mut().resize(60, 20);
    let app = App::new(&data).unwrap();
    let theme = Theme::default();
    terminal
        .draw(|f| ui::view::render(f, &app, &theme))
        .unwrap();
    let small_text = buffer_text(terminal.backend());
    assert!(small_text.contains("terminal too small: need 80x24, have 60x20"));

    // Resize back to 100x30 via event loop
    terminal.backend_mut().resize(100, 30);
    let (in_tx, mut in_rx) = mpsc::unbounded_channel();
    let (_sync_tx, mut sync_rx) = mpsc::unbounded_channel();
    in_tx.send(Event::Resize(100, 30)).unwrap();
    in_tx
        .send(Event::Key(KeyEvent::new(
            KeyCode::Char('q'),
            KeyModifiers::NONE,
        )))
        .unwrap();

    let _app = run_with_backend(
        &mut terminal,
        &config,
        &data,
        &mut in_rx,
        &mut sync_rx,
        None,
    )
    .await
    .unwrap();

    let large_text = buffer_text(terminal.backend());
    assert!(!large_text.contains("terminal too small"));
    assert!(large_text.contains("Navigation"));
    assert!(large_text.contains("Detail"));
}

#[tokio::test]
async fn keymap_override_from_temp_config_updates_help_overlay() {
    let tmp = TempDir::new().unwrap();
    let cfg_path = tmp.path().join("config.toml");
    std::fs::write(&cfg_path, "[keymap]\ndown = \"n\"\n").unwrap();

    let config = Config::load_from(&cfg_path).unwrap();
    let keymap = Keymap::from_config(&config.keymap).unwrap();
    let theme = Theme::default();

    let backend = TestBackend::new(80, 24);
    let mut terminal = Terminal::new(backend).unwrap();
    terminal
        .draw(|f| ui::view::help::render(f, f.area(), &keymap, &theme))
        .unwrap();

    let txt = buffer_text(terminal.backend());
    let down_row = txt
        .lines()
        .find(|l| l.contains("Move selection down"))
        .expect("down binding row");
    assert!(down_row.contains(" n "));
    assert!(!down_row.contains(" j "));
}

#[test]
fn panic_in_draw_path_still_restores_terminal_writer_and_ops() {
    #[derive(Clone, Default)]
    struct MockOps {
        disabled: Arc<Mutex<bool>>,
    }
    impl TerminalOps for MockOps {
        fn enable_raw_mode(&mut self) -> io::Result<()> {
            Ok(())
        }
        fn disable_raw_mode(&mut self) -> io::Result<()> {
            *self.disabled.lock().unwrap() = true;
            Ok(())
        }
    }

    #[derive(Clone, Default)]
    struct SharedBuf(Arc<Mutex<Vec<u8>>>);
    impl io::Write for SharedBuf {
        fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
            self.0.lock().unwrap().extend_from_slice(buf);
            Ok(buf.len())
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    let ops = MockOps::default();
    let disabled_flag = Arc::clone(&ops.disabled);
    let buf = SharedBuf::default();
    let raw_bytes = Arc::clone(&buf.0);

    let res = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let _guard = TerminalGuard::enter_with(buf, ops).unwrap();
        panic!("simulated draw panic");
    }));

    assert!(res.is_err());
    assert!(
        *disabled_flag.lock().unwrap(),
        "raw mode must be disabled on unwind"
    );
    let bytes = raw_bytes.lock().unwrap().clone();
    assert!(
        bytes
            .windows(LEAVE_ALT_SCREEN_SEQ.len())
            .any(|w| w == LEAVE_ALT_SCREEN_SEQ),
        "must emit LeaveAlternateScreen on drop/panic"
    );
    assert!(
        bytes
            .windows(SHOW_CURSOR_SEQ.len())
            .any(|w| w == SHOW_CURSOR_SEQ),
        "must emit ShowCursor on drop/panic"
    );
}
