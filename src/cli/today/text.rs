//! Deterministic plain-text line renderer for `todoist-tui today` (`T-1006`).
//!
//! # Output Contract
//!
//! - **Empty state**: writes `0` bytes to stdout and exits cleanly.
//! - **Non-empty state**: emits one LF-terminated (`\n`) line per task, with no
//!   header, footer, or trailing blank line.
//! - **Field separator**: a single ASCII tab character (`\t`, [`FIELD_SEPARATOR`]).
//! - **Columns** (always 6 fields in fixed order):
//!   1. `id`
//!   2. `priority` (`p1`..`p4`, where API priority `4` is `p1` and `1` is `p4`)
//!   3. `due` (`YYYY-MM-DD` / `YYYY-MM-DDTHH:MM:SS`, or `-` if absent)
//!   4. `project_name`
//!   5. `labels` (comma-joined label names, or `-` if empty)
//!   6. `content`
//! - **Sanitization**: any `\t`, `\n`, or `\r` inside field values is replaced
//!   with an ASCII space (`' '`) so `line.split('\t').count() == 6` always holds.

#![allow(dead_code)]

use std::io::Write;

use crate::cli::output::Output;
use crate::cli::today::model::TodayRow;
use crate::error::{CliError, Result};

/// Single-character separator between fields in plain-text `today` output.
pub const FIELD_SEPARATOR: char = '\t';

/// Number of tab-separated columns emitted on every plain-text `today` line.
pub const COLUMN_COUNT: usize = 6;

/// Map Todoist API priority (`1..=4`, where `4` is urgent/P1 and `1` is normal/P4)
/// to its human/CLI token (`"p1"`..`"p4"`).
#[must_use]
pub const fn format_priority(priority: u8) -> &'static str {
    match priority {
        4 => "p1",
        3 => "p2",
        2 => "p3",
        _ => "p4",
    }
}

/// Sanitize a field value so it cannot break the single-line tab-separated
/// output contract.
///
/// Replaces `\r\n` sequences with a single space, and any remaining `\t`, `\n`,
/// or `\r` characters with `' '`.
#[must_use]
pub fn sanitize_field(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut chars = raw.chars().peekable();
    while let Some(ch) = chars.next() {
        match ch {
            '\r' => {
                if chars.peek() == Some(&'\n') {
                    let _ = chars.next();
                }
                out.push(' ');
            }
            '\n' | '\t' => out.push(' '),
            other => out.push(other),
        }
    }
    out
}

/// Format a single [`TodayRow`] into its 6-column tab-separated line (without
/// a trailing newline).
#[must_use]
pub fn format_today_line(row: &TodayRow) -> String {
    let id = sanitize_field(&row.id);
    let priority = format_priority(row.priority);
    let due = match row.due.as_deref() {
        Some(d) if !d.is_empty() => sanitize_field(d),
        _ => "-".to_owned(),
    };
    let project = if row.project_name.is_empty() {
        "-".to_owned()
    } else {
        sanitize_field(&row.project_name)
    };
    let labels = if row.labels.is_empty() {
        "-".to_owned()
    } else {
        let joined: Vec<String> = row.labels.iter().map(|l| sanitize_field(l)).collect();
        joined.join(",")
    };
    let content = sanitize_field(&row.content);

    let sep = FIELD_SEPARATOR;
    format!("{id}{sep}{priority}{sep}{due}{sep}{project}{sep}{labels}{sep}{content}")
}

/// Render `rows` as deterministic plain-text lines to `out` (`T-1006`).
///
/// When `rows` is empty, nothing is written to `out` (`0` bytes).
///
/// # Errors
///
/// Returns [`CliError::Io`] wrapped in [`crate::error::AppError::Cli`] if writing
/// to the output stream fails.
pub fn render_today_text<W: Write, E: Write>(
    rows: &[TodayRow],
    out: &mut Output<W, E>,
) -> Result<()> {
    for row in rows {
        let line = format_today_line(row);
        out.write_line(&line)
            .map_err(|source| CliError::Io { source })?;
    }
    Ok(())
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used)]
mod tests {
    use super::*;

    fn sample_row() -> TodayRow {
        TodayRow {
            id: "6X4VfakeTask01".to_owned(),
            content: "Ship plain-text renderer".to_owned(),
            project_id: "6X4VfakeProj01".to_owned(),
            project_name: "Todoist TUI".to_owned(),
            priority: 4,
            due: Some("2026-04-17".to_owned()),
            is_overdue: false,
            labels: vec!["cli".to_owned(), "rust".to_owned()],
            checked: false,
            child_order: 1,
        }
    }

    #[test]
    fn renders_exact_column_order_and_single_lf_without_ansi_bytes() {
        let rows = vec![
            sample_row(),
            TodayRow {
                id: "6X4VfakeTask02".to_owned(),
                content: "Review PR".to_owned(),
                project_id: "6X4VfakeProj01".to_owned(),
                project_name: "Inbox".to_owned(),
                priority: 1,
                due: Some("2026-04-16".to_owned()),
                is_overdue: true,
                labels: Vec::new(),
                checked: false,
                child_order: 2,
            },
        ];

        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        {
            let mut out = Output::new(&mut stdout, &mut stderr, false);
            render_today_text(&rows, &mut out).unwrap();
        }

        assert_eq!(stderr, Vec::<u8>::new());
        assert!(
            !stdout.contains(&0x1b),
            "stdout must not contain ANSI bytes"
        );

        let rendered = String::from_utf8(stdout).unwrap();
        let expected = concat!(
            "6X4VfakeTask01\tp1\t2026-04-17\tTodoist TUI\tcli,rust\tShip plain-text renderer\n",
            "6X4VfakeTask02\tp4\t2026-04-16\tInbox\t-\tReview PR\n",
        );
        assert_eq!(rendered, expected);
        assert!(!rendered.ends_with("\n\n"));
    }

    #[test]
    fn empty_task_slice_writes_zero_bytes_to_stdout() {
        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        {
            let mut out = Output::new(&mut stdout, &mut stderr, false);
            render_today_text(&[], &mut out).unwrap();
        }

        assert_eq!(stdout, Vec::<u8>::new());
        assert_eq!(stderr, Vec::<u8>::new());
    }

    #[test]
    fn sanitizes_tabs_and_newlines_inside_content_and_project_names() {
        let mut row = sample_row();
        row.content = "Line 1\twith tab\nand LF\r\nand CRLF\rwith CR".to_owned();
        row.project_name = "Proj\tName\nHere".to_owned();

        let mut stdout = Vec::<u8>::new();
        let mut stderr = Vec::<u8>::new();
        {
            let mut out = Output::new(&mut stdout, &mut stderr, false);
            render_today_text(&[row], &mut out).unwrap();
        }

        let text = String::from_utf8(stdout).unwrap();
        let lines: Vec<&str> = text.lines().collect();
        assert_eq!(lines.len(), 1);

        let fields: Vec<&str> = lines[0].split(FIELD_SEPARATOR).collect();
        assert_eq!(fields.len(), COLUMN_COUNT);
        assert_eq!(fields[3], "Proj Name Here");
        assert_eq!(fields[5], "Line 1 with tab and LF and CRLF with CR");
    }

    #[test]
    fn rendering_same_slice_twice_produces_byte_identical_output() {
        let rows = vec![sample_row()];

        let mut stdout1 = Vec::<u8>::new();
        let mut stderr1 = Vec::<u8>::new();
        let mut stdout2 = Vec::<u8>::new();
        let mut stderr2 = Vec::<u8>::new();
        {
            let mut out1 = Output::new(&mut stdout1, &mut stderr1, false);
            let mut out2 = Output::new(&mut stdout2, &mut stderr2, false);

            render_today_text(&rows, &mut out1).unwrap();
            render_today_text(&rows, &mut out2).unwrap();
        }

        assert_eq!(stdout1, stdout2);
    }
}
