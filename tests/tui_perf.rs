//! Performance gate tests for the TUI shell (`T-615`, EPIC-07):
//! - Cold start render (<500ms) at 5,000 items
//! - Key-to-render p95 (<16ms) across 200 actions at 5,000 items

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

use std::time::{Duration, Instant, SystemTime};

use config::Config;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use ui::keymap::{Action, Keymap};
use ui::status::StatusModel;
use ui::{App, FixtureData, Theme};

#[test]
fn cold_start_5k_items_under_500ms() {
    let data = FixtureData::generated(5_000);
    let config = Config::default();
    let keymap = Keymap::from_config(&config.keymap).unwrap();
    let theme = Theme::default();
    let status = StatusModel::new();
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();

    let start = Instant::now();
    let app = App::with_default_view(&data, config.default_view).unwrap();
    terminal
        .draw(|f| ui::view::render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme))
        .unwrap();
    let elapsed = start.elapsed();

    eprintln!("tui_perf: 5k cold start render = {elapsed:?}");
    assert!(
        elapsed < Duration::from_millis(500),
        "cold start render took {elapsed:?}, threshold is <500ms"
    );
}

#[test]
fn key_to_render_p95_5k_items_under_16ms() {
    let data = FixtureData::generated(5_000);
    let config = Config::default();
    let keymap = Keymap::from_config(&config.keymap).unwrap();
    let theme = Theme::default();
    let status = StatusModel::new();
    let backend = TestBackend::new(100, 30);
    let mut terminal = Terminal::new(backend).unwrap();
    let mut app = App::with_default_view(&data, config.default_view).unwrap();

    // Move focus to List (where 5k items live in Inbox / Today)
    app.apply(Action::Right, &data).unwrap();

    // Warm-up draw excluded from timings
    terminal
        .draw(|f| ui::view::render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme))
        .unwrap();

    let actions = [Action::Down, Action::Up, Action::Last, Action::First];
    let mut samples = Vec::with_capacity(200);
    for i in 0..200 {
        let act = actions[i % actions.len()];
        let t0 = Instant::now();
        app.apply(act, &data).unwrap();
        terminal
            .draw(|f| ui::view::render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme))
            .unwrap();
        samples.push(t0.elapsed());
    }

    samples.sort_unstable();
    let p50 = samples[samples.len() / 2];
    let p95 = samples[(samples.len() * 95) / 100];
    let max = samples[samples.len() - 1];

    eprintln!("tui_perf: 200 frames at 5k items -> p50={p50:?}, p95={p95:?}, max={max:?}");
    assert!(
        p95 < Duration::from_millis(16),
        "p95 key-to-render frame took {p95:?}, threshold is <16ms (p50={p50:?}, max={max:?})"
    );
}
