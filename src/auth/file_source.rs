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
//! Like every [`TokenSource`] implementation, this backend MUST NOT log
//! token material (see the trait docs in [`crate::auth::source`]); `store`
//! writes the exposed token directly to the file descriptor and never
//! through a formatting/log path.

use std::fs;
use std::io;
use std::path::{Path, PathBuf};

#[cfg(unix)]
use std::os::unix::fs::PermissionsExt;

use crate::auth::Token;
use crate::error::{AuthError, Result};

/// Permission bits applied to the token file: owner read/write only.
#[cfg(unix)]
const OWNER_READ_WRITE: u32 = 0o600;

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
    pub fn new(path: PathBuf) -> Self {
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
        Ok(Self::new(crate::config::paths::token_file()?))
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
}

impl super::source::TokenSource for FileTokenSource {
    fn load(&self) -> Result<Option<Token>> {
        match fs::read_to_string(&self.path) {
            Ok(raw) => Ok(Some(Token::new(raw)?)),
            Err(err) if err.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(err) => Err(self.io_err(err).into()),
        }
    }

    fn store(&self, token: &Token) -> Result<()> {
        if let Some(parent) = self.path.parent() {
            fs::create_dir_all(parent).map_err(|source| self.io_err(source))?;
        }
        fs::write(&self.path, token.expose()).map_err(|source| self.io_err(source))?;

        #[cfg(unix)]
        {
            let permissions = fs::Permissions::from_mode(OWNER_READ_WRITE);
            fs::set_permissions(&self.path, permissions).map_err(|source| self.io_err(source))?;
        }

        Ok(())
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
        let source = FileTokenSource::new(path.clone());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        let loaded = source.load().expect("load ok").expect("token present");
        assert_eq!(loaded.expose(), token.expose());
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn load_on_missing_file_returns_none() {
        let path = temp_token_path();
        let source = FileTokenSource::new(path);
        assert!(source.load().expect("load ok").is_none());
    }

    #[test]
    fn clear_removes_the_file_and_is_idempotent_when_missing() {
        let path = temp_token_path();
        let source = FileTokenSource::new(path.clone());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");
        assert!(path.exists());
        source.clear().expect("clear ok");
        assert!(!path.exists());
        source.clear().expect("clear on missing file is a no-op");
    }

    #[test]
    fn describe_names_the_backend() {
        let source = FileTokenSource::new(temp_token_path());
        assert_eq!(source.describe(), "token file");
    }

    #[cfg(unix)]
    #[test]
    fn store_sets_owner_only_permissions() {
        let path = temp_token_path();
        let source = FileTokenSource::new(path.clone());
        let token = Token::new(FIXTURE_TOKEN.to_string()).expect("non-empty token");
        source.store(&token).expect("store ok");

        let mode = fs::metadata(&path).expect("metadata").permissions().mode();
        assert_eq!(mode & 0o777, OWNER_READ_WRITE, "mode was {mode:o}");
        let _ = fs::remove_file(&path);
    }

    #[test]
    fn store_creates_parent_directories() {
        let path = temp_token_path().join("nested").join("token");
        let source = FileTokenSource::new(path.clone());
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
        let source = FileTokenSource::new(path.clone());
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
        let source = FileTokenSource::new(root.join("token"));
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
        let source = FileTokenSource::new(path.clone());
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
        // it — no trimming, escaping, newline appending or reformatting —
        // for adversarial shapes (whitespace padding, marker-bearing,
        // newline-bearing, unicode, 1 KiB).
        let root = temp_root("bytes");
        let path = root.join("token");
        let source = FileTokenSource::new(path.clone());
        let mut values: Vec<String> = [
            "x",
            "***",
            "tok***fake",
            "  tok_fake_pad_51  ",
            "a\nb\tc",
            "токе́н_fäke",
        ]
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

    // ---- Unix permission contract (acceptance) ----

    #[cfg(unix)]
    #[test]
    fn store_re_tightens_a_pre_existing_world_readable_file_to_0600() {
        // `fs::write` keeps an existing file's mode, so the explicit
        // set_permissions call is the only thing enforcing 0600 on the
        // overwrite path: a pre-planted 0666 file must end owner-only.
        let root = temp_root("retighten");
        fs::create_dir_all(&root).unwrap();
        let path = root.join("token");
        fs::write(&path, b"stale").unwrap();
        fs::set_permissions(&path, fs::Permissions::from_mode(0o666)).unwrap();
        let source = FileTokenSource::new(path.clone());
        source.store(&token(FAKE_A)).expect("store over 0666 file");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");
        // Re-store keeps the tightened mode.
        source.store(&token(FAKE_B)).expect("store again");
        let mode = fs::metadata(&path).unwrap().permissions().mode();
        assert_eq!(mode & 0o777, 0o600, "mode was {mode:o}");
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
            match FileTokenSource::new(path.clone()).load() {
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
        match FileTokenSource::new(path).load() {
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
        let source = FileTokenSource::new(path);
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
        let source = FileTokenSource::new(expected.clone());
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
        let source = FileTokenSource::new(root.join("no").join("such").join("token"));
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
        let source = FileTokenSource::new(blocker.join("nested").join("token"));
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
        let source = FileTokenSource::new(p.clone());
        assert_eq!(source.path(), p.as_path());
        assert!(!source.path().exists());
    }

    // ---- EPIC-02 usage shapes ----

    #[test]
    fn works_behind_box_dyn_and_in_a_memory_then_file_fallback_chain() {
        let root = temp_root("chain");
        let file: Box<dyn TokenSource> = Box::new(FileTokenSource::new(root.join("token")));
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
        let boxed: Box<dyn TokenSource + Send + Sync> = Box::new(FileTokenSource::new(
            PathBuf::from("/nonexistent/token-t104"),
        ));
        assert!(boxed.load().expect("load").is_none());
    }
}
