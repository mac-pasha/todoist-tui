//! Offline durability and restart recovery tests for EPIC-09 structure mutations (`T-811`).

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

use std::sync::Arc;
use std::time::Duration;

use api::client::SyncClient;
use api::model::{Id, Item, Priority, Project};
use api::retry::{RecordingSleeper, RetryPolicy};
use auth::Token;
use db::{migrations, open, resources};
use queue::store::CommandStore;
use serde_json::{json, Value};
use structure::{cascade, label, project, section};
use sync::cycle::SyncCycle;
use sync::SqliteSyncStore;
use tempfile::TempDir;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, Request, Respond, ResponseTemplate};

fn seed_disk_db(db_path: &std::path::Path) {
    let mut conn = open::open_at(db_path).unwrap();
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
}

fn no_jitter_policy() -> RetryPolicy {
    let mut p = RetryPolicy::new();
    p.max_attempts = 1;
    p.base_delay = Duration::from_millis(1);
    p.jitter_fraction = 0.0;
    p.with_jitter(|| 0.0)
}

#[tokio::test]
async fn offline_structure_mutations_persist_across_reopen_and_flush_on_reconnect() {
    struct OkAck;
    impl Respond for OkAck {
        fn respond(&self, request: &Request) -> ResponseTemplate {
            let body: Value = request.body_json().unwrap();
            let mut status = serde_json::Map::new();
            if let Some(cmds) = body.get("commands").and_then(Value::as_array) {
                for c in cmds {
                    status.insert(c["uuid"].as_str().unwrap().to_string(), json!("ok"));
                }
            }
            ResponseTemplate::new(200).set_body_json(json!({
                "sync_token": "tok_reconnected",
                "full_sync": false,
                "sync_status": Value::Object(status),
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

    let (proj_id, sub_id, sec_id, lbl_id);
    {
        let mut conn = open::open_at(&db_path).unwrap();
        proj_id = project::add(&mut conn, "OfflineProj").unwrap();
        sub_id = project::add_child(&mut conn, "OfflineSub", &proj_id).unwrap();
        sec_id = section::add(&mut conn, "OfflineSec", &proj_id).unwrap();
        lbl_id = label::add(&mut conn, "offline_lbl").unwrap();

        resources::item::upsert(
            &conn,
            &Item {
                id: Id::new("item_off_1"),
                content: "Offline Task".to_string(),
                description: String::new(),
                project_id: sub_id.clone(),
                section_id: Some(sec_id.clone()),
                parent_id: None,
                child_order: 1,
                priority: Priority::new(1).unwrap(),
                labels: vec!["offline_lbl".to_string()],
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

        // Simulate failed offline sync cycle (stopped port)
        let stopped = MockServer::start().await;
        let uri = stopped.uri();
        drop(stopped);

        let store = Arc::new(SqliteSyncStore::new(conn));
        let client = Arc::new(
            SyncClient::with_clock(
                Token::new("tok_fake_abc123".to_string()).unwrap(),
                &uri,
                RecordingSleeper::new(),
            )
            .unwrap()
            .with_retry_policy(no_jitter_policy),
        );
        let cycle = SyncCycle::new(client, Arc::clone(&store), Arc::clone(&store));
        assert!(cycle.run_once(1_700_000_000).await.is_err());

        let shared = store.shared_connection();
        let guard = shared.lock().unwrap();
        // All 4 commands must remain pending after failed offline attempt
        assert_eq!(CommandStore::new(&guard).pending(20).unwrap().len(), 4);
    }

    // Reopen cache from disk and verify optimistic hierarchy & queued commands survived
    let conn2 = open::open_at(&db_path).unwrap();
    assert_eq!(CommandStore::new(&conn2).pending(20).unwrap().len(), 4);
    let preview = cascade::preview_project_delete(&conn2, &proj_id).unwrap();
    assert_eq!(preview.projects, 2);
    assert_eq!(preview.sections, 1);
    assert_eq!(preview.items, 1);
    let _ = (sub_id, sec_id, lbl_id);

    // Server comes online -> flush
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(OkAck)
        .mount(&server)
        .await;

    let store2 = Arc::new(SqliteSyncStore::new(conn2));
    let client2 = Arc::new(
        SyncClient::with_clock(
            Token::new("tok_fake_abc123".to_string()).unwrap(),
            &server.uri(),
            RecordingSleeper::new(),
        )
        .unwrap()
        .with_retry_policy(no_jitter_policy),
    );
    let cycle2 = SyncCycle::new(client2, Arc::clone(&store2), Arc::clone(&store2));
    let outcome = cycle2.run_once(1_700_000_050).await.unwrap();
    assert_eq!(outcome.flushed_commands, 4);

    let shared2 = store2.shared_connection();
    let guard2 = shared2.lock().unwrap();
    assert_eq!(CommandStore::new(&guard2).pending(20).unwrap().len(), 0);
}
