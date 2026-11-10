//! Security integration tests for on-disk permissions (T-1105, EPIC-12).
//!
//! Unix only: the whole body is `#[cfg(unix)]`, so on other platforms this
//! file compiles to an empty test binary (skipped cleanly).
//!
//! Everything lives under a [`TestHome`] temp dir; nothing is written to the
//! real HOME/XDG directories.
//!
//! Pinned contracts (what production does today):
//!
//! * SQLite cache file, and its `-wal`/`-shm` siblings: `0600`
//!   (`db::open_at` chmods the file *before* enabling WAL, so SQLite creates
//!   the siblings with the same owner-only mode).
//! * File-backed token written by `auth`: `0600`.
//! * Pre-existing world-readable token file: **`load` tightens it to `0600`
//!   in place before reading** (upgrade compatibility per T-1114) and
//!   **`store` (re-running `auth`) also tightens/replaces it at `0600`**.
//! * Parent directories (`config_dir` and `state_dir`): enforced at `0700`
//!   by `ensure_config_dir` / `ensure_state_dir` (T-1114).

#![cfg(unix)]
#![allow(clippy::unwrap_used, clippy::expect_used)]

#[path = "../src/error.rs"]
mod error;

#[path = "../src/config/paths.rs"]
mod paths;

#[path = "../src/log.rs"]
mod log;

/// Path shim for the included files' `crate::config::...` references.
///
/// `test_support::ENV_LOCK` is re-exported from this file's own
/// [`ENV_MUTEX`] (not a second, independent `Mutex`) so that
/// `src/config/paths.rs`'s own env-mutating tests (`paths::tests`,
/// `paths::unit_tests`, included into this same test binary) serialise
/// against *this* file's env-mutating tests too. Two separate mutexes here
/// would each individually "work" but not guard against each other,
/// leaving exactly the kind of cross-module env race this file exists to
/// prevent.
#[allow(unused_imports)]
mod config {
    pub(crate) use crate::paths;

    pub(crate) mod test_support {
        pub(crate) use crate::ENV_MUTEX as ENV_LOCK;
    }
}

#[allow(dead_code)]
#[path = "../src/db/open.rs"]
mod open;

mod support;

use std::fs;
use std::io::Write;
use std::os::unix::fs::PermissionsExt;
use std::path::Path;
use std::process::Stdio;
use std::sync::Mutex;

use crate::support::{MockSyncApi, TestHome, FAKE_TOKEN};

/// Serialises tests that mutate this process's environment (XDG vars).
///
/// Every test that calls `std::env::set_var`/`remove_var` directly (via
/// [`open_cache_in`]) *or* spawns the binary (via [`run_auth`] /
/// `TestHome::command`) must hold this lock first: `Command::spawn`
/// snapshots the whole process environment, and reading it concurrently
/// with an unguarded `set_var`/`remove_var` on another thread is a data
/// race (std's env functions are not internally synchronised), not just a
/// logical race over which XDG root wins.
static ENV_MUTEX: Mutex<()> = Mutex::new(());

fn mode(path: &Path) -> u32 {
    fs::metadata(path)
        .unwrap_or_else(|e| panic!("stat {}: {e}", path.display()))
        .permissions()
        .mode()
        & 0o777
}

/// Opens the default cache DB with every XDG root pointed into `home`,
/// writes a row so WAL/SHM siblings certainly exist, and returns the DB path
/// with the connection kept open.
fn open_cache_in(home: &TestHome) -> (rusqlite::Connection, std::path::PathBuf) {
    std::env::set_var("HOME", home.root());
    std::env::set_var("XDG_CONFIG_HOME", home.root().join("cfg"));
    std::env::set_var("XDG_STATE_HOME", home.root().join("state"));
    std::env::set_var("XDG_DATA_HOME", home.data_dir());
    let conn = open::open_default().expect("open default cache db");
    conn.execute_batch("CREATE TABLE t(x); INSERT INTO t VALUES (1);")
        .expect("write to db");
    let path = paths::cache_db_file().expect("cache db path");
    assert!(
        path.starts_with(home.root()),
        "cache db {} escaped the temp HOME",
        path.display()
    );
    (conn, path)
}

fn run_auth(home: &TestHome, token: &str) -> std::process::Output {
    let mut child = home
        .command()
        .arg("auth")
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("spawn binary");
    let mut stdin = child.stdin.take().unwrap();
    stdin.write_all(format!("{token}\n").as_bytes()).unwrap();
    drop(stdin);
    child.wait_with_output().expect("wait for binary")
}

#[test]
fn sqlite_cache_and_wal_shm_siblings_are_mode_0600() {
    let _env = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::without_token("http://127.0.0.1:1");
    let (_conn, db) = open_cache_in(&home);

    assert_eq!(mode(&db), 0o600, "cache db mode");
    for suffix in ["-wal", "-shm"] {
        let sibling = db.with_file_name(format!(
            "{}{suffix}",
            db.file_name().unwrap().to_string_lossy()
        ));
        assert!(sibling.exists(), "{} should exist", sibling.display());
        assert_eq!(mode(&sibling), 0o600, "{} mode", sibling.display());
    }
}

#[test]
fn reopening_a_world_readable_cache_db_tightens_it_to_0600() {
    let _env = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::without_token("http://127.0.0.1:1");
    let (conn, db) = open_cache_in(&home);
    drop(conn);
    fs::set_permissions(&db, fs::Permissions::from_mode(0o666)).unwrap();
    let _again = open::open_default().expect("reopen");
    assert_eq!(mode(&db), 0o600);
}

#[tokio::test]
async fn auth_writes_the_file_backed_token_as_0600() {
    let api = MockSyncApi::full_sync().await;
    let _env = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::without_token(&api.base_url());
    let out = run_auth(&home, FAKE_TOKEN);
    assert!(
        out.status.success(),
        "auth failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(mode(&home.token_file()), 0o600, "token file mode");
}

/// CONTRACT (pinned): a pre-existing world-readable token file is tightened
/// to `0600` in place on *read* (`FileTokenSource::load`, T-1114) and accepted
/// so upgrading users with a legacy loose-mode token file keep working safely.
#[test]
fn preexisting_world_readable_token_is_accepted_and_tightened_to_0600_on_read() {
    let _env = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::new("http://127.0.0.1:1");
    let token = home.token_file();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o644)).unwrap();

    let out = home.command().arg("today").output().expect("spawn binary");
    let stderr = String::from_utf8_lossy(&out.stderr);
    assert!(
        !stderr.contains("not authenticated") && !stderr.to_lowercase().contains("permission"),
        "token must be found, not rejected; stderr: {stderr}"
    );
    assert_eq!(mode(&token), 0o600, "load must tighten the mode to 0600");
}

/// CONTRACT (pinned): re-running `auth` over a world-readable token file
/// replaces it atomically with a fresh `0600` file.
#[tokio::test]
async fn auth_retightens_a_preexisting_world_readable_token_file() {
    let api = MockSyncApi::full_sync().await;
    let _env = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::new(&api.base_url());
    let token = home.token_file();
    fs::set_permissions(&token, fs::Permissions::from_mode(0o666)).unwrap();

    let out = run_auth(&home, FAKE_TOKEN);
    assert!(
        out.status.success(),
        "auth failed: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    assert_eq!(mode(&token), 0o600);
}

/// Parent directories must be 0700 (REQ-001 security, enforced by T-1114).
#[test]
fn cache_db_parent_dir_is_mode_0700() {
    let _env = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::without_token("http://127.0.0.1:1");
    let (_conn, db) = open_cache_in(&home);
    assert_eq!(mode(db.parent().unwrap()), 0o700, "state dir mode");
}

#[tokio::test]
async fn token_parent_dir_is_mode_0700() {
    let api = MockSyncApi::full_sync().await;
    let _env = ENV_MUTEX
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let home = TestHome::without_token(&api.base_url());
    let out = run_auth(&home, FAKE_TOKEN);
    assert!(out.status.success());
    assert_eq!(mode(home.token_file().parent().unwrap()), 0o700);
}
