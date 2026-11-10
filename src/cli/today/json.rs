//! `--json` renderer for `todoist-tui today` (`T-1007`).
//!
//! # Schema Contract
//!
//! Emits a single-line UTF-8 JSON document terminated by a trailing `\n`:
//! ```json
//! {"tasks":[{"id":"...","content":"...","project_id":"...","project_name":"...","priority":4,"due":"2026-04-17","is_overdue":false,"labels":["cli"],"completed":false}]}
//! ```
//!
//! Dedicated DTO structs ([`TodayJsonDocument`] and [`TodayTaskDto`]) decouple
//! the public CLI JSON wire schema from internal domain/database types so
//! schema changes can never silently leak into `--json` output.

#![allow(dead_code)]

use std::io::Write;

use serde::{Deserialize, Serialize};

use crate::cli::output::Output;
use crate::cli::today::model::TodayRow;
use crate::error::{CliError, Result};

/// Top-level JSON envelope emitted by `todoist-tui today --json` (`T-1007`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodayJsonDocument {
    /// Ordered list of tasks due today or overdue.
    pub tasks: Vec<TodayTaskDto>,
}

/// Stable `snake_case` DTO for a single task inside [`TodayJsonDocument`] (`T-1007`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TodayTaskDto {
    /// Task identifier.
    pub id: String,
    /// Task content/title.
    pub content: String,
    /// Owning project identifier.
    pub project_id: String,
    /// Resolved human-readable project name.
    pub project_name: String,
    /// Todoist API priority (`1..=4`, where `4` is P1/urgent and `1` is P4/normal).
    pub priority: u8,
    /// Due date or datetime string (`YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS`), or
    /// `null` when absent.
    pub due: Option<String>,
    /// Whether the task's due date is strictly before the user's local day.
    pub is_overdue: bool,
    /// Label names attached to the task.
    pub labels: Vec<String>,
    /// Completion state (`false` for active today tasks).
    pub completed: bool,
}

impl From<&TodayRow> for TodayTaskDto {
    fn from(row: &TodayRow) -> Self {
        Self {
            id: row.id.clone(),
            content: row.content.clone(),
            project_id: row.project_id.clone(),
            project_name: row.project_name.clone(),
            priority: row.priority,
            due: row.due.clone(),
            is_overdue: row.is_overdue,
            labels: row.labels.clone(),
            completed: row.checked,
        }
    }
}

/// Build the [`TodayJsonDocument`] DTO from a slice of [`TodayRow`] items.
#[must_use]
pub fn build_today_json_document(rows: &[TodayRow]) -> TodayJsonDocument {
    TodayJsonDocument {
        tasks: rows.iter().map(TodayTaskDto::from).collect(),
    }
}

/// Serialize `rows` as a single-line JSON document + trailing LF to `out`
/// (`T-1007`).
///
/// # Errors
///
/// Returns [`CliError::Io`] if writing to `out` fails.
pub fn render_today_json<W: Write, E: Write>(
    rows: &[TodayRow],
    out: &mut Output<W, E>,
) -> Result<()> {
    let doc = build_today_json_document(rows);
    let payload = serde_json::to_string(&doc).map_err(|err| CliError::Io {
        source: std::io::Error::new(std::io::ErrorKind::InvalidData, err),
    })?;
    out.write_line(&payload)
        .map_err(|source| CliError::Io { source })?;
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn sample_row_with_due(
        id: &str,
        content: &str,
        priority: u8,
        due: Option<&str>,
        is_overdue: bool,
    ) -> TodayRow {
        TodayRow {
            id: id.to_owned(),
            content: content.to_owned(),
            project_id: "6X4VfakeProj01".to_owned(),
            project_name: "Platform".to_owned(),
            priority,
            due: due.map(str::to_owned),
            is_overdue,
            labels: vec!["urgent".to_owned()],
            checked: false,
            child_order: 1,
        }
    }

    #[test]
    fn empty_today_emits_exact_empty_tasks_json_object() {
        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        {
            let mut out = Output::new(&mut stdout, &mut stderr, false);
            render_today_json(&[], &mut out).unwrap();
        }

        assert_eq!(stderr, Vec::<u8>::new());
        assert_eq!(String::from_utf8(stdout).unwrap(), "{\"tasks\":[]}\n");
    }

    #[test]
    fn json_schema_has_exact_snake_case_keys_and_null_when_due_absent() {
        let rows = vec![sample_row_with_due(
            "6X4VfakeTask01",
            "No due fallback",
            3,
            None,
            false,
        )];

        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        {
            let mut out = Output::new(&mut stdout, &mut stderr, true);
            render_today_json(&rows, &mut out).unwrap();
        }

        assert!(
            !stdout.contains(&0x1b),
            "--json output must never contain ANSI escape sequences"
        );

        let parsed: serde_json::Value = serde_json::from_slice(&stdout).unwrap();
        let tasks = parsed
            .get("tasks")
            .and_then(serde_json::Value::as_array)
            .unwrap();
        assert_eq!(tasks.len(), 1);

        let obj = tasks[0].as_object().unwrap();
        let mut keys: Vec<&str> = obj.keys().map(String::as_str).collect();
        keys.sort_unstable();
        assert_eq!(
            keys,
            vec![
                "completed",
                "content",
                "due",
                "id",
                "is_overdue",
                "labels",
                "priority",
                "project_id",
                "project_name",
            ]
        );
        assert!(obj.get("due").unwrap().is_null());
        assert_eq!(
            obj.get("completed").unwrap(),
            &serde_json::Value::Bool(false)
        );
    }

    #[test]
    fn special_characters_quotes_backslashes_and_unicode_round_trip_cleanly() {
        let tricky = "Quote \"here\", backslash \\, newline \n, tab \t, unicode: 日本語 🚀";
        let rows = vec![sample_row_with_due(
            "6X4VfakeTaskUnicode",
            tricky,
            4,
            Some("2026-04-17"),
            false,
        )];

        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        {
            let mut out = Output::new(&mut stdout, &mut stderr, false);
            render_today_json(&rows, &mut out).unwrap();
        }

        let raw = String::from_utf8(stdout).unwrap();
        // Single-line JSON + trailing LF.
        assert_eq!(raw.lines().count(), 1);
        assert!(raw.ends_with('\n'));

        let decoded: TodayJsonDocument = serde_json::from_str(&raw).unwrap();
        assert_eq!(decoded.tasks.len(), 1);
        assert_eq!(decoded.tasks[0].content, tricky);
    }

    #[test]
    fn preserves_deterministic_ordering_from_model() {
        let rows = vec![
            sample_row_with_due("6X4VfakeFirst", "First", 4, Some("2026-04-16"), true),
            sample_row_with_due("6X4VfakeSecond", "Second", 2, Some("2026-04-17"), false),
        ];

        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        {
            let mut out = Output::new(&mut stdout, &mut stderr, false);
            render_today_json(&rows, &mut out).unwrap();
        }

        let decoded: TodayJsonDocument = serde_json::from_slice(&stdout).unwrap();
        let ids: Vec<&str> = decoded.tasks.iter().map(|t| t.id.as_str()).collect();
        assert_eq!(ids, vec!["6X4VfakeFirst", "6X4VfakeSecond"]);
    }
}
