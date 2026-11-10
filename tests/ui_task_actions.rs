//! Integration tests for all 8 task actions, offline queue durability, `temp_id` resolution, and failed command surfacing (`T-712`, EPIC-08).

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
use api::model::{Id, Priority, Project};
use api::retry::{RecordingSleeper, RetryPolicy};
use auth::Token;
use db::{migrations, queries, resources};
use queue::store::CommandStore;
use queue::temp_ids::verify_no_temp_ids;
use rusqlite::Connection;
use serde_json::{json, Value};
use sync::cycle::SyncCycle;
use sync::SqliteSyncStore;
use ui::actions::{
    add, comment, complete, edit, errors, move_task, reorder, AddPayload, TaskAction,
};
use ui::data::{SqliteViewData, ViewData};
use ui::model::ViewId;
use ui::widgets::{ConfirmEvent, DestinationTarget};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn setup_db() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    migrations::run(&mut conn).unwrap();
    resources::project::upsert(
        &conn,
        &Project {
            id: Id::new("proj_inbox"),
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
    resources::project::upsert(
        &conn,
        &Project {
            id: Id::new("proj_work"),
            name: "Work".to_string(),
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
    conn
}

fn no_jitter_policy(max_attempts: u32, base_delay: Duration) -> RetryPolicy {
    let mut policy = RetryPolicy::new();
    policy.max_attempts = max_attempts;
    policy.base_delay = base_delay;
    policy.max_delay = Duration::from_secs(60);
    policy.jitter_fraction = 0.0;
    policy.with_jitter(|| 0.0)
}

struct RecordingSyncResponder {
    seen: Arc<Mutex<Vec<String>>>,
    id1: String,
    id2: String,
}

impl Respond for RecordingSyncResponder {
    fn respond(&self, request: &Request) -> ResponseTemplate {
        let body: Value = request.body_json().unwrap();
        let mut status = serde_json::Map::new();
        if let Some(cmds) = body.get("commands").and_then(Value::as_array) {
            let mut guard = self.seen.lock().unwrap();
            for c in cmds {
                let u = c["uuid"].as_str().unwrap().to_string();
                guard.push(u.clone());
                status.insert(u, json!("ok"));
            }
        }
        ResponseTemplate::new(200).set_body_json(json!({
            "sync_token": "tok_ui_actions_1",
            "full_sync": false,
            "sync_status": status,
            "temp_id_mapping": {
                self.id1.clone(): "real_item_1",
                self.id2.clone(): "real_item_2"
            },
            "items": [],
            "projects": [],
            "sections": [],
            "labels": [],
            "notes": [],
            "filters": []
        }))
    }
}

#[tokio::test]
async fn all_eight_actions_reflect_immediately_offline_and_flush_once_online() {
    let mut conn = setup_db();

    // 1. Add task 1 & task 2
    let r1 = add::submit_add(
        &mut conn,
        true,
        &TaskAction::Add(AddPayload {
            content: "Task One".to_string(),
            description: String::new(),
            project: Id::new("proj_inbox"),
            section: None,
            parent: None,
            due: None,
            priority: Priority::new(1).unwrap(),
            labels: Vec::new(),
        }),
    )
    .unwrap();
    let id1 = r1.target_id.clone();

    let r2 = add::submit_add(
        &mut conn,
        true,
        &TaskAction::Add(AddPayload {
            content: "Task Two".to_string(),
            description: String::new(),
            project: Id::new("proj_inbox"),
            section: None,
            parent: None,
            due: None,
            priority: Priority::new(1).unwrap(),
            labels: Vec::new(),
        }),
    )
    .unwrap();
    let id2 = r2.target_id.clone();

    // Immediate view visibility before any network request
    {
        let view = SqliteViewData::new(&conn, "2026-11-10");
        let rows = view.tasks(&ViewId::Project(Id::new("proj_inbox"))).unwrap();
        assert_eq!(rows.len(), 2);
    }

    // 2. Edit task 1 priority & due
    edit::quick_priority(&mut conn, true, id1.clone(), 4).unwrap();
    edit::quick_due(&mut conn, true, id1.clone(), "tomorrow").unwrap();

    // 3. Reorder task 2 above task 1
    reorder::reorder_task(&mut conn, true, &id2, reorder::ReorderDirection::Up)
        .unwrap()
        .expect("reordered");

    // 4. AddComment on task 1
    comment::submit_comment(&mut conn, true, id1.clone(), "First note").unwrap();

    // 5. Complete & Uncomplete task 2
    complete::toggle_complete(&mut conn, true, id2.clone(), false).unwrap();
    complete::toggle_complete(&mut conn, true, id2.clone(), true).unwrap();

    // 6. Move task 2 -> proj_work
    move_task::move_task(
        &mut conn,
        true,
        id2.clone(),
        DestinationTarget::Project(Id::new("proj_work")),
    )
    .unwrap();

    // 7. Delete: declining confirm does nothing; confirming enqueues item_delete
    let mut dlg = complete::open_delete_confirm("Task Two");
    let cancel = dlg.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Esc,
        crossterm::event::KeyModifiers::NONE,
    ));
    assert_eq!(cancel, ConfirmEvent::Cancelled);
    assert!(queries::item_by_id(&conn, &id2).unwrap().is_some());

    complete::confirm_delete(&mut conn, true, id2.clone()).unwrap();
    assert!(queries::item_by_id(&conn, &id2).unwrap().is_none());

    let pre_flush_cmds = CommandStore::new(&conn).pending(100).unwrap();
    assert_eq!(pre_flush_cmds.len(), 10);

    // Flush everything once against mock API and check zero duplicate UUIDs
    let server = MockServer::start().await;
    let seen_uuids = Arc::new(Mutex::new(Vec::new()));
    let seen_clone = Arc::clone(&seen_uuids);

    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(RecordingSyncResponder {
            seen: seen_clone,
            id1: id1.as_str().to_string(),
            id2: id2.as_str().to_string(),
        })
        .mount(&server)
        .await;

    let store = Arc::new(SqliteSyncStore::new(conn));
    let client = Arc::new(
        SyncClient::with_clock(
            Token::new("tok_fake_abc123".to_string()).unwrap(),
            &server.uri(),
            RecordingSleeper::new(),
        )
        .unwrap()
        .with_retry_policy(|| no_jitter_policy(1, Duration::from_millis(1))),
    );

    let cycle = SyncCycle::new(client, Arc::clone(&store), Arc::clone(&store));
    let outcome = cycle.run_once(1_700_000_000).await.unwrap();
    assert_eq!(outcome.flushed_commands, 10);

    // Every UUID seen once, zero remaining pending, zero temp_ids left
    let seen = seen_uuids.lock().unwrap().clone();
    assert_eq!(seen.len(), 10);
    let uniq: HashSet<String> = seen.into_iter().collect();
    assert_eq!(uniq.len(), 10);

    let conn_guard = store.shared_connection();
    let guard = conn_guard.lock().unwrap();
    assert_eq!(CommandStore::new(&guard).pending(10).unwrap().len(), 0);
    verify_no_temp_ids(
        &guard,
        &[id1.as_str().to_string(), id2.as_str().to_string()],
    )
    .unwrap();
}

#[tokio::test]
async fn failed_sync_status_surfaces_in_errors_view_and_clears_on_dismiss() {
    let mut conn = setup_db();

    let res = add::submit_add(
        &mut conn,
        true,
        &TaskAction::Add(AddPayload {
            content: "Doomed Task".to_string(),
            description: String::new(),
            project: Id::new("proj_inbox"),
            section: None,
            parent: None,
            due: None,
            priority: Priority::new(1).unwrap(),
            labels: Vec::new(),
        }),
    )
    .unwrap();
    let cmd_uuid = res.command_uuids[0].to_string();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(ResponseTemplate::new(200).set_body_json(json!({
            "sync_token": "tok_fail",
            "full_sync": false,
            "sync_status": {
                cmd_uuid.clone(): {
                    "error_code": 32,
                    "error": "Project not found"
                }
            },
            "temp_id_mapping": {},
            "items": [],
            "projects": [],
            "sections": [],
            "labels": [],
            "notes": [],
            "filters": []
        })))
        .mount(&server)
        .await;

    let store = Arc::new(SqliteSyncStore::new(conn));
    let client = Arc::new(
        SyncClient::with_clock(
            Token::new("tok_fake_abc123".to_string()).unwrap(),
            &server.uri(),
            RecordingSleeper::new(),
        )
        .unwrap()
        .with_retry_policy(|| no_jitter_policy(1, Duration::from_millis(1))),
    );

    let cycle = SyncCycle::new(client, Arc::clone(&store), Arc::clone(&store));
    cycle.run_once(1_700_000_000).await.unwrap();

    let conn_guard = store.shared_connection();
    let guard = conn_guard.lock().unwrap();
    assert_eq!(errors::failed_command_count(&guard).unwrap(), 1);
    let modal = errors::open_errors_modal(&guard).unwrap();
    assert_eq!(modal.items.len(), 1);
    assert!(modal.items[0].error.contains("Project not found"));

    let remaining = errors::dismiss_failed_command(&guard, &modal.items[0].uuid).unwrap();
    assert_eq!(remaining.len(), 0);
    assert_eq!(errors::failed_command_count(&guard).unwrap(), 0);
}
