//! Drift test asserting `README.md`'s keymap and config reference tables match
//! the code defaults (`T-1111`, EPIC-12).

#![allow(
    dead_code,
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_lines,
    clippy::uninlined_format_args
)]

#[path = "../src/error.rs"]
mod error;

#[path = "../src/config/mod.rs"]
mod config;

#[path = "../src/log.rs"]
mod log;

#[path = "../src/auth/mod.rs"]
mod auth;

#[path = "../src/api/mod.rs"]
mod api;

#[path = "../src/db/mod.rs"]
mod db;

#[path = "../src/queue/mod.rs"]
mod queue;

#[path = "../src/sync/mod.rs"]
mod sync;

#[path = "../src/search/mod.rs"]
mod search;

#[path = "../src/filters/mod.rs"]
mod filters;

#[path = "../src/structure/mod.rs"]
mod structure;

#[path = "../src/ui/mod.rs"]
mod ui;

use std::collections::BTreeSet;
use std::path::PathBuf;

use config::{Config, DefaultView};
use ui::keymap::Keymap;

fn extract_marker_block(doc: &str, begin: &str, end: &str) -> Vec<Vec<String>> {
    let start_pos = doc
        .find(begin)
        .unwrap_or_else(|| panic!("README.md missing marker `{begin}` — update README.md"));
    let after_start = &doc[start_pos + begin.len()..];
    let end_pos = after_start
        .find(end)
        .unwrap_or_else(|| panic!("README.md missing end marker `{end}` — update README.md"));
    let block = after_start[..end_pos].trim();
    assert!(
        !block.is_empty(),
        "README.md marker block `{begin}`..`{end}` must not be empty"
    );

    let mut rows = Vec::new();
    for (idx, line) in block.lines().enumerate() {
        let trimmed = line.trim();
        if trimmed.is_empty() {
            continue;
        }
        assert!(
            trimmed.starts_with('|') && trimmed.ends_with('|'),
            "malformed markdown table row in `{begin}`: `{trimmed}`"
        );
        let cols: Vec<String> = trimmed
            .trim_matches('|')
            .split('|')
            .map(|c| c.trim().replace('`', ""))
            .collect();
        // Skip header row (0) and separator row (1)
        if idx < 2 {
            continue;
        }
        rows.push(cols);
    }

    assert!(
        !rows.is_empty(),
        "marker block `{begin}`..`{end}` parsed zero data rows"
    );
    rows
}

fn read_readme() -> String {
    let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("README.md");
    std::fs::read_to_string(&path)
        .unwrap_or_else(|e| panic!("failed to read {}: {e}", path.display()))
}

#[test]
fn readme_keymap_table_matches_code_defaults() {
    let readme = read_readme();
    let parsed_rows = extract_marker_block(&readme, "<!-- BEGIN KEYMAP -->", "<!-- END KEYMAP -->");

    let mut doc_set: BTreeSet<(String, String, String)> = BTreeSet::new();
    for cols in parsed_rows {
        assert_eq!(
            cols.len(),
            3,
            "KEYMAP row expected 3 columns `(Key, Context, Action)`, got {cols:?}"
        );
        doc_set.insert((cols[0].clone(), cols[1].clone(), cols[2].clone()));
    }

    let code_keymap = Keymap::default();
    let mut code_set: BTreeSet<(String, String, String)> = BTreeSet::new();
    for (key, action, desc) in code_keymap.bindings() {
        code_set.insert((key, action.context().to_string(), desc.to_string()));
    }

    let missing_in_doc: Vec<_> = code_set.difference(&doc_set).collect();
    let extra_in_doc: Vec<_> = doc_set.difference(&code_set).collect();

    assert!(
        missing_in_doc.is_empty() && extra_in_doc.is_empty(),
        "README.md KEYMAP table drifted from Keymap::default()! Update README.md (do not weaken this test).\nMissing in README.md: {missing_in_doc:#?}\nExtra in README.md: {extra_in_doc:#?}"
    );
}

#[test]
fn readme_config_table_matches_config_defaults() {
    let readme = read_readme();
    let parsed_rows = extract_marker_block(&readme, "<!-- BEGIN CONFIG -->", "<!-- END CONFIG -->");

    let mut doc_defaults: BTreeSet<(String, String)> = BTreeSet::new();
    for cols in &parsed_rows {
        assert_eq!(
            cols.len(),
            4,
            "CONFIG row expected 4 columns `(Key, Type, Default, Description)`, got {cols:?}"
        );
        doc_defaults.insert((cols[0].clone(), cols[2].clone()));
    }

    let cfg = Config::default();
    let expected_default_view = match cfg.default_view {
        DefaultView::Inbox => "\"inbox\"",
        DefaultView::Today => "\"today\"",
        DefaultView::Upcoming => "\"upcoming\"",
    };
    let expected: BTreeSet<(String, String)> = [
        (
            "sync_interval".to_string(),
            cfg.sync_interval.as_secs().to_string(),
        ),
        ("theme".to_string(), format!("\"{}\"", cfg.theme)),
        (
            "keymap".to_string(),
            if cfg.keymap.is_empty() {
                "{} (empty)".to_string()
            } else {
                format!("{:?}", cfg.keymap)
            },
        ),
        (
            "default_view".to_string(),
            expected_default_view.to_string(),
        ),
    ]
    .into_iter()
    .collect();

    let missing: Vec<_> = expected.difference(&doc_defaults).collect();
    let extra: Vec<_> = doc_defaults.difference(&expected).collect();

    assert!(
        missing.is_empty() && extra.is_empty(),
        "README.md CONFIG table drifted from Config::default()! Update README.md (do not weaken this test).\nMissing in README.md: {missing:#?}\nExtra in README.md: {extra:#?}"
    );
}
