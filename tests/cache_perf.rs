//! Integration performance test for the 5,000-item SQLite cache query bound
//! (`T-311`, `EPIC-04`).
//!
//! Guards the `EPIC-04` acceptance criterion:
//! > "5k items: today / project tasks query each <50ms"
//!
//! ## Benchmark & bound policy
//!
//! * **Dataset**: 5,000 generated fake items (`6X4VfakeItm0000` ..
//!   `6X4VfakeItm4999`) distributed across 25 projects (`6X4VfakePrj0000` ..
//!   `6X4VfakePrj0024`, 200 items per project) with scrambled `child_order`
//!   values (including ties to exercise the `id ASC` tiebreak) and varied
//!   `due_date` values (overdue, due today as date, due today as datetime,
//!   upcoming, and `None`), plus a subset of `checked: true` items.
//! * **Measurement**: Seeding runs inside a single [`apply_sync`] transaction
//!   before any timing starts. Each query (`queries::today` and
//!   `queries::items_by_project`) executes 1 warm-up run followed by 5 timed
//!   runs; the median (3rd of 5 sorted durations) must be strictly under
//!   `50ms` (`MAX_MEDIAN_DURATION`), even in unoptimised debug builds (`cargo
//!   test --all`).
//! * **No `#[ignore]`**: Indexed SQLite queries plus `serde_json` decoding of
//!   the matching subset complete well within 50ms in debug builds, so this
//!   test runs by default in `cargo test --all` without `#[ignore]`.

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::uninlined_format_args
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
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::api::model::{Due, Id, Item, Priority, Project};
use crate::api::sync::SyncResponse;
use crate::db::apply::apply_sync;
use crate::db::{migrations, open, queries};

const TOTAL_PROJECTS: usize = 25;
const TOTAL_ITEMS: usize = 5_000;
const MEASURED_RUNS: usize = 5;
const TODAY_DATE: &str = "2024-05-06";
const MAX_MEDIAN_DURATION: Duration = Duration::from_millis(50);

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
            "todoist-tui-t311-{}-{label}-{nanos}-{n}",
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

fn generate_seed_response() -> SyncResponse {
    let projects: Vec<Project> = (0..TOTAL_PROJECTS)
        .map(|p| Project {
            id: Id::new(format!("6X4VfakePrj{p:04}")),
            name: format!("Fake Project {p:04}"),
            parent_id: None,
            child_order: i64::try_from(p + 1).unwrap(),
            color: "berry_red".to_string(),
            is_favorite: p == 0,
            is_archived: false,
            is_deleted: false,
            view_style: "list".to_string(),
        })
        .collect();

    let items: Vec<Item> = (0..TOTAL_ITEMS)
        .map(|i| {
            let project_idx = i % TOTAL_PROJECTS;
            // Scrambled child_order in 0..80 so every project of 200 items has
            // non-monotonic insertion order AND equal `child_order` collisions
            // that exercise the `id ASC` tiebreak.
            let child_order = i64::try_from((i * 37 + 11) % 80).unwrap();
            let priority_raw = u8::try_from((i % 4) + 1).unwrap();
            let checked = i % 11 == 0;

            let due_date_opt = match i % 20 {
                0 => Some("2024-05-01"),           // overdue
                1 => Some("2024-05-06"),           // due today (date)
                2 => Some("2024-05-06T14:30:00Z"), // due today (datetime)
                3..=6 => Some("2024-05-08"),       // upcoming
                7..=10 => Some("2024-05-15"),      // later upcoming
                _ => None,                         // unscheduled
            };

            let due = due_date_opt.map(|d| Due {
                date: d.to_string(),
                string: None,
                lang: Some("en".to_string()),
                is_recurring: i % 40 == 0,
                timezone: None,
            });

            Item {
                id: Id::new(format!("6X4VfakeItm{i:04}")),
                content: format!("Generated fake task {i:04}"),
                description: format!("Synthetic description for item {i:04}"),
                project_id: Id::new(format!("6X4VfakePrj{project_idx:04}")),
                section_id: None,
                parent_id: None,
                child_order,
                priority: Priority::new(priority_raw).unwrap(),
                labels: if i % 5 == 0 {
                    vec!["synthetic".to_string()]
                } else {
                    Vec::new()
                },
                due,
                deadline: None,
                checked,
                is_deleted: false,
                added_at: "2024-05-01T09:00:00Z".to_string(),
                completed_at: if checked {
                    Some("2024-05-05T12:00:00Z".to_string())
                } else {
                    None
                },
                user_id: Id::new("6X4VfakeUsr0001"),
                responsible_uid: None,
            }
        })
        .collect();

    SyncResponse {
        sync_token: "fakeSyncTokenPerf5000".to_string(),
        full_sync: true,
        full_sync_date_utc: Some("2024-05-01T09:00:00Z".to_string()),
        projects,
        items,
        ..SyncResponse::default()
    }
}

fn assert_non_empty_and_ordered(items: &[Item], query_name: &str) {
    assert!(
        !items.is_empty(),
        "{query_name} returned an empty result set"
    );

    let mut saw_tiebreak = false;
    for pair in items.windows(2) {
        let a = &pair[0];
        let b = &pair[1];
        assert!(
            !a.checked && !a.is_deleted && !b.checked && !b.is_deleted,
            "{query_name} must exclude checked and deleted items"
        );
        assert!(
            (a.child_order, a.id.as_str()) < (b.child_order, b.id.as_str()),
            "{query_name} ordering violation: ({}, {}) came before ({}, {})",
            a.child_order,
            a.id.as_str(),
            b.child_order,
            b.id.as_str()
        );
        if a.child_order == b.child_order {
            saw_tiebreak = true;
        }
    }
    assert!(
        saw_tiebreak,
        "{query_name} result set must include equal child_order ties broken by id ASC"
    );
}

fn median_of_five(mut durations: [Duration; MEASURED_RUNS]) -> Duration {
    durations.sort_unstable();
    durations[MEASURED_RUNS / 2]
}

#[test]
fn five_thousand_items_today_and_project_queries_stay_under_50ms_median() {
    let tmp = TempDb::new("perf-5k");
    let mut conn = tmp.open_migrated();

    // Seed phase (explicitly excluded from the measured query timings).
    let seed = generate_seed_response();
    let stats = apply_sync(&mut conn, &seed).expect("seed 5,000 items");
    assert_eq!(stats.projects.upserted, TOTAL_PROJECTS);
    assert_eq!(stats.items.upserted, TOTAL_ITEMS);

    let stored_count: i64 = conn
        .query_row("SELECT COUNT(*) FROM items", [], |r| r.get(0))
        .unwrap();
    assert_eq!(usize::try_from(stored_count).unwrap(), TOTAL_ITEMS);

    // --- 1. `queries::today` ---
    let warmup_today = queries::today(&conn, TODAY_DATE).expect("warm-up today query");
    assert_non_empty_and_ordered(&warmup_today, "today(warmup)");

    let mut today_runs = [Duration::ZERO; MEASURED_RUNS];
    for slot in &mut today_runs {
        let start = Instant::now();
        let result = queries::today(&conn, TODAY_DATE).expect("timed today query");
        *slot = start.elapsed();
        assert_eq!(result.len(), warmup_today.len());
    }
    let today_median = median_of_five(today_runs);
    assert!(
        today_median < MAX_MEDIAN_DURATION,
        "`queries::today` over 5,000 items exceeded {:?} budget: median was {:?} (runs: {:?})",
        MAX_MEDIAN_DURATION,
        today_median,
        today_runs
    );

    // --- 2. `queries::items_by_project` ---
    let target_project = Id::new("6X4VfakePrj0000");
    let warmup_proj =
        queries::items_by_project(&conn, &target_project).expect("warm-up items_by_project query");
    assert_non_empty_and_ordered(&warmup_proj, "items_by_project(warmup)");

    let mut proj_runs = [Duration::ZERO; MEASURED_RUNS];
    for slot in &mut proj_runs {
        let start = Instant::now();
        let result =
            queries::items_by_project(&conn, &target_project).expect("timed items_by_project");
        *slot = start.elapsed();
        assert_eq!(result.len(), warmup_proj.len());
    }
    let proj_median = median_of_five(proj_runs);
    assert!(
        proj_median < MAX_MEDIAN_DURATION,
        "`queries::items_by_project` over 5,000 items exceeded {:?} budget: median was {:?} (runs: {:?})",
        MAX_MEDIAN_DURATION,
        proj_median,
        proj_runs
    );
}
