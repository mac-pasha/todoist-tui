//! Key/value accessors for the `meta` table (T-304, EPIC-04).
//!
//! `meta` is a generic `key TEXT PRIMARY KEY, value TEXT` table (schema
//! `T-303`) used for small bits of sync bookkeeping that do not belong to
//! any single resource table: the incremental sync cursor and the
//! timestamps around the last successful sync. This module knows nothing
//! about resource tables (`items`, `projects`, ...) on purpose, so later
//! scheduling logic (EPIC-06) can depend on it in isolation.
//!
//! Sync tokens are user-scoped but not secret; even so, this module never
//! logs a value — only callers that explicitly choose to may, and even
//! then only the key's presence/absence should be logged, never the
//! value itself.
//!
//! All writes are upserts (`INSERT .. ON CONFLICT(key) DO UPDATE`), so
//! setting the same key twice always leaves exactly one row.

use rusqlite::Connection;

use crate::error::{DbError, Result};

/// Key under which the incremental sync cursor returned by the Todoist
/// Sync API is stored.
const KEY_SYNC_TOKEN: &str = "sync_token";

/// Key under which the timestamp of the last successful sync (of any
/// kind, full or incremental) is stored.
const KEY_LAST_SYNC_AT: &str = "last_sync_at";

/// Key under which the UTC date of the last successful *full* sync is
/// stored.
const KEY_FULL_SYNC_DATE_UTC: &str = "full_sync_date_utc";

/// Reads the incremental sync cursor returned by the Todoist Sync API.
///
/// Returns `Ok(None)` on a fresh database (no row for this key yet) —
/// callers must treat `None` as "no sync has ever completed, perform a
/// full sync" rather than an error. Once a sync completes, [`set_sync_token`]
/// stores the cursor so the next sync can be incremental.
///
/// # Errors
///
/// Returns an error if the underlying query fails.
pub fn sync_token(conn: &Connection) -> Result<Option<String>> {
    get(conn, KEY_SYNC_TOKEN)
}

/// Stores the incremental sync cursor returned by the Todoist Sync API,
/// replacing any previously stored value.
///
/// # Errors
///
/// Returns an error if the underlying upsert fails.
pub fn set_sync_token(conn: &Connection, value: &str) -> Result<()> {
    set(conn, KEY_SYNC_TOKEN, value)
}

/// Reads the timestamp of the last successful sync (full or incremental).
///
/// Returns `Ok(None)` when no sync has ever completed.
///
/// # Errors
///
/// Returns an error if the underlying query fails.
pub fn last_sync_at(conn: &Connection) -> Result<Option<String>> {
    get(conn, KEY_LAST_SYNC_AT)
}

/// Stores the timestamp of the last successful sync, replacing any
/// previously stored value.
///
/// # Errors
///
/// Returns an error if the underlying upsert fails.
pub fn set_last_sync_at(conn: &Connection, value: &str) -> Result<()> {
    set(conn, KEY_LAST_SYNC_AT, value)
}

/// Reads the UTC date of the last successful full sync.
///
/// Returns `Ok(None)` when no full sync has ever completed.
///
/// # Errors
///
/// Returns an error if the underlying query fails.
pub fn full_sync_date_utc(conn: &Connection) -> Result<Option<String>> {
    get(conn, KEY_FULL_SYNC_DATE_UTC)
}

/// Stores the UTC date of the last successful full sync, replacing any
/// previously stored value.
///
/// # Errors
///
/// Returns an error if the underlying upsert fails.
pub fn set_full_sync_date_utc(conn: &Connection, value: &str) -> Result<()> {
    set(conn, KEY_FULL_SYNC_DATE_UTC, value)
}

/// Reads the value stored under `key` in the `meta` table, or `None` if no
/// row for `key` exists yet.
fn get(conn: &Connection, key: &str) -> Result<Option<String>> {
    conn.query_row("SELECT value FROM meta WHERE key = ?1", [key], |row| {
        row.get(0)
    })
    .map(Some)
    .or_else(|err| match err {
        rusqlite::Error::QueryReturnedNoRows => Ok(None),
        source => Err(DbError::Sqlite { source }.into()),
    })
}

/// Upserts `value` under `key` in the `meta` table: inserts a new row if
/// none exists yet, otherwise overwrites the existing row's value so
/// exactly one row per key is ever stored.
fn set(conn: &Connection, key: &str, value: &str) -> Result<()> {
    conn.execute(
        "INSERT INTO meta (key, value) VALUES (?1, ?2) \
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        [key, value],
    )
    .map_err(|source| DbError::Sqlite { source })?;

    Ok(())
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicU64, Ordering};
    use std::time::{SystemTime, UNIX_EPOCH};

    use rusqlite::Connection;

    use super::*;
    use crate::db::migrations;

    /// Per-process counter so concurrent tests never collide on a tempdir
    /// name — same pattern as `src/db/open.rs` tests (no `tempfile`
    /// dev-dependency in this crate).
    static COUNTER: AtomicU64 = AtomicU64::new(0);

    fn tmp_db_path(label: &str) -> std::path::PathBuf {
        let nanos = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .unwrap()
            .as_nanos();
        let n = COUNTER.fetch_add(1, Ordering::SeqCst);
        let dir = std::env::temp_dir().join(format!("todoist-tui-test-meta-{label}-{nanos}-{n}"));
        std::fs::create_dir_all(&dir).unwrap();
        dir.join("meta.db")
    }

    fn migrated_connection() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        conn
    }

    #[test]
    fn sync_token_is_none_on_a_fresh_database() {
        let conn = migrated_connection();
        assert_eq!(sync_token(&conn).unwrap(), None);
    }

    #[test]
    fn last_sync_at_and_full_sync_date_utc_are_none_on_a_fresh_database() {
        let conn = migrated_connection();
        assert_eq!(last_sync_at(&conn).unwrap(), None);
        assert_eq!(full_sync_date_utc(&conn).unwrap(), None);
    }

    #[test]
    fn set_then_get_round_trips_each_key() {
        let conn = migrated_connection();

        set_sync_token(&conn, "token-abc").unwrap();
        set_last_sync_at(&conn, "2024-01-01T00:00:00Z").unwrap();
        set_full_sync_date_utc(&conn, "2024-01-01").unwrap();

        assert_eq!(sync_token(&conn).unwrap(), Some("token-abc".to_string()));
        assert_eq!(
            last_sync_at(&conn).unwrap(),
            Some("2024-01-01T00:00:00Z".to_string())
        );
        assert_eq!(
            full_sync_date_utc(&conn).unwrap(),
            Some("2024-01-01".to_string())
        );
    }

    #[test]
    fn setting_the_same_key_twice_leaves_exactly_one_row() {
        let conn = migrated_connection();

        set_sync_token(&conn, "first").unwrap();
        set_sync_token(&conn, "second").unwrap();

        let count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM meta WHERE key = 'sync_token'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(count, 1);
        assert_eq!(sync_token(&conn).unwrap(), Some("second".to_string()));
    }

    #[test]
    fn sync_token_survives_closing_and_reopening_the_same_file() {
        let path = tmp_db_path("roundtrip");

        {
            let mut conn = Connection::open(&path).unwrap();
            migrations::run(&mut conn).unwrap();
            set_sync_token(&conn, "persisted-token").unwrap();
        }

        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            sync_token(&conn).unwrap(),
            Some("persisted-token".to_string())
        );

        drop(conn);
        let _ = std::fs::remove_dir_all(path.parent().unwrap());
    }
}
