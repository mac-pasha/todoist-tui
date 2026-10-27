//! Platform directory resolution (T-004, EPIC-01).
//!
//! Pure functions that read the process environment at call time — no
//! caching, no globals — so tests can redirect resolution by setting
//! `XDG_CONFIG_HOME` / `XDG_STATE_HOME` (env mutation is process-global,
//! so those tests must serialise; see the task notes).
//!
//! Resolution order is identical on every platform, so the behaviour is
//! testable anywhere:
//!
//! 1. `XDG_CONFIG_HOME` / `XDG_STATE_HOME`, when set *and* non-empty (an
//!    empty value counts as unset, per the XDG Base Directory spec). The
//!    application directory `todoist-tui` is appended, matching the Linux
//!    `ProjectDirs` layout.
//! 2. Otherwise the platform default via `directories::ProjectDirs` with
//!    qualifier `com`, organization `todoist`, application `todoist-tui`:
//!    Linux `~/.config` / `~/.local/state`, macOS `Application Support`,
//!    Windows `AppData` (best-effort per NFR).
//!
//! `ProjectDirs::state_dir` is `None` on macOS and Windows (no conventional
//! state home), so the state fallback is the project data directory — kept
//! durable, never the purgeable cache directory — which gives the log file
//! (T-006) and cache DB (EPIC-04) a stable home.

// Consumers land in T-005 (config loader) and T-006 (log file), and the
// unit tester wires the XDG tests into main.rs: `pub` alone does not
// silence `dead_code` in a binary crate, so allow it at module scope —
// same pattern as error.rs (T-003). Remove once both consumers exist.
#![allow(dead_code)]

use std::env;
use std::fs;
use std::io;
use std::path::PathBuf;

use directories::ProjectDirs;

use crate::error::ConfigError;

/// Reverse-DNS qualifier used by `ProjectDirs` on macOS / Windows.
const QUALIFIER: &str = "com";
/// Organization component used by `ProjectDirs` on Windows.
const ORGANIZATION: &str = "todoist";
/// Application directory appended to every resolved base directory.
const APPLICATION: &str = "todoist-tui";
/// File name of the TOML config file inside the config directory.
const CONFIG_FILE_NAME: &str = "config.toml";
/// File name of the log file inside the state directory (T-006).
const LOG_FILE_NAME: &str = "todoist-tui.log";
/// File name of the file-backed token fallback inside the config directory
/// (T-104).
const TOKEN_FILE_NAME: &str = "token";

/// Absolute path of the config directory: `<base>/todoist-tui`.
///
/// `<base>` is `XDG_CONFIG_HOME` when set and non-empty on any platform;
/// otherwise the platform default (`~/.config` on Linux, `Application
/// Support` on macOS, `AppData` on Windows).
///
/// # Errors
///
/// Returns [`ConfigError::Io`] when neither the XDG variable nor a platform
/// home directory can be determined.
fn config_dir() -> Result<PathBuf, ConfigError> {
    if let Some(base) = xdg_base("XDG_CONFIG_HOME") {
        return Ok(base.join(APPLICATION));
    }
    Ok(project_dirs()?.config_dir().to_path_buf())
}

/// Absolute path of the user's config file: `<config_dir>/config.toml`.
///
/// # Errors
///
/// Same failure modes as [`config_dir`].
pub fn config_file() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join(CONFIG_FILE_NAME))
}

/// Absolute path of the state directory: `<base>/todoist-tui`.
///
/// `<base>` is `XDG_STATE_HOME` when set and non-empty on any platform;
/// otherwise the platform default (`~/.local/state` on Linux; the project
/// data directory on macOS / Windows, where no conventional state home
/// exists — best-effort per NFR).
///
/// # Errors
///
/// Returns [`ConfigError::Io`] when neither the XDG variable nor a platform
/// home directory can be determined.
pub fn state_dir() -> Result<PathBuf, ConfigError> {
    if let Some(base) = xdg_base("XDG_STATE_HOME") {
        return Ok(base.join(APPLICATION));
    }
    let dirs = project_dirs()?;
    // `ProjectDirs::state_dir` is `None` on macOS and Windows; keep state
    // next to the data dir so the log file (T-006) has a durable home.
    Ok(dirs.state_dir().unwrap_or(dirs.data_dir()).to_path_buf())
}

/// Absolute path of the log file: `<state_dir>/todoist-tui.log`.
///
/// # Errors
///
/// Same failure modes as [`state_dir`].
pub fn log_file() -> Result<PathBuf, ConfigError> {
    Ok(state_dir()?.join(LOG_FILE_NAME))
}

/// Absolute path of the file-backed token fallback:
/// `<config_dir>/token`.
///
/// Used by [`crate::auth::file_source::FileTokenSource`] (T-104) as the
/// fallback token store when the OS keyring is unavailable. The file itself
/// is written with owner-only (`0600`) permissions by that backend; this
/// function only resolves the path.
///
/// # Errors
///
/// Same failure modes as [`config_dir`].
pub fn token_file() -> Result<PathBuf, ConfigError> {
    Ok(config_dir()?.join(TOKEN_FILE_NAME))
}

/// Creates the config directory (recursively) and returns its path.
///
/// Idempotent: calling it when the directory already exists succeeds.
///
/// # Errors
///
/// Returns [`ConfigError::Io`] when the config directory cannot be created,
/// or when the home directory cannot be determined.
pub fn ensure_config_dir() -> Result<PathBuf, ConfigError> {
    let dir = config_dir()?;
    fs::create_dir_all(&dir).map_err(|source| ConfigError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// Creates the state directory (recursively) and returns its path.
///
/// Idempotent: calling it when the directory already exists succeeds.
///
/// # Errors
///
/// Returns [`ConfigError::Io`] when the state directory cannot be created,
/// or when the home directory cannot be determined.
pub fn ensure_state_dir() -> Result<PathBuf, ConfigError> {
    let dir = state_dir()?;
    fs::create_dir_all(&dir).map_err(|source| ConfigError::Io {
        path: dir.clone(),
        source,
    })?;
    Ok(dir)
}

/// Returns `Some(dir)` when the environment variable is set and non-empty
/// (an empty value counts as unset, per the XDG Base Directory spec).
fn xdg_base(var: &str) -> Option<PathBuf> {
    env::var_os(var)
        .filter(|v| !v.is_empty())
        .map(PathBuf::from)
}

/// Platform project directories, or [`ConfigError::Io`] when no valid home
/// directory can be determined for the current platform.
fn project_dirs() -> Result<ProjectDirs, ConfigError> {
    ProjectDirs::from(QUALIFIER, ORGANIZATION, APPLICATION).ok_or_else(|| ConfigError::Io {
        path: PathBuf::new(),
        source: io::Error::new(
            io::ErrorKind::NotFound,
            "home directory could not be determined for the current platform",
        ),
    })
}

/// Unit tests (T-004). Environment variables are process-global and cargo
/// runs tests on parallel threads, so every test takes the shared
/// `crate::config::test_support::ENV_LOCK` through `EnvGuard`, which also
/// snapshots and restores the variables it touches. That lock is shared with
/// `config::load`'s tests (T-104 follow-up): both modules' `#[cfg(test)]`
/// code runs in the same test binary, so a per-module mutex gave no real
/// mutual exclusion between them. Tempdirs are built from
/// `std::env::temp_dir()` + pid + counter (no `tempfile` dev-dependency).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::test_support::ENV_LOCK;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::MutexGuard;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    const VARS: [&str; 3] = ["XDG_CONFIG_HOME", "XDG_STATE_HOME", "HOME"];

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            // A poisoned lock (earlier failed test) must not cascade.
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved = VARS.iter().map(|v| (*v, env::var_os(v))).collect();
            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
    }

    fn tempdir(tag: &str) -> PathBuf {
        let d = env::temp_dir().join(format!(
            "todoist-tui-t004-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn config_file_uses_xdg_config_home() {
        let _g = EnvGuard::new();
        let tmp = tempdir("cfg");
        env::set_var("XDG_CONFIG_HOME", &tmp);
        assert_eq!(
            config_file().unwrap(),
            tmp.join("todoist-tui").join("config.toml")
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn xdg_is_read_at_call_time() {
        let _g = EnvGuard::new();
        let a = tempdir("a");
        let b = tempdir("b");
        env::set_var("XDG_CONFIG_HOME", &a);
        assert!(config_file().unwrap().starts_with(&a));
        env::set_var("XDG_CONFIG_HOME", &b);
        assert!(config_file().unwrap().starts_with(&b));
        let _ = fs::remove_dir_all(&a);
        let _ = fs::remove_dir_all(&b);
    }

    #[test]
    fn state_dir_and_log_file_use_xdg_state_home() {
        let _g = EnvGuard::new();
        let tmp = tempdir("state");
        env::set_var("XDG_STATE_HOME", &tmp);
        assert_eq!(state_dir().unwrap(), tmp.join("todoist-tui"));
        assert_eq!(
            log_file().unwrap(),
            tmp.join("todoist-tui").join("todoist-tui.log")
        );
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_xdg_values_count_as_unset() {
        let _g = EnvGuard::new();
        let tmp = tempdir("empty");
        env::set_var("XDG_CONFIG_HOME", "");
        env::set_var("XDG_STATE_HOME", "");
        // Falls through to the platform default: must not be a relative
        // path rooted at "" (i.e. "todoist-tui/config.toml").
        let cfg = config_file().unwrap();
        assert!(cfg.is_absolute(), "got {cfg:?}");
        assert!(state_dir().unwrap().is_absolute());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn config_file_platform_default_ignores_tempdir() {
        let _g = EnvGuard::new();
        let tmp = tempdir("unset");
        env::remove_var("XDG_CONFIG_HOME");
        // Platform-dependent branch (ProjectDirs): Linux
        // ~/.config/todoist-tui, macOS ~/Library/Application Support/
        // com.todoist.todoist-tui, Windows AppData\...\todoist-tui\config.
        // Assert only the shape common to all, plus independence from the
        // redirected tempdir.
        let cfg = config_file().unwrap();
        assert!(cfg.is_absolute(), "got {cfg:?}");
        assert_eq!(cfg.file_name().unwrap(), "config.toml");
        assert!(!cfg.starts_with(&tmp));
        let s = cfg.to_string_lossy().into_owned();
        assert!(s.contains("todoist-tui"), "got {s}");
        #[cfg(target_os = "linux")]
        assert!(s.ends_with("todoist-tui/config.toml"), "got {s}");
        #[cfg(target_os = "macos")]
        assert!(s.contains("Application Support"), "got {s}");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn state_dir_platform_default_ignores_tempdir() {
        let _g = EnvGuard::new();
        let tmp = tempdir("unset-state");
        env::remove_var("XDG_STATE_HOME");
        let dir = state_dir().unwrap();
        assert!(dir.is_absolute(), "got {dir:?}");
        assert!(!dir.starts_with(&tmp));
        assert!(dir.to_string_lossy().contains("todoist-tui"));
        assert_eq!(
            log_file().unwrap(),
            dir.join("todoist-tui.log"),
            "log_file must be <state_dir>/todoist-tui.log"
        );
        #[cfg(target_os = "linux")]
        assert!(dir.ends_with(".local/state/todoist-tui"), "got {dir:?}");
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_state_dir_creates_recursively_and_is_idempotent() {
        let _g = EnvGuard::new();
        let tmp = tempdir("ensure");
        // Parent does not exist: exercises create_dir_all recursion.
        let base = tmp.join("deep").join("er");
        env::set_var("XDG_STATE_HOME", &base);
        let expected = base.join("todoist-tui");
        assert!(!expected.exists());
        assert_eq!(ensure_state_dir().unwrap(), expected);
        assert!(expected.is_dir());
        assert_eq!(ensure_state_dir().unwrap(), expected);
        assert!(expected.is_dir());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_state_dir_errors_with_io_when_path_is_a_file() {
        let _g = EnvGuard::new();
        let tmp = tempdir("blocked");
        let file = tmp.join("blocker");
        fs::write(&file, b"x").unwrap();
        env::set_var("XDG_STATE_HOME", &file); // base is a regular file
        match ensure_state_dir() {
            Err(ConfigError::Io { path, .. }) => {
                assert_eq!(path, file.join("todoist-tui"));
            }
            other => panic!("expected ConfigError::Io, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn unset_home_never_panics() {
        let _g = EnvGuard::new();
        env::remove_var("XDG_CONFIG_HOME");
        env::remove_var("XDG_STATE_HOME");
        env::remove_var("HOME");
        // On Unix `directories` falls back to the passwd database when HOME
        // is unset, so the home dir is usually still determinable and Ok is
        // legitimate; on a platform/sandbox where it is not, the contract is
        // Err(ConfigError::Io), never a panic. Assert exactly that.
        for r in [config_file(), state_dir(), log_file()] {
            match r {
                Ok(p) => assert!(p.is_absolute(), "got {p:?}"),
                Err(ConfigError::Io { .. }) => {}
                Err(e) => panic!("unexpected error variant: {e:?}"),
            }
        }
        env::set_var("HOME", "");
        for r in [config_file(), state_dir()] {
            match r {
                Ok(p) => assert!(p.is_absolute(), "got {p:?}"),
                Err(ConfigError::Io { .. }) => {}
                Err(e) => panic!("unexpected error variant: {e:?}"),
            }
        }
    }

    #[test]
    fn token_file_uses_xdg_config_home() {
        let _g = EnvGuard::new();
        let tmp = tempdir("token");
        env::set_var("XDG_CONFIG_HOME", &tmp);
        assert_eq!(token_file().unwrap(), tmp.join("todoist-tui").join("token"));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_config_dir_creates_recursively_and_is_idempotent() {
        let _g = EnvGuard::new();
        let tmp = tempdir("ensure-cfg");
        let base = tmp.join("deep").join("er");
        env::set_var("XDG_CONFIG_HOME", &base);
        let expected = base.join("todoist-tui");
        assert!(!expected.exists());
        assert_eq!(ensure_config_dir().unwrap(), expected);
        assert!(expected.is_dir());
        assert_eq!(ensure_config_dir().unwrap(), expected);
        assert!(expected.is_dir());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn xdg_base_helper_filters_empty_and_unset() {
        let _g = EnvGuard::new();
        env::remove_var("XDG_CONFIG_HOME");
        assert_eq!(xdg_base("XDG_CONFIG_HOME"), None);
        env::set_var("XDG_CONFIG_HOME", "");
        assert_eq!(xdg_base("XDG_CONFIG_HOME"), None);
        env::set_var("XDG_CONFIG_HOME", "/x");
        assert_eq!(xdg_base("XDG_CONFIG_HOME"), Some(Path::new("/x").into()));
    }
}

/// Extended T-104 coverage (unit-tester, different model family from the
/// coder): `token_file`/`config_file` sibling invariant, empty-XDG
/// fallthrough for the token path, `ensure_config_dir` failure when the
/// base path is a regular file, and that creating the config directory
/// conjures no files.
#[cfg(test)]
mod unit_tests {
    use super::*;
    use crate::config::test_support::ENV_LOCK;
    use std::ffi::OsString;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::MutexGuard;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    const VARS: [&str; 3] = ["XDG_CONFIG_HOME", "XDG_STATE_HOME", "HOME"];

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Vec<(&'static str, Option<OsString>)>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            let saved = VARS.iter().map(|v| (*v, env::var_os(v))).collect();
            Self { _lock: lock, saved }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            for (k, v) in &self.saved {
                match v {
                    Some(v) => env::set_var(k, v),
                    None => env::remove_var(k),
                }
            }
        }
    }

    fn tempdir(tag: &str) -> PathBuf {
        let d = env::temp_dir().join(format!(
            "todoist-tui-t104u-paths-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    #[test]
    fn token_file_is_a_sibling_of_config_file_in_the_config_dir() {
        // The token fallback must live beside config.toml (same config
        // dir), never in the state dir where T-006 puts logs.
        let _g = EnvGuard::new();
        let tmp = tempdir("sibling");
        env::set_var("XDG_CONFIG_HOME", &tmp);
        let tok = token_file().unwrap();
        let cfg = config_file().unwrap();
        assert_eq!(
            tok.parent(),
            cfg.parent(),
            "token must live beside config.toml"
        );
        assert_eq!(tok.file_name().unwrap(), "token");
        assert_ne!(tok, cfg);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn token_file_treats_empty_xdg_as_unset() {
        // Per the XDG spec an empty value counts as unset: must fall to
        // the platform default, never a relative "token/config"-ish path.
        let _g = EnvGuard::new();
        env::set_var("XDG_CONFIG_HOME", "");
        let tok = token_file().unwrap();
        assert!(tok.is_absolute(), "got {tok:?}");
        assert_eq!(tok.file_name().unwrap(), "token");
        #[cfg(target_os = "linux")]
        assert!(tok.ends_with(".config/todoist-tui/token"), "got {tok:?}");
    }

    #[test]
    fn ensure_config_dir_errors_with_io_when_base_is_a_file() {
        let _g = EnvGuard::new();
        let tmp = tempdir("blocked");
        let file = tmp.join("blocker");
        fs::write(&file, b"x").unwrap();
        env::set_var("XDG_CONFIG_HOME", &file); // base is a regular file
        match ensure_config_dir() {
            Err(ConfigError::Io { path, .. }) => {
                assert_eq!(path, file.join("todoist-tui"));
            }
            other => panic!("expected ConfigError::Io, got {other:?}"),
        }
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn ensure_config_dir_conjures_no_files() {
        // Directory creation only: no token file, no config.toml may
        // appear as a side effect (a TUI must never conjure config).
        let _g = EnvGuard::new();
        let tmp = tempdir("bare");
        env::set_var("XDG_CONFIG_HOME", &tmp);
        let dir = ensure_config_dir().unwrap();
        assert_eq!(dir, tmp.join("todoist-tui"));
        assert!(dir.is_dir());
        assert_eq!(
            fs::read_dir(&dir).unwrap().count(),
            0,
            "no files may appear as a side effect"
        );
        let _ = fs::remove_dir_all(&tmp);
    }
}
