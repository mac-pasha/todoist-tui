//! Performance regression gates against a deterministic 5k-item / 50-project dataset (`T-1103`, EPIC-12).
//!
//! These perf assertions are gated behind `#[ignore]` so default `cargo test --all` stays fast
//! and deterministic across debug builds and busy machines. Run in release mode via:
//! `cargo test --release --test perf_gates -- --ignored --nocapture`

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

mod support;

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
use std::time::{Duration, Instant, SystemTime};

use api::sync::{SyncRequest, SyncResponse};
use db::{migrations, open, queries};
use error::ApiError;
use ratatui::backend::TestBackend;
use ratatui::Terminal;
use support::dataset::{
    generate_sync_response_json, populate_cache_db, DATASET_ITEMS, DATASET_PROJECTS,
};
use sync::cycle::SyncCycle;
use sync::ports::{BoxFuture, SyncApi};
use sync::SqliteSyncStore;
use tempfile::TempDir;
use ui::data::SqliteViewData;
use ui::keymap::{Action, Keymap};
use ui::status::StatusModel;
use ui::view;
use ui::{App, Theme};
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

/// REQ-001 NFR: cold start (DB open + initial load + first rendered frame) < 500ms.
const MAX_COLD_START: Duration = Duration::from_millis(500);

/// REQ-001 NFR: input-to-render latency p95 over >= 100 simulated key actions < 16ms.
const MAX_INPUT_TO_RENDER_P95: Duration = Duration::from_millis(16);

/// REQ-001 NFR: mocked Sync API round trip p95 over >= 20 requests < 2s.
const MAX_SYNC_ROUNDTRIP_P95: Duration = Duration::from_secs(2);

fn p95(samples: &mut [Duration]) -> Duration {
    samples.sort_unstable();
    let idx = (samples.len() * 95).div_ceil(100).saturating_sub(1);
    samples[idx.min(samples.len() - 1)]
}

#[test]
fn dataset_fixture_generates_5k_items_and_50_projects() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("cache.db");
    populate_cache_db(&db_path);

    let conn = open::open_at(&db_path).unwrap();
    assert_eq!(
        queries::all_projects(&conn).unwrap().len(),
        DATASET_PROJECTS
    );
    assert_eq!(
        queries::all_active_items(&conn).unwrap().len(),
        DATASET_ITEMS
    );

    let json_payload = generate_sync_response_json();
    let val: serde_json::Value = serde_json::from_str(&json_payload).unwrap();
    assert_eq!(val["projects"].as_array().unwrap().len(), DATASET_PROJECTS);
    assert_eq!(val["items"].as_array().unwrap().len(), DATASET_ITEMS);
}

#[test]
#[ignore = "PERF gate: run with `cargo test --release --test perf_gates -- --ignored --nocapture`"]
fn perf_gate_cold_start_and_input_to_render_at_5k() {
    let tmp = TempDir::new().unwrap();
    let db_path = tmp.path().join("cache.db");
    populate_cache_db(&db_path);

    // Warm up filesystem page cache once
    {
        let conn = open::open_at(&db_path).unwrap();
        let data = SqliteViewData::new(&conn, "2026-11-10");
        let _ = App::new(&data).unwrap();
    }

    // Gate 1: Cold start (open DB + App::new + first frame draw) < 500ms
    let cold_t0 = Instant::now();
    let conn = open::open_at(&db_path).unwrap();
    let data = SqliteViewData::new(&conn, "2026-11-10");
    let mut app = App::new(&data).unwrap();
    let status = StatusModel::new();
    let keymap = Keymap::default();
    let theme = Theme::default();
    let backend = TestBackend::new(120, 40);
    let mut term = Terminal::new(backend).unwrap();
    term.draw(|f| view::render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme))
        .unwrap();
    let cold_elapsed = cold_t0.elapsed();
    println!(
        "PERF cold_start: {:?} (threshold: {:?})",
        cold_elapsed, MAX_COLD_START
    );
    assert!(
        cold_elapsed < MAX_COLD_START,
        "cold start {:?} exceeded {:?}",
        cold_elapsed,
        MAX_COLD_START
    );

    // Gate 2: Input-to-render p95 over 120 key events < 16ms
    let actions = [
        Action::Down,
        Action::Down,
        Action::Up,
        Action::Right,
        Action::Down,
        Action::Down,
        Action::Up,
        Action::Left,
    ];
    let mut samples = Vec::with_capacity(120);
    for i in 0..120 {
        let act = actions[i % actions.len()];
        let t0 = Instant::now();
        app.apply(act, &data).unwrap();
        term.draw(|f| view::render_frame(f, &app, &status, &keymap, SystemTime::now(), &theme))
            .unwrap();
        samples.push(t0.elapsed());
    }
    let measured_p95 = p95(&mut samples);
    println!(
        "PERF input_to_render p95 (n=120): {:?} (threshold: {:?})",
        measured_p95, MAX_INPUT_TO_RENDER_P95
    );
    assert!(
        measured_p95 < MAX_INPUT_TO_RENDER_P95,
        "input-to-render p95 {:?} exceeded {:?}",
        measured_p95,
        MAX_INPUT_TO_RENDER_P95
    );
}

struct LoopbackHttpSyncApi {
    client: reqwest::Client,
    url: String,
}

impl SyncApi for LoopbackHttpSyncApi {
    fn sync<'a>(&'a self, req: &'a SyncRequest) -> BoxFuture<'a, error::Result<SyncResponse>> {
        Box::pin(async move {
            let resp = self
                .client
                .post(&self.url)
                .json(req)
                .send()
                .await
                .map_err(|source| ApiError::Transport { source })?;
            let decoded: SyncResponse = resp
                .json()
                .await
                .map_err(|source| ApiError::Decode { source })?;
            Ok(decoded)
        })
    }
}

#[tokio::test]
#[ignore = "PERF gate: run with `cargo test --release --test perf_gates -- --ignored --nocapture`"]
async fn perf_gate_sync_roundtrip_p95_at_5k() {
    let body = generate_sync_response_json();
    let server = MockServer::start().await;
    Mock::given(method("POST"))
        .and(path("/api/v1/sync"))
        .respond_with(
            ResponseTemplate::new(200)
                .insert_header("content-type", "application/json")
                .set_body_string(body),
        )
        .mount(&server)
        .await;

    let mut conn = rusqlite::Connection::open_in_memory().unwrap();
    migrations::run(&mut conn).unwrap();
    let store = Arc::new(SqliteSyncStore::new(conn));
    // Note: `SyncClient` strictly forbids plaintext `http://127.0.0.1` when `debug_assertions`
    // is off (`--release`); `LoopbackHttpSyncApi` measures real loopback HTTP + JSON decode +
    // full `SyncCycle` 5k-row SQLite apply in release mode without relaxing production TLS policy.
    let api = Arc::new(LoopbackHttpSyncApi {
        client: reqwest::Client::new(),
        url: format!("{}/api/v1/sync", server.uri()),
    });
    let cycle = SyncCycle::new(api, Arc::clone(&store), Arc::clone(&store));

    // Warm up once
    cycle.run_once(1_800_000_000).await.unwrap();

    let mut samples = Vec::with_capacity(20);
    for i in 0..20 {
        let t0 = Instant::now();
        cycle.run_once(1_800_000_000 + i).await.unwrap();
        samples.push(t0.elapsed());
    }
    let measured_p95 = p95(&mut samples);
    println!(
        "PERF sync_roundtrip p95 (n=20, 5k items): {:?} (threshold: {:?})",
        measured_p95, MAX_SYNC_ROUNDTRIP_P95
    );
    assert!(
        measured_p95 < MAX_SYNC_ROUNDTRIP_P95,
        "sync round-trip p95 {:?} exceeded {:?}",
        measured_p95,
        MAX_SYNC_ROUNDTRIP_P95
    );
}
