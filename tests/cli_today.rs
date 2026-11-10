//! Golden-file CLI tests for `todoist-tui today` text and JSON output (`T-1008`, EPIC-11).
//!
//! To regenerate golden files after an intentional contract update:
//! Run `cargo test --test cli_today` and inspect the stdout bytes produced against
//! `tests/fixtures/cli_today.golden.txt` and `tests/fixtures/cli_today.golden.json`.

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

mod support;

use std::fs;

use api::sync::SyncResponse;
use db::{apply, migrations, open};
use support::TestHome;

fn seed_cache_from_fixture(home: &TestHome) {
    fs::create_dir_all(home.state_dir()).unwrap();
    let db_path = home.state_dir().join("cache.db");
    let mut conn = open::open_at(&db_path).unwrap();
    migrations::run(&mut conn).unwrap();

    let fixture_raw = include_str!("fixtures/cli_today_cache.json");
    let resp: SyncResponse = serde_json::from_str(fixture_raw).unwrap();
    apply::apply_sync(&mut conn, &resp).unwrap();
}

#[test]
fn today_plain_text_matches_golden_file_byte_for_byte_with_no_ansi() {
    let home = TestHome::new("http://127.0.0.1:0");
    seed_cache_from_fixture(&home);

    let mut cmd = home.command();
    cmd.env("TODOIST_TUI_TODAY", "2026-04-17").arg("today");

    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(
        !out.stdout.contains(&0x1b),
        "stdout must never contain ANSI escape bytes"
    );
    let actual = String::from_utf8(out.stdout).unwrap();
    assert!(!actual.contains("\x1b[?1049"));

    let expected = include_str!("fixtures/cli_today.golden.txt");
    assert_eq!(actual, expected);
}

#[test]
fn today_json_matches_golden_file_byte_for_byte_with_no_ansi() {
    let home = TestHome::new("http://127.0.0.1:0");
    seed_cache_from_fixture(&home);

    let mut cmd = home.command();
    cmd.env("TODOIST_TUI_TODAY", "2026-04-17")
        .args(["today", "--json"]);

    let out = cmd.output().unwrap();
    assert_eq!(
        out.status.code(),
        Some(0),
        "stderr: {}",
        String::from_utf8_lossy(&out.stderr)
    );

    assert!(!out.stdout.contains(&0x1b));
    let actual = String::from_utf8(out.stdout).unwrap();
    let _parsed: serde_json::Value = serde_json::from_str(&actual).unwrap();

    let expected = include_str!("fixtures/cli_today.golden.json");
    assert_eq!(actual, expected);
}

#[test]
fn today_clock_override_changes_output_deterministically() {
    let home = TestHome::new("http://127.0.0.1:0");
    seed_cache_from_fixture(&home);

    // Date 2026-04-18 includes "Tomorrow planning" (`6X4VfakeT004`)
    let mut cmd = home.command();
    cmd.env("TODOIST_TUI_TODAY", "2026-04-18").arg("today");

    let out = cmd.output().unwrap();
    assert_eq!(out.status.code(), Some(0));
    let actual = String::from_utf8(out.stdout).unwrap();
    assert!(actual.contains("6X4VfakeT004"));
    assert!(actual.contains("Tomorrow planning"));
}
