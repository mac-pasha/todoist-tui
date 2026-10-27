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
/// runs tests on parallel threads, so every test takes `ENV_LOCK` through
/// `EnvGuard`, which also snapshots and restores the variables it touches.
/// A plain `Mutex` is used instead of a `serial_test` dependency (no new
/// crates; `Cargo.toml` is out of scope). Tempdirs are built from
/// `std::env::temp_dir()` + pid + counter (no `tempfile` dev-dependency).
#[cfg(test)]
mod tests {
    use super::*;
    use std::ffi::OsString;
    use std::path::Path;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::{Mutex, MutexGuard};

    static ENV_LOCK: Mutex<()> = Mutex::new(());
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
