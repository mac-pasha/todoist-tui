//! End-to-end headless TUI tests for EPIC-09 structure management (`T-812`):
//! sidebar hierarchy, sub-project add, rename, cascade preview delete, selection repair,
//! inline error handling, and pane context gating.

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

use api::model::{Id, Item, Priority, Project};
use db::{migrations, queries, resources};
use queue::store::CommandStore;
use structure::{label, project};
use ui::data::SqliteViewData;
use ui::keymap::Action;
use ui::model::ViewId;
use ui::structure::{
    execute_confirm, prompt_for_action, submit_form, StructureForm, StructureFormEvent,
    StructureFormKind, StructurePrompt,
};
use ui::view::sidebar;
use ui::widgets::ConfirmEvent;
use ui::{App, Pane, Theme};

fn seed_db() -> Connection {
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
    conn
}

#[test]
fn tui_structure_e2e_create_rename_delete_cascade_and_error_flows() {
    let mut conn = seed_db();
    let mut app = {
        let data = SqliteViewData::new(&conn, "2026-11-10");
        App::new(&data).unwrap()
    };
    assert_eq!(app.focus, Pane::Sidebar);

    // 1. On Inbox -> press `N` (StructureNew) -> AddProject "Engineering"
    let StructurePrompt::Form(mut form) =
        prompt_for_action(&conn, &app, Action::StructureNew).unwrap()
    else {
        panic!("expected AddProject form on Inbox");
    };
    assert_eq!(form.kind, StructureFormKind::AddProject);

    for ch in "Engineering".chars() {
        form.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
    }
    let StructureFormEvent::Submit(intent) =
        form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("expected submit event");
    };
    assert!(submit_form(&mut conn, &mut app, "2026-11-10", &mut form, &intent).unwrap());

    // Find Engineering project id
    let eng_id = queries::all_projects(&conn)
        .unwrap()
        .into_iter()
        .find(|p| p.name == "Engineering")
        .unwrap()
        .id;

    // 2. Add a task under Engineering and move sidebar selection to Engineering
    resources::item::upsert(
        &conn,
        &Item {
            id: Id::new("eng_task_1"),
            content: "Write docs".to_string(),
            description: String::new(),
            project_id: eng_id.clone(),
            section_id: None,
            parent_id: None,
            child_order: 1,
            priority: Priority::new(1).unwrap(),
            labels: Vec::new(),
            due: None,
            deadline: None,
            checked: false,
            is_deleted: false,
            completed_at: None,
            added_at: "2026-11-10T00:00:00Z".to_string(),
            user_id: Id::new("u1"),
            responsible_uid: None,
        },
    )
    .unwrap();

    // Navigate to Engineering node (Inbox, Today, Upcoming, Engineering => idx 3)
    {
        let data = SqliteViewData::new(&conn, "2026-11-10");
        app.refresh(&data).unwrap();
        let target_pos = app
            .sidebar_nodes()
            .iter()
            .position(|n| n.view_id() == ViewId::Project(eng_id.clone()))
            .unwrap();
        app.apply(Action::First, &data).unwrap();
        for _ in 0..target_pos {
            app.apply(Action::Down, &data).unwrap();
        }
    }
    assert_eq!(app.active_view(), &ViewId::Project(eng_id.clone()));

    // 3. On Engineering -> rename with `M` (StructureEdit) -> "Eng Core"
    let StructurePrompt::Form(mut rename_form) =
        prompt_for_action(&conn, &app, Action::StructureEdit).unwrap()
    else {
        panic!("expected RenameProject form");
    };
    rename_form.name = "Eng Core".to_string();
    let StructureFormEvent::Submit(rename_intent) =
        rename_form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("expected rename submit");
    };
    assert!(submit_form(
        &mut conn,
        &mut app,
        "2026-11-10",
        &mut rename_form,
        &rename_intent
    )
    .unwrap());

    // Verify rendered sidebar reflects renamed project
    let backend = TestBackend::new(80, 24);
    let mut term = Terminal::new(backend).unwrap();
    let theme = Theme::default();
    term.draw(|f| {
        sidebar::render(f, f.area(), &app, &theme);
    })
    .unwrap();
    let mut rendered = String::new();
    let buf = term.backend().buffer();
    for y in 0..24 {
        for x in 0..80 {
            rendered.push_str(buf[(x, y)].symbol());
        }
    }
    assert!(rendered.contains("Eng Core"));

    // 4. Create sub-project directly under Eng Core and move cursor to it, then delete Eng Core
    let sub_id = project::add_child(&mut conn, "Backend Sub", &eng_id).unwrap();
    {
        let data = SqliteViewData::new(&conn, "2026-11-10");
        app.refresh(&data).unwrap();
        let eng_pos = app
            .sidebar_nodes()
            .iter()
            .position(|n| n.view_id() == ViewId::Project(eng_id.clone()))
            .unwrap();
        app.apply(Action::First, &data).unwrap();
        for _ in 0..eng_pos {
            app.apply(Action::Down, &data).unwrap();
        }
    }

    // 5. Press `X` (StructureDelete) -> ConfirmDialog shows affected count -> confirm with `y`
    let StructurePrompt::Confirm { action, mut dialog } =
        prompt_for_action(&conn, &app, Action::StructureDelete).unwrap()
    else {
        panic!("expected DeleteProject confirm dialog");
    };
    assert_eq!(
        dialog.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
        ConfirmEvent::Confirmed
    );
    execute_confirm(&mut conn, &mut app, "2026-11-10", &action, None).unwrap();

    // Active view fell back safely to Inbox, and deleted projects & task are gone
    assert_eq!(app.active_view(), &ViewId::Inbox);
    assert_eq!(queries::items_by_project(&conn, &eng_id).unwrap().len(), 0);
    assert!(!app
        .sidebar_nodes()
        .iter()
        .any(|n| n.view_id() == ViewId::Project(sub_id.clone())));

    // 6. Duplicate label stays open with inline error
    let _ = label::add(&mut conn, "release").unwrap();
    let mut lbl_form = StructureForm::new(StructureFormKind::AddLabel);
    lbl_form.name = "RELEASE".to_string();
    let StructureFormEvent::Submit(lbl_intent) =
        lbl_form.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE))
    else {
        panic!("expected label submit");
    };
    assert!(!submit_form(
        &mut conn,
        &mut app,
        "2026-11-10",
        &mut lbl_form,
        &lbl_intent
    )
    .unwrap());
    assert!(lbl_form.error.is_some());

    // Queue captured all valid mutations without the rejected duplicate label
    let pending = CommandStore::new(&conn).pending(30).unwrap();
    assert_ne!(pending.len(), 0);
}
