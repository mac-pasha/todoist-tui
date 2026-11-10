//! Integration tests for full-then-incremental Sync API fixture application
//! against the SQLite cache (`T-310`, `EPIC-04`).
//!
//! Exercises [`crate::db::open::open_at`], [`crate::db::migrations::run`],
//! [`crate::db::apply::apply_sync`], [`crate::db::meta`] and
//! [`crate::db::queries`] end to end using the committed
//! `tests/fixtures/sync_full.json` and `tests/fixtures/sync_incremental.json`
//! fixtures.
//!
//! All fixture data is fake: every id starts with `6X4Vfake`, the only email
//! is `user@example.invalid`, and sync tokens start with `fake`.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines
)]

#[path = "../src/error.rs"]
mod error;

#[path = "../src/config/paths.rs"]
mod paths;

#[path = "../src/api/model/mod.rs"]
mod model;

#[path = "../src/api/sync.rs"]
mod sync;

#[path = "../src/db/apply.rs"]
mod db_apply;

#[path = "../src/db/meta.rs"]
mod db_meta;

#[path = "../src/db/migrations.rs"]
mod db_migrations;

#[path = "../src/db/open.rs"]
mod db_open;

#[path = "../src/db/queries.rs"]
mod db_queries;

#[path = "../src/db/resources/mod.rs"]
mod db_resources;

#[allow(unused_imports)]
mod config {
    pub(crate) use crate::paths;

    pub(crate) mod test_support {
        pub(crate) static ENV_LOCK: std::sync::Mutex<()> = std::sync::Mutex::new(());
    }
}

#[allow(unused_imports)]
mod api {
    pub(crate) use crate::model;
    pub(crate) use crate::sync;
    pub(crate) use crate::sync::{
        CommandError, CommandStatus, SyncRequest, SyncResponse, RESOURCE_TYPES,
    };
}

#[allow(dead_code, unused_imports)]
mod db {
    pub(crate) use crate::db_apply as apply;
    pub(crate) use crate::db_apply::{apply_sync, ApplyStats, ResourceCounts};
    pub(crate) use crate::db_meta as meta;
    pub(crate) use crate::db_migrations as migrations;
    pub(crate) use crate::db_open as open;
    pub(crate) use crate::db_queries as queries;
    pub(crate) use crate::db_resources as resources;
}

use std::fs;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;
use serde_json::Value;

use crate::api::model::{Id, Item, Priority};
use crate::api::sync::SyncResponse;
use crate::db::apply::{apply_sync, ResourceCounts};
use crate::db::{meta, migrations, open, queries, resources};

static COUNTER: AtomicU64 = AtomicU64::new(0);

/// Guard that creates a unique temporary directory for an on-disk SQLite
/// database and removes it on drop. Follows the same `std::env::temp_dir()` +
/// pid + atomic counter pattern as `src/config/paths.rs` and `src/db/open.rs`.
struct TempDb {
    dir: PathBuf,
    path: PathBuf,
}

impl TempDb {
    fn new(label: &str) -> Self {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!(
            "todoist-tui-t310-{}-{label}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.db");
        Self { dir, path }
    }

    fn path(&self) -> &Path {
        &self.path
    }

    fn open_migrated(&self) -> Connection {
        let mut conn = open::open_at(&self.path).expect("open temp db");
        migrations::run(&mut conn).expect("run schema migrations");
        conn
    }
}

impl Drop for TempDb {
    fn drop(&mut self) {
        let _ = fs::remove_dir_all(&self.dir);
    }
}

fn full_fixture_text() -> &'static str {
    include_str!("fixtures/sync_full.json")
}

fn incremental_fixture_text() -> &'static str {
    include_str!("fixtures/sync_incremental.json")
}

fn decode_full() -> SyncResponse {
    serde_json::from_str(full_fixture_text()).expect("sync_full.json must decode")
}

fn decode_incremental() -> SyncResponse {
    serde_json::from_str(incremental_fixture_text()).expect("sync_incremental.json must decode")
}

/// Captures a deterministic byte-for-row snapshot of every table in the cache
/// database so tests can assert that a failed apply leaves the database
/// completely unchanged.
fn snapshot_database(conn: &Connection) -> Vec<(String, Vec<String>)> {
    let tables = [
        ("projects", "SELECT id, name, IFNULL(parent_id, ''), child_order, IFNULL(color, ''), is_archived, is_deleted, is_favorite, raw_json FROM projects ORDER BY id"),
        ("sections", "SELECT id, project_id, name, section_order, is_archived, is_deleted, raw_json FROM sections ORDER BY id"),
        ("items", "SELECT id, IFNULL(project_id, ''), IFNULL(section_id, ''), IFNULL(parent_id, ''), content, IFNULL(description, ''), child_order, checked, is_deleted, priority, IFNULL(due_date, ''), labels, IFNULL(added_at, ''), raw_json FROM items ORDER BY id"),
        ("labels", "SELECT id, name, IFNULL(color, ''), item_order, is_deleted, is_favorite, raw_json FROM labels ORDER BY id"),
        ("notes", "SELECT id, item_id, content, IFNULL(posted_at, ''), is_deleted, raw_json FROM notes ORDER BY id"),
        ("filters", "SELECT id, name, query, item_order, is_deleted, is_favorite, raw_json FROM filters ORDER BY id"),
        ("user", "SELECT id, IFNULL(full_name, ''), IFNULL(email, ''), raw_json FROM user ORDER BY id"),
        ("day_orders", "SELECT id, raw_json FROM day_orders ORDER BY id"),
        ("meta", "SELECT key, IFNULL(value, '') FROM meta ORDER BY key"),
    ];

    let mut dump = Vec::with_capacity(tables.len());
    for (name, sql) in tables {
        let mut stmt = conn.prepare(sql).unwrap();
        let col_count = stmt.column_count();
        let rows = stmt
            .query_map([], |row| {
                let mut cols = Vec::with_capacity(col_count);
                for idx in 0..col_count {
                    let val: rusqlite::types::Value = row.get(idx)?;
                    cols.push(format!("{val:?}"));
                }
                Ok(cols.join("|"))
            })
            .unwrap()
            .collect::<rusqlite::Result<Vec<String>>>()
            .unwrap();
        dump.push((name.to_string(), rows));
    }
    dump
}

#[test]
fn incremental_fixture_references_full_fixture_ids_and_contains_only_fake_data() {
    let full_raw: Value = serde_json::from_str(full_fixture_text()).unwrap();
    let inc_raw: Value = serde_json::from_str(incremental_fixture_text()).unwrap();
    let inc_text = incremental_fixture_text();

    assert_eq!(inc_raw["full_sync"], Value::Bool(false));
    assert!(inc_raw.get("full_sync_date_utc").is_none());

    let token = inc_raw["sync_token"].as_str().unwrap();
    assert!(
        token.starts_with("fake"),
        "sync_token must be fake: {token}"
    );

    let full_project_ids: Vec<&str> = full_raw["projects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|p| p["id"].as_str().unwrap())
        .collect();
    let full_section_ids: Vec<&str> = full_raw["sections"]
        .as_array()
        .unwrap()
        .iter()
        .map(|s| s["id"].as_str().unwrap())
        .collect();
    let full_item_ids: Vec<&str> = full_raw["items"]
        .as_array()
        .unwrap()
        .iter()
        .map(|i| i["id"].as_str().unwrap())
        .collect();
    let full_user_id = full_raw["user"]["id"].as_str().unwrap();

    let inc_items = inc_raw["items"].as_array().unwrap();
    assert_eq!(
        inc_items.len(),
        3,
        "incremental fixture must have 1 updated, 1 deleted, and 1 new item"
    );

    // 1 updated existing item, 1 deleted existing item, 1 new item.
    let updated_existing = inc_items
        .iter()
        .filter(|i| {
            i["is_deleted"] == Value::Bool(false)
                && full_item_ids.contains(&i["id"].as_str().unwrap())
        })
        .count();
    let deleted_existing = inc_items
        .iter()
        .filter(|i| {
            i["is_deleted"] == Value::Bool(true)
                && full_item_ids.contains(&i["id"].as_str().unwrap())
        })
        .count();
    let new_items = inc_items
        .iter()
        .filter(|i| {
            i["is_deleted"] == Value::Bool(false)
                && !full_item_ids.contains(&i["id"].as_str().unwrap())
        })
        .count();
    assert_eq!(updated_existing, 1);
    assert_eq!(deleted_existing, 1);
    assert_eq!(new_items, 1);

    // Every project_id, section_id, parent_id and user_id references ids from sync_full.json,
    // and every id starts with `6X4Vfake`.
    for item in inc_items {
        let id = item["id"].as_str().unwrap();
        assert!(id.starts_with("6X4Vfake"), "non-fake id `{id}`");

        let project_id = item["project_id"].as_str().unwrap();
        assert!(
            full_project_ids.contains(&project_id),
            "unknown project_id `{project_id}`"
        );

        if let Some(sec_id) = item["section_id"].as_str() {
            assert!(
                full_section_ids.contains(&sec_id),
                "unknown section_id `{sec_id}`"
            );
        }
        if let Some(parent_id) = item["parent_id"].as_str() {
            assert!(
                full_item_ids.contains(&parent_id),
                "unknown parent_id `{parent_id}`"
            );
        }
        let user_id = item["user_id"].as_str().unwrap();
        assert_eq!(user_id, full_user_id);
    }

    for forbidden in [
        "Bearer ",
        "api.todoist.com",
        "todoist.com",
        "sk_live",
        "ghp_",
        "@",
    ] {
        assert!(
            !inc_text.contains(forbidden),
            "incremental fixture must not contain `{forbidden}`"
        );
    }
}

#[test]
fn full_then_incremental_apply_produces_exact_final_row_set() {
    let tmp = TempDb::new("full-then-inc");
    let mut conn = tmp.open_migrated();

    let full = decode_full();
    let full_stats = apply_sync(&mut conn, &full).expect("apply full fixture");
    assert!(full_stats.full_sync);
    assert_eq!(
        full_stats.items,
        ResourceCounts {
            upserted: 3,
            deleted: 1,
        }
    );

    let inc = decode_incremental();
    let inc_stats = apply_sync(&mut conn, &inc).expect("apply incremental fixture");
    assert!(!inc_stats.full_sync);
    assert_eq!(
        inc_stats.items,
        ResourceCounts {
            upserted: 2,
            deleted: 1,
        }
    );

    // Exact set of item ids remaining in the `items` table:
    // - 6X4VfakeItm0001 (updated)
    // - 6X4VfakeItm0003 (untouched checked item from full sync)
    // - 6X4VfakeItm0005 (new item from incremental sync)
    // - 6X4VfakeItm0002 was deleted by incremental sync
    // - 6X4VfakeItm0004 was deleted in full sync
    let mut stmt = conn
        .prepare("SELECT id FROM items ORDER BY id ASC")
        .unwrap();
    let stored_ids: Vec<String> = stmt
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<rusqlite::Result<Vec<String>>>()
        .unwrap();
    assert_eq!(
        stored_ids,
        vec![
            "6X4VfakeItm0001".to_string(),
            "6X4VfakeItm0003".to_string(),
            "6X4VfakeItm0005".to_string(),
        ]
    );

    // 1. Updated item has the new content, description and priority.
    let updated = resources::item::fetch(&conn, "6X4VfakeItm0001")
        .unwrap()
        .expect("updated item 6X4VfakeItm0001 must exist");
    assert_eq!(updated.content, "Buy oat milk (updated)");
    assert_eq!(updated.description, "Three cartons, barista edition");
    assert_eq!(updated.priority, Priority::new(3).unwrap());
    assert_eq!(updated.project_id, Id::new("6X4VfakePrj0001"));
    assert_eq!(updated.section_id, Some(Id::new("6X4VfakeSec0001")));
    assert!(!updated.checked);
    assert!(!updated.is_deleted);

    // 2. Deleted item is absent from both low-level fetch and view queries.
    assert!(resources::item::fetch(&conn, "6X4VfakeItm0002")
        .unwrap()
        .is_none());
    assert!(queries::item_by_id(&conn, &Id::new("6X4VfakeItm0002"))
        .unwrap()
        .is_none());
    assert!(resources::item::fetch(&conn, "6X4VfakeItm0004")
        .unwrap()
        .is_none());

    // 3. New item is present with all expected fields.
    let added = resources::item::fetch(&conn, "6X4VfakeItm0005")
        .unwrap()
        .expect("new item 6X4VfakeItm0005 must exist");
    assert_eq!(added.content, "Pick up dry cleaning");
    assert_eq!(added.description, "Ticket on kitchen counter");
    assert_eq!(added.project_id, Id::new("6X4VfakePrj0001"));
    assert_eq!(added.section_id, Some(Id::new("6X4VfakeSec0001")));
    assert_eq!(added.child_order, 3);
    assert_eq!(added.priority, Priority::new(2).unwrap());
    assert_eq!(
        added.due.as_ref().map(|d| d.date.as_str()),
        Some("2024-05-06")
    );

    // 4. Untouched item (6X4VfakeItm0003) and other tables are unchanged.
    let untouched = resources::item::fetch(&conn, "6X4VfakeItm0003")
        .unwrap()
        .expect("untouched item 6X4VfakeItm0003 must remain");
    let expected_untouched = full
        .items
        .iter()
        .find(|i| i.id.as_str() == "6X4VfakeItm0003")
        .unwrap();
    assert_eq!(&untouched, expected_untouched);

    assert_eq!(queries::all_projects(&conn).unwrap().len(), 2);
    assert_eq!(queries::all_sections(&conn).unwrap().len(), 1);
    assert_eq!(queries::all_labels(&conn).unwrap().len(), 2);
    assert_eq!(queries::all_filters(&conn).unwrap().len(), 1);
    assert_eq!(
        queries::notes_for_item(&conn, &Id::new("6X4VfakeItm0001"))
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn sync_token_round_trips_across_connection_close_and_reopen() {
    let tmp = TempDb::new("token-restart");

    {
        let mut conn = tmp.open_migrated();
        assert_eq!(meta::sync_token(&conn).unwrap(), None);

        let full = decode_full();
        apply_sync(&mut conn, &full).unwrap();
        assert_eq!(
            meta::sync_token(&conn).unwrap().as_deref(),
            Some("fakeSyncTokenForT208Fixture0001")
        );

        let inc = decode_incremental();
        apply_sync(&mut conn, &inc).unwrap();
        assert_eq!(
            meta::sync_token(&conn).unwrap().as_deref(),
            Some("fakeSyncTokenIncremental0002")
        );
        // Connection dropped at end of scope, simulating process exit.
    }

    // Reopen the exact same on-disk database path and read the cursor back.
    let mut reopened = open::open_at(tmp.path()).expect("reopen existing db");
    migrations::run(&mut reopened).expect("idempotent migrations on reopen");
    assert_eq!(
        meta::sync_token(&reopened).unwrap().as_deref(),
        Some("fakeSyncTokenIncremental0002")
    );
    assert_eq!(
        meta::full_sync_date_utc(&reopened).unwrap().as_deref(),
        Some("2024-05-01T09:00:00Z")
    );
    assert!(meta::last_sync_at(&reopened).unwrap().is_some());
}

#[test]
fn malformed_delta_rolls_back_and_leaves_database_byte_for_row_identical() {
    let tmp = TempDb::new("rollback");
    let mut conn = tmp.open_migrated();

    // Seed with the full sync fixture.
    let full = decode_full();
    apply_sync(&mut conn, &full).expect("apply full sync");

    // Guard on the items table that rejects a malformed/empty content payload
    // injected in-test *after* earlier items in the same incremental delta
    // have already been upserted/deleted inside the transaction.
    conn.execute_batch(
        "CREATE TRIGGER reject_empty_item_content
         BEFORE INSERT ON items
         WHEN length(trim(NEW.content)) = 0
         BEGIN
             SELECT RAISE(ABORT, 'malformed item payload: empty content');
         END;",
    )
    .unwrap();

    let pre_snapshot = snapshot_database(&conn);
    let pre_token = meta::sync_token(&conn).unwrap();

    // 1. Mutate the incremental fixture JSON into a wire-invalid payload
    // (out-of-range priority = 99) and verify decoding fails before touching DB.
    let mut bad_json: Value = serde_json::from_str(incremental_fixture_text()).unwrap();
    bad_json["items"][0]["priority"] = Value::from(99);
    assert!(
        serde_json::from_value::<SyncResponse>(bad_json).is_err(),
        "out-of-range priority in mutated fixture must fail deserialisation"
    );
    assert_eq!(snapshot_database(&conn), pre_snapshot);

    // 2. Mutate the decoded incremental delta so that after updating
    // `6X4VfakeItm0001` and deleting `6X4VfakeItm0002`, the third item has
    // an invalid empty content payload that aborts mid-transaction.
    let mut bad_delta = decode_incremental();
    bad_delta.sync_token = "fakeSyncTokenShouldRollBack9999".to_string();
    bad_delta.items[2].content = "   ".to_string();

    let res = apply_sync(&mut conn, &bad_delta);
    assert!(
        res.is_err(),
        "applying delta with invalid item must fail and roll back"
    );

    // Every table and meta row must be byte-for-row identical to pre_snapshot.
    let post_snapshot = snapshot_database(&conn);
    assert_eq!(post_snapshot, pre_snapshot);
    assert_eq!(meta::sync_token(&conn).unwrap(), pre_token);
    assert_eq!(
        resources::item::fetch(&conn, "6X4VfakeItm0001")
            .unwrap()
            .unwrap()
            .content,
        "Buy milk"
    );
    assert!(resources::item::fetch(&conn, "6X4VfakeItm0002")
        .unwrap()
        .is_some());
    assert!(resources::item::fetch(&conn, "6X4VfakeItm0005")
        .unwrap()
        .is_none());
}

#[test]
fn view_queries_return_expected_ids_and_order_after_full_and_incremental_apply() {
    let tmp = TempDb::new("views");
    let mut conn = tmp.open_migrated();

    apply_sync(&mut conn, &decode_full()).unwrap();
    apply_sync(&mut conn, &decode_incremental()).unwrap();

    let ids = |items: &[Item]| -> Vec<String> {
        items.iter().map(|i| i.id.as_str().to_string()).collect()
    };

    // Today ("2024-05-06"):
    // - 6X4VfakeItm0001 has due.date = "2024-05-06", child_order = 1, checked = false
    // - 6X4VfakeItm0005 has due.date = "2024-05-06", child_order = 3, checked = false
    // - 6X4VfakeItm0002 was deleted
    // - 6X4VfakeItm0003 has due = None and checked = true
    let today_items = queries::today(&conn, "2024-05-06").unwrap();
    assert_eq!(
        ids(&today_items),
        vec!["6X4VfakeItm0001".to_string(), "6X4VfakeItm0005".to_string(),]
    );
    assert_eq!(today_items[0].content, "Buy oat milk (updated)");
    assert_eq!(today_items[1].content, "Pick up dry cleaning");

    // Project 6X4VfakePrj0001 ("Home"):
    // Active items in child_order ASC, id ASC are 6X4VfakeItm0001 (order 1)
    // and 6X4VfakeItm0005 (order 3).
    let home_items = queries::items_by_project(&conn, &Id::new("6X4VfakePrj0001")).unwrap();
    assert_eq!(
        ids(&home_items),
        vec!["6X4VfakeItm0001".to_string(), "6X4VfakeItm0005".to_string(),]
    );

    // Project 6X4VfakePrj0002 ("Work"):
    // 6X4VfakeItm0003 is checked and 6X4VfakeItm0004 is deleted, so active
    // project view is empty while `item_by_id` still finds 6X4VfakeItm0003.
    let work_items = queries::items_by_project(&conn, &Id::new("6X4VfakePrj0002")).unwrap();
    assert_eq!(work_items, Vec::<Item>::new());
    assert!(queries::item_by_id(&conn, &Id::new("6X4VfakeItm0003"))
        .unwrap()
        .is_some());
}
