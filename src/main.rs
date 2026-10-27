//! Placeholder entrypoint for `todoist-tui`.
//!
//! Bootstrap task T-001: sets up the crate and the pinned dependency stack
//! only. UI, sync, storage and config modules arrive in later tasks.

/// Typed error hierarchy and crate-wide `Result` alias (T-003).
mod error;

fn main() {
    println!("todoist-tui bootstrap placeholder (T-001)");
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
        if s.starts_with(['^', '>', '<', '=']) || s.contains('*') || s.contains('-') || s.contains('+') {
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
        assert_eq!(bins.len(), 1, "expected exactly one [[bin]] target, found {}", bins.len());
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
            "ratatui", "crossterm", "tokio", "reqwest", "rusqlite",
            "keyring", "uuid", "thiserror", "clap", "toml",
            "directories", "serde_json",
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
        assert!(feats.iter().any(|x| x == "json"), "reqwest missing feature json: {feats:?}");
        assert!(feats.iter().any(|x| x == "rustls-tls"), "reqwest missing feature rustls-tls: {feats:?}");
    }

    #[test]
    fn no_dependency_requests_openssl() {
        // Manifest-level proxy for `cargo tree -i openssl-sys` (that command
        // is run separately as PR evidence; the manifest feature set is the
        // source of truth for which TLS backend gets pulled in).
        let spec = manifest();
        for (name, tbl) in spec["dependencies"].as_table().expect("[dependencies] missing") {
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
            .map(str::to_string)
            .collect();
        assert!(offenders.is_empty(), "unexpected module declarations: {offenders:?}");
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
        let ignores_target = lines.iter().any(|l| {
            l.trim_start_matches('-').trim_start_matches('!') == "/target"
        });
        assert!(ignores_target, ".gitignore must ignore /target: {lines:?}");
        let ignores_lock = lines.iter().any(|l| {
            let pat = l.trim_start_matches('-').trim_start_matches('!');
            pat == "Cargo.lock" || pat == "/Cargo.lock" || pat == "**/Cargo.lock"
        });
        assert!(!ignores_lock, ".gitignore must NOT ignore Cargo.lock: {lines:?}");
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
