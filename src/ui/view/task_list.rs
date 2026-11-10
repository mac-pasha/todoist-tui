//! Middle task list widget (`T-609`, EPIC-07).

#![allow(dead_code)]

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::api::model::Id;
use crate::ui::app::{App, Pane};
use crate::ui::model::TaskRow;
use crate::ui::theme::Theme;

/// Maps Todoist API priority (`1..=4`, where `4` is urgent/P1 and `1` is normal/P4)
/// to display tag and semantic [`Style`].
#[must_use]
pub fn priority_badge(priority: u8, theme: &Theme) -> (&'static str, Style) {
    match priority {
        4 => ("P1", theme.priority_p1),
        3 => ("P2", theme.priority_p2),
        2 => ("P3", theme.priority_p3),
        _ => ("P4", theme.priority_p4),
    }
}

fn truncate_ellipsis(text: &str, max_chars: usize) -> String {
    if max_chars == 0 {
        return String::new();
    }
    let char_count = text.chars().count();
    if char_count <= max_chars {
        return text.to_string();
    }
    if max_chars == 1 {
        return "…".to_string();
    }
    let prefix: String = text.chars().take(max_chars - 1).collect();
    format!("{prefix}…")
}

/// Renders the task list pane into `area`.
#[allow(clippy::too_many_lines)]
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let focused = app.focus() == Pane::List;
    let border_style = if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let view_title = app.selected_sidebar_node().map_or("Tasks", |n| n.title());

    let filter_state = crate::ui::filters_view::evaluate_filter_state(app);
    if matches!(
        filter_state,
        crate::ui::filters_view::FilterViewState::Unsupported { .. }
            | crate::ui::filters_view::FilterViewState::Empty
    ) {
        crate::ui::filters_view::render_status_pane(
            frame,
            area,
            view_title,
            focused,
            &filter_state,
            theme,
        );
        return;
    }

    let title = format!(" {view_title} ");

    let block = Block::default()
        .title(title)
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let tasks = app.tasks();
    if tasks.is_empty() {
        let p = Paragraph::new(Line::from("(no tasks)")).style(theme.muted);
        frame.render_widget(p, inner);
        return;
    }

    let width = usize::from(inner.width);
    let height = usize::from(inner.height);

    // First pass: find `selected_line` without allocating per-row strings or spans.
    let mut selected_line = 0usize;
    if app.list_index < tasks.len() {
        let mut line_idx = 0usize;
        let mut current_section: Option<&Id> = None;
        let mut saw_any_section = false;
        for (idx, task) in tasks.iter().take(app.list_index + 1).enumerate() {
            if let Some(ref sec_id) = task.section_id {
                if current_section != Some(sec_id) {
                    current_section = Some(sec_id);
                    saw_any_section = true;
                    line_idx += 1;
                }
            } else if saw_any_section && current_section.is_some() {
                current_section = None;
            }
            if idx == app.list_index {
                selected_line = line_idx;
                break;
            }
            line_idx += 1;
        }
    }

    let scroll_y = if selected_line >= height {
        selected_line + 1 - height
    } else {
        0
    };
    let end_line = scroll_y.saturating_add(height);

    // Second pass: format only the visible viewport slice `[scroll_y, end_line)`
    // so 5k-item lists stay O(viewport_height) per frame instead of O(N) allocations.
    let mut visible_lines: Vec<Line<'static>> = Vec::with_capacity(height);
    let mut line_idx = 0usize;
    let mut current_section: Option<&Id> = None;
    let mut saw_any_section = false;

    for (idx, task) in tasks.iter().enumerate() {
        if line_idx >= end_line {
            break;
        }

        // Group by section: show section header when transitioning to a section
        if let Some(ref sec_id) = task.section_id {
            if current_section != Some(sec_id) {
                current_section = Some(sec_id);
                saw_any_section = true;
                if line_idx >= scroll_y {
                    let sec_name = task.section_name.as_deref().unwrap_or("Section");
                    let hdr = truncate_ellipsis(&format!("── {sec_name} ──"), width);
                    visible_lines.push(Line::from(Span::styled(
                        hdr,
                        theme.muted.add_modifier(Modifier::BOLD),
                    )));
                }
                line_idx += 1;
                if line_idx >= end_line {
                    break;
                }
            }
        } else if saw_any_section && current_section.is_some() {
            current_section = None;
        }

        if line_idx < scroll_y {
            line_idx += 1;
            continue;
        }

        let is_selected = idx == app.list_index;
        let row_base_style = if is_selected {
            if focused {
                theme.list_row_selected
            } else {
                theme
                    .list_row_selected
                    .remove_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::DIM)
            }
        } else {
            theme.list_row
        };

        let indent = "  ".repeat(task.depth);
        let check_box = if task.checked { "[x] " } else { "[ ] " };
        let (prio_tag, prio_style) = priority_badge(task.priority.get(), theme);

        let mut spans: Vec<Span<'static>> = Vec::new();
        let mut used = 0usize;

        let push_segment =
            |spans: &mut Vec<Span<'static>>, used: &mut usize, text: String, style: Style| {
                let rem = width.saturating_sub(*used);
                if rem == 0 || text.is_empty() {
                    return;
                }
                let clipped = truncate_ellipsis(&text, rem);
                *used += clipped.chars().count();
                let eff_style = if is_selected {
                    row_base_style.patch(style.bg(row_base_style.bg.unwrap_or_default()))
                } else {
                    style
                };
                spans.push(Span::styled(clipped, eff_style));
            };

        // Prefix: indent + checkbox
        push_segment(
            &mut spans,
            &mut used,
            format!("{indent}{check_box}"),
            row_base_style,
        );
        // Priority badge
        push_segment(&mut spans, &mut used, format!("{prio_tag} "), prio_style);

        // Metadata suffix calculations (due, deadline, labels)
        let mut meta_parts: Vec<(String, Style)> = Vec::new();
        if let Some(ref due) = task.due {
            let due_style = if task.is_overdue {
                theme.overdue
            } else {
                theme.muted
            };
            let due_txt = due.string.as_deref().unwrap_or(&due.date);
            meta_parts.push((format!(" [{due_txt}]"), due_style));
        }
        if let Some(ref dl) = task.deadline {
            meta_parts.push((format!(" (dl:{})", dl.date), theme.status_warn));
        }
        if !task.labels.is_empty() {
            let lbls: Vec<String> = task.labels.iter().map(|l| format!("@{l}")).collect();
            meta_parts.push((format!(" {}", lbls.join(" ")), theme.label));
        }

        let meta_width: usize = meta_parts.iter().map(|(s, _)| s.chars().count()).sum();
        let content_budget = width.saturating_sub(used + meta_width);

        if content_budget > 0 {
            let clipped_content = truncate_ellipsis(&task.content, content_budget);
            used += clipped_content.chars().count();
            spans.push(Span::styled(clipped_content, row_base_style));
        } else {
            let clipped_content = truncate_ellipsis(&task.content, width.saturating_sub(used));
            used += clipped_content.chars().count();
            spans.push(Span::styled(clipped_content, row_base_style));
        }

        for (txt, st) in meta_parts {
            push_segment(&mut spans, &mut used, txt, st);
        }

        // Fill remaining line width with row_base_style when selected
        if is_selected && used < width {
            let pad = " ".repeat(width - used);
            spans.push(Span::styled(pad, row_base_style));
        }

        visible_lines.push(Line::from(spans));
        line_idx += 1;
    }

    let para = Paragraph::new(visible_lines);
    frame.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::data::FixtureData;
    use crate::ui::keymap::Action;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn snapshot_task_list_against_sample_fixture() {
        let data = FixtureData::sample();
        let mut app = App::new(&data).unwrap();
        // Select Today or Work project to cover priorities/due/labels/subtasks
        let theme = Theme::default();

        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &app, &theme))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut all_text = String::new();
        for y in 0..20 {
            let row: String = (0..60)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            all_text.push_str(&row);
            all_text.push('\n');
        }
        assert!(all_text.contains("[ ] "));
        assert!(
            all_text.contains("P1")
                || all_text.contains("P2")
                || all_text.contains("P3")
                || all_text.contains("P4")
        );

        // Move to Today view to check other tasks
        app.apply(Action::Down, &data).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &app, &theme))
            .unwrap();
    }

    #[test]
    fn handles_200_rows_and_scrolling_without_overflow_at_width_30() {
        let data = FixtureData::generated(200);
        let mut app = App::new(&data).unwrap();
        let theme = Theme::default();

        // Jump to last item
        app.apply(Action::Right, &data).unwrap();
        app.apply(Action::Last, &data).unwrap();

        let backend = TestBackend::new(30, 15);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &app, &theme))
            .unwrap();
    }

    #[test]
    fn empty_list_shows_placeholder() {
        let data = FixtureData::generated(0);
        let app = App::new(&data).unwrap();
        let theme = Theme::default();

        let backend = TestBackend::new(30, 10);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &app, &theme))
            .unwrap();

        let buf = terminal.backend().buffer();
        let row1: String = (0..30)
            .map(|x| buf[(x, 1)].symbol().chars().next().unwrap_or(' '))
            .collect();
        assert!(row1.contains("(no tasks)"));
    }
}
