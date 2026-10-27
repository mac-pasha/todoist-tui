//! Process-level CLI tests for T-007 (EPIC-01). Spawns the real binary via
//! `CARGO_BIN_EXE_todoist-tui` (std only, no extra dev-dependency). HOME and
//! XDG dirs are redirected into a per-test temp dir so no real user
//! config/state is read or written.

#![allow(clippy::unwrap_used, clippy::expect_used)]

use std::path::PathBuf;
use std::process::{Command, Output};

fn sandbox(name: &str) -> PathBuf {
    let dir = PathBuf::from(env!("CARGO_TARGET_TMPDIR")).join(format!("cli-{name}"));
    std::fs::create_dir_all(&dir).unwrap();
    dir
}

fn run(name: &str, args: &[&str]) -> Output {
    let home = sandbox(name);
    Command::new(env!("CARGO_BIN_EXE_todoist-tui"))
        .args(args)
        .env("HOME", &home)
        .env("XDG_CONFIG_HOME", home.join("cfg"))
        .env("XDG_STATE_HOME", home.join("state"))
        .env("XDG_DATA_HOME", home.join("data"))
        .env("APPDATA", home.join("appdata"))
        .env("LOCALAPPDATA", home.join("localappdata"))
        .env_remove("RUST_LOG")
        .env_remove("RUST_BACKTRACE")
        .output()
        .unwrap()
}

fn stdout(o: &Output) -> String {
    String::from_utf8_lossy(&o.stdout).into_owned()
}
fn stderr(o: &Output) -> String {
    String::from_utf8_lossy(&o.stderr).into_owned()
}

#[test]
fn help_lists_all_five_subcommands_with_descriptions() {
    let o = run("help", &["--help"]);
    assert!(o.status.success());
    let out = stdout(&o);
    for (name, about) in [
        ("auth", "Store a Todoist API token in the OS keychain."),
        (
            "logout",
            "Remove the stored API token from the OS keychain.",
        ),
        ("sync", "Sync tasks and projects with Todoist."),
        ("add", "Quick-add a task from a single line of text."),
        ("today", "Show tasks due today."),
    ] {
        let line = out
            .lines()
            .find(|l| l.trim_start().starts_with(name) && l.contains(about.trim_end_matches('.')))
            .unwrap_or_else(|| panic!("no help line for `{name}` in:\n{out}"));
        let _ = line;
    }
    assert!(out.contains("--config"), "global --config missing:\n{out}");
}

#[test]
fn version_flag_prints_crate_version() {
    for flag in ["--version", "-V"] {
        let o = run("version", &[flag]);
        assert!(o.status.success());
        let out = stdout(&o);
        assert!(out.contains("todoist-tui"), "{out}");
        assert!(out.contains(env!("CARGO_PKG_VERSION")), "{out}");
    }
}

#[test]
fn version_is_propagated_to_subcommands() {
    let o = run("subver", &["sync", "--version"]);
    assert!(o.status.success());
    assert!(stdout(&o).contains(env!("CARGO_PKG_VERSION")));
}

#[test]
fn add_help_shows_single_positional_text() {
    let o = run("addhelp", &["add", "--help"]);
    assert!(o.status.success());
    let out = stdout(&o);
    assert!(out.contains("<TEXT>"), "{out}");
}

#[test]
fn today_help_shows_json_flag() {
    let o = run("todayhelp", &["today", "--help"]);
    assert!(o.status.success());
    assert!(stdout(&o).contains("--json"));
}

#[test]
fn stubs_without_extra_args_report_not_implemented_exit_1() {
    for (args, name) in [
        (vec!["auth"], "auth"),
        (vec!["logout"], "logout"),
        (vec!["sync"], "sync"),
        (vec!["add", "buy milk tomorrow p1"], "add"),
        (vec!["today"], "today"),
        (vec!["today", "--json"], "today"),
        (vec![], "tui"),
    ] {
        let o = run(&format!("stub-{name}"), &args);
        assert_eq!(o.status.code(), Some(1), "args {args:?}: {}", stderr(&o));
        let err = stderr(&o);
        assert!(
            err.contains(&format!("not implemented: {name}")),
            "args {args:?}: {err}"
        );
        assert!(!err.contains("panicked"), "{err}");
        assert!(!err.contains("backtrace"), "{err}");
        assert!(
            stdout(&o).is_empty(),
            "stdout should be empty: {}",
            stdout(&o)
        );
    }
}

#[test]
fn add_requires_text() {
    let o = run("add-missing", &["add"]);
    assert_eq!(o.status.code(), Some(2));
    assert!(stderr(&o).contains("<TEXT>"), "{}", stderr(&o));
}

#[test]
fn add_rejects_second_positional() {
    let o = run("add-two", &["add", "one", "two"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn add_accepts_dash_prefixed_text_after_double_dash() {
    let o = run("add-dash", &["add", "--", "-weird text"]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("not implemented: add"));
}

#[test]
fn add_accepts_empty_text_without_parsing_it() {
    let o = run("add-empty", &["add", ""]);
    assert_eq!(o.status.code(), Some(1));
    assert!(stderr(&o).contains("not implemented: add"));
}

#[test]
fn no_extra_args_for_auth_logout_sync() {
    for name in ["auth", "logout", "sync"] {
        let o = run(&format!("extra-{name}"), &[name, "extra"]);
        assert_eq!(o.status.code(), Some(2), "{name}");
        let o = run(&format!("extraflag-{name}"), &[name, "--json"]);
        assert_eq!(o.status.code(), Some(2), "{name} --json");
    }
}

#[test]
fn today_rejects_unknown_flag_and_positional() {
    assert_eq!(run("t-bad", &["today", "--bogus"]).status.code(), Some(2));
    assert_eq!(run("t-pos", &["today", "x"]).status.code(), Some(2));
}

#[test]
fn unknown_subcommand_is_usage_error() {
    let o = run("unknown", &["frobnicate"]);
    assert_eq!(o.status.code(), Some(2));
}

#[test]
fn config_flag_is_global_and_accepts_missing_file() {
    let dir = sandbox("cfg-missing");
    let missing = dir.join("nope.toml");
    let m = missing.to_str().unwrap();
    for args in [vec!["--config", m, "sync"], vec!["sync", "--config", m]] {
        let o = run("cfg-missing", &args);
        assert_eq!(o.status.code(), Some(1), "{}", stderr(&o));
        assert!(
            stderr(&o).contains("not implemented: sync"),
            "{}",
            stderr(&o)
        );
    }
}

#[test]
fn config_flag_requires_value() {
    assert_eq!(run("cfg-noval", &["--config"]).status.code(), Some(2));
}

#[test]
fn config_flag_path_is_actually_loaded_malformed_file_fails_before_dispatch() {
    let dir = sandbox("cfg-bad");
    let bad = dir.join("bad.toml");
    std::fs::write(&bad, "this is = = not toml").unwrap();
    let o = run("cfg-bad", &["--config", bad.to_str().unwrap(), "sync"]);
    assert_eq!(o.status.code(), Some(1));
    let err = stderr(&o);
    assert!(err.starts_with("config: "), "{err}");
    assert!(err.contains("bad.toml"), "{err}");
    assert!(!err.contains("not implemented"), "{err}");
}
