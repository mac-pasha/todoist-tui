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

        // Sidecars first, main file last: if a failure strikes partway
        // through (e.g. the main file's removal fails after the sidecars
        // are already gone), the result is a stale-but-reopenable database
        // rather than orphaned WAL/SHM sidecars with no main file to pair
        // them with.
        remove_if_exists(&wal_path(&path))?;
        remove_if_exists(&shm_path(&path))?;
        remove_if_exists(&path)?;

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
/// Any other failure removing an existing file (permission denied, the
/// path being a directory, a read-only filesystem, an interrupted syscall,
/// a sharing violation, ...) is reported via [`DbError::Permissions`].
///
/// That variant's name is a slight misnomer here — not every non-`NotFound`
/// `io::Error` is actually a permissions problem — but it is the closest
/// existing fit in [`DbError`]'s current vocabulary, since nothing in that
/// enum carries a raw [`io::Error`] today ([`DbError::Open`] and friends
/// thread through `rusqlite::Error`, not `io::Error`). Adding a dedicated
/// `DbError::Io { path, source: io::Error }` variant (mirroring
/// [`crate::error::ConfigError::Io`]) would preserve the real underlying
/// error instead of discarding it, but `src/error.rs` is outside this
/// task's (T-309) file scope, so that change is left for a follow-up task
/// rather than made here.
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
    use std::env;
    use std::sync::MutexGuard;

    use tempfile::tempdir;

    use super::*;
    use crate::config::test_support::ENV_LOCK;
    use crate::db::{migrations, open};

    /// Redirects [`paths::cache_db_file`] resolution at a tempdir for the
    /// lifetime of the guard, via `XDG_STATE_HOME` — the same seam
    /// `config::paths`' own tests use (T-004). Environment variables are
    /// process-global, so this takes the shared `ENV_LOCK`, serialising
    /// against every other env-mutating test in the binary (see
    /// `config::paths`' test module docs).
    struct StateHomeGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Option<std::ffi::OsString>,
    }

    impl StateHomeGuard {
        fn new(dir: &std::path::Path) -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved = env::var_os("XDG_STATE_HOME");
            env::set_var("XDG_STATE_HOME", dir);
            Self { _lock: lock, saved }
        }
    }

    impl Drop for StateHomeGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => env::set_var("XDG_STATE_HOME", v),
                None => env::remove_var("XDG_STATE_HOME"),
            }
        }
    }

    #[test]
    fn describe_identifies_the_cache() {
        assert_eq!(SqliteCacheWipe.describe(), "cache");
    }

    #[test]
    fn wipe_end_to_end_removes_the_real_resolved_cache_file() {
        let dir = tempdir().unwrap();
        let _guard = StateHomeGuard::new(dir.path());

        let path = paths::cache_db_file().unwrap();
        assert!(path.starts_with(dir.path()));
        paths::ensure_state_dir().unwrap();

        let mut conn = open::open_at(&path).unwrap();
        migrations::run(&mut conn).unwrap();
        drop(conn);
        std::fs::write(wal_path(&path), b"wal").unwrap();
        std::fs::write(shm_path(&path), b"shm").unwrap();
        assert!(path.exists());
        assert!(wal_path(&path).exists());
        assert!(shm_path(&path).exists());

        // Drives the real CacheWipe::wipe() path end to end: resolves the
        // path via config::paths::cache_db_file() exactly like production
        // code, rather than only exercising remove_if_exists() directly.
        SqliteCacheWipe.wipe().unwrap();

        assert!(!path.exists());
        assert!(!wal_path(&path).exists());
        assert!(!shm_path(&path).exists());

        // Idempotent: a second wipe with nothing left to remove still
        // succeeds.
        SqliteCacheWipe.wipe().unwrap();
    }

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

    // This test drives the same removal logic as `wipe_end_to_end_removes_
    // the_real_resolved_cache_file` above, but against an explicit path
    // rather than one resolved through `paths::cache_db_file`, and it also
    // asserts the post-wipe reopen behaviour (fresh schema, zero rows, no
    // `sync_token`) that the acceptance criteria call for.
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
