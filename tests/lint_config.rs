//! T-002 (EPIC-01): pin lint/format configuration by parsing committed files.
//! True compile-fail checks for `unwrap_used` would need trybuild (new dep),
//! so the policy is asserted structurally instead.

use toml::Value;

fn parse(name: &str, src: &str) -> Value {
    toml::from_str::<Value>(src).unwrap_or_else(|e| panic!("`{name}` invalid TOML: {e}"))
}
fn manifest() -> Value {
    parse("Cargo.toml", include_str!("../Cargo.toml"))
}
fn clippy() -> Value {
    parse("clippy.toml", include_str!("../clippy.toml"))
}
fn rustfmt() -> Value {
    parse("rustfmt.toml", include_str!("../rustfmt.toml"))
}

/// Returns (level, priority) for a lint entry (string or table form).
fn lint(table: &str, name: &str) -> (String, i64) {
    let entry = manifest()
        .get("lints")
        .and_then(|l| l.get(table))
        .and_then(|t| t.get(name))
        .cloned()
        .unwrap_or_else(|| panic!("[lints.{table}] `{name}` missing"));
    if let Some(s) = entry.as_str() {
        return (s.to_string(), 0);
    }
    let level = entry
        .get("level")
        .and_then(Value::as_str)
        .unwrap_or_else(|| panic!("`{name}` has no level"))
        .to_string();
    let prio = entry
        .get("priority")
        .and_then(Value::as_integer)
        .unwrap_or(0);
    (level, prio)
}

// Acceptance 1
#[test]
fn rustfmt_sets_explicit_edition_and_max_width() {
    let r = rustfmt();
    assert_eq!(r.get("edition").and_then(Value::as_str), Some("2021"));
    assert_eq!(r.get("max_width").and_then(Value::as_integer), Some(100));
}

#[test]
fn rustfmt_edition_matches_package_edition() {
    let pkg = manifest();
    assert_eq!(
        rustfmt().get("edition").and_then(Value::as_str),
        pkg["package"]["edition"].as_str()
    );
}

// Acceptance 2
#[test]
fn unwrap_and_expect_are_denied() {
    assert_eq!(lint("clippy", "unwrap_used").0, "deny");
    assert_eq!(lint("clippy", "expect_used").0, "deny");
}

#[test]
fn all_and_pedantic_are_warn() {
    assert_eq!(lint("clippy", "all").0, "warn");
    assert_eq!(lint("clippy", "pedantic").0, "warn");
}

#[test]
fn group_priorities_are_below_individual_lints() {
    for g in ["all", "pedantic"] {
        let (_, gp) = lint("clippy", g);
        assert!(gp < 0, "group `{g}` must have negative priority");
        for l in ["unwrap_used", "expect_used"] {
            assert!(gp < lint("clippy", l).1, "`{g}` must rank below `{l}`");
        }
    }
}

#[test]
fn rust_lints_table_present_and_forbids_unsafe() {
    assert_eq!(lint("rust", "unsafe_code").0, "forbid");
    assert_eq!(lint("rust", "rust_2018_idioms").0, "warn");
}

#[test]
fn no_blanket_allow_of_denied_lints() {
    let m = manifest();
    let clippy_tbl = m["lints"]["clippy"].as_table().expect("table");
    for (k, v) in clippy_tbl {
        let level = v
            .as_str()
            .or_else(|| v.get("level").and_then(Value::as_str));
        assert_ne!(
            level,
            Some("allow"),
            "lint `{k}` must not be allowed globally"
        );
    }
}

// Acceptance 3
#[test]
fn clippy_toml_has_thresholds() {
    let c = clippy();
    let n = |k: &str| c.get(k).and_then(Value::as_integer);
    let lines = n("too-many-lines-threshold").expect("too-many-lines-threshold");
    let cog = n("cognitive-complexity-threshold").expect("cognitive-complexity-threshold");
    assert!(lines > 0 && lines <= 200, "sane too-many-lines: {lines}");
    assert!(cog > 0 && cog <= 50, "sane cognitive complexity: {cog}");
}

#[test]
fn clippy_toml_scopes_unwrap_expect_allowance_to_tests() {
    let c = clippy();
    assert_eq!(
        c.get("allow-unwrap-in-tests").and_then(Value::as_bool),
        Some(true)
    );
    assert_eq!(
        c.get("allow-expect-in-tests").and_then(Value::as_bool),
        Some(true)
    );
}

#[test]
fn clippy_toml_has_no_unknown_keys() {
    let known = [
        "too-many-lines-threshold",
        "cognitive-complexity-threshold",
        "allow-unwrap-in-tests",
        "allow-expect-in-tests",
    ];
    for k in clippy().as_table().expect("table").keys() {
        assert!(
            known.contains(&k.as_str()),
            "unexpected clippy.toml key `{k}`"
        );
    }
}

// T-002 context: do not re-pin dependencies / keep T-001 invariants
#[test]
fn lint_edit_did_not_drop_dependencies() {
    let m = manifest();
    for d in ["ratatui", "tokio", "reqwest", "rusqlite", "toml", "clap"] {
        assert!(
            m["dependencies"].get(d).is_some(),
            "dependency `{d}` missing"
        );
    }
}
