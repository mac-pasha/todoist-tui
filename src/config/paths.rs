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
