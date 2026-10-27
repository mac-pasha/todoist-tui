//! TOML config loading (T-005, EPIC-01).
//!
//! [`Config::load`] resolves the platform config file through
//! [`paths::config_file`](crate::config::paths::config_file) at call time and
//! delegates to the pure [`Config::load_from`] seam, which reads and parses
//! exactly one file and nothing else — most importantly it never writes, so a
//! missing config file stays missing and the app starts on defaults. Parse
//! failures map to `ConfigError::Parse` with 1-based line/column recovered
//! from the `toml` parser's byte span.

// Same ahead-of-consumer situation as model.rs: `Config::load` has no caller
// until a later task wires the binary, and `pub` alone does not silence
// `dead_code` in a binary crate. Remove once a consumer exists.
#![allow(dead_code)]

use std::fs;
use std::io::ErrorKind;
use std::path::Path;

use crate::config::model::Config;
use crate::config::paths;
use crate::error::ConfigError;

impl Config {
    /// Loads the configuration from the platform config file.
    ///
    /// The path is resolved at call time: setting `XDG_CONFIG_HOME` (or
    /// changing `HOME`) before calling changes the result, which is what makes
    /// the loader testable without globals or caching (same contract as
    /// [`paths`]).
    ///
    /// A missing file is not an error: the returned [`Config`] is
    /// [`Config::default`] and the file is **not** created — a TUI must never
    /// conjure config the user never wrote.
    ///
    /// # Errors
    ///
    /// - `ConfigError::Io` — the file exists but cannot be read.
    /// - `ConfigError::Parse` — the file is not valid TOML, carries an unknown
    ///   key, or holds a value of the wrong shape.
    pub fn load() -> Result<Self, ConfigError> {
        let path = paths::config_file()?;
        Self::load_from(&path)
    }

    /// Loads the configuration from an explicit `path`.
    ///
    /// Pure test seam behind [`Config::load`]: path resolution happens in the
    /// caller, so tests can point this at a tempdir `config.toml` without
    /// touching the process environment.
    ///
    /// A missing file (or a missing parent directory) yields
    /// [`Config::default`], exactly like [`Config::load`]; the file is never
    /// created.
    ///
    /// # Errors
    ///
    /// Same as [`Config::load`].
    pub fn load_from(path: &Path) -> Result<Self, ConfigError> {
        let source = match fs::read_to_string(path) {
            Ok(source) => source,
            // Missing file *or* missing parent directory: both surface as
            // NotFound and both mean "no config yet" -> defaults.
            Err(source) if source.kind() == ErrorKind::NotFound => return Ok(Self::default()),
            Err(source) => {
                return Err(ConfigError::Io {
                    path: path.to_path_buf(),
                    source,
                });
            }
        };
        toml::from_str(&source).map_err(|error| parse_error(path, &source, &error))
    }
}

/// Maps a `toml` parse/serde error onto `ConfigError::Parse`.
///
/// The `toml` crate reports a byte span into the source (the parser yields
/// token boundaries, so span starts are char boundaries); converting it here
/// keeps the `ConfigError::Parse` 1-based position contract without leaking
/// parser internals. When a serde-level error arrives without a span, the
/// position falls back to 1:1 (the top of the file) rather than a
/// meaningless 0.
fn parse_error(path: &Path, source: &str, error: &toml::de::Error) -> ConfigError {
    let (line, column) = error
        .span()
        .map_or((1, 1), |span| line_col(source, span.start));
    ConfigError::Parse {
        path: path.to_path_buf(),
        line,
        column,
        message: error.message().to_owned(),
    }
}

/// Converts a byte offset in `source` to a 1-based `(line, column)` pair —
/// line is the newline count before the offset plus one, column the number of
/// chars since the last newline plus one, the same arithmetic the `toml` crate
/// uses when rendering its own errors.
///
/// Rounds a non-boundary offset down to the nearest char boundary first, so
/// the function stays panic-free for arbitrary input even though the parser
/// never produces such offsets.
fn line_col(source: &str, offset: usize) -> (usize, usize) {
    let mut offset = offset.min(source.len());
    while !source.is_char_boundary(offset) {
        offset -= 1;
    }
    let prefix = &source[..offset];
    let line = prefix.bytes().filter(|b| *b == b'\n').count();
    let column = prefix.chars().rev().take_while(|c| *c != '\n').count();
    (line + 1, column + 1)
}

/// Unit tests (T-005). `XDG_CONFIG_HOME` is process-global, so env-touching
/// tests take the shared `crate::config::test_support::ENV_LOCK` via
/// `EnvGuard`, which restores the variable on drop. That lock is shared with
/// `config::paths`'s tests (T-104 follow-up): both modules' `#[cfg(test)]`
/// code runs in the same test binary (not separate processes), so a
/// per-module `static ENV_LOCK` gave no real mutual exclusion between the
/// two sets of tests — a test here could still race a `paths` test mutating
/// the same variable.
#[cfg(test)]
mod tests {
    use super::*;
    use crate::config::model::DefaultView;
    use crate::config::test_support::ENV_LOCK;
    use std::env;
    use std::ffi::OsString;
    use std::path::PathBuf;
    use std::sync::atomic::{AtomicUsize, Ordering};
    use std::sync::MutexGuard;
    use std::time::Duration;

    static COUNTER: AtomicUsize = AtomicUsize::new(0);

    struct EnvGuard {
        _lock: MutexGuard<'static, ()>,
        saved: Option<OsString>,
    }

    impl EnvGuard {
        fn new() -> Self {
            let lock = ENV_LOCK
                .lock()
                .unwrap_or_else(std::sync::PoisonError::into_inner);
            Self {
                _lock: lock,
                saved: env::var_os("XDG_CONFIG_HOME"),
            }
        }
    }

    impl Drop for EnvGuard {
        fn drop(&mut self) {
            match &self.saved {
                Some(v) => env::set_var("XDG_CONFIG_HOME", v),
                None => env::remove_var("XDG_CONFIG_HOME"),
            }
        }
    }

    fn tempdir(tag: &str) -> PathBuf {
        let d = env::temp_dir().join(format!(
            "todoist-tui-t005-{}-{}-{tag}",
            std::process::id(),
            COUNTER.fetch_add(1, Ordering::SeqCst)
        ));
        fs::create_dir_all(&d).unwrap();
        d
    }

    /// `EnvGuard` now shares `ENV_LOCK` with `paths::tests` (T-104 follow-up),
    /// so a `paths` test cannot run concurrently with this one; the retry
    /// below is kept anyway as a defence against any future test that reads
    /// `XDG_CONFIG_HOME` without holding the shared lock — re-assert the
    /// variable and retry (bounded) when interference is detected. A real
    /// loader bug still fails because the retry only covers "env changed
    /// under us".
    fn load_with_xdg(tmp: &PathBuf) -> Config {
        for _ in 0..200 {
            env::set_var("XDG_CONFIG_HOME", tmp);
            let result = Config::load();
            if env::var_os("XDG_CONFIG_HOME").as_deref() == Some(tmp.as_os_str()) {
                return result.unwrap();
            }
        }
        panic!("XDG_CONFIG_HOME kept being changed by another test");
    }

    /// Writes `body` to a fresh tempdir `config.toml` and loads it.
    fn load_body(tag: &str, body: &str) -> (PathBuf, Result<Config, ConfigError>) {
        let dir = tempdir(tag);
        let path = dir.join("config.toml");
        fs::write(&path, body).unwrap();
        let r = Config::load_from(&path);
        let _ = fs::remove_dir_all(&dir);
        (path, r)
    }

    fn parse_err(r: Result<Config, ConfigError>) -> (PathBuf, usize, usize, String) {
        match r {
            Err(ConfigError::Parse {
                path,
                line,
                column,
                message,
            }) => (path, line, column, message),
            other => panic!("expected ConfigError::Parse, got {other:?}"),
        }
    }

    #[test]
    fn default_values() {
        let c = Config::default();
        assert_eq!(c.sync_interval, Duration::from_secs(60));
        assert_eq!(c.default_view, DefaultView::Inbox);
        assert!(c.keymap.is_empty());
        assert_eq!(c.theme, "default");
    }

    #[test]
    fn load_without_file_returns_defaults_and_creates_nothing() {
        let _g = EnvGuard::new();
        let tmp = tempdir("nofile");
        let c = load_with_xdg(&tmp);
        assert_eq!(c.sync_interval.as_secs(), 60);
        assert_eq!(c, Config::default());
        let file = tmp.join("todoist-tui").join("config.toml");
        assert!(!file.exists(), "load must not create the file");
        assert!(!file.parent().unwrap().exists(), "nor its directory");
        assert_eq!(fs::read_dir(&tmp).unwrap().count(), 0);
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_from_missing_path_and_missing_parent_yield_defaults() {
        let tmp = tempdir("missing");
        let p = tmp.join("no").join("such").join("config.toml");
        assert_eq!(Config::load_from(&p).unwrap(), Config::default());
        assert!(!tmp.join("no").exists());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn load_reads_xdg_config_home_file() {
        let _g = EnvGuard::new();
        let tmp = tempdir("xdg");
        let file = tmp.join("todoist-tui").join("config.toml");
        fs::create_dir_all(file.parent().unwrap()).unwrap();
        fs::write(&file, "sync_interval = 120\ndefault_view = \"today\"\n").unwrap();
        let c = load_with_xdg(&tmp);
        assert_eq!(c.sync_interval, Duration::from_secs(120));
        assert_eq!(c.default_view, DefaultView::Today);
        // untouched fields stay default
        assert_eq!(c.theme, Config::default().theme);
        assert!(c.keymap.is_empty());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn empty_file_yields_defaults() {
        let (_, r) = load_body("empty", "");
        assert_eq!(r.unwrap(), Config::default());
    }

    #[test]
    fn all_fields_and_views_parse() {
        let (_, r) = load_body(
            "full",
            "sync_interval = 5\ntheme = \"solar\"\ndefault_view = \"upcoming\"\n[keymap]\nquit = \"x\"\n\"weird key!\" = \"\"\n",
        );
        let c = r.unwrap();
        assert_eq!(c.sync_interval, Duration::from_secs(5));
        assert_eq!(c.theme, "solar");
        assert_eq!(c.default_view, DefaultView::Upcoming);
        assert_eq!(c.keymap.get("quit").map(String::as_str), Some("x"));
        assert_eq!(c.keymap.get("weird key!").map(String::as_str), Some(""));
        let (_, r) = load_body("inbox", "default_view = \"inbox\"");
        assert_eq!(r.unwrap().default_view, DefaultView::Inbox);
    }

    #[test]
    fn malformed_toml_reports_path_and_one_based_line() {
        let (path, r) = load_body(
            "bad",
            "sync_interval = 120\ntheme = \"ok\"\nthis is not toml\n",
        );
        let (p, line, column, _) = parse_err(r);
        assert_eq!(p, path);
        assert_eq!(line, 3);
        assert!(column >= 1);
    }

    #[test]
    fn malformed_on_first_line_is_line_one() {
        let (_, r) = load_body("bad1", "= nope\n");
        let (_, line, column, _) = parse_err(r);
        assert_eq!((line, column), (1, 1));
    }

    #[test]
    fn parse_error_display_contains_path_and_line() {
        let (path, r) = load_body("disp", "a = 1\nb = = 2\n");
        let msg = r.unwrap_err().to_string();
        assert!(msg.contains(&path.to_string_lossy().into_owned()), "{msg}");
        assert!(msg.contains("line 2"), "{msg}");
    }

    #[test]
    fn unknown_top_level_key_is_rejected_with_key_and_line() {
        // Documented in config/mod.rs and model.rs: unknown keys are REJECTED.
        let (path, r) = load_body("unk", "sync_interval = 10\nsyncc_interval = 5\n");
        let (p, line, _, message) = parse_err(r);
        assert_eq!(p, path);
        assert_eq!(line, 2);
        assert!(message.contains("syncc_interval"), "{message}");
    }

    #[test]
    fn unknown_key_alone_is_rejected() {
        let (_, r) = load_body("unk2", "bogus = true\n");
        assert!(matches!(r, Err(ConfigError::Parse { .. })));
    }

    #[test]
    fn keymap_entries_are_free_form_not_rejected() {
        let (_, r) = load_body("km", "[keymap]\nanything_goes = \"whatever\"\n");
        assert_eq!(r.unwrap().keymap.len(), 1);
    }

    #[test]
    fn wrong_value_shapes_are_parse_errors() {
        for (tag, body) in [
            ("neg", "sync_interval = -1"),
            ("float", "sync_interval = 1.5"),
            ("str", "sync_interval = \"60\""),
            ("view", "default_view = \"someday\""),
            ("viewcase", "default_view = \"Today\""),
            ("theme", "theme = 3"),
            ("keymap", "[keymap]\nquit = 1"),
        ] {
            let (_, r) = load_body(tag, body);
            assert!(matches!(r, Err(ConfigError::Parse { .. })), "{tag}: {r:?}");
        }
    }

    #[test]
    fn zero_sync_interval_is_accepted_as_is() {
        // Documents current behaviour: no range validation at load time.
        let (_, r) = load_body("zero", "sync_interval = 0");
        assert_eq!(r.unwrap().sync_interval, Duration::ZERO);
    }

    #[test]
    fn non_ascii_content_before_error_gives_char_columns_without_panic() {
        let (_, r) = load_body("utf8", "theme = \"héllo ✓\"\nbad = = 1\n");
        let (_, line, _, _) = parse_err(r);
        assert_eq!(line, 2);
        let (_, r) = load_body("utf8b", "théme = = 1\n");
        let (_, line, column, _) = parse_err(r);
        assert_eq!(line, 1);
        assert!(column >= 1);
    }

    #[test]
    fn directory_at_config_path_is_io_error_not_panic() {
        let tmp = tempdir("dirpath");
        let p = tmp.join("config.toml");
        fs::create_dir_all(&p).unwrap();
        assert!(matches!(Config::load_from(&p), Err(ConfigError::Io { .. })));
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn invalid_utf8_file_does_not_panic() {
        let tmp = tempdir("bin");
        let p = tmp.join("config.toml");
        fs::write(&p, [0xff, 0xfe, 0x00]).unwrap();
        assert!(Config::load_from(&p).is_err());
        let _ = fs::remove_dir_all(&tmp);
    }

    #[test]
    fn line_col_helper_boundaries() {
        assert_eq!(line_col("", 0), (1, 1));
        assert_eq!(line_col("ab\ncd", 3), (2, 1));
        assert_eq!(line_col("ab\ncd", 4), (2, 2));
        assert_eq!(line_col("ab", 999), (1, 3));
        // offset inside a multibyte char rounds down, no panic
        assert_eq!(line_col("é", 1), (1, 1));
    }

    #[test]
    fn debug_output_has_only_plain_fields() {
        // Fields: sync_interval, theme, keymap, default_view - no secrets.
        let c = Config::default();
        let d = format!("{c:?}").to_lowercase();
        for f in ["sync_interval", "theme", "keymap", "default_view"] {
            assert!(d.contains(f), "{d}");
        }
        for bad in ["token", "secret", "password", "api_key"] {
            assert!(!d.contains(bad), "{d}");
        }
    }

    #[test]
    fn env_guard_shares_one_lock_with_paths_tests() {
        // T-104 fix: this module and `paths::tests` must bind the single
        // shared lock in `config::test_support`, not private per-module
        // statics (two independent mutexes gave no cross-module mutual
        // exclusion and raced on XDG_CONFIG_HOME). Identity of the bound
        // static is what the fix hinges on, so assert it: re-introducing a
        // local `static ENV_LOCK` here makes this fail deterministically
        // instead of the old intermittent env-leak flake.
        let local: &std::sync::Mutex<()> = &ENV_LOCK;
        let shared: &std::sync::Mutex<()> = &crate::config::test_support::ENV_LOCK;
        assert!(
            std::ptr::eq(local, shared),
            "load tests must share one ENV_LOCK with paths tests"
        );
    }
}
