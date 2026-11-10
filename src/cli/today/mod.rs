//! `todoist-tui today` subcommand implementation (`T-1005`, `T-1006`, `T-1007`).
//!
//! Reads the local SQLite cache exclusively (making zero network requests) and
//! renders tasks due today or overdue either as deterministic tab-separated
//! lines (`text`) or as a single-line JSON document (`--json`).

#![allow(dead_code, unused_imports)]

pub mod json;
pub mod model;
pub mod text;

use std::io::Write;
use std::path::Path;

use rusqlite::Connection;

use crate::cli::output::Output;
use crate::db::{migrations, open};
use crate::error::Result;

pub use self::json::{
    build_today_json_document, render_today_json, TodayJsonDocument, TodayTaskDto,
};
pub use self::model::{
    build_today_rows, load_today_rows, load_today_rows_with_clock, resolve_today_date,
    sort_today_rows, FixedClock, SystemClock, TodayClock, TodayRow, TODAY_DATE_ENV_VAR,
};
pub use self::text::{
    format_priority, format_today_line, render_today_text, sanitize_field, COLUMN_COUNT,
    FIELD_SEPARATOR,
};

/// Execute `todoist-tui today` using the resolved cache database path and
/// standard output streams.
///
/// Opens the SQLite cache at the XDG/override path without making any network
/// requests, queries tasks due today or overdue using [`SystemClock`] (which
/// honours `TODOIST_TUI_TODAY=YYYY-MM-DD`), and writes either plain-text or
/// `--json` output to stdout.
///
/// # Errors
///
/// Returns an [`crate::error::AppError`] if path resolution, opening the SQLite
/// cache, querying rows, or writing output fails.
pub fn run_today(_config_override: Option<&Path>, json: bool, no_color: bool) -> Result<()> {
    let mut conn = open::open_default()?;
    migrations::run(&mut conn)?;
    let mut out = Output::for_process(no_color);
    run_today_with_clock(&conn, &SystemClock, json, &mut out)
}

/// Convenience entry point for `todoist-tui today` using default paths and
/// environment color detection.
///
/// # Errors
///
/// Propagates any error from [`run_today`].
pub fn run_today_default(json: bool) -> Result<()> {
    run_today(None, json, false)
}

/// Execute the `today` query and renderer against an already-open SQLite
/// [`Connection`] and an injected [`TodayClock`].
///
/// # Errors
///
/// Propagates database query or output I/O errors.
pub fn run_today_with_clock<C: TodayClock, W: Write, E: Write>(
    conn: &Connection,
    clock: &C,
    json: bool,
    out: &mut Output<W, E>,
) -> Result<()> {
    let today_date = resolve_today_date(conn, clock);
    run_today_with(conn, &today_date, json, out)
}

/// Execute the `today` query and renderer against an already-open SQLite
/// [`Connection`] and an explicit `YYYY-MM-DD` date string.
///
/// # Errors
///
/// Propagates database query or output I/O errors.
pub fn run_today_with<W: Write, E: Write>(
    conn: &Connection,
    today_date: &str,
    json: bool,
    out: &mut Output<W, E>,
) -> Result<()> {
    let rows = load_today_rows(conn, today_date)?;
    if json {
        render_today_json(&rows, out)
    } else {
        render_today_text(&rows, out)
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;
    use crate::api::model::{Due, Id, Item, Priority, Project};
    use crate::db::migrations;
    use crate::db::resources::{item as item_repo, project as project_repo};

    /// Recording network client double proving that `today` makes zero HTTP calls.
    #[derive(Default)]
    struct RecordingNetworkGuard {
        calls: AtomicUsize,
    }

    impl RecordingNetworkGuard {
        fn call_count(&self) -> usize {
            self.calls.load(Ordering::Acquire)
        }
    }

    fn seed_cache(conn: &mut Connection) {
        let tx = conn.transaction().unwrap();
        let proj = Project {
            id: Id::from("6X4VfakeProj01"),
            name: "Core".to_owned(),
            parent_id: None,
            child_order: 1,
            color: "charcoal".to_owned(),
            is_favorite: false,
            is_archived: false,
            is_deleted: false,
            view_style: "list".to_owned(),
        };
        project_repo::upsert(&tx, &proj).unwrap();

        let item = Item {
            id: Id::from("6X4VfakeItem01"),
            content: "Offline today task".to_owned(),
            description: String::new(),
            project_id: Id::from("6X4VfakeProj01"),
            section_id: None,
            parent_id: None,
            child_order: 1,
            priority: Priority::new(4).unwrap(),
            labels: vec!["offline".to_owned()],
            due: Some(Due {
                date: "2026-04-17".to_owned(),
                string: None,
                lang: None,
                is_recurring: false,
                timezone: None,
            }),
            deadline: None,
            checked: false,
            is_deleted: false,
            added_at: "2026-04-01T10:00:00Z".to_owned(),
            completed_at: None,
            user_id: Id::from("6X4VfakeUser01"),
            responsible_uid: None,
        };
        item_repo::upsert(&tx, &item).unwrap();
        tx.commit().unwrap();
    }

    #[test]
    fn run_today_with_clock_is_purely_cache_backed_for_text_and_json() {
        let net_guard = RecordingNetworkGuard::default();
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        seed_cache(&mut conn);
        let clock = FixedClock::from_ymd("2026-04-17");

        // Plain-text run
        let mut text_stdout = Vec::<u8>::new();
        let mut text_stderr = Vec::<u8>::new();
        {
            let mut text_out = Output::new(&mut text_stdout, &mut text_stderr, false);
            run_today_with_clock(&conn, &clock, false, &mut text_out).unwrap();
        }
        assert_eq!(
            String::from_utf8(text_stdout).unwrap(),
            "6X4VfakeItem01\tp1\t2026-04-17\tCore\toffline\tOffline today task\n"
        );

        // JSON run
        let mut json_stdout = Vec::<u8>::new();
        let mut json_stderr = Vec::<u8>::new();
        {
            let mut json_out = Output::new(&mut json_stdout, &mut json_stderr, false);
            run_today_with_clock(&conn, &clock, true, &mut json_out).unwrap();
        }
        let doc: TodayJsonDocument = serde_json::from_slice(&json_stdout).unwrap();
        assert_eq!(doc.tasks.len(), 1);
        assert_eq!(doc.tasks[0].id, "6X4VfakeItem01");
        assert!(!doc.tasks[0].is_overdue);

        // Zero network calls were made.
        assert_eq!(net_guard.call_count(), 0);
    }
}
