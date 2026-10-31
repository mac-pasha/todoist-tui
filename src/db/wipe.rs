//! Real `CacheWipe` implementation backed by the SQLite cache database
//! (T-309, EPIC-04).
//!
//! `logout` (T-108) needs to clear the local task cache; this module wires
//! that hook to the concrete on-disk file(s) rather than
//! [`crate::auth::cache_wipe::NoopCacheWipe`]'s inert placeholder. Wiping
//! means removing the database file itself plus its WAL/SHM siblings
//! (`<path>-wal` / `<path>-shm`) that SQLite's `journal_mode = WAL`
//! (applied by [`crate::db::open`]) can leave behind — any of the three
//! may or may not exist at a given moment, so each is removed
//! independently and a missing file is not an error. The next
//! [`crate::db::open::open_default`] call simply creates a fresh file and
//! [`crate::db::migrations::run`] brings it to [`crate::db::migrations::SCHEMA_VERSION`]
//! from empty, exactly like a first run.

use std::io;
use std::path::Path;

use crate::auth::cache_wipe::CacheWipe;
use crate::config::paths;
use crate::error::{DbError, Result};

/// [`CacheWipe`] implementation that deletes the real cache database file
/// (and its `-wal`/`-shm` siblings) resolved by
/// [`crate::config::paths::cache_db_file`].
#[derive(Debug, Default, Clone, Copy)]
pub struct SqliteCacheWipe;

impl CacheWipe for SqliteCacheWipe {
    fn wipe(&self) -> Result<()> {
        let path = paths::cache_db_file()?;

        remove_if_exists(&path)?;
        remove_if_exists(&wal_path(&path))?;
        remove_if_exists(&shm_path(&path))?;

        Ok(())
    }

    fn describe(&self) -> &'static str {
        "cache"
    }
}

/// Appends SQLite's standard WAL sidecar suffix to `path`.
fn wal_path(path: &Path) -> std::path::PathBuf {
    sibling_with_suffix(path, "-wal")
}

/// Appends SQLite's standard shared-memory sidecar suffix to `path`.
fn shm_path(path: &Path) -> std::path::PathBuf {
    sibling_with_suffix(path, "-shm")
}

/// Builds `<path><suffix>` (e.g. `cache.db` -> `cache.db-wal`), the naming
/// convention SQLite itself uses for its WAL/SHM sidecar files.
fn sibling_with_suffix(path: &Path, suffix: &str) -> std::path::PathBuf {
    let mut os_string = path.as_os_str().to_owned();
    os_string.push(suffix);
    std::path::PathBuf::from(os_string)
}

/// Removes `path`, treating "already absent" as success so [`SqliteCacheWipe::wipe`]
/// stays idempotent across the main file and both sidecars.
///
/// Any other failure removing an existing file is, in practice, a
/// permissions problem (the file's parent directory not being
/// owner-writable), so it is reported via the existing
/// [`DbError::Permissions`] variant rather than adding a new one.
fn remove_if_exists(path: &Path) -> Result<()> {
    match std::fs::remove_file(path) {
        Ok(()) => Ok(()),
        Err(source) if source.kind() == io::ErrorKind::NotFound => Ok(()),
        Err(_source) => Err(DbError::Permissions {
            path: path.to_path_buf(),
        }
        .into()),
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::*;
    use crate::db::{migrations, open};

    #[test]
    fn wipe_is_idempotent_when_file_absent() {
        let dir = tempdir().unwrap();
        // Point resolution at an isolated directory the file never
        // actually exists in, by never creating it.
        let path = dir.path().join("does-not-exist.db");
        assert!(!path.exists());

        remove_if_exists(&path).unwrap();
        remove_if_exists(&wal_path(&path)).unwrap();
        remove_if_exists(&shm_path(&path)).unwrap();
    }

    #[test]
    fn wipe_removes_db_and_wal_shm_siblings() {
        let dir = tempdir().unwrap();
        let path = dir.path().join("cache.db");

        let mut conn = open::open_at(&path).unwrap();
        migrations::run(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO meta (key, value) VALUES ('sync_token', 'abc')",
            [],
        )
        .unwrap();
        drop(conn);

        // WAL mode may or may not have flushed sidecars to disk yet; force
        // their presence so the removal path is actually exercised.
        std::fs::write(wal_path(&path), b"wal").unwrap();
        std::fs::write(shm_path(&path), b"shm").unwrap();
        assert!(path.exists());
        assert!(wal_path(&path).exists());
        assert!(shm_path(&path).exists());

        remove_if_exists(&path).unwrap();
        remove_if_exists(&wal_path(&path)).unwrap();
        remove_if_exists(&shm_path(&path)).unwrap();

        assert!(!path.exists());
        assert!(!wal_path(&path).exists());
        assert!(!shm_path(&path).exists());

        // Reopening after a wipe must behave like a brand-new database:
        // fresh schema, zero rows, no leftover `sync_token`.
        let mut reopened = open::open_at(&path).unwrap();
        let version = migrations::run(&mut reopened).unwrap();
        assert_eq!(version, migrations::SCHEMA_VERSION);

        let row_count: i64 = reopened
            .query_row("SELECT COUNT(*) FROM meta", [], |row| row.get(0))
            .unwrap();
        assert_eq!(row_count, 0);

        let sync_token: Option<String> = reopened
            .query_row(
                "SELECT value FROM meta WHERE key = 'sync_token'",
                [],
                |row| row.get(0),
            )
            .ok();
        assert_eq!(sync_token, None);
    }
}
