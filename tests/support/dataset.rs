//! Deterministic 5k-item / 50-project synthetic dataset generator (`T-1103`, EPIC-12).
//!
//! Generates identical, fake-only projects and tasks both as a Sync API response JSON
//! payload and as a pre-populated SQLite cache database matching `src/db/schema.sql`.

#![allow(dead_code)]

use std::path::Path;

use rusqlite::{params, Connection};
use serde_json::{json, Value};

/// Number of projects in the deterministic benchmark dataset.
pub const DATASET_PROJECTS: usize = 50;

/// Number of items in the deterministic benchmark dataset.
pub const DATASET_ITEMS: usize = 5000;

const BASE_SCHEMA_SQL: &str = include_str!("../../src/db/schema.sql");
const COMMANDS_SCHEMA_SQL: &str = "
CREATE TABLE IF NOT EXISTS commands (
    uuid TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    args TEXT NOT NULL,
    temp_id TEXT NULL,
    state TEXT NOT NULL,
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    error TEXT NULL
);
CREATE INDEX IF NOT EXISTS idx_commands_state_created ON commands (state, created_at);
";

/// Deterministic LCG for reproducible fake attributes without extra crate dependencies.
struct DetRng(u64);

impl DetRng {
    fn next_u32(&mut self) -> u32 {
        self.0 = self
            .0
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1);
        (self.0 >> 32) as u32
    }
}

fn project_values() -> Vec<Value> {
    (0..DATASET_PROJECTS)
        .map(|i| {
            let name = if i == 0 {
                "Inbox".to_string()
            } else {
                format!("Project {i:02}")
            };
            json!({
                "id": format!("6X4VperfPrj{i:04}"),
                "name": name,
                "parent_id": Value::Null,
                "child_order": i64::try_from(i).unwrap_or(0),
                "color": "charcoal",
                "is_favorite": false,
                "is_archived": false,
                "is_deleted": false,
                "view_style": "list"
            })
        })
        .collect()
}

fn item_values() -> Vec<Value> {
    let mut rng = DetRng(0x5EED_CAFE_BEEF_1234);
    let verbs = ["Review", "Draft", "Refactor", "Deploy", "Verify", "Audit"];
    let nouns = [
        "pipeline",
        "fixture",
        "migration",
        "widget",
        "metrics",
        "docs",
    ];

    (0..DATASET_ITEMS)
        .map(|i| {
            let proj_idx = i % DATASET_PROJECTS;
            let verb = verbs[(rng.next_u32() as usize) % verbs.len()];
            let noun = nouns[(rng.next_u32() as usize) % nouns.len()];
            let prio = 1 + (i % 4);
            let due = if i % 5 == 0 {
                json!({
                    "date": "2026-11-10",
                    "string": "today",
                    "lang": "en",
                    "is_recurring": false,
                    "timezone": Value::Null
                })
            } else {
                Value::Null
            };

            json!({
                "id": format!("6X4VperfItm{i:05}"),
                "user_id": "6X4VperfUsr0001",
                "project_id": format!("6X4VperfPrj{proj_idx:04}"),
                "section_id": Value::Null,
                "parent_id": Value::Null,
                "child_order": i64::try_from(i + 1).unwrap_or(1),
                "content": format!("{verb} {noun} #{i:04}"),
                "description": format!("Synthetic task payload {i:04} for deterministic perf checks"),
                "priority": prio,
                "labels": [],
                "due": due,
                "deadline": Value::Null,
                "checked": false,
                "is_deleted": false,
                "added_at": "2026-11-01T09:00:00Z",
                "completed_at": Value::Null,
                "responsible_uid": Value::Null
            })
        })
        .collect()
}

/// Builds a complete synthetic `SyncResponse` JSON string containing 50 projects and 5000 items.
pub fn generate_sync_response_json() -> String {
    let projects = project_values();
    let items = item_values();
    let now_epoch = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(1_700_000_000, |d| d.as_secs());
    let _ = now_epoch;

    json!({
        "sync_token": "fakePerfSyncToken0001",
        "full_sync": true,
        "temp_id_mapping": {},
        "sync_status": {},
        "user": {
            "id": "6X4VperfUsr0001",
            "full_name": "Perf Harness",
            "email": "perf@example.invalid",
            "inbox_project_id": "6X4VperfPrj0000"
        },
        "projects": projects,
        "sections": [],
        "labels": [],
        "items": items,
        "notes": [],
        "filters": []
    })
    .to_string()
}

/// Populates a fresh SQLite cache database at `db_path` with 50 projects and 5000 items.
pub fn populate_cache_db(db_path: &Path) {
    if let Some(parent) = db_path.parent() {
        std::fs::create_dir_all(parent).unwrap();
    }
    let mut conn = Connection::open(db_path).unwrap();
    conn.execute_batch(BASE_SCHEMA_SQL).unwrap();
    conn.execute_batch(COMMANDS_SCHEMA_SQL).unwrap();
    conn.pragma_update(None, "user_version", 2).unwrap();

    let projects = project_values();
    let items = item_values();

    let tx = conn.transaction().unwrap();
    tx.execute(
        "INSERT OR REPLACE INTO user (id, full_name, email, raw_json) VALUES (?1, ?2, ?3, ?4)",
        params![
            "6X4VperfUsr0001",
            "Perf Harness",
            "perf@example.invalid",
            json!({
                "id": "6X4VperfUsr0001",
                "full_name": "Perf Harness",
                "email": "perf@example.invalid",
                "inbox_project_id": "6X4VperfPrj0000"
            })
            .to_string(),
        ],
    )
    .unwrap();

    tx.execute(
        "INSERT OR REPLACE INTO meta (key, value) VALUES ('sync_token', 'fakePerfSyncToken0001')",
        [],
    )
    .unwrap();

    for p in &projects {
        tx.execute(
            "INSERT OR REPLACE INTO projects (id, name, parent_id, child_order, color, is_archived, is_deleted, is_favorite, raw_json)
             VALUES (?1, ?2, NULL, ?3, ?4, 0, 0, 0, ?5)",
            params![
                p["id"].as_str().unwrap(),
                p["name"].as_str().unwrap(),
                p["child_order"].as_i64().unwrap(),
                p["color"].as_str().unwrap(),
                p.to_string(),
            ],
        )
        .unwrap();
    }

    for it in &items {
        let due_date = it
            .get("due")
            .and_then(|d| d.get("date"))
            .and_then(Value::as_str);
        tx.execute(
            "INSERT OR REPLACE INTO items (id, project_id, section_id, parent_id, content, description, child_order, checked, is_deleted, priority, due_date, labels, added_at, raw_json)
             VALUES (?1, ?2, NULL, NULL, ?3, ?4, ?5, 0, 0, ?6, ?7, '[]', ?8, ?9)",
            params![
                it["id"].as_str().unwrap(),
                it["project_id"].as_str().unwrap(),
                it["content"].as_str().unwrap(),
                it["description"].as_str().unwrap(),
                it["child_order"].as_i64().unwrap(),
                it["priority"].as_i64().unwrap(),
                due_date,
                it["added_at"].as_str().unwrap(),
                it.to_string(),
            ],
        )
        .unwrap();
    }

    tx.commit().unwrap();
}
