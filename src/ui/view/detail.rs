//! Right task detail and comments widget (`T-610`, EPIC-07).

#![allow(dead_code)]

use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::app::{App, Pane};
use crate::ui::theme::Theme;
use crate::ui::view::task_list::priority_badge;

/// Renders the detail pane into `area`.
#[allow(clippy::too_many_lines)]
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let focused = app.focus() == Pane::Detail;
    let border_style = if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let block = Block::default()
        .title(" Detail ")
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let Some(detail) = app.detail() else {
        let p = Paragraph::new(Line::from("(no task selected)")).style(theme.muted);
        frame.render_widget(p, inner);
        return;
    };

    let mut lines: Vec<Line<'static>> = Vec::new();

    // Title
    lines.push(Line::from(Span::styled(
        detail.content.clone(),
        theme.list_row.add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(""));

    // Priority
    let (prio_tag, prio_style) = priority_badge(detail.priority.get(), theme);
    lines.push(Line::from(vec![
        Span::styled("Priority: ", theme.muted),
        Span::styled(prio_tag, prio_style),
    ]));

    // Project / Section
    if let Some(ref proj_name) = detail.project_name {
        let mut proj_line = vec![
            Span::styled("Project:  ", theme.muted),
            Span::styled(proj_name.clone(), theme.list_row),
        ];
        if let Some(ref sec_name) = detail.section_name {
            proj_line.push(Span::styled(format!(" / {sec_name}"), theme.muted));
        }
        lines.push(Line::from(proj_line));
    }

    // Due
    if let Some(ref due) = detail.due {
        let due_str = due.string.as_deref().unwrap_or(&due.date).to_string();
        let due_style = if detail.is_overdue {
            theme.overdue
        } else {
            theme.list_row
        };
        lines.push(Line::from(vec![
            Span::styled("Due:      ", theme.muted),
            Span::styled(due_str, due_style),
        ]));
    }

    // Deadline
    if let Some(ref dl) = detail.deadline {
        lines.push(Line::from(vec![
            Span::styled("Deadline: ", theme.muted),
            Span::styled(dl.date.clone(), theme.status_warn),
        ]));
    }

    // Labels
    if !detail.labels.is_empty() {
        let lbls: Vec<String> = detail.labels.iter().map(|l| format!("@{l}")).collect();
        lines.push(Line::from(vec![
            Span::styled("Labels:   ", theme.muted),
            Span::styled(lbls.join(" "), theme.label),
        ]));
    }

    // Description
    if !detail.description.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "Description:",
            theme.muted.add_modifier(Modifier::BOLD),
        )));
        for desc_line in detail.description.lines() {
            lines.push(Line::from(Span::styled(
                desc_line.to_string(),
                theme.list_row,
            )));
        }
    }

    // Comments (oldest-first / newest-last)
    if !detail.comments.is_empty() {
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            format!("Comments ({}):", detail.comments.len()),
            theme.muted.add_modifier(Modifier::BOLD),
        )));
        for comment in &detail.comments {
            lines.push(Line::from(vec![
                Span::styled(
                    format!("{} ", comment.author),
                    theme.status_ok.add_modifier(Modifier::BOLD),
                ),
                Span::styled(format!("({})", comment.posted_at), theme.muted),
            ]));
            for c_line in comment.content.lines() {
                lines.push(Line::from(Span::styled(
                    format!("  {c_line}"),
                    theme.list_row,
                )));
            }
            if let Some(ref att) = comment.attachment_name {
                lines.push(Line::from(Span::styled(
                    format!("  [attachment: {att}]"),
                    theme.muted,
                )));
            }
        }
    }

    #[allow(clippy::cast_possible_truncation)]
    let scroll_y = app.detail_scroll.min(usize::from(u16::MAX)) as u16;
    let para = Paragraph::new(lines)
        .wrap(Wrap { trim: false })
        .scroll((scroll_y, 0));

    frame.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::data::FixtureData;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn snapshot_detail_with_comments_and_wrapping() {
        let data = FixtureData::sample();
        let app = App::new(&data).unwrap();
        let theme = Theme::default();

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &app, &theme))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut out = String::new();
        for y in 0..10 {
            let row: String = (0..30)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            out.push_str(&row);
            out.push('\n');
        }
        assert!(out.contains("Priority:"));
    }

    #[test]
    fn empty_selection_renders_placeholder() {
        let data = FixtureData::generated(0);
        let app = App::new(&data).unwrap();
        let theme = Theme::default();

        let backend = TestBackend::new(30, 8);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &app, &theme))
            .unwrap();

        let buf = terminal.backend().buffer();
        let row1: String = (0..30)
            .map(|x| buf[(x, 1)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(row1.contains("(no task selected)"));
    }
}
