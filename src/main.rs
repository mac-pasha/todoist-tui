//! Entrypoint for `todoist-tui`.
//!
//! T-007 wires the CLI skeleton end to end: load config, initialise logging,
//! parse arguments, dispatch to a subcommand stub (or the not-yet-built TUI
//! default). UI, sync and storage still arrive in later epics.

/// Typed error hierarchy and crate-wide `Result` alias (T-003).
mod error;

/// Platform config/state directory resolution (T-004); TOML config loading
/// (T-005).
mod config;

/// Tracing to a rotating state-dir log file, with secret redaction (T-006).
mod log;

/// Clap CLI surface and subcommand stubs (T-007).
mod cli;

/// API token handling: the `Token` newtype wrapping `log::Secret` (T-101,
/// EPIC-02).
mod auth;

/// Todoist REST API client scaffold and error vocabulary (T-201, EPIC-03).
mod api;

/// Local SQLite store scaffold and error vocabulary (T-301, EPIC-04).
mod db;

/// Fuzzy matching core for task/filter search (T-902, EPIC-10).
mod search;

/// Offline command queue scaffold and error vocabulary (T-402, EPIC-05).
mod queue;

/// Sync engine scaffold and error vocabulary (T-501, EPIC-06).
mod sync;

use std::process::ExitCode;

use config::Config;
use error::AppError;
use log::LogOptions;

fn main() -> ExitCode {
    let cli = cli::parse();

    // `--config` is accepted on the CLI surface in this task; T-005's loader
    // only resolves the platform-default path today, so an explicit override
    // falls back to `Config::load_from` instead of re-deriving `config_dir`
    // logic here.
    let config_result = match &cli.config {
        Some(path) => Config::load_from(path).map_err(AppError::from),
        None => Config::load().map_err(AppError::from),
    };
    let _config = match config_result {
        Ok(config) => config,
        Err(error) => return fail(&error),
    };

    // Hold the guard for the process lifetime: dropping it flushes buffered
    // log records and stops the writer thread.
    let _log_guard = match log::init(LogOptions::new()) {
        Ok(guard) => guard,
        Err(error) => return fail(&error),
    };

    match cli::run(&cli) {
        Ok(()) => ExitCode::SUCCESS,
        Err(error) => fail(&error),
    }
}

/// Prints `error` to stderr and returns exit code 1 — no panic, no
/// backtrace, used for every failure path in `main` (config load, log init,
/// subcommand dispatch).
fn fail(error: &AppError) -> ExitCode {
    eprintln!("{error}");
    ExitCode::FAILURE
}

/// Bootstrap invariants for T-001 (EPIC-01).
///
/// The crate is greenfield: `main()` is a placeholder with no runtime surface,
/// so these tests pin the manifest / toolchain / gitignore invariants that the
/// task's acceptance items describe, by parsing the committed text files.
/// No production logic is exercised or mutated.
#[cfg(test)]
mod bootstrap_tests {
    use toml::Value;

    fn parse(path: &str, source: &str) -> Value {
        toml::from_str::<Value>(source)
            .unwrap_or_else(|e| panic!("`{path}` is not valid TOML: {e}"))
    }

    fn manifest() -> Value {
        parse("Cargo.toml", include_str!("../Cargo.toml"))
    }

    /// A version spec counts as major.minor pinned when it is exactly `x.y`
    /// with numeric components — no caret/`*`/`>=` prefix, no patch.
    fn is_major_minor(spec: &str) -> bool {
        let s = spec.trim();
        if s.starts_with(['^', '>', '<', '='])
            || s.contains('*')
            || s.contains('-')
            || s.contains('+')
        {
            return false;
        }
        let parts: Vec<&str> = s.split('.').filter(|p| !p.is_empty()).collect();
        parts.len() == 2 && parts[0].parse::<u32>().is_ok() && parts[1].parse::<u32>().is_ok()
    }

    fn dep<'a>(spec: &'a Value, name: &str) -> &'a Value {
        spec.get("dependencies")
            .and_then(|d| d.get(name))
            .unwrap_or_else(|| panic!("dependency `{name}` missing from Cargo.toml"))
    }

    fn dep_version(spec: &Value, name: &str) -> String {
        let tbl = dep(spec, name);
        // Bare-string syntax (`ratatui = "0.30"`) vs table syntax
        // (`tokio = { version = "1.54", ... }`).
        if let Some(v) = tbl.as_str() {
            return v.to_string();
        }
        tbl.get("version")
            .and_then(Value::as_str)
            .unwrap_or_else(|| panic!("dependency `{name}` has no version"))
            .to_string()
    }

    fn dep_features(spec: &Value, name: &str) -> Vec<String> {
        dep(spec, name)
            .get("features")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
            .iter()
            .map(|v| v.as_str().expect("feature must be a string").to_string())
            .collect()
    }

    // ---- Acceptance item 1: package / target / dependency stack ----

    #[test]
    fn package_name_and_edition() {
        let spec = manifest();
        assert_eq!(spec["package"]["name"].as_str(), Some("todoist-tui"));
        assert_eq!(spec["package"]["edition"].as_str(), Some("2021"));
    }

    #[test]
    fn single_binary_target_named_todoist_tui() {
        let spec = manifest();
        let bins = spec["bin"]
            .as_array()
            .cloned()
            .expect("[[bin]] must be an array");
        assert_eq!(
            bins.len(),
            1,
            "expected exactly one [[bin]] target, found {}",
            bins.len()
        );
        assert_eq!(bins[0]["name"].as_str(), Some("todoist-tui"));
        assert_eq!(bins[0]["path"].as_str(), Some("src/main.rs"));
    }

    #[test]
    fn required_dependencies_present_with_expected_shape() {
        let spec = manifest();
        // (name, features that MUST be present)
        let required: &[(&str, &[&str])] = &[
            ("ratatui", &[]),
            ("crossterm", &[]),
            ("tokio", &["rt-multi-thread", "macros"]),
            ("reqwest", &["json", "rustls-tls"]),
            ("rusqlite", &["bundled"]),
            ("serde", &[]),
            ("serde_json", &[]),
            ("keyring", &[]),
            ("uuid", &["v4"]),
            ("tracing", &[]),
            ("tracing-subscriber", &[]),
            ("tracing-appender", &[]),
            ("thiserror", &[]),
            ("anyhow", &[]),
            ("clap", &["derive"]),
            ("toml", &[]),
            ("directories", &[]),
        ];
        for (name, needed_features) in required {
            let name: &str = name;
            let needed_features: &[&str] = needed_features;
            let _ = dep_version(&spec, name); // panics if missing
            let feats = dep_features(&spec, name);
            for f in needed_features {
                assert!(
                    feats.iter().any(|x| x == f),
                    "{name}: missing feature `{f}` (has {feats:?})"
                );
            }
        }
    }

    #[test]
    fn called_out_crates_are_major_minor_pinned() {
        let spec = manifest();
        let pinned: &[&str] = &[
            "ratatui",
            "crossterm",
            "tokio",
            "reqwest",
            "rusqlite",
            "keyring",
            "uuid",
            "thiserror",
            "clap",
            "toml",
            "directories",
            "serde_json",
        ];
        for name in pinned {
            let v = dep_version(&spec, name);
            assert!(
                is_major_minor(&v),
                "{name} = \"{v}\" is not an exact major.minor pin"
            );
        }
    }

    #[test]
    fn major_only_pins_are_only_the_known_set() {
        // Adversarial guard: catch any NEW major-only pin creeping in. serde,
        // anyhow and the three tracing crates are currently pinned major-only
        // ("1.0" / "0.1" / "0.3" / "0.2") although the task text asks for
        // major.minor across the board — flagged in the unit-test report.
        let spec = manifest();
        let major_only: Vec<String> = spec["dependencies"]
            .as_table()
            .expect("[dependencies] missing")
            .iter()
            .filter_map(|(name, tbl)| {
                tbl.get("version")
                    .and_then(Value::as_str)
                    .filter(|v| !is_major_minor(v))
                    .map(|v| format!("{name} = \"{v}\""))
            })
            .collect();
        let allowed: &[&str] = &[
            "serde = \"1.0\"",
            "anyhow = \"1.0\"",
            "tracing = \"0.1\"",
            "tracing-subscriber = \"0.3\"",
            "tracing-appender = \"0.2\"",
        ];
        assert!(
            major_only.iter().all(|s| allowed.contains(&s.as_str())),
            "unexpected major-only version pins: {major_only:?}; \
             only the pre-existing set {allowed:?} was expected"
        );
    }

    // ---- Acceptance items 1 + 2: reqwest rustls-only, no OpenSSL ----

    #[test]
    fn reqwest_has_default_features_disabled_and_rustls_tls() {
        let spec = manifest();
        match dep(&spec, "reqwest")["default-features"].as_bool() {
            Some(false) => {}
            other => panic!("reqwest default-features must be false, got {other:?}"),
        }
        let feats = dep_features(&spec, "reqwest");
        assert!(
            feats.iter().any(|x| x == "json"),
            "reqwest missing feature json: {feats:?}"
        );
        assert!(
            feats.iter().any(|x| x == "rustls-tls"),
            "reqwest missing feature rustls-tls: {feats:?}"
        );
    }

    #[test]
    fn no_dependency_requests_openssl() {
        // Manifest-level proxy for `cargo tree -i openssl-sys` (that command
        // is run separately as PR evidence; the manifest feature set is the
        // source of truth for which TLS backend gets pulled in).
        let spec = manifest();
        for (name, tbl) in spec["dependencies"]
            .as_table()
            .expect("[dependencies] missing")
        {
            assert!(
                !name.starts_with("openssl"),
                "openssl-family dependency declared: {name}"
            );
            for f in tbl
                .get("features")
                .and_then(Value::as_array)
                .cloned()
                .unwrap_or_default()
                .iter()
                .filter_map(|v| v.as_str())
            {
                assert!(
                    !f.contains("openssl"),
                    "dependency {name} requests openssl feature `{f}`"
                );
            }
        }
    }

    // ---- Acceptance item 3: toolchain pin ----

    #[test]
    fn toolchain_pins_stable_1_99_1_with_rustfmt_and_clippy() {
        let spec = parse(
            "rust-toolchain.toml",
            include_str!("../rust-toolchain.toml"),
        );
        assert_eq!(
            spec["toolchain"]["channel"].as_str(),
            Some("1.99.1"),
            "toolchain channel must be pinned to stable 1.99.1"
        );
        let components = spec["toolchain"]["components"]
            .as_array()
            .cloned()
            .expect("toolchain.components must be an array");
        let comps: Vec<&str> = components.iter().filter_map(|v| v.as_str()).collect();
        assert!(
            comps.contains(&"rustfmt"),
            "missing rustfmt component: {comps:?}"
        );
        assert!(
            comps.contains(&"clippy"),
            "missing clippy component: {comps:?}"
        );
    }

    // ---- Acceptance item 4: placeholder entrypoint, no modules ----

    #[test]
    fn main_source_declares_no_modules() {
        let src = include_str!("main.rs");
        let offenders: Vec<String> = src
            .lines()
            .map(str::trim_start)
            .filter(|l| {
                l.starts_with("mod ")
                    || l.starts_with("pub mod ")
                    || l.starts_with("pub(crate) mod ")
            })
            // Ignore this test module's own declaration; acceptance asks
            // that the placeholder crate declares no production modules.
            .filter(|l| !l.starts_with("mod bootstrap_tests"))
            // T-003 requires `mod error;`, T-004 requires `mod config;`,
            // T-006 requires `mod log;`, T-007 requires `mod cli;`, T-101
            // requires `mod auth;`, T-201 requires `mod api;`, T-301
            // requires `mod db;`, T-902 requires `mod search;`, T-402
            // requires `mod queue;`. The only other permitted lines are the
            // cfg(test) modules below.
            .filter(|l| {
                *l != "mod error;"
                    && *l != "mod config;"
                    && *l != "mod log;"
                    && *l != "mod cli;"
                    && *l != "mod auth;"
                    && *l != "mod api;"
                    && *l != "mod db;"
                    && *l != "mod search;"
                    && *l != "mod queue;"
                    && *l != "mod sync;"
                    && !l.starts_with("mod error_tests")
            })
            .map(str::to_string)
            .collect();
        assert!(
            offenders.is_empty(),
            "unexpected module declarations: {offenders:?}"
        );
    }

    // ---- Acceptance item 6: .gitignore ----

    #[test]
    fn gitignore_ignores_target_but_not_cargo_lock() {
        let raw = include_str!("../.gitignore");
        let lines: Vec<&str> = raw
            .lines()
            .map(str::trim)
            .filter(|l| !l.is_empty() && !l.starts_with('#'))
            .collect();
        let ignores_target = lines
            .iter()
            .any(|l| l.trim_start_matches('-').trim_start_matches('!') == "/target");
        assert!(ignores_target, ".gitignore must ignore /target: {lines:?}");
        let ignores_lock = lines.iter().any(|l| {
            let pat = l.trim_start_matches('-').trim_start_matches('!');
            pat == "Cargo.lock" || pat == "/Cargo.lock" || pat == "**/Cargo.lock"
        });
        assert!(
            !ignores_lock,
            ".gitignore must NOT ignore Cargo.lock: {lines:?}"
        );
    }

    // ---- Acceptance item 5: Cargo.lock committed (git-level, checked here) ----

    #[test]
    fn cargo_lock_is_tracked_in_git() {
        let out = std::process::Command::new("git")
            .args(["ls-files", "--error-unmatch", "Cargo.lock"])
            .output()
            .expect("git available in test env");
        assert!(
            out.status.success(),
            "Cargo.lock must be committed (git ls-files --error-unmatch Cargo.lock failed: {})",
            String::from_utf8_lossy(&out.stderr)
        );
    }
}

/// Unit tests for `error.rs` (T-003). Kept here because `error.rs` is
/// production code that the tester must not edit.
#[cfg(test)]
mod error_tests {
    use crate::error::{ApiError, AppError, AuthError, ConfigError, LogError, Result};
    use std::error::Error as _;
    use std::io;
    use std::path::PathBuf;
    use std::time::Duration;

    fn ioe() -> io::Error {
        io::Error::new(io::ErrorKind::PermissionDenied, "denied")
    }

    fn parse(path: &str, line: usize, col: usize, msg: &str) -> ConfigError {
        ConfigError::Parse {
            path: PathBuf::from(path),
            line,
            column: col,
            message: msg.into(),
        }
    }

    // Acceptance 4
    #[test]
    fn app_error_from_parse_contains_path_and_line() {
        let e = AppError::from(parse("/tmp/fake/config.toml", 42, 7, "bad key"));
        let s = e.to_string();
        assert!(s.contains("/tmp/fake/config.toml"), "{s}");
        assert!(s.contains("42"), "{s}");
        assert!(s.contains('7'), "{s}");
        assert!(s.contains("bad key"), "{s}");
        assert!(matches!(
            e,
            AppError::Config(ConfigError::Parse { line: 42, .. })
        ));
    }

    // Acceptance 3
    #[test]
    fn every_file_variant_display_includes_path() {
        let p = "/fake/dir/file.x";
        let errs: Vec<String> = vec![
            parse(p, 1, 1, "m").to_string(),
            ConfigError::Io {
                path: p.into(),
                source: ioe(),
            }
            .to_string(),
            LogError::CreateDir {
                path: p.into(),
                source: ioe(),
            }
            .to_string(),
            LogError::OpenFile {
                path: p.into(),
                source: ioe(),
            }
            .to_string(),
            AppError::from(ConfigError::Io {
                path: p.into(),
                source: ioe(),
            })
            .to_string(),
            AppError::from(LogError::CreateDir {
                path: p.into(),
                source: ioe(),
            })
            .to_string(),
            AppError::from(LogError::OpenFile {
                path: p.into(),
                source: ioe(),
            })
            .to_string(),
        ];
        for s in errs {
            assert!(s.contains(p), "missing path in {s:?}");
        }
    }

    #[test]
    fn io_variants_include_underlying_message_and_chain_source() {
        let e = ConfigError::Io {
            path: "/x/c.toml".into(),
            source: ioe(),
        };
        assert!(e.to_string().contains("denied"));
        assert_eq!(e.source().unwrap().to_string(), "denied");
        let l = LogError::OpenFile {
            path: "/x/l".into(),
            source: ioe(),
        };
        assert_eq!(l.source().unwrap().to_string(), "denied");
        // AppError::Config delegates Display; source() is the inner error.
        let a = AppError::from(e);
        assert!(a.source().is_some());
    }

    #[test]
    fn parse_variant_has_no_source() {
        assert!(parse("/p", 1, 1, "m").source().is_none());
    }

    #[test]
    fn app_error_prefixes_and_not_implemented() {
        let c = AppError::from(parse("/p", 1, 2, "m")).to_string();
        assert!(c.starts_with("config: "), "{c}");
        let l = AppError::from(LogError::OpenFile {
            path: "/p".into(),
            source: ioe(),
        })
        .to_string();
        assert!(l.starts_with("logging: "), "{l}");
        assert_eq!(
            AppError::NotImplemented("sync").to_string(),
            "not implemented: sync"
        );
    }

    // Adversarial: paths show (even when they look sensitive); fields are
    // structurally only PathBuf/usize/io::Error/Parser message -- no variant
    // has a dedicated secret/token field (verified by code inspection).
    #[test]
    fn sensitive_looking_path_is_displayed_and_debugged() {
        let p = "/home/u/token/secret.toml";
        let e = AppError::from(parse(p, 3, 4, "m"));
        assert!(e.to_string().contains("token"));
        assert!(format!("{e:?}").contains(p));
    }

    #[test]
    fn display_does_not_include_debug_struct_noise() {
        let s = AppError::from(parse("/p", 1, 1, "m")).to_string();
        assert!(!s.contains("Parse {") && !s.contains("PathBuf"), "{s}");
    }

    #[test]
    fn empty_path_does_not_panic() {
        let s = parse("", 0, 0, "").to_string();
        assert!(s.contains("line 0, column 0"), "{s}");
        let _ = ConfigError::Io {
            path: PathBuf::new(),
            source: ioe(),
        }
        .to_string();
        let _ = format!(
            "{:?}",
            LogError::CreateDir {
                path: PathBuf::new(),
                source: ioe()
            }
        );
    }

    #[test]
    fn very_long_message_is_preserved_untruncated() {
        let msg = "x".repeat(100_000);
        let s = parse("/p", usize::MAX, usize::MAX, &msg).to_string();
        assert!(s.contains(&msg));
        assert!(s.contains(&usize::MAX.to_string()));
    }

    #[test]
    fn path_with_spaces_and_unicode_renders() {
        let s = parse("/tmp/мой конфиг/c o.toml", 1, 1, "m").to_string();
        assert!(s.contains("/tmp/мой конфиг/c o.toml"), "{s}");
    }

    #[test]
    fn result_alias_propagates_with_question_mark() {
        fn inner() -> std::result::Result<(), ConfigError> {
            Err(parse("/p", 9, 1, "m"))
        }
        fn outer() -> Result<()> {
            inner()?;
            Ok(())
        }
        let e = outer().unwrap_err();
        assert!(matches!(e, AppError::Config(_)));
        assert!(e.to_string().contains("line 9"));
    }

    #[test]
    fn errors_are_send_sync_static() {
        fn assert_traits<T: std::error::Error + Send + Sync + 'static>() {}
        assert_traits::<AppError>();
        assert_traits::<ConfigError>();
        assert_traits::<LogError>();
    }

    // T-101 acceptance 2: adversarial proof that no `AuthError` variant can
    // leak raw token material through `{}` or `{:?}`. The fixture value
    // below is a fake token string, never a real credential.
    #[test]
    fn auth_error_variants_never_render_token_material() {
        use crate::error::AuthError;

        const FIXTURE_TOKEN: &str = "tok_fake_abc123";

        // Note: the `Io` variant legitimately renders its `path` field (by
        // design, same as `ConfigError::Io`); the path below deliberately
        // does NOT embed the fixture token so this test proves no variant
        // leaks token *material* specifically, not that paths are hidden.
        let p = PathBuf::from("/home/u/.config/todoist-tui/token");
        let variants: Vec<AuthError> = vec![
            AuthError::NotAuthenticated,
            AuthError::InvalidToken,
            AuthError::Backend {
                source: keyring::Error::NoEntry,
            },
            AuthError::Io {
                path: p,
                source: ioe(),
            },
            AuthError::EmptyToken,
        ];
        for v in &variants {
            let disp = v.to_string();
            let dbg = format!("{v:?}");
            assert!(
                !disp.contains(FIXTURE_TOKEN),
                "Display leaked fixture token: {disp:?}"
            );
            assert!(
                !dbg.contains(FIXTURE_TOKEN),
                "Debug leaked fixture token: {dbg:?}"
            );
        }

        // `AppError::Auth` renders with the documented "auth: " prefix and
        // still never leaks the fixture token via either formatter.
        let wrapped = AppError::from(AuthError::InvalidToken);
        assert!(wrapped.to_string().starts_with("auth: "), "{wrapped}");
        assert!(!wrapped.to_string().contains(FIXTURE_TOKEN));
        assert!(!format!("{wrapped:?}").contains(FIXTURE_TOKEN));
    }

    // ---- T-101 extended coverage (unit-tester) ----

    /// Every `AuthError` variant, with benign (non-secret) payloads.
    fn auth_variants() -> Vec<AuthError> {
        vec![
            AuthError::NotAuthenticated,
            AuthError::InvalidToken,
            AuthError::Backend {
                source: keyring::Error::NoEntry,
            },
            AuthError::Io {
                path: PathBuf::from("/home/fake/.config/todoist-tui/token"),
                source: ioe(),
            },
            AuthError::EmptyToken,
        ]
    }

    #[test]
    fn auth_error_each_variant_display_carries_expected_context() {
        // Unit variants keep their full message; Backend wraps its source's
        // message; Io names the offending file path and the OS error.
        assert!(AuthError::NotAuthenticated
            .to_string()
            .contains("not authenticated"));
        assert!(AuthError::NotAuthenticated
            .to_string()
            .contains("no API token is stored"));
        assert!(AuthError::InvalidToken
            .to_string()
            .contains("the API token is invalid"));

        let backend = AuthError::Backend {
            source: keyring::Error::NoEntry,
        };
        let s = backend.to_string();
        assert!(s.contains("keyring backend failed"), "{s}");
        assert!(s.contains("No matching credential found"), "{s}");

        let path = "/home/fake/.config/todoist-tui/token";
        let io_err = AuthError::Io {
            path: PathBuf::from(path),
            source: ioe(),
        };
        let s = io_err.to_string();
        assert!(s.contains("token file I/O failed"), "{s}");
        assert!(s.contains(path), "path missing: {s}");
        assert!(s.contains("denied"), "source message missing: {s}");

        assert!(AuthError::EmptyToken
            .to_string()
            .contains("the API token must not be empty"));
    }

    #[test]
    fn auth_error_io_renders_spaced_and_unicode_paths() {
        // Same path-visibility contract as ConfigError::Io (T-003): the
        // offending file is named verbatim, even with spaces/unicode.
        let p = "/tmp/fäke dir/töken.toml";
        let s = AuthError::Io {
            path: PathBuf::from(p),
            source: ioe(),
        }
        .to_string();
        assert!(s.contains(p), "{s}");
    }

    #[test]
    fn auth_error_source_chain_backend_and_io_only() {
        // `#[source]`-annotated variants expose their cause; the unit
        // variants have none (mirrors the ConfigError contract).
        use std::error::Error as _;

        assert!(AuthError::NotAuthenticated.source().is_none());
        assert!(AuthError::InvalidToken.source().is_none());
        assert!(AuthError::EmptyToken.source().is_none());

        let backend = AuthError::Backend {
            source: keyring::Error::NoEntry,
        };
        let src = backend.source().expect("Backend must expose source");
        assert_eq!(src.to_string(), "No matching credential found");

        let io_err = AuthError::Io {
            path: PathBuf::from("/tmp/fake/token"),
            source: io::Error::new(io::ErrorKind::NotFound, "no such file"),
        };
        let src = io_err.source().expect("Io must expose source");
        assert_eq!(src.to_string(), "no such file");
    }

    #[test]
    fn app_error_wraps_every_auth_variant_with_auth_prefix() {
        for v in auth_variants() {
            let expected = format!("auth: {v}");
            let wrapped = AppError::from(v);
            assert!(matches!(wrapped, AppError::Auth(_)));
            assert_eq!(wrapped.to_string(), expected);
            assert!(wrapped.to_string().starts_with("auth: "), "{wrapped}");
            assert!(format!("{wrapped:?}").contains("Auth"));
        }
    }

    #[test]
    fn auth_error_propagates_via_question_mark_into_app_error() {
        fn inner() -> std::result::Result<(), AuthError> {
            Err(AuthError::EmptyToken)
        }
        fn outer() -> Result<()> {
            inner()?;
            Ok(())
        }
        let e = outer().unwrap_err();
        assert!(matches!(e, AppError::Auth(AuthError::EmptyToken)));
        assert_eq!(e.to_string(), "auth: the API token must not be empty");
    }

    #[test]
    fn auth_error_is_error_send_sync_static() {
        fn assert_traits<T: std::error::Error + Send + Sync + 'static>() {}
        assert_traits::<AuthError>();
    }

    #[test]
    fn auth_error_variants_never_leak_realistic_secret_shapes() {
        // Goes beyond the single fixture above: several realistic
        // secret-shaped strings (hex digest, PAT shape, bearer header,
        // base64 blob, marker-colliding, unicode) must never appear in any
        // variant's Display or Debug, bare or wrapped in `AppError::Auth`.
        // All fixture values are fakes.
        const FAKE_SECRETS: [&str; 7] = [
            "tok_fake_abc123",
            "deadbeefcafebabe1234567890abcdef",
            "ghp_fake16C7e42F292c6912E7710c838347Ae17",
            "Bearer fake 9f8e7d6c5b4a3928",
            "c2VjcmV0LWZha2UtdG9rZW4=",
            "tok***fake",
            "токе́н_fäke",
        ];
        for v in &auth_variants() {
            let disp = v.to_string();
            let dbg = format!("{v:?}");
            for s in FAKE_SECRETS {
                assert!(!disp.contains(s), "Display leaked {s:?} via {dbg:?}");
                assert!(!dbg.contains(s), "Debug leaked {s:?} via {dbg:?}");
            }
        }
        for w in auth_variants().into_iter().map(AppError::from) {
            let disp = w.to_string();
            let dbg = format!("{w:?}");
            for s in FAKE_SECRETS {
                assert!(
                    !disp.contains(s),
                    "AppError::Auth Display leaked {s:?} via {disp:?}"
                );
                assert!(
                    !dbg.contains(s),
                    "AppError::Auth Debug leaked {s:?} via {dbg:?}"
                );
            }
        }
    }

    // ==== T-201 (EPIC-03): `ApiError` vocabulary and the `api` scaffold ====
    //
    // Covers acceptance items 1-5. Item 6 (fmt/clippy/test green) is proven
    // by running the three commands, not from inside a test.

    /// Fake token fixture required by acceptance item 3. Never a real
    /// credential, and no code path may ever render it.
    const API_FIXTURE_TOKEN: &str = "tok_fake_abc123";

    /// A `reqwest::Error` built fully offline (no socket is opened): a
    /// relative URL makes `RequestBuilder::build` fail with `Kind::Builder`.
    /// Stands in for the opaque transport/decode sources `ApiError` wraps.
    fn reqwest_stub_error() -> reqwest::Error {
        reqwest::Client::new()
            .get("::not a url")
            .build()
            .expect_err("a relative URL must fail to build")
    }

    /// Every `ApiError` variant with benign, non-secret payloads.
    fn api_variants() -> Vec<ApiError> {
        vec![
            ApiError::Unauthorized,
            ApiError::RateLimited { retry_after: None },
            ApiError::RateLimited {
                retry_after: Some(Duration::from_secs(30)),
            },
            ApiError::Server { status: 500 },
            ApiError::Server { status: 503 },
            ApiError::Transport {
                source: reqwest_stub_error(),
            },
            ApiError::Decode {
                source: reqwest_stub_error(),
            },
            ApiError::InsecureBaseUrl,
            ApiError::RetriesExhausted { attempts: 3 },
        ]
    }

    /// Acceptance item 1: `ApiError` must be `#[non_exhaustive]` +
    /// `thiserror`-derived. `#[non_exhaustive]` is not observable from a
    /// test body, so it is pinned from the committed source text (same
    /// technique as `bootstrap_tests`); the variant *set* is proven at
    /// compile time by `api_variants()`.
    #[test]
    fn api_error_is_non_exhaustive_and_thiserror_derived() {
        let src = include_str!("error.rs");
        let lines: Vec<&str> = src.lines().map(str::trim).collect();
        let decl = lines
            .iter()
            .position(|l| *l == "pub enum ApiError {")
            .expect("`pub enum ApiError {` missing from src/error.rs");

        // Attributes sit immediately above the declaration (doc comments
        // are further up).
        let attributes: Vec<&str> = lines[..decl].iter().rev().take(3).copied().collect();
        assert!(
            attributes.contains(&"#[non_exhaustive]"),
            "ApiError must be #[non_exhaustive]; preceding lines: {attributes:?}"
        );
        assert!(
            attributes
                .iter()
                .any(|l| l.starts_with("#[derive(") && l.contains("Debug") && l.contains("Error")),
            "ApiError must derive Debug + thiserror::Error; preceding lines: {attributes:?}"
        );

        // Every required variant name must be declared in the enum body.
        let body: Vec<&str> = lines[decl..].to_vec();
        for required in [
            "Unauthorized,",
            "RateLimited {",
            "Server {",
            "Transport {",
            "Decode {",
            "InsecureBaseUrl,",
            "RetriesExhausted {",
        ] {
            assert!(
                body.contains(&required),
                "ApiError variant `{required}` not found in src/error.rs"
            );
        }
    }

    /// Anti-staleness guard for acceptance item 3: the `api_variants()`
    /// fixture must exercise *every* variant declared in `error.rs`, so a
    /// future variant cannot silently escape the token-leak scan. Variant
    /// names are read from the committed source (unit- and struct-like
    /// declarations alike) and compared with the names the fixtures produce.
    #[test]
    fn api_variants_fixture_covers_every_declared_variant() {
        let src = include_str!("error.rs");
        let lines: Vec<&str> = src.lines().map(str::trim).collect();
        let start = lines
            .iter()
            .position(|l| *l == "pub enum ApiError {")
            .expect("ApiError enum missing");
        let mut declared: Vec<String> = Vec::new();
        for line in &lines[start + 1..] {
            if *line == "}" {
                break;
            }
            if let Some(name) = line
                .strip_suffix(',')
                .or_else(|| line.strip_suffix(" {"))
                .map(str::trim)
            {
                if !name.is_empty()
                    && name.starts_with(char::is_uppercase)
                    && name.chars().all(char::is_alphanumeric)
                {
                    declared.push(name.to_string());
                }
            }
        }
        assert_eq!(
            declared.len(),
            7,
            "expected 7 ApiError variants, parsed {declared:?}"
        );

        // Distinct variant names exercised by the fixture set (Debug output
        // starts with the variant name for thiserror-derived enums).
        let mut exercised: Vec<String> = api_variants()
            .iter()
            .map(|v| {
                format!("{v:?}")
                    .chars()
                    .take_while(|c| c.is_alphanumeric())
                    .collect::<String>()
            })
            .collect();
        exercised.sort_unstable();
        exercised.dedup();

        let mut missing: Vec<&String> =
            declared.iter().filter(|d| !exercised.contains(d)).collect();
        missing.sort();
        assert!(
            missing.is_empty(),
            "these ApiError variants are not covered by api_variants(), so the \
             token-leak scan would skip them: {missing:?}"
        );
        // ... and the fixtures add nothing that error.rs does not declare.
        let declared_sorted = {
            let mut d = declared.clone();
            d.sort_unstable();
            d
        };
        assert_eq!(exercised, declared_sorted);
    }

    /// Acceptance item 1: `is_retryable` must exist with that exact
    /// signature on `ApiError`.
    #[test]
    fn api_error_exposes_is_retryable() {
        let _: fn(&ApiError) -> bool = ApiError::is_retryable;
        // And it is not a blanket allow/deny: of the 9 fixtures, exactly 5
        // (two `RateLimited`, two `Server`, one `Transport`) are retryable.
        let retryable = api_variants().iter().filter(|e| e.is_retryable()).count();
        assert_eq!(retryable, 5, "expected 5 retryable fixtures of 9");
    }

    /// Acceptance item 3: no variant renders the fixture token through
    /// `{}` or `{:?}`, bare or wrapped in `AppError::Api`.
    #[test]
    fn api_error_variants_never_render_fixture_token() {
        assert_eq!(API_FIXTURE_TOKEN, "tok_fake_abc123");
        for v in api_variants() {
            let disp = v.to_string();
            let dbg = format!("{v:?}");
            assert!(
                !disp.contains(API_FIXTURE_TOKEN),
                "ApiError Display leaked fixture token: {disp:?}"
            );
            assert!(
                !dbg.contains(API_FIXTURE_TOKEN),
                "ApiError Debug leaked fixture token: {dbg:?}"
            );

            let wrapped = AppError::from(v);
            let disp = wrapped.to_string();
            let dbg = format!("{wrapped:?}");
            assert!(
                !disp.contains(API_FIXTURE_TOKEN),
                "AppError::Api Display leaked fixture token: {disp:?}"
            );
            assert!(
                !dbg.contains(API_FIXTURE_TOKEN),
                "AppError::Api Debug leaked fixture token: {dbg:?}"
            );
        }
    }

    /// Acceptance item 3, adversarial half: the only token-adjacent surface
    /// `ApiError` has is the opaque `reqwest::Error` source, which renders
    /// verbatim — including the URL reqwest attached to it. The production
    /// client must therefore keep the token out of URLs (it travels in the
    /// `Authorization` header) and/or sanitise the source with
    /// `reqwest::Error::without_url` before wrapping. Pinned here so the
    /// hazard stays visible.
    #[test]
    fn api_error_transport_source_renders_its_url_verbatim() {
        let source = reqwest::Client::new()
            .get("mailto:fake.bad.scheme.invalid")
            .build()
            .expect_err("mailto: URLs are rejected by reqwest");
        let rendered = ApiError::Transport { source }.to_string();
        assert!(
            rendered.starts_with("request to the Todoist API failed: "),
            "{rendered}"
        );
        assert!(rendered.contains("fake.bad.scheme.invalid"), "{rendered}");

        // Sanitised the way the future HTTP client must: the URL (the only
        // way a mis-built request smuggles a token into an error) is gone.
        let sanitised = reqwest::Client::new()
            .get("mailto:fake.bad.scheme.invalid")
            .build()
            .expect_err("mailto: URLs are rejected by reqwest")
            .without_url();
        let rendered = ApiError::Transport { source: sanitised }.to_string();
        assert!(!rendered.contains("fake.bad.scheme.invalid"), "{rendered}");
    }

    /// Non-vacuity guard for the leak scan above: prove the
    /// `!contains(FIXTURE_TOKEN)` technique actually detects a leak. Without
    /// this, a typosquatted fixture constant could make the token-free test
    /// pass forever while a real leak goes unnoticed.
    #[test]
    fn api_error_token_leak_scan_is_not_vacuous() {
        let source = reqwest::Client::new()
            .get(format!("mailto:{API_FIXTURE_TOKEN}"))
            .build()
            .expect_err("mailto: URLs are rejected by reqwest");
        let rendered = ApiError::Transport { source }.to_string();
        assert!(
            rendered.contains(API_FIXTURE_TOKEN),
            "the scan used by the token-free test must be able to see a leak: {rendered:?}"
        );
    }

    /// Byte-exact Display pin (repo convention for `AuthError`), proving the
    /// `#[error(...)]` messages carry no token slot and no `{:?}` noise.
    #[test]
    fn api_error_display_is_byte_exact() {
        assert_eq!(
            ApiError::Unauthorized.to_string(),
            "the API token is unauthorized"
        );
        assert_eq!(
            ApiError::RateLimited { retry_after: None }.to_string(),
            "rate limited by the Todoist API"
        );
        assert_eq!(
            ApiError::Server { status: 503 }.to_string(),
            "the Todoist API returned a server error: HTTP 503"
        );
        assert_eq!(
            ApiError::InsecureBaseUrl.to_string(),
            "refusing to call the Todoist API over a non-HTTPS URL"
        );
        assert_eq!(
            ApiError::RetriesExhausted { attempts: 3 }.to_string(),
            "request to the Todoist API failed after 3 attempt(s)"
        );
        assert_eq!(
            AppError::from(ApiError::RetriesExhausted { attempts: 1 }).to_string(),
            "api: request to the Todoist API failed after 1 attempt(s)"
        );
    }

    /// Acceptance item 4: `is_retryable` is true for `Server`,
    /// `RateLimited` and `Transport` only, per variant.
    #[test]
    fn api_error_retryable_only_for_server_rate_limited_and_transport() {
        assert!(ApiError::Server { status: 500 }.is_retryable());
        assert!(ApiError::Server { status: 503 }.is_retryable());
        assert!(ApiError::RateLimited { retry_after: None }.is_retryable());
        assert!(ApiError::RateLimited {
            retry_after: Some(Duration::from_secs(30))
        }
        .is_retryable());
        assert!(ApiError::Transport {
            source: reqwest_stub_error()
        }
        .is_retryable());

        assert!(!ApiError::Unauthorized.is_retryable());
        assert!(!ApiError::Decode {
            source: reqwest_stub_error()
        }
        .is_retryable());
        assert!(!ApiError::InsecureBaseUrl.is_retryable());
        assert!(!ApiError::RetriesExhausted { attempts: 0 }.is_retryable());
        assert!(!ApiError::RetriesExhausted { attempts: 3 }.is_retryable());
    }

    /// Independent, exhaustive oracle for acceptance item 4. Written as a
    /// `match` on every variant (legal inside the crate even though the enum
    /// is `#[non_exhaustive]`) so a future variant forces a deliberate
    /// retry classification instead of silently defaulting to `false`.
    fn expected_is_retryable(err: &ApiError) -> bool {
        match err {
            ApiError::Server { .. } | ApiError::RateLimited { .. } | ApiError::Transport { .. } => {
                true
            }
            ApiError::Unauthorized
            | ApiError::Decode { .. }
            | ApiError::InsecureBaseUrl
            | ApiError::RetriesExhausted { .. } => false,
        }
    }

    #[test]
    fn api_error_retryable_matches_the_full_variant_table() {
        for v in api_variants() {
            assert_eq!(
                v.is_retryable(),
                expected_is_retryable(&v),
                "is_retryable() disagrees with the table for {v:?}"
            );
        }
    }

    /// Acceptance item 2: `AppError::Api(#[from] ApiError)` renders with the
    /// documented `api: ` prefix, matching `Auth(..)`'s style.
    #[test]
    fn app_error_wraps_every_api_variant_with_api_prefix() {
        for v in api_variants() {
            let expected = format!("api: {v}");
            let wrapped = AppError::from(v);
            assert!(matches!(wrapped, AppError::Api(_)));
            assert_eq!(wrapped.to_string(), expected);
            assert!(wrapped.to_string().starts_with("api: "), "{wrapped}");
            assert!(format!("{wrapped:?}").contains("Api"));
        }
    }

    #[test]
    fn api_error_propagates_via_question_mark_into_app_error() {
        fn inner() -> std::result::Result<(), ApiError> {
            Err(ApiError::Unauthorized)
        }
        fn outer() -> Result<()> {
            inner()?;
            Ok(())
        }
        let e = outer().expect_err("inner() always fails");
        assert!(matches!(e, AppError::Api(ApiError::Unauthorized)));
        assert_eq!(e.to_string(), "api: the API token is unauthorized");
    }

    #[test]
    fn api_error_is_error_send_sync_static() {
        fn assert_traits<T: std::error::Error + Send + Sync + 'static>() {}
        assert_traits::<ApiError>();
    }

    /// Acceptance item 1 shape: only the wrapping variants expose a
    /// `#[source]`; the unit/telemetry variants have no cause.
    #[test]
    fn api_error_source_chain_exposes_transport_and_decode_only() {
        assert!(ApiError::Unauthorized.source().is_none());
        assert!(ApiError::InsecureBaseUrl.source().is_none());
        assert!(ApiError::Server { status: 500 }.source().is_none());
        assert!(ApiError::RateLimited { retry_after: None }
            .source()
            .is_none());
        assert!(ApiError::RetriesExhausted { attempts: 2 }
            .source()
            .is_none());

        for wrapped in [
            ApiError::Transport {
                source: reqwest_stub_error(),
            },
            ApiError::Decode {
                source: reqwest_stub_error(),
            },
        ] {
            assert!(
                wrapped.source().is_some(),
                "variant must expose its cause: {wrapped:?}"
            );
        }
    }

    /// Display must carry the actionable context for every variant (status
    /// code, retry attempts, the reason for refusing a scheme).
    #[test]
    fn api_error_display_carries_expected_context() {
        let cases: Vec<(ApiError, &[&str])> = vec![
            (ApiError::Unauthorized, &["unauthorized"]),
            (
                ApiError::RateLimited { retry_after: None },
                &["rate limited"],
            ),
            (
                ApiError::RateLimited {
                    retry_after: Some(Duration::from_secs(30)),
                },
                &["rate limited"],
            ),
            (ApiError::Server { status: 503 }, &["503", "server error"]),
            (
                ApiError::Transport {
                    source: reqwest_stub_error(),
                },
                &["failed"],
            ),
            (
                ApiError::Decode {
                    source: reqwest_stub_error(),
                },
                &["decode"],
            ),
            (ApiError::InsecureBaseUrl, &["non-HTTPS"]),
            (
                ApiError::RetriesExhausted { attempts: 3 },
                &["attempt", "3"],
            ),
        ];
        for (err, needles) in cases {
            let disp = err.to_string();
            assert!(!disp.is_empty(), "empty Display for {err:?}");
            for needle in needles {
                assert!(
                    disp.contains(needle),
                    "Display of {err:?} lacks {needle:?}: {disp:?}"
                );
            }
        }
    }

    /// Boundary payloads must render without panicking and without losing
    /// the number.
    #[test]
    fn api_error_boundary_payloads_render() {
        for attempts in [0, 1, u32::MAX] {
            let s = ApiError::RetriesExhausted { attempts }.to_string();
            assert!(s.contains(&attempts.to_string()), "{s}");
        }
        for status in [0, 400, 499, 500, 599, u16::MAX] {
            let s = ApiError::Server { status }.to_string();
            assert!(s.contains(&status.to_string()), "{s}");
        }
        for retry_after in [
            Duration::ZERO,
            Duration::from_millis(1),
            Duration::from_secs(u64::MAX),
        ] {
            let err = ApiError::RateLimited {
                retry_after: Some(retry_after),
            };
            assert_ne!(err.to_string(), "");
            assert!(format!("{err:?}").contains("RateLimited"));
            assert!(err.is_retryable());
        }
    }

    /// Acceptance item 5: `src/api/mod.rs` exists, opens with module docs and
    /// scopes a module-level `dead_code` allow exactly like
    /// `src/auth/mod.rs` (inner attribute, not a per-item allow).
    #[test]
    fn api_module_scaffold_has_docs_and_scoped_dead_code_allow() {
        let src = include_str!("api/mod.rs");
        let lines: Vec<&str> = src.lines().collect();

        assert!(
            lines.first().is_some_and(|l| l.starts_with("//!")),
            "src/api/mod.rs must open with `//!` module docs"
        );
        let docs = lines.iter().filter(|l| l.starts_with("//!")).count();
        assert!(docs >= 3, "thin module docs ({docs} line(s))");

        let allow_idx = lines
            .iter()
            .position(|l| l.trim_start().starts_with("#![allow(") && l.contains("dead_code"))
            .expect("src/api/mod.rs needs a module-level `#![allow(dead_code)]`");
        assert!(
            lines[allow_idx].trim_start().starts_with("#!["),
            "the allow must be an inner attribute so it scopes the module"
        );

        // It must precede every real item (only blank lines, `//`/`//!`
        // comments between the docs and the attribute).
        let first_item = lines
            .iter()
            .position(|l| {
                let t = l.trim();
                !t.is_empty() && !t.starts_with("//")
            })
            .expect("module is not empty");
        assert_eq!(
            first_item, allow_idx,
            "the module-level allow must be the first item in src/api/mod.rs"
        );
    }

    /// Acceptance item 5: the `api` scaffold's scoping really matches
    /// `src/auth/mod.rs` (the file the task names as the model).
    #[test]
    fn api_module_scaffold_mirrors_auth_module_scoping() {
        fn module_level_dead_code_allow(src: &str) -> bool {
            src.lines()
                .any(|l| l.trim_start().starts_with("#![allow(") && l.contains("dead_code"))
        }
        assert!(
            module_level_dead_code_allow(include_str!("auth/mod.rs")),
            "auth/mod.rs no longer uses a module-level dead_code allow"
        );
        assert!(
            module_level_dead_code_allow(include_str!("api/mod.rs")),
            "api/mod.rs does not use the auth/mod.rs scoping pattern"
        );
        assert!(
            include_str!("auth/mod.rs").starts_with("//!"),
            "auth/mod.rs must keep opening with module docs"
        );
    }

    /// Acceptance item 5: `src/main.rs` declares `mod api;` in the
    /// production half (not inside a test module) and documents it.
    #[test]
    fn main_declares_mod_api() {
        let src = include_str!("main.rs");
        let production = src
            .split("#[cfg(test)]")
            .next()
            .expect("main.rs has a production half");
        assert!(
            production.lines().any(|l| l.trim() == "mod api;"),
            "src/main.rs must declare `mod api;` outside the test modules"
        );
        // The declaration is documented like `mod auth;` is.
        let lines: Vec<&str> = production.lines().map(str::trim).collect();
        let idx = lines
            .iter()
            .position(|l| *l == "mod api;")
            .expect("`mod api;` missing");
        assert!(
            lines[idx - 1].starts_with("///"),
            "`mod api;` needs a doc comment, found {:?}",
            lines[idx - 1]
        );
        assert!(
            lines[..idx].contains(&"mod auth;"),
            "mod api; should follow the existing module declaration block"
        );
    }
}
