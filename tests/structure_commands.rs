//! Integration tests for EPIC-09 structure commands (`project_*`, `section_*`, `label_*`),
//! `temp_id_mapping` resolution, cascade cleanup, and task cross-reference remapping (`T-810`).

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
use structure::{label, project, section};
use sync::cycle::SyncCycle;
use sync::SqliteSyncStore;
use ui::actions::{add, AddPayload, TaskAction};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn setup_db() -> Connection {
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

fn fast_policy() -> RetryPolicy {
    let mut p = RetryPolicy::new();
    p.max_attempts = 1;
    p.base_delay = Duration::from_millis(1);
    p.jitter_fraction = 0.0;
    p.with_jitter(|| 0.0)
}

#[tokio::test]
async fn project_section_label_commands_flush_and_resolve_temp_ids_with_task_remapping() {
    struct FixtureAckResponder {
        types: Arc<Mutex<Vec<String>>>,
        tmp_p: String,
        tmp_sub: String,
        tmp_sec: String,
        tmp_lbl: String,
        tmp_task: String,
    }

    impl Respond for FixtureAckResponder {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = request.body_json().unwrap();
            let mut sync_status = serde_json::Map::new();
            if let Some(cmds) = body.get("commands").and_then(Value::as_array) {
                let mut g = self.types.lock().unwrap();
                for c in cmds {
                    g.push(c["type"].as_str().unwrap().to_string());
                    let uuid = c["uuid"].as_str().unwrap().to_string();
                    sync_status.insert(uuid, json!("ok"));
                }
            }
            let raw = include_str!("fixtures/structure_sync.json");
            let mut resp: Value = serde_json::from_str(raw).unwrap();
            resp["sync_status"] = Value::Object(sync_status);
            resp["temp_id_mapping"] = json!({
                self.tmp_p.clone(): "srv_proj_101",
                self.tmp_sub.clone(): "srv_proj_102",
                self.tmp_sec.clone(): "srv_sec_201",
                self.tmp_lbl.clone(): "srv_lbl_301",
                self.tmp_task.clone(): "srv_task_401"
            });
            ResponseTemplate::new(200).set_body_json(resp)
        }
    }

    let mut conn = setup_db();

    // 1. Add parent project -> temp_id, sub-project -> temp_id, section -> temp_id, label -> temp_id
    let tmp_proj = project::add(&mut conn, "Roadmap").unwrap();
    let tmp_sub = project::add_child(&mut conn, "Q4 Milestones", &tmp_proj).unwrap();
    let tmp_sec = section::add(&mut conn, "In Progress", &tmp_proj).unwrap();
    let tmp_lbl = label::add(&mut conn, "backend").unwrap();

    // 2. Also add a task referencing tmp_proj & tmp_sec before syncing
    let task_res = add::submit_add(
        &mut conn,
        true,
        &TaskAction::Add(AddPayload {
            content: "Ship sync crate".to_string(),
            description: String::new(),
            project: tmp_proj.clone(),
            section: Some(tmp_sec.clone()),
            parent: None,
            due: None,
            priority: Priority::new(2).unwrap(),
            labels: vec!["backend".to_string()],
        }),
    )
    .unwrap();
    let tmp_task = task_res.target_id.clone();

    let recorded_types = Arc::new(Mutex::new(Vec::<String>::new()));
    let types_clone = Arc::clone(&recorded_types);
    let tmp_p_str = tmp_proj.as_str().to_string();
    let tmp_sub_str = tmp_sub.as_str().to_string();
    let tmp_sec_str = tmp_sec.as_str().to_string();
    let tmp_lbl_str = tmp_lbl.as_str().to_string();
    let tmp_task_str = tmp_task.as_str().to_string();

    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(FixtureAckResponder {
            types: types_clone,
            tmp_p: tmp_p_str.clone(),
            tmp_sub: tmp_sub_str.clone(),
            tmp_sec: tmp_sec_str.clone(),
            tmp_lbl: tmp_lbl_str.clone(),
            tmp_task: tmp_task_str.clone(),
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
        .with_retry_policy(fast_policy),
    );

    let cycle = SyncCycle::new(client, Arc::clone(&store), Arc::clone(&store));
    let outcome = cycle.run_once(1_700_000_000).await.unwrap();
    assert_eq!(outcome.flushed_commands, 5);

    let seen = recorded_types.lock().unwrap().clone();
    assert_eq!(
        seen,
        vec![
            "project_add",
            "project_add",
            "section_add",
            "label_add",
            "item_add"
        ]
    );

    let shared = store.shared_connection();
    let guard = shared.lock().unwrap();
    assert_eq!(CommandStore::new(&guard).pending(20).unwrap().len(), 0);
    verify_no_temp_ids(
        &guard,
        &[
            tmp_p_str,
            tmp_sub_str,
            tmp_sec_str,
            tmp_lbl_str,
            tmp_task_str,
        ],
    )
    .unwrap();

    // Task points to mapped server project/section IDs
    let items = queries::items_by_project(&guard, &Id::new("srv_proj_101")).unwrap();
    assert_eq!(items.len(), 1);
    assert_eq!(items[0].id, Id::new("srv_task_401"));
    assert_eq!(items[0].section_id, Some(Id::new("srv_sec_201")));
}

#[tokio::test]
async fn rename_archive_unarchive_reorder_and_delete_all_emit_expected_sync_commands() {
    let mut conn = setup_db();

    let p_id = project::add(&mut conn, "Alpha").unwrap();
    project::rename(&mut conn, &p_id, "Alpha Renamed").unwrap();
    project::archive(&mut conn, &p_id).unwrap();
    project::unarchive(&mut conn, &p_id).unwrap();

    let s1 = section::add(&mut conn, "Todo", &p_id).unwrap();
    let s2 = section::add(&mut conn, "Done", &p_id).unwrap();
    section::rename(&mut conn, &s1, "Backlog").unwrap();
    section::reorder(&mut conn, &s2, 0).unwrap();
    section::delete(&mut conn, &s1).unwrap();

    let lbl = label::add(&mut conn, "ops").unwrap();
    label::rename(&mut conn, &lbl, "devops").unwrap();
    label::delete(&mut conn, &lbl).unwrap();

    project::delete(&mut conn, &p_id).unwrap();

    let pending = CommandStore::new(&conn).pending(50).unwrap();
    let kinds: Vec<&str> = pending.iter().map(|c| c.kind.as_str()).collect();
    assert_eq!(
        kinds,
        vec![
            "project_add",
            "project_update",
            "project_archive",
            "project_unarchive",
            "section_add",
            "section_add",
            "section_update",
            "section_reorder",
            "section_delete",
            "label_add",
            "label_update",
            "label_delete",
            "project_delete",
        ]
    );
}
