//! File-backed [`TokenSource`] fallback (T-104, EPIC-02).
//!
//! Stores the Todoist API token in a plain file under the platform config
//! directory (see [`crate::config::paths::token_file`]), intended as the
//! fallback backend when the OS keyring (T-103) is unavailable (headless
//! Linux without a secret service, CI, etc.).
//!
//! The file is written with owner-only (`0600`) permissions on Unix so
//! other local users cannot read the token off disk; this is the module's
//! central security property and is covered by an inline test. Permission
//! enforcement is Unix-specific ([`std::os::unix::fs::PermissionsExt`]);
//! on non-Unix platforms the file is written with the platform default
//! permissions (no equivalent primitive in `std` — tightening Windows ACLs
//! is out of scope for this task).
//!
//! **Loose-mode contract (T-1114, EPIC-12):** [`FileTokenSource::load`]
//! chmods a pre-existing token file to `0600` *in place, before reading
//! it* whenever its on-disk mode is wider than `0600` (e.g. a file left at
//! `0644`/`0666` by an older build that predates this guard, or restored
//! from a backup). This is a silent repair, not a rejection: `load`
//! returns the token successfully either way. Hard-failing here would
//! break every user upgrading from a version that wrote the token under
//! the process umask instead of `0600` at creation time — the file is
//! already on disk and already readable by other local users, so refusing
//! to read it back gains nothing and forces a re-`auth` for no security
//! benefit. Do not change this to a rejection without also considering
//! how existing installs recover.
//!
//! Like every [`TokenSource`] implementation, this backend MUST NOT log
//! token material (see the trait docs in [`crate::auth::source`]); `store`
//! writes the exposed token directly to the file descriptor and never
//! through a formatting/log path.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};

#[cfg(unix)]
use std::io::Write;
#[cfg(unix)]
use std::os::unix::fs::{OpenOptionsExt, PermissionsExt};

use crate::auth::Token;
use crate::error::{AuthError, Result};

/// Permission bits applied to the token file: owner read/write only.
#[cfg(unix)]
const OWNER_READ_WRITE: u32 = 0o600;

/// Per-process counter used to make concurrent temp-file names for the
/// same target path unique (pid alone is not enough if a process calls
/// [`FileTokenSource::store`] more than once, e.g. in tests).
#[cfg(unix)]
static TMP_COUNTER: AtomicUsize = AtomicUsize::new(0);

/// File-backed [`TokenSource`] fallback, storing the token at a fixed path.
#[derive(Debug, Clone)]
pub struct FileTokenSource {
    path: PathBuf,
}

impl FileTokenSource {
    /// Builds a source backed by the given file `path`.
    ///
    /// Does not touch the filesystem; the file is created lazily on the
    /// first [`FileTokenSource::store`] call.
    #[must_use]
    pub fn at(path: PathBuf) -> Self {
        Self { path }
    }

    /// Builds a source backed by the default token file path
    /// ([`crate::config::paths::token_file`]).
    ///
    /// # Errors
    ///
    /// Returns an error when the config directory's location cannot be
    /// determined (see [`crate::config::paths::token_file`]).
    pub fn default_location() -> Result<Self> {
        Ok(Self::at(crate::config::paths::token_file()?))
    }

    /// Path of the backing token file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }

    fn io_err(&self, source: io::Error) -> AuthError {
        AuthError::Io {
            path: self.path.clone(),
            source,
        }
    }

    /// If the token file exists and its mode grants any permission bit
    /// beyond owner-read-write (`0600`) — e.g. group/other read, write or
    /// execute, or an owner-execute bit — tightens it to `0600` in place.
    ///
    /// Crucially this is a one-directional ratchet: a mode that is already
    /// equal to or *more* restrictive than `0600` (e.g. `0400`, `0440`,
    /// `0000`) is a strict subset of `0600`'s bits and is left untouched.
    /// We never loosen a deliberately hardened file back up to `0600` —
    /// doing so would silently undo an intentional owner/ops decision to
    /// make the token file read-only or otherwise more restrictive. The
    /// check is therefore `mode & 0o777 & !0o600 != 0` ("are there bits
    /// set outside owner-rw?"), not `mode & 0o777 != 0o600` ("is it not
    /// exactly 0600?").
    ///
    /// A missing file (not yet created, or removed) is not an error here:
    /// [`FileTokenSource::load`] handles `NotFound` itself on the read
    /// that follows.
    #[cfg(unix)]
    fn tighten_if_loose(&self) -> Result<()> {
        let metadata = match fs::metadata(&self.path) {
            Ok(metadata) => metadata,
            Err(err) if err.kind() == io::ErrorKind::NotFound => return Ok(()),
            Err(err) => return Err(self.io_err(err).into()),
        };

        if metadata.permissions().mode() & 0o777 & !OWNER_READ_WRITE != 0 {
            fs::set_permissions(&self.path, fs::Permissions::from_mode(OWNER_READ_WRITE))
                .map_err(|source| self.io_err(source))?;
        }

        Ok(())
    }

    /// Atomic, mode-at-creation store for Unix: write the token to a
    /// sibling temp file created with `0600` permissions from the first
    /// `open(2)` call (never a separate `chmod` after the fact), then
    /// [`fs::rename`] it over the target. Rename within one directory is
    /// atomic on every Unix filesystem that matters here, so readers never
    /// observe a partially written file, and the temp file is never
    /// reachable at a permissive mode.
    #[cfg(unix)]
    fn store_atomic_unix(&self, token: &Token) -> Result<()> {
        let tmp_path = self.tmp_path();

        let write_result = (|| -> io::Result<()> {
            let mut file = fs::OpenOptions::new()
                .write(true)
                .create(true)
                .truncate(true)
                .mode(OWNER_READ_WRITE)
                .open(&tmp_path)?;
            file.write_all(token.expose().as_bytes())?;
            file.sync_all()?;
            Ok(())
        })();

        if let Err(err) = write_result {
            let _ = fs::remove_file(&tmp_path);
            return Err(self.io_err(err).into());
        }

        if let Err(err) = fs::rename(&tmp_path, &self.path) {
            let _ = fs::remove_file(&tmp_path);
            return Err(self.io_err(err).into());
        }

        Ok(())
    }

    /// Builds a unique temp-file path alongside the target, named so it
    /// sorts invisibly next to the real file (`.`-prefixed) and never
    /// collides across processes or repeated calls within one process.
    #[cfg(unix)]
    fn tmp_path(&self) -> PathBuf {
        let file_name = self
            .path
            .file_name()
            .map_or_else(|| "token".into(), std::ffi::OsStr::to_os_string);
        let tmp_name = format!(
            ".{}.tmp-{}-{}",
            file_name.to_string_lossy(),
            std::process::id(),
            TMP_COUNTER.fetch_add(1, Ordering::SeqCst)
        );
        match self.path.parent() {
            Some(parent) => parent.join(tmp_name),
            None => PathBuf::from(tmp_name),
        }
    }
}

impl super::source::TokenSource for FileTokenSource {
    /// Reads the token file and trims trailing newline/whitespace before
    /// constructing the [`Token`] (editors and `echo >file` always leave a
    /// trailing `\n`; the raw byte would otherwise become part of the
    /// token and silently break every API call). `trim_end` is used
    /// rather than a full `trim` so leading whitespace a caller
    /// deliberately stored — unlikely, but not this layer's business to
    /// second-guess — is preserved; an empty-or-whitespace-only result
    /// still surfaces as [`AuthError::EmptyToken`] via [`Token::new`].
    ///
    /// Invalid UTF-8 is intentionally surfaced as [`AuthError::Io`] (via
    /// `fs::read_to_string`'s own error) rather than a dedicated variant:
    /// a non-UTF-8 token file is corruption indistinguishable in practice
    /// from any other unreadable file, and callers already handle `Io`.
    ///
    /// On Unix, a pre-existing file whose mode is wider than `0600` is
    /// chmod'd to `0600` in place before being read (see the "Loose-mode
    /// contract" module docs); this never turns into an error, so
    /// upgrading from an older build that left the file loose keeps
    /// working.
    fn load(&self) -> Result<Option<Token>> {
        #[cfg(unix)]
        self.tighten_if_loose()?;

        match fs::read_to_string(&self.path) {
            Ok(raw) => Ok(Some(Token::new(raw.trim_end().to_string())?)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(self.io_err(err).into()),
        }
    }

    /// Writes `token` to the backing file atomically and with owner-only
    /// (`0600`) permissions applied *at creation time* on Unix, so there is
    /// never a window where the file exists on disk with a more permissive
    /// mode.
    ///
    /// Implementation: the token is written to a fresh temp file created
    /// in the same directory as the target (so the final rename stays on
    /// one filesystem and is atomic), flushed and closed, then moved into
    /// place with [`fs::rename`]. A failure after the temp file is created
    /// triggers a best-effort cleanup of that temp file; cleanup errors are
    /// ignored so the original error is always what callers see.
    fn store(&self, token: &Token) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            // Shares the ancestor-walk logic with `ensure_config_dir`/
            // `ensure_state_dir` (T-1114, EPIC-12): any parent component
            // this call itself creates (normally the config directory,
            // the first time `store` runs) is left at `0700` on Unix,
            // same umask-gap fix, same create-then-tighten shape. The
            // `_new_only` variant never re-tightens `parent` itself when
            // it already existed, because callers may construct a
            // `FileTokenSource` at an arbitrary pre-existing directory
            // this crate does not own (e.g. a shared temp dir in tests).
            // Errors are reported against `self.path` (the token file),
            // not the inner helper's directory path, matching every other
            // `AuthError::Io` this module raises.
            crate::config::paths::create_dir_all_tightened_new_only(parent).map_err(|err| {
                let crate::error::ConfigError::Io { source, .. } = err else {
                    unreachable!("create_dir_all_tightened_new_only only produces ConfigError::Io")
                };
                self.io_err(source)
            })?;
        }

        #[cfg(unix)]
        {
            self.store_atomic_unix(token)
        }

        #[cfg(not(unix))]
        {
            fs::write(&self.path, token.expose()).map_err(|source| self.io_err(source))
        }
    }

    fn clear(&self) -> Result<()> {
        match fs::remove_file(&self.path) {
            Ok(()) => Ok(()),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(err) => Err(self.io_err(err).into()),
        }
    }

    fn describe(&self) -> &'static str {
        "token file"
    }
}

#[cfg(test)]
mod tests {
    //! Sanity checks proving the T-104 design constraints hold; the
    //! unit-tester adds the full suite separately. Tempdirs are built from
    //! `std::env::temp_dir()` + pid + counter (no `tempfile`
    //! dev-dependency), matching `config::paths`' T-004 test pattern.
    use std::env;
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::auth::TokenSource;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    const FIXTURE_TOKEN: &str = "tok_fake_file_source_abc123";

    fn temp_token_path() -> PathBuf {
        env::temp_dir().join(format!(
            "todoist-tui-t104-{}-{}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
    }

    #[test]
    fn store_then_load_round_trips_an_equal_token() {
        let path = temp_token_path();
        let source = FileTokenSource::at(path.clone());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        let loaded = source.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), token.expose());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_on_missing_file_returns_none() {
        let path = temp_token_path();
        let source = FileTokenSource::at(path);
        assert!(source.load().expect("load ok").is_none());
    }

    #[test]
    fn clear_removes_the_file_and_is_idempotent_when_missing() {
        let path = temp_token_path();
        let source = FileTokenSource::at(path.clone());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        assert!(path.exists());
        source.clear().expect("clear ok");
        assert!(!path.exists());
        source.clear().expect("clear on missing file is a no-op");
    }

    #[test]
    fn describe_names_the_backend() {
        let source = FileTokenSource::at(temp_token_path());
        assert_eq!(source.describe(), "token file");
    }

    #[cfg(unix)]
    #[test]
    fn store_sets_owner_only_permissions() {
        let path = temp_token_path();
        let source = FileTokenSource::at(path.clone());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");

        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, OWNER_READ_WRITE, "mode was {mode:o}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn store_leaves_no_stray_temp_file_behind() {
        // The atomic write path creates a sibling `.{name}.tmp-*` file and
        // renames it over the target; after a successful store only the
        // final file may exist in the directory.
        let root = temp_token_path().join("dir");
        let path = root.join("token");
        let source = FileTokenSource::at(path.clone());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");

        let entries: Vec<_> = fs::read_dir(&root)
            .expect("dir readable")
            .map(|e| e.expect("entry").file_name())
            .collect();
        assert_eq!(entries, vec![std::ffi::OsString::from("token")]);
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn store_creates_parent_directories() {
        let path = temp_token_path().join("nested").join("token");
        let source = FileTokenSource::at(path.clone());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        assert!(path.exists());
        let _ = fs::remove_dir_all(path.parent().unwrap().parent().unwrap());
    }
}

/// Extended T-104 coverage (unit-tester, different model family from the
/// coder). Complements — does not duplicate — the sanity `tests` module
/// above: overwrite semantics, permission re-tightening of a pre-existing
/// world-readable file, byte-exact on-disk contents (the "never through a
/// formatting path" contract), corrupt-file behaviour (whitespace-only,
/// invalid UTF-8, directory at the path), unreachable parents, missing
/// parents for load/clear, store-clear-store cycles, the never-leak
/// contract for `Debug`/error renderings, `default_location` env resolution
/// under the shared config env lock, trait-object/fallback-chain usage,
/// and `Send + Sync` for later async wiring.
#[cfg(test)]
mod unit_tests {
    use std::env;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::MutexGuard;

    use super::*;
    use crate::auth::{MemoryTokenSource, TokenSource};
    use crate::error::{AppError, ConfigError};

    #[cfg(unix)]
    use std::os::unix::fs::PermissionsExt;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    const FAKE_A: &str = "tok_fake_file_aaa111";
    const FAKE_B: &str = "tok_fake_file_bbb222";

    /// Fakes only — never real credentials (repo rule 7).
    fn token(value: &str) -> Token {
        Token::new(value.to_string()).expect("non-empty fake token")
    }

    /// Fresh, never-created directory root; tests build files inside it and
    /// remove the whole root on the way out.
    fn temp_root(tag: &str) -> PathBuf {
        env::temp_dir().join(format!(
            "todoist-tui-t104u-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ))
    }

    /// Holds the *shared* `config::test_support::ENV_LOCK` while mutating
    /// `XDG_CONFIG_HOME` — the same discipline the T-104 follow-up imposes
    /// on `config::paths`/`config::load` tests: every env-touching test in
    /// this binary must serialise on that one lock, this module included.
    struct XdgGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Option<OsString>,
    }

    impl XdgGuard {
        fn new() -> Self {
            let lock = crate::config::test_support::ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self {
                _lock: lock,
                saved: env::var_os("XDG_CONFIG_HOME"),
            }
        }
    }

    impl Drop for XdgGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => env::set_var("XDG_CONFIG_HOME", v),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    // ---- store/load semantics beyond the happy path ----

    #[test]
    fn second_store_wins_and_the_file_holds_only_the_latest_token() {
        let root = temp_root("overwrite");
        let path = root.join("token");
        let source = FileTokenSource::at(path.clone());
        source.store(&token(FAKE_A)).expect("store A");
        source.store(&token(FAKE_B)).expect("store B");
        assert_eq!(
            source.load().expect("load").expect("present").expose(),
            FAKE_B,
            "latest store must win, not append"
        );
        let raw = fs::read_to_string(&path).expect("file readable");
        assert_eq!(raw, FAKE_B, "stale token must not linger on disk");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn store_clear_store_cycles_keep_the_source_usable() {
        let root = temp_root("cycles");
        let source = FileTokenSource::at(root.join("token"));
        for (i, v) in [FAKE_A, FAKE_B, FAKE_A].iter().enumerate() {
            source.store(&token(v)).expect("store ok");
            assert_eq!(
                source.load().expect("load ok").expect("present").expose(),
                *v,
                "cycle {i}: token must be readable after re-store"
            );
            source.clear().expect("clear ok");
            assert!(source.load().expect("load ok").is_none(), "cycle {i}");
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn load_is_repeatable_and_leaves_the_file_untouched() {
        let root = temp_root("repeat");
        let path = root.join("token");
        let source = FileTokenSource::at(path.clone());
        source.store(&token(FAKE_A)).expect("store ok");
        for _ in 0..3 {
            assert_eq!(
                source.load().expect("load").expect("present").expose(),
                FAKE_A
            );
        }
        assert!(path.exists(), "load must not consume the file");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn on_disk_bytes_are_exactly_the_exposed_token() {
        // The file layer must persist exactly what `Token::expose` hands
        // it — no escaping or reformatting — for adversarial shapes
        // (marker-bearing, *interior* whitespace/newlines, unicode, 1 KiB).
        // Leading/trailing whitespace is deliberately excluded here: `load`
        // trims trailing whitespace (see `trailing_newline_is_stripped_on_load`
        // below), so it is not part of the byte-exact round-trip contract.
        let root = temp_root("bytes");
        let path = root.join("token");
        let source = FileTokenSource::at(path.clone());
        let mut values: Vec<String> = ["x", "***", "tok***fake", "a\nb\tc", "токе́н_fäke"]
            .iter()
            .map(|s| (*s).to_string())
            .collect();
        values.push(format!("tok_fake_{}", "z".repeat(1024)));
        for v in &values {
            source.store(&token(v)).expect("store ok");
            assert_eq!(
                fs::read_to_string(&path).expect("read back"),
                *v,
                "file bytes must equal the exposed token exactly"
            );
            let loaded = source.load().expect("load").expect("present");
            assert_eq!(loaded.expose(), v);
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn trailing_newline_is_stripped_on_load() {
        // Any editor or `echo tok > file` leaves a trailing `\n`; `load`
        // must strip it rather than fold it into the token value.
        let root = temp_root("trailing-newline");
        let path = root.join("token");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, format!("{FAKE_A}\n")).unwrap();
        let loaded = FileTokenSource::at(path)
            .load()
            .expect("load ok")
            .expect("present");
        assert_eq!(loaded.expose(), FAKE_A);
        let _ = fs::remove_dir_all(&root);
    }

    // ---- Unix permission contract (acceptance) ----

    #[cfg(unix)]
    #[test]
    fn store_re_tightens_a_pre_existing_world_readable_file_to_0600() {
        // `store` never writes to an existing file in place: it always
        // creates a fresh 0600 temp file and renames it over the target,
        // so a pre-planted 0666 file must end up owner-only afterwards
        // regardless of its prior mode.
        let root = temp_root("retighten");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        fs::write(&path, b"stale").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let source = FileTokenSource::at(path.clone());
        source.store(&token(FAKE_A)).expect("store over 0666 file");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");
        // Re-store keeps the tightened mode.
        source.store(&token(FAKE_B)).expect("store again");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");
        let _ = fs::remove_dir_all(&root);
    }

    // ---- T-1114: load() tightens a loose pre-existing token file (sdlc-unit-tester) ----

    /// A token file left at 0644 (classic umask-022 artifact) by an older
    /// build must be accepted, read back intact, and re-chmodded to 0600 on
    /// disk — never rejected (upgrade-compatibility contract).
    #[cfg(unix)]
    #[test]
    fn load_tightens_pre_existing_0644_file_to_0600_and_returns_token() {
        let root = temp_root("load-tighten-644");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        fs::write(&path, FAKE_A.as_bytes()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o644)).unwrap();
        let source = FileTokenSource::at(path.clone());
        let loaded = source.load().expect("load of 0644 file must not error");
        let loaded = loaded.expect("token present");
        assert_eq!(
            loaded.expose(),
            FAKE_A,
            "token value must survive tightening"
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "on-disk mode after load was {mode:o}");
        let _ = fs::remove_dir_all(&root);
    }

    /// World-writable 0666 is the loosest realistic case: same tightening
    /// assertion, and the returned value must be byte-identical to what was
    /// written (tightening must not touch contents).
    #[cfg(unix)]
    #[test]
    fn load_tightens_pre_existing_0666_file_to_0600_and_preserves_content() {
        let root = temp_root("load-tighten-666");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        let body = format!("{FAKE_B}\n"); // trailing newline: content check
        fs::write(&path, body.as_bytes()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let source = FileTokenSource::at(path.clone());
        let loaded = source.load().expect("load of 0666 file must not error");
        let loaded = loaded.expect("token present");
        // load trims trailing whitespace into a Token; the file on disk must
        // still contain exactly what we wrote — chmod does not rewrite data.
        assert_eq!(loaded.expose(), FAKE_B);
        let on_disk = std::fs::read_to_string(&path).unwrap();
        assert_eq!(
            on_disk, body,
            "file contents must be byte-identical after tightening"
        );
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "on-disk mode after load was {mode:o}");
        let _ = fs::remove_dir_all(&root);
    }

    /// A file already at exactly 0600 must load fine and stay at 0600.
    /// NOTE: we deliberately do NOT assert "no chmod syscall happened"
    /// via mtime: the implementation's condition is `mode & 0o777 != 0o600`,
    /// which already guarantees no `set_permissions` call on a 0600 file,
    /// and mtime resolution on some filesystems is 1s, making an mtime
    /// equality assertion flaky. The mode-equality assertion plus the
    /// source-level condition check below are sufficient.
    #[cfg(unix)]
    #[test]
    fn load_on_already_0600_file_is_untouched_and_returns_token() {
        let root = temp_root("load-0600-untouched");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        fs::write(&path, FAKE_A.as_bytes()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o600)).unwrap();
        let before = fs::metadata(&path).unwrap();
        let source = FileTokenSource::at(path.clone());
        let loaded = source.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), FAKE_A);
        let after = fs::metadata(&path).unwrap();
        assert_eq!(
            after.permissions().mode() & 0o777,
            0o600,
            "0600 file must stay 0600"
        );
        assert_eq!(
            before.modified().unwrap(),
            after.modified().unwrap(),
            "a 0600 file must not be rewritten (mtime changed)"
        );
        assert_eq!(
            std::os::unix::fs::MetadataExt::mtime_nsec(&before),
            std::os::unix::fs::MetadataExt::mtime_nsec(&after)
        );
        let _ = fs::remove_dir_all(&root);
    }

    /// Missing file: `load` must still return `Ok(None)`, not panic, and
    /// must not create the file as a side effect of the tighten step.
    #[cfg(unix)]
    #[test]
    fn load_on_missing_file_returns_none_and_creates_nothing() {
        let root = temp_root("load-missing");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        assert!(!path.exists());
        let result = FileTokenSource::at(path.clone())
            .load()
            .expect("missing file is not an error");
        assert!(result.is_none());
        assert!(!path.exists(), "load() must not create the token file");
        let _ = fs::remove_dir_all(&root);
    }

    /// Adversarial probe for point 9 of the test brief, now updated after
    /// the T-1114 bug fix: the coder's original condition was
    /// `mode & 0o777 != 0o600`, which was NOT "wider-than-0600" — a MORE
    /// restrictive mode like 0400 (owner read-only) also failed that check
    /// and got CHMODDED back UP to 0600. This test used to pin that
    /// observed (buggy) behaviour. `tighten_if_loose` now uses a
    /// one-directional ratchet (`mode & 0o777 & !0o600 != 0`), so a 0400
    /// file is correctly left untouched by `load()`. The assertion below
    /// has been flipped to match the corrected, documented contract:
    /// tightening never loosens a file that is already at or below `0600`.
    #[cfg(unix)]
    #[test]
    fn load_on_0400_file_observed_behavior_pins_loosening_bug() {
        let root = temp_root("load-0400-probe");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        fs::write(&path, FAKE_B.as_bytes()).unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o400)).unwrap();
        let source = FileTokenSource::at(path.clone());
        let loaded = source.load().expect("load of 0400 file must not error");
        let loaded = loaded.expect("token present");
        assert_eq!(loaded.expose(), FAKE_B);
        let mode = fs::metadata(&path).unwrap().permissions().mode() & 0o777;
        // FIXED behaviour: 0400 is a strict subset of 0600's bits, so it is
        // left untouched by `tighten_if_loose` — no loosening occurs.
        assert_eq!(
            mode,
            0o400,
            "expected 0400 token file to stay at 0400 after load(); tighten_if_loose must never loosen a mode that is already a subset of 0600"
        );
        let _ = fs::remove_dir_all(&root);
    }

    // ---- corrupt / hostile on-disk states ----

    #[test]
    fn whitespace_only_token_file_surfaces_empty_token_error() {
        let root = temp_root("corrupt-ws");
        let path = root.join("token");
        fs::create_dir_all(&root).unwrap();
        for body in ["", "   \n\t"] {
            fs::write(&path, body).unwrap();
            match FileTokenSource::at(path.clone()).load() {
                // OBSERVED CONTRACT (documented, asserted): a present but
                // empty/whitespace-only file is corruption, not "no
                // token" — load surfaces AuthError::EmptyToken rather
                // than Ok(None) or a panic, and never echoes the input.
                Err(AppError::Auth(AuthError::EmptyToken)) => {}
                other => panic!("body {body:?}: expected EmptyToken, got {other:?}"),
            }
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn invalid_utf8_token_file_is_an_io_error_never_a_panic() {
        let root = temp_root("corrupt-utf8");
        let path = root.join("token");
        fs::create_dir_all(&root).unwrap();
        fs::write(&path, [0xff, 0xfe, 0x00, 0x10]).unwrap();
        match FileTokenSource::at(path).load() {
            Err(AppError::Auth(AuthError::Io { .. })) => {}
            other => panic!("expected AuthError::Io, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn directory_at_the_token_path_errors_for_both_load_and_store() {
        let root = temp_root("isdir");
        let path = root.join("token");
        fs::create_dir_all(&path).unwrap(); // the path itself is a directory
        let source = FileTokenSource::at(path);
        match source.load() {
            Err(AppError::Auth(AuthError::Io { .. })) => {}
            other => panic!("load: expected AuthError::Io, got {other:?}"),
        }
        match source.store(&token(FAKE_A)) {
            Err(AppError::Auth(AuthError::Io { .. })) => {}
            other => panic!("store: expected AuthError::Io, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn store_fails_cleanly_when_a_parent_is_a_regular_file() {
        let root = temp_root("blocked");
        fs::create_dir_all(&root).unwrap();
        let blocker = root.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let expected = blocker.join("nested").join("token");
        let source = FileTokenSource::at(expected.clone());
        match source.store(&token(FAKE_A)) {
            Err(AppError::Auth(AuthError::Io { path, .. })) => {
                assert_eq!(path, expected, "error must name the token file");
            }
            other => panic!("expected AuthError::Io, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn clear_and_load_on_a_missing_parent_directory_are_not_errors() {
        // NotADirectory/NotFound on every access: missing parents are the
        // same "nothing stored yet" state as a missing file.
        let root = temp_root("ghost"); // deliberately never created
        let source = FileTokenSource::at(root.join("no").join("such").join("token"));
        assert!(source.load().expect("load on missing parent").is_none());
        source.clear().expect("clear on missing parent is Ok(())");
        source.clear().expect("clear twice is still Ok(())");
        assert!(!root.exists(), "no directory may be conjured by reading");
    }

    // ---- never-leak contract (trait docs bind every backend) ----

    #[test]
    fn debug_describe_and_error_renderings_never_contain_token_material() {
        let root = temp_root("leak");
        fs::create_dir_all(&root).unwrap();
        let blocker = root.join("blocker");
        fs::write(&blocker, b"x").unwrap();
        let source = FileTokenSource::at(blocker.join("nested").join("token"));
        // Debug holds a path only — before and after a token ever flows
        // through the source.
        let dbg_before = format!("{source:?}");
        let pretty_before = format!("{source:#?}");
        // A failing store renders path + io error, never the token.
        let err = source.store(&token(FAKE_A)).expect_err("store must fail");
        let msg = err.to_string();
        let err_dbg = format!("{err:?}");
        let dbg_after = format!("{source:?}");
        for rendered in [&dbg_before, &pretty_before, &msg, &err_dbg, &dbg_after] {
            assert!(
                !rendered.contains(FAKE_A),
                "rendering leaked token: {rendered}"
            );
        }
        assert_eq!(source.describe(), "token file");
        assert!(!source.describe().contains(FAKE_A));
        let _ = fs::remove_dir_all(&root);
    }

    // ---- default_location + path accessor ----

    #[test]
    fn default_location_resolves_via_xdg_and_touches_nothing_lazily() {
        let _g = XdgGuard::new();
        let root = temp_root("default-loc");
        fs::create_dir_all(&root).unwrap();
        env::set_var("XDG_CONFIG_HOME", &root);
        let source = FileTokenSource::default_location().expect("XDG_CONFIG_HOME set");
        assert_eq!(source.path(), root.join("todoist-tui").join("token"));
        assert!(!source.path().exists(), "construction must be lazy");
        assert!(source.load().expect("load").is_none());
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn default_location_without_xdg_is_absolute_or_a_config_error() {
        let _g = XdgGuard::new();
        env::remove_var("XDG_CONFIG_HOME");
        match FileTokenSource::default_location() {
            Ok(s) => assert!(s.path().is_absolute(), "got {:?}", s.path()),
            Err(AppError::Config(ConfigError::Io { .. })) => {}
            other => panic!("unexpected result: {other:?}"),
        }
    }

    #[test]
    fn path_accessor_returns_exactly_the_configured_path() {
        let p = PathBuf::from("/definitely/not/created/token-t104");
        let source = FileTokenSource::at(p.clone());
        assert_eq!(source.path(), p.as_path());
        assert!(!source.path().exists());
    }

    // ---- EPIC-02 usage shapes ----

    #[test]
    fn works_behind_box_dyn_and_in_a_memory_then_file_fallback_chain() {
        let root = temp_root("chain");
        let file: Box<dyn TokenSource> = Box::new(FileTokenSource::at(root.join("token")));
        assert!(file.load().expect("load").is_none());
        file.store(&token(FAKE_A)).expect("store via Box");
        assert_eq!(
            file.load().expect("load").expect("present").expose(),
            FAKE_A
        );
        file.clear().expect("clear via Box");
        assert_ne!(file.describe(), "");

        // Fallback-chain shape: primary (keyring in T-103) + file fallback
        // behind one trait-object collection; first backend holding a
        // token wins, and the chain degrades to the file cleanly.
        let mem = MemoryTokenSource::new();
        let chain: Vec<&dyn TokenSource> = vec![&mem, file.as_ref()];
        assert!(chain.iter().find_map(|s| s.load().expect("load")).is_none());
        mem.store(&token(FAKE_B)).expect("store in primary");
        let found = chain
            .iter()
            .find_map(|s| s.load().expect("load"))
            .expect("primary holds a token");
        assert_eq!(found.expose(), FAKE_B);
        mem.clear().expect("clear primary");
        file.store(&token(FAKE_A)).expect("store in fallback");
        let found = chain
            .iter()
            .find_map(|s| s.load().expect("load"))
            .expect("fallback holds a token");
        assert_eq!(found.expose(), FAKE_A, "chain must degrade to the file");
        let _ = fs::remove_dir_all(&root);
    }

    #[test]
    fn file_source_is_send_sync_for_async_wiring() {
        // PathBuf-only state: movable across tokio task boundaries and
        // shareable behind `Box<dyn TokenSource + Send + Sync>`, the shape
        // app state will need once auth crosses await points.
        fn assert_send_sync<T: Send + Sync + 'static>() {}
        assert_send_sync::<FileTokenSource>();
        let boxed: Box<dyn TokenSource + Send + Sync> = Box::new(FileTokenSource::at(
            PathBuf::from("/nonexistent/token-t104"),
        ));
        assert!(boxed.load().expect("load").is_none());
    }
}
