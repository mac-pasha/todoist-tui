//! Integration tests for completed-item (`checked: true`) cache retention
//! lifetime (`T-312`, `EPIC-04`, `REQ-001` D3 / Open Question 3).
//!
//! ## Retention window contract (`REQ-001` D3)
//!
//! In `v1`, completing a task (`item_complete` / `checked: true`) does **not**
//! delete the row from the local SQLite `items` table:
//!
//! 1. **Transition to `checked: true`**: When an incremental Sync API delta
//!    marks an item `checked: true`, [`apply_sync`] upserts the row with
//!    `checked = 1` (and preserves its full JSON in `raw_json`).
//! 2. **View exclusion vs. direct lookup**: Active-task view queries
//!    ([`queries::today`], [`queries::items_by_project`],
//!    [`queries::items_by_section`], [`queries::subtasks_of`],
//!    [`queries::upcoming`], [`queries::items_by_label`], [`queries::inbox`])
//!    filter on `checked = 0 AND is_deleted = 0`, hiding completed tasks from
//!    normal lists. However, [`queries::item_by_id`] only filters on
//!    `is_deleted = 0`, so `uncomplete` (`FR-27`, `EPIC-08`) can still read
//!    the completed item back by id to undo recent completions.
//! 3. **Retention lifetime**: A `checked: true` item stays in the local cache
//!    across arbitrarily many incremental syncs until either:
//!    * a subsequent incremental delta explicitly marks that item
//!      `is_deleted: true` (which deletes the row), **or**
//!    * a subsequent full sync (`full_sync: true` or `full_sync_date_utc`
//!      present) replaces all resource tables and omits the archived/old
//!      completed item from its snapshot.
//!
//! All fixture payloads in this file are in-test JSON literals with fake ids
//! (`6X4Vfake...`) and fake tokens (`fake...`).

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
use std::path::PathBuf;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::{params, Connection};
use serde_json::json;

use crate::api::model::Id;
use crate::api::sync::SyncResponse;
use crate::db::apply::apply_sync;
use crate::db::{migrations, open, queries};

static COUNTER: AtomicU64 = AtomicU64::new(0);

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
            "todoist-tui-t312-{}-{label}-{nanos}-{n}",
            std::process::id()
        ));
        fs::create_dir_all(&dir).unwrap();
        let path = dir.join("cache.db");
        Self { dir, path }
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

fn seed_full_sync_response() -> SyncResponse {
    serde_json::from_value(json!({
        "sync_token": "fakeSyncTokenRetention0001",
        "full_sync": true,
        "full_sync_date_utc": "2024-05-01T09:00:00Z",
        "projects": [
            {
                "id": "6X4VfakePrj0001",
                "name": "Home",
                "parent_id": null,
                "child_order": 1,
                "color": "berry_red",
                "is_favorite": false,
                "is_archived": false,
                "is_deleted": false,
                "view_style": "list"
            }
        ],
        "items": [
            {
                "id": "6X4VfakeItm0001",
                "user_id": "6X4VfakeUsr0001",
                "project_id": "6X4VfakePrj0001",
                "section_id": null,
                "parent_id": null,
                "child_order": 1,
                "content": "Water the ferns",
                "description": "Living room and balcony",
                "priority": 2,
                "labels": ["home"],
                "due": {
                    "date": "2024-05-06",
                    "string": "May 6",
                    "lang": "en",
                    "is_recurring": false,
                    "timezone": null
                },
                "deadline": null,
                "checked": false,
                "is_deleted": false,
                "added_at": "2024-05-01T08:00:00Z",
                "completed_at": null,
                "responsible_uid": null
            },
            {
                "id": "6X4VfakeItm0002",
                "user_id": "6X4VfakeUsr0001",
                "project_id": "6X4VfakePrj0001",
                "section_id": null,
                "parent_id": null,
                "child_order": 2,
                "content": "Sweep the porch",
                "description": "",
                "priority": 1,
                "labels": ["home"],
                "due": {
                    "date": "2024-05-06",
                    "string": "May 6",
                    "lang": "en",
                    "is_recurring": false,
                    "timezone": null
                },
                "deadline": null,
                "checked": false,
                "is_deleted": false,
                "added_at": "2024-05-01T08:30:00Z",
                "completed_at": null,
                "responsible_uid": null
            }
        ]
    }))
    .unwrap()
}

fn complete_item_delta() -> SyncResponse {
    serde_json::from_value(json!({
        "sync_token": "fakeSyncTokenRetention0002",
        "full_sync": false,
        "items": [
            {
                "id": "6X4VfakeItm0001",
                "user_id": "6X4VfakeUsr0001",
                "project_id": "6X4VfakePrj0001",
                "section_id": null,
                "parent_id": null,
                "child_order": 1,
                "content": "Water the ferns",
                "description": "Living room and balcony",
                "priority": 2,
                "labels": ["home"],
                "due": {
                    "date": "2024-05-06",
                    "string": "May 6",
                    "lang": "en",
                    "is_recurring": false,
                    "timezone": null
                },
                "deadline": null,
                "checked": true,
                "is_deleted": false,
                "added_at": "2024-05-01T08:00:00Z",
                "completed_at": "2024-05-06T10:15:00Z",
                "responsible_uid": null
            }
        ]
    }))
    .unwrap()
}

#[test]
fn transitioning_item_to_checked_retains_row_with_checked_one_and_hides_from_views_but_not_item_by_id(
) {
    let tmp = TempDb::new("checked-transition");
    let mut conn = tmp.open_migrated();

    apply_sync(&mut conn, &seed_full_sync_response()).unwrap();
    apply_sync(&mut conn, &complete_item_delta()).unwrap();

    // 1. Row remains in the SQLite `items` table with `checked = 1` and `is_deleted = 0`.
    let (checked_col, is_deleted_col): (i64, i64) = conn
        .query_row(
            "SELECT checked, is_deleted FROM items WHERE id = ?1",
            params!["6X4VfakeItm0001"],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .expect("completed item row must remain in `items` table");
    assert_eq!(checked_col, 1, "checked column must be 1");
    assert_eq!(is_deleted_col, 0, "is_deleted column must be 0");

    // 2. Absent from `today` and `items_by_project` active-task view queries.
    let today_items = queries::today(&conn, "2024-05-06").unwrap();
    assert_eq!(today_items.len(), 1);
    assert_eq!(today_items[0].id, Id::new("6X4VfakeItm0002"));

    let proj_items = queries::items_by_project(&conn, &Id::new("6X4VfakePrj0001")).unwrap();
    assert_eq!(proj_items.len(), 1);
    assert_eq!(proj_items[0].id, Id::new("6X4VfakeItm0002"));

    // 3. Still returned by `queries::item_by_id` so `uncomplete` (FR-27) can read it.
    let fetched = queries::item_by_id(&conn, &Id::new("6X4VfakeItm0001"))
        .unwrap()
        .expect("item_by_id must return checked item for undo/uncomplete");
    assert!(fetched.checked);
    assert!(!fetched.is_deleted);
    assert_eq!(fetched.content, "Water the ferns");
    assert_eq!(
        fetched.completed_at.as_deref(),
        Some("2024-05-06T10:15:00Z")
    );
}

#[test]
fn checked_item_survives_unrelated_deltas_and_is_only_removed_when_marked_is_deleted() {
    let tmp = TempDb::new("delete-removes");
    let mut conn = tmp.open_migrated();

    apply_sync(&mut conn, &seed_full_sync_response()).unwrap();
    apply_sync(&mut conn, &complete_item_delta()).unwrap();

    // Unrelated incremental delta that does not mention the completed item.
    let unrelated_delta: SyncResponse = serde_json::from_value(json!({
        "sync_token": "fakeSyncTokenRetention0003",
        "full_sync": false,
        "items": [
            {
                "id": "6X4VfakeItm0002",
                "user_id": "6X4VfakeUsr0001",
                "project_id": "6X4VfakePrj0001",
                "section_id": null,
                "parent_id": null,
                "child_order": 2,
                "content": "Sweep the front and back porch",
                "description": "",
                "priority": 1,
                "labels": ["home"],
                "due": {
                    "date": "2024-05-06",
                    "string": "May 6",
                    "lang": "en",
                    "is_recurring": false,
                    "timezone": null
                },
                "deadline": null,
                "checked": false,
                "is_deleted": false,
                "added_at": "2024-05-01T08:30:00Z",
                "completed_at": null,
                "responsible_uid": null
            }
        ]
    }))
    .unwrap();
    apply_sync(&mut conn, &unrelated_delta).unwrap();

    // Checked item is still retained in cache.
    assert!(
        queries::item_by_id(&conn, &Id::new("6X4VfakeItm0001"))
            .unwrap()
            .is_some(),
        "checked item must survive subsequent incremental deltas that do not delete it"
    );

    // Later delta explicitly marks the checked item `is_deleted: true`.
    let delete_delta: SyncResponse = serde_json::from_value(json!({
        "sync_token": "fakeSyncTokenRetention0004",
        "full_sync": false,
        "items": [
            {
                "id": "6X4VfakeItm0001",
                "user_id": "6X4VfakeUsr0001",
                "project_id": "6X4VfakePrj0001",
                "section_id": null,
                "parent_id": null,
                "child_order": 1,
                "content": "Water the ferns",
                "description": "Living room and balcony",
                "priority": 2,
                "labels": ["home"],
                "due": null,
                "deadline": null,
                "checked": true,
                "is_deleted": true,
                "added_at": "2024-05-01T08:00:00Z",
                "completed_at": "2024-05-06T10:15:00Z",
                "responsible_uid": null
            }
        ]
    }))
    .unwrap();
    let stats = apply_sync(&mut conn, &delete_delta).unwrap();
    assert_eq!(stats.items.deleted, 1);

    assert!(
        queries::item_by_id(&conn, &Id::new("6X4VfakeItm0001"))
            .unwrap()
            .is_none(),
        "item_by_id must return None once the item is deleted"
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE id = '6X4VfakeItm0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0, "row must be physically removed from `items`");
}

#[test]
fn subsequent_full_sync_omitting_checked_item_drops_it_via_replace_semantics() {
    let tmp = TempDb::new("full-sync-evicts");
    let mut conn = tmp.open_migrated();

    apply_sync(&mut conn, &seed_full_sync_response()).unwrap();
    apply_sync(&mut conn, &complete_item_delta()).unwrap();
    assert!(queries::item_by_id(&conn, &Id::new("6X4VfakeItm0001"))
        .unwrap()
        .is_some());

    // Later full sync whose snapshot no longer includes completed item 6X4VfakeItm0001.
    let later_full_sync: SyncResponse = serde_json::from_value(json!({
        "sync_token": "fakeSyncTokenRetention0005",
        "full_sync": true,
        "full_sync_date_utc": "2024-05-07T09:00:00Z",
        "projects": [
            {
                "id": "6X4VfakePrj0001",
                "name": "Home",
                "parent_id": null,
                "child_order": 1,
                "color": "berry_red",
                "is_favorite": false,
                "is_archived": false,
                "is_deleted": false,
                "view_style": "list"
            }
        ],
        "items": [
            {
                "id": "6X4VfakeItm0002",
                "user_id": "6X4VfakeUsr0001",
                "project_id": "6X4VfakePrj0001",
                "section_id": null,
                "parent_id": null,
                "child_order": 2,
                "content": "Sweep the porch",
                "description": "",
                "priority": 1,
                "labels": ["home"],
                "due": {
                    "date": "2024-05-06",
                    "string": "May 6",
                    "lang": "en",
                    "is_recurring": false,
                    "timezone": null
                },
                "deadline": null,
                "checked": false,
                "is_deleted": false,
                "added_at": "2024-05-01T08:30:00Z",
                "completed_at": null,
                "responsible_uid": null
            }
        ]
    }))
    .unwrap();

    let stats = apply_sync(&mut conn, &later_full_sync).unwrap();
    assert!(stats.full_sync);

    // Full-sync replace semantics purge the omitted checked item.
    assert!(
        queries::item_by_id(&conn, &Id::new("6X4VfakeItm0001"))
            .unwrap()
            .is_none(),
        "full sync omitting the checked item must drop it from the cache"
    );
    let count: i64 = conn
        .query_row(
            "SELECT COUNT(*) FROM items WHERE id = '6X4VfakeItm0001'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(count, 0);

    // Active item included in the new full sync remains present.
    assert!(queries::item_by_id(&conn, &Id::new("6X4VfakeItm0002"))
        .unwrap()
        .is_some());
}
