//! Integration tests for EPIC-10 search & filter views backed by cached SQLite state (`T-910`).

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

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use rusqlite::Connection;
use serde::Deserialize;

use api::model::{Filter, Id, Item, Label, Project};
use db::{migrations, resources};
use queue::store::CommandStore;
use search::query::MatchedField;
use search::state::SessionScope;
use ui::data::SqliteViewData;
use ui::filters_view::{
    allow_action_in_view, evaluate_filter_state, is_mutating_action, FilterViewState,
};
use ui::keymap::{Action, Keymap};
use ui::model::ViewId;
use ui::search::{
    apply_accept, apply_cancel, load_search_items_from_db, SearchOverlay, SearchUiEvent,
};
use ui::view;
use ui::{App, Theme};

#[derive(Deserialize)]
struct SearchFixture {
    projects: Vec<Project>,
    labels: Vec<Label>,
    items: Vec<Item>,
    filters: Vec<Filter>,
}

fn seed_from_fixture() -> Connection {
    let raw = include_str!("fixtures/search_cache.json");
    let fix: SearchFixture = serde_json::from_str(raw).unwrap();

    let mut conn = Connection::open_in_memory().unwrap();
    migrations::run(&mut conn).unwrap();
    conn.execute(
        "INSERT INTO user (id, full_name, email, raw_json) VALUES (?1, ?2, ?3, ?4)",
        rusqlite::params![
            "u_fake_1",
            "Test User",
            "user@example.invalid",
            serde_json::json!({
                "id": "u_fake_1",
                "full_name": "Test User",
                "email": "user@example.invalid",
                "inbox_project_id": "p_inbox"
            })
            .to_string(),
        ],
    )
    .unwrap();
    for p in &fix.projects {
        resources::project::upsert(&conn, p).unwrap();
    }
    for l in &fix.labels {
        resources::label::upsert(&conn, l).unwrap();
    }
    for it in &fix.items {
        resources::item::upsert(&conn, it).unwrap();
    }
    for f in &fix.filters {
        resources::filter::upsert(&conn, f).unwrap();
    }
    conn
}

fn render_screen(app: &App) -> String {
    let backend = TestBackend::new(100, 30);
    let mut term = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    term.draw(|f| view::render(f, app, &theme)).unwrap();
    let area = term.size().unwrap();
    let list_rect =
        view::layout::split(ratatui::layout::Rect::new(0, 0, area.width, area.height)).list;
    let buf = term.backend().buffer();
    let mut out = String::new();
    for y in (list_rect.y + 1)..(list_rect.y + list_rect.height.saturating_sub(1)) {
        for x in (list_rect.x + 1)..(list_rect.x + list_rect.width.saturating_sub(1)) {
            out.push_str(buf[(x, y)].symbol());
        }
        out.push(' ');
    }
    out
}

#[test]
fn search_e2e_description_match_scope_toggle_esc_restore_and_enter_jump() {
    let conn = seed_from_fixture();
    let data = SqliteViewData::new(&conn, "2026-11-10");
    let mut app = App::new(&data).unwrap();
    let keymap = Keymap::default();

    // Start in Inbox with second item ("it_inbox_desc_only") selected
    assert_eq!(app.active_view(), &ViewId::Inbox);
    app.apply(Action::Right, &data).unwrap();
    app.apply(Action::Down, &data).unwrap();
    assert_eq!(
        app.selected_task().unwrap().id,
        Id::new("it_inbox_desc_only")
    );

    // Open search scoped to CurrentView (Inbox)
    let (items, project_names) = load_search_items_from_db(&conn).unwrap();
    app.apply(Action::OpenSearch, &data).unwrap();
    let mut overlay = SearchOverlay::open(&app, items, project_names);
    assert_eq!(overlay.scope(), SessionScope::CurrentView);

    // Type "telemetry" -> in Inbox only "it_inbox_desc_only" matches (via description!)
    for c in "telemetry".chars() {
        overlay.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &keymap);
    }
    assert_eq!(overlay.results().len(), 1);
    assert_eq!(overlay.results()[0].id, "it_inbox_desc_only");
    assert_eq!(overlay.results()[0].field, MatchedField::Description);

    // Toggle scope via Tab -> widens to All, returning both "it_backend_1" (content) and "it_inbox_desc_only" (desc)
    overlay.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &keymap);
    assert_eq!(overlay.scope(), SessionScope::All);
    assert_eq!(overlay.results().len(), 2);
    // Content match ranks above description match
    assert_eq!(overlay.results()[0].id, "it_backend_1");
    assert_eq!(overlay.results()[1].id, "it_inbox_desc_only");

    // Esc restores exact previous view (Inbox) and selected task (it_inbox_desc_only)
    let SearchUiEvent::Cancelled(snap) =
        overlay.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &keymap)
    else {
        panic!("expected Cancelled event on Esc");
    };
    apply_cancel(&mut app, &snap, &data).unwrap();
    assert_eq!(app.active_view(), &ViewId::Inbox);
    assert_eq!(
        app.selected_task().unwrap().id,
        Id::new("it_inbox_desc_only")
    );

    // Re-open search in All scope, pick "it_backend_1", press Enter -> jumps to Backend project with it_backend_1 selected
    let (items2, project_names2) = load_search_items_from_db(&conn).unwrap();
    let mut overlay2 = SearchOverlay::open(&app, items2, project_names2);
    overlay2.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &keymap);
    for c in "Optimize".chars() {
        overlay2.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &keymap);
    }
    let SearchUiEvent::JumpToTask {
        project_id,
        task_id,
    } = overlay2.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &keymap)
    else {
        panic!("expected JumpToTask on Enter");
    };
    apply_accept(&mut app, &project_id, &task_id, &data).unwrap();
    assert_eq!(app.active_view(), &ViewId::Project(Id::new("p_backend")));
    assert_eq!(app.selected_task().unwrap().id, Id::new("it_backend_1"));
}

#[test]
fn saved_filter_supported_empty_unsupported_and_readonly_queue_guard() {
    let conn = seed_from_fixture();
    let data = SqliteViewData::new(&conn, "2026-11-10");
    let mut app = App::new(&data).unwrap();

    // 1. Supported filter ("p1 & @urgent") -> matches it_inbox_1 and it_backend_1 with zero network I/O
    app.select_view_and_task(&ViewId::Filter(Id::new("flt_urgent_p1")), None, &data)
        .unwrap();
    let ids: Vec<&str> = app.tasks().iter().map(|t| t.id.as_str()).collect();
    assert_eq!(ids.len(), 2);
    assert!(ids.contains(&"it_inbox_1"));
    assert!(ids.contains(&"it_backend_1"));

    // Pressing every mutating key while Filter view is focused is rejected & leaves queue empty
    let keymap = Keymap::default();
    for (_, action, _) in keymap.bindings() {
        if is_mutating_action(action) {
            assert!(!allow_action_in_view(app.active_view(), action));
        }
    }
    assert_eq!(CommandStore::new(&conn).pending(20).unwrap().len(), 0);

    // 2. Empty supported filter ("#Backend & p4") -> explicit empty state
    app.select_view_and_task(&ViewId::Filter(Id::new("flt_empty")), None, &data)
        .unwrap();
    assert_eq!(evaluate_filter_state(&app), FilterViewState::Empty);
    assert!(render_screen(&app).contains("no matching tasks"));

    // 3. Unsupported filter ("assigned to: me") -> explicit unsupported banner + zero rows
    app.select_view_and_task(&ViewId::Filter(Id::new("flt_server_only")), None, &data)
        .unwrap();
    assert!(matches!(
        evaluate_filter_state(&app),
        FilterViewState::Unsupported { .. }
    ));
    assert_eq!(app.tasks().len(), 0);
    let screen: String = render_screen(&app)
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ");
    assert!(screen.contains("server-defined filter - cannot be evaluated locally"));
}
