//! Opening the local SQLite cache database (T-302, EPIC-04).
//!
//! Connections are opened at the state-dir path resolved by
//! [`crate::config::paths::cache_db_file`] (or an explicit path, for
//! tests), with the on-disk file locked down to owner-only (`0600`)
//! permissions on Unix — same create-then-tighten shape as
//! [`crate::auth::file_source`], since SQLite (unlike the token file) must
//! open the path itself rather than write through an already-open handle,
//! so permissions are applied/verified after `rusqlite::Connection::open`
//! rather than at a single `open(2)` call.
//!
//! Every connection opened here has `foreign_keys = ON`, `journal_mode =
//! WAL` and a non-zero `busy_timeout` applied before it is handed back, so
//! callers never have to remember to set per-connection pragmas themselves.
//!
//! No schema/DDL lands in this module — later EPIC-04 tasks add migrations
//! on top of the connections opened here.

use std::fs;
use std::path::Path;

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use rusqlite::Connection;

use crate::config::paths;
use crate::error::{DbError, Result};

/// Permission bits applied to the database file: owner read/write only.
#[cfg(unix)]
const OWNER_READ_WRITE: u32 = 0o600;

/// Busy timeout (milliseconds) applied to every opened connection so
/// concurrent access (e.g. a future sync task writing while the UI reads)
/// waits for the lock instead of failing immediately with `SQLITE_BUSY`.
const BUSY_TIMEOUT_MS: u64 = 5_000;

/// Opens the default cache database, under the platform state directory
/// (see [`crate::config::paths::cache_db_file`]), creating the state
/// directory and the database file if either does not already exist.
///
/// # Errors
///
/// Returns an error when the state directory cannot be resolved/created
/// ([`crate::error::ConfigError`], wrapped into [`crate::error::AppError`]
/// by `?`), when SQLite fails to open the file
/// ([`DbError::Open`]), or when the file's permissions cannot be
/// tightened to owner-only on Unix ([`DbError::Permissions`]).
pub fn open_default() -> Result<Connection> {
    paths::ensure_state_dir()?;
    let path = paths::cache_db_file()?;
    open_at(&path)
}

/// Opens (creating if absent) the cache database at an explicit `path`,
/// for tests that need an isolated location instead of the platform state
/// directory.
///
/// # Errors
///
/// Returns [`DbError::Open`] when SQLite fails to open the file, or
/// [`DbError::Permissions`] when the file's permissions cannot be
/// tightened to owner-only on Unix.
pub fn open_at(path: &Path) -> Result<Connection> {
    let conn = Connection::open(path).map_err(|source| DbError::Open {
        path: path.to_path_buf(),
        source,
    })?;

    tighten_permissions(path)?;
    apply_pragmas(&conn, path)?;

    Ok(conn)
}

/// Tightens the database file's permissions to owner-only (`0600`) on
/// Unix, regardless of whether the file was just created or already
/// existed with a wider mode (e.g. `0644`). No-op on non-Unix platforms:
/// `std` has no portable primitive for tightening Windows ACLs, matching
/// [`crate::auth::file_source`]'s documented scope.
#[cfg(unix)]
fn tighten_permissions(path: &Path) -> Result<()> {
    fs::set_permissions(path, fs::Permissions::from_mode(OWNER_READ_WRITE)).map_err(|_| {
        DbError::Permissions {
            path: path.to_path_buf(),
        }
        .into()
    })
}

#[cfg(not(unix))]
fn tighten_permissions(_path: &Path) -> Result<()> {
    Ok(())
}

/// Applies the per-connection pragmas every caller of this module relies
/// on: foreign key enforcement, WAL journalling and a busy timeout so
/// lock contention waits instead of erroring immediately.
fn apply_pragmas(conn: &Connection, path: &Path) -> Result<()> {
    conn.pragma_update(None, "foreign_keys", "ON")
        .map_err(|source| DbError::Open {
            path: path.to_path_buf(),
            source,
        })?;
    conn.pragma_update(None, "journal_mode", "WAL")
        .map_err(|source| DbError::Open {
            path: path.to_path_buf(),
            source,
        })?;
    conn.busy_timeout(std::time::Duration::from_millis(BUSY_TIMEOUT_MS))
        .map_err(|source| DbError::Open {
            path: path.to_path_buf(),
            source,
        })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    /// Per-process counter so concurrent tests never collide on a tempdir
    /// name, same pattern as `src/config/paths.rs` tests (no `tempfile`
    /// dev-dependency in this crate).
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_db_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("todoist-tui-test-db-{label}-{nanos}-{n}"));
        fs::create_dir_all(&dir).unwrap();
        dir.join("cache.db")
    }

    #[test]
    fn open_at_creates_database_file() {
        let path = tmp_db_path("create");
        let _conn = open_at(&path).expect("open_at");
        assert!(path.is_file());
        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[test]
    fn open_at_sets_foreign_keys_wal_and_busy_timeout() {
        let path = tmp_db_path("pragmas");
        let conn = open_at(&path).expect("open_at");

        let foreign_keys: i64 = conn
            .pragma_query_value(None, "foreign_keys", |row| row.get(0))
            .unwrap();
        assert_eq!(foreign_keys, 1);

        let journal_mode: String = conn
            .pragma_query_value(None, "journal_mode", |row| row.get(0))
            .unwrap();
        assert_eq!(journal_mode.to_lowercase(), "wal");

        let busy_timeout: i64 = conn
            .pragma_query_value(None, "busy_timeout", |row| row.get(0))
            .unwrap();
        assert!(busy_timeout > 0);

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn open_at_sets_mode_0600_on_a_fresh_file() {
        let path = tmp_db_path("fresh-mode");
        let _conn = open_at(&path).expect("open_at");

        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }

    #[cfg(unix)]
    #[test]
    fn open_at_tightens_a_pre_existing_wider_mode_to_0600() {
        let path = tmp_db_path("tighten");
        // Create the file up front with a wider mode than 0600.
        {
            let conn = Connection::open(&path).expect("pre-create");
            drop(conn);
        }
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o644);

        let _conn = open_at(&path).expect("open_at");

        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");

        let _ = fs::remove_dir_all(path.parent().unwrap());
    }
}
