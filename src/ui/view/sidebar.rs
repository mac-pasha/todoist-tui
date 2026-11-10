//! Left navigation sidebar widget (`T-608`, EPIC-07).

#![allow(dead_code)]

use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;

use crate::ui::app::{App, Pane};
use crate::ui::model::{SidebarKind, SidebarNode};
use crate::ui::theme::Theme;

/// Truncates `text` to fit within `max_chars`, appending `…` if truncated.
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

#[derive(Clone, Copy, PartialEq, Eq)]
enum SidebarGroup {
    Views,
    Projects,
    Labels,
    Filters,
}

impl SidebarGroup {
    const fn heading(self) -> &'static str {
        match self {
            Self::Views => "Views",
            Self::Projects => "Projects",
            Self::Labels => "Labels",
            Self::Filters => "Filters",
        }
    }

    const fn from_node(node: &SidebarNode) -> Self {
        match node.kind {
            SidebarKind::Inbox | SidebarKind::Today | SidebarKind::Upcoming => Self::Views,
            SidebarKind::Project { .. } => Self::Projects,
            SidebarKind::Label { .. } => Self::Labels,
            SidebarKind::Filter { .. } => Self::Filters,
        }
    }
}

/// Renders the sidebar pane into `area`.
#[allow(clippy::too_many_lines)]
pub fn render(frame: &mut Frame<'_>, area: Rect, app: &App, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let focused = app.focus() == Pane::Sidebar;
    let border_style = if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let block = Block::default()
        .title("Navigation")
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let nodes = app.sidebar_nodes();
    if nodes.is_empty() {
        let p = Paragraph::new(Line::from("(no views)")).style(theme.muted);
        frame.render_widget(p, inner);
        return;
    }

    let width = usize::from(inner.width);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(nodes.len() + 4);
    let mut node_to_line: Vec<usize> = Vec::with_capacity(nodes.len());
    let mut current_group: Option<SidebarGroup> = None;

    for (idx, node) in nodes.iter().enumerate() {
        let group = SidebarGroup::from_node(node);
        if current_group != Some(group) {
            current_group = Some(group);
            let hdr = truncate_ellipsis(group.heading(), width);
            lines.push(Line::from(Span::styled(
                hdr,
                theme.muted.add_modifier(Modifier::BOLD),
            )));
        }

        let is_selected = idx == app.sidebar_index;
        let row_style = if is_selected {
            if focused {
                theme.sidebar_selected
            } else {
                theme
                    .sidebar_selected
                    .remove_modifier(Modifier::BOLD)
                    .add_modifier(Modifier::DIM)
            }
        } else {
            theme.sidebar
        };

        let indent = "  ".repeat(node.depth());
        let marker = if node.has_children {
            if node.is_collapsed() {
                "▸ "
            } else {
                "▾ "
            }
        } else {
            match node.kind {
                SidebarKind::Label { .. } => "@ ",
                SidebarKind::Filter { .. } => "/ ",
                _ => "  ",
            }
        };

        let count_suffix = if node.task_count > 0 {
            format!(" ({})", node.task_count)
        } else {
            String::new()
        };

        let prefix = format!("{indent}{marker}");
        let prefix_len = prefix.chars().count();
        let suffix_len = count_suffix.chars().count();
        let avail_for_label = width.saturating_sub(prefix_len + suffix_len);
        let title_str = truncate_ellipsis(node.title(), avail_for_label);
        let full_text = format!("{prefix}{title_str}{count_suffix}");

        // Pad to pane width if selected so highlight bar fills width
        let padded = if is_selected && full_text.chars().count() < width {
            let pad = width - full_text.chars().count();
            format!("{full_text}{}", " ".repeat(pad))
        } else {
            truncate_ellipsis(&full_text, width)
        };

        node_to_line.push(lines.len());
        lines.push(Line::from(Span::styled(padded, row_style)));
    }

    // Keep selected line in viewport
    let height = usize::from(inner.height);
    let selected_line = node_to_line.get(app.sidebar_index).copied().unwrap_or(0);
    let scroll_y = if selected_line >= height {
        // Check if preceding group header should stay visible when at top
        selected_line + 1 - height
    } else {
        0
    };

    #[allow(clippy::cast_possible_truncation)]
    let para = Paragraph::new(lines).scroll((scroll_y as u16, 0));
    frame.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::Id;
    use crate::ui::data::FixtureData;
    use crate::ui::keymap::Action;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn snapshot_sidebar_at_80x24() {
        let data = FixtureData::sample();
        let app = App::new(&data).unwrap();
        let theme = Theme::default();

        let backend = TestBackend::new(24, 23);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render(f, f.area(), &app, &theme);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..23 {
            let row: String = (0..24)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            text.push_str(&row);
            text.push('\n');
        }

        assert!(text.contains("Views"));
        assert!(text.contains("Inbox"));
        assert!(text.contains("Today"));
        assert!(text.contains("Upcoming"));
        assert!(text.contains("Projects"));
        assert!(text.contains("Labels"));
        assert!(text.contains("Filters"));
    }

    #[test]
    fn truncates_long_project_name_at_width_20() {
        use crate::ui::data::ViewData;
        use crate::ui::model::{TaskDetail, TaskRow};

        struct LongProjectData;
        impl ViewData for LongProjectData {
            fn sidebar(&self) -> crate::error::Result<Vec<SidebarNode>> {
                Ok(vec![
                    SidebarNode::inbox(1),
                    SidebarNode::project(
                        Id::new("p1"),
                        "VeryLongProjectNameExceedingTwentyColumns",
                        None,
                        0,
                        false,
                        false,
                        0,
                    ),
                ])
            }
            fn tasks(&self, _: &crate::ui::model::ViewId) -> crate::error::Result<Vec<TaskRow>> {
                Ok(Vec::new())
            }
            fn detail(&self, _: &Id) -> crate::error::Result<Option<TaskDetail>> {
                Ok(None)
            }
        }

        let app = App::new(&LongProjectData).unwrap();
        let theme = Theme::default();

        let backend = TestBackend::new(20, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                render(f, f.area(), &app, &theme);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut found_ellipsis = false;
        for y in 0..20 {
            let row: String = (0..20)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            if row.contains('…') {
                found_ellipsis = true;
                break;
            }
        }
        assert!(found_ellipsis, "Long project name must truncate with …");
    }

    #[test]
    fn focus_changes_selection_style() {
        let data = FixtureData::sample();
        let mut app = App::new(&data).unwrap();
        let theme = Theme::default();

        // Focused sidebar
        let backend_focused = TestBackend::new(24, 20);
        let mut term1 = Terminal::new(backend_focused).unwrap();
        term1.draw(|f| render(f, f.area(), &app, &theme)).unwrap();

        // Focus switched to List
        app.apply(Action::Right, &data).unwrap();
        assert_eq!(app.focus(), Pane::List);

        let backend_unfocused = TestBackend::new(24, 20);
        let mut term2 = Terminal::new(backend_unfocused).unwrap();
        term2.draw(|f| render(f, f.area(), &app, &theme)).unwrap();

        // Cell style of Inbox row differs when focus moves away
        let style_focused = term1.backend().buffer()[(2, 2)].style();
        let style_unfocused = term2.backend().buffer()[(2, 2)].style();
        assert_ne!(style_focused, style_unfocused);
    }
}
