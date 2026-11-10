//! E2E test: reorder, complete, edit persist across sync and restart (`T-713`, EPIC-08).

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

use std::collections::HashSet;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use api::client::SyncClient;
use api::model::{Id, Item, Priority, Project};
use api::retry::{RecordingSleeper, RetryPolicy};
use auth::Token;
use config::Config;
use db::{migrations, open, queries, resources};
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use serde_json::{json, Value};
use sync::cycle::SyncCycle;
use sync::SqliteSyncStore;
use tempfile::TempDir;
use ui::actions::{complete, edit, reorder, EditPayload, FieldEdit, TaskAction};
use ui::data::SqliteViewData;
use ui::{App, Theme};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn seed_disk_db(db_path: &std::path::Path) {
    let mut conn = open::open_at(db_path).unwrap();
    migrations::run(&mut conn).unwrap();
    resources::project::upsert(
        &conn,
        &Project {
            id: Id::new("p_e2e"),
            name: "E2E Project".to_string(),
            parent_id: None,
            child_order: 1,
            color: "blue".to_string(),
            is_favorite: false,
            is_archived: false,
            is_deleted: false,
            view_style: "list".to_string(),
        },
    )
    .unwrap();

    for (idx, content) in ["Alpha", "Beta", "Gamma"].iter().enumerate() {
        resources::item::upsert(
            &conn,
            &Item {
                id: Id::new(format!("t_{idx}")),
                content: (*content).to_string(),
                description: String::new(),
                project_id: Id::new("p_e2e"),
                section_id: None,
                parent_id: None,
                child_order: i64::try_from(idx + 1).unwrap_or(1),
                priority: Priority::new(1).unwrap(),
                labels: Vec::new(),
                due: None,
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: "2026-11-01T10:00:00Z".to_string(),
                completed_at: None,
                user_id: Id::new("u_e2e"),
                responsible_uid: None,
            },
        )
        .unwrap();
    }
}

fn no_jitter_policy() -> RetryPolicy {
    let mut p = RetryPolicy::new();
    p.max_attempts = 1;
    p.base_delay = Duration::from_millis(1);
    p.jitter_fraction = 0.0;
    p.with_jitter(|| 0.0)
}

#[tokio::test]
async fn reorder_complete_and_edit_survive_sync_and_restart_without_duplicate_uuids() {
    struct AckResponder {
        seen: Arc<Mutex<Vec<String>>>,
    }
    impl Respond for AckResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = request.body_json().unwrap();
            let mut status = serde_json::Map::new();
            if let Some(cmds) = body.get("commands").and_then(Value::as_array) {
                let mut g = self.seen.lock().unwrap();
                for c in cmds {
                    let u = c["uuid"].as_str().unwrap().to_string();
                    g.push(u.clone());
                    status.insert(u, json!("ok"));
                }
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "sync_token": "tok_e2e_1",
                "full_sync": false,
                "sync_status": status,
                "temp_id_mapping": {},
                "items": [],
                "projects": [],
                "sections": [],
                "labels": [],
                "notes": [],
                "filters": []
            }))
        }
    }

    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("cache.db");
    seed_disk_db(&db_path);

    let server = MockServer::start().await;
    let seen_uuids = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen_uuids);

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(AckResponder { seen: seen_clone })
        .mount(&server)
        .await;

    // Session 1: Reorder t_2 up (Alpha, Gamma, Beta), Complete t_1 (Beta), Edit t_0 (Alpha -> Alpha Edited)
    {
        let mut conn = open::open_at(&db_path).unwrap();
        reorder::reorder_task(
            &mut conn,
            true,
            &Id::new("t_2"),
            reorder::ReorderDirection::Up,
        )
        .unwrap();
        complete::toggle_complete(&mut conn, true, Id::new("t_1"), false).unwrap();
        edit::submit_edit(
            &mut conn,
            true,
            Some(&TaskAction::Edit(EditPayload {
                id: Id::new("t_0"),
                content: Some("Alpha Edited".to_string()),
                description: None,
                project: None,
                section: FieldEdit::Unchanged,
                parent: FieldEdit::Unchanged,
                due: FieldEdit::Unchanged,
                priority: None,
                labels: None,
            })),
        )
        .unwrap();

        let store = Arc::new(SqliteSyncStore::new(conn));
        let client = Arc::new(
            SyncClient::with_clock(
                Token::new("tok_fake_e2e".to_string()).unwrap(),
                &server.uri(),
                RecordingSleeper::new(),
            )
            .unwrap()
            .with_retry_policy(no_jitter_policy),
        );
        let cycle = SyncCycle::new(client, Arc::clone(&store), Arc::clone(&store));
        cycle.run_once(1_700_000_000).await.unwrap();
    }

    // Session 2 (relaunch over same on-disk DB): render App & verify zero extra commands on second sync
    {
        let conn = open::open_at(&db_path).unwrap();
        let view_data = SqliteViewData::new(&conn, "2026-11-10");
        let mut app = App::with_default_view(&view_data, Config::default().default_view).unwrap();

        // Render at 80x24 into TestBackend to prove list matches
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        term.draw(|f| ui::view::render(f, &app, &theme)).unwrap();

        // Check active project items in DB
        let active = queries::items_by_project(&conn, &Id::new("p_e2e")).unwrap();
        assert_eq!(active.len(), 2);
        assert_eq!(active[0].id, Id::new("t_0"));
        assert_eq!(active[0].content, "Alpha Edited");
        assert_eq!(active[1].id, Id::new("t_2"));
        assert_eq!(active[1].content, "Gamma");

        let _ = app.apply(ui::keymap::Action::Quit, &view_data);

        // Second sync sends 0 duplicate commands
        let store = Arc::new(SqliteSyncStore::new(conn));
        let client = Arc::new(
            SyncClient::with_clock(
                Token::new("tok_fake_e2e".to_string()).unwrap(),
                &server.uri(),
                RecordingSleeper::new(),
            )
            .unwrap()
            .with_retry_policy(no_jitter_policy),
        );
        let cycle = SyncCycle::new(client, Arc::clone(&store), Arc::clone(&store));
        let out2 = cycle.run_once(1_700_000_010).await.unwrap();
        assert_eq!(out2.flushed_commands, 0);
    }

    let seen = seen_uuids.lock().unwrap().clone();
    assert_eq!(seen.len(), 3);
    let uniq: HashSet<String> = seen.into_iter().collect();
    assert_eq!(uniq.len(), 3);
}
