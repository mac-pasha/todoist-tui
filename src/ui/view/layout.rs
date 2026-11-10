//! Frame layout splitting and the 80x24 terminal-too-small guard (`T-607`, EPIC-07).

#![allow(dead_code)]

use ratatui::layout::{Alignment, Constraint, Direction, Layout, Rect};
use ratatui::text::Line;
use ratatui::widgets::{Clear, Paragraph};
use ratatui::Frame;

use crate::ui::theme::Theme;

/// Minimum supported terminal width in columns.
pub const MIN_WIDTH: u16 = 80;
/// Minimum supported terminal height in rows.
pub const MIN_HEIGHT: u16 = 24;
/// Target sidebar width in columns.
pub const SIDEBAR_WIDTH: u16 = 24;
/// Minimum sidebar width when shrinking near 80 cols.
pub const MIN_SIDEBAR_WIDTH: u16 = 20;

/// Four non-overlapping panes dividing the main terminal area.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FrameAreas {
    /// Left navigation sidebar pane.
    pub sidebar: Rect,
    /// Center task list pane.
    pub list: Rect,
    /// Right task detail pane.
    pub detail: Rect,
    /// Bottom 1-row status bar.
    pub status_bar: Rect,
}

/// Returns `true` when `area` is smaller than [`MIN_WIDTH`] x [`MIN_HEIGHT`].
#[must_use]
pub const fn is_too_small(area: Rect) -> bool {
    area.width < MIN_WIDTH || area.height < MIN_HEIGHT
}

/// Splits `area` into `(sidebar, list, detail, status_bar)` with zero gaps and zero overlaps.
#[must_use]
pub fn split(area: Rect) -> FrameAreas {
    if area.width == 0 || area.height == 0 {
        return FrameAreas {
            sidebar: Rect::default(),
            list: Rect::default(),
            detail: Rect::default(),
            status_bar: Rect::default(),
        };
    }

    let vert = Layout::default()
        .direction(Direction::Vertical)
        .constraints([Constraint::Min(0), Constraint::Length(1)])
        .split(area);

    let body = vert[0];
    let status_bar = vert[1];

    let sidebar_w = SIDEBAR_WIDTH.clamp(MIN_SIDEBAR_WIDTH, 32).min(body.width);
    let rem_w = body.width.saturating_sub(sidebar_w);
    let list_w = (rem_w * 55) / 100;
    let detail_w = rem_w.saturating_sub(list_w);

    let sidebar = Rect {
        x: body.x,
        y: body.y,
        width: sidebar_w,
        height: body.height,
    };
    let list = Rect {
        x: body.x + sidebar_w,
        y: body.y,
        width: list_w,
        height: body.height,
    };
    let detail = Rect {
        x: body.x + sidebar_w + list_w,
        y: body.y,
        width: detail_w,
        height: body.height,
    };

    FrameAreas {
        sidebar,
        list,
        detail,
        status_bar,
    }
}

/// Renders the centered terminal-too-small guard message across `area`.
pub fn render_too_small(frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(Clear, area);

    let msg = format!(
        "terminal too small: need {}x{}, have {}x{}",
        MIN_WIDTH, MIN_HEIGHT, area.width, area.height
    );

    let v_center = area.y + area.height / 2;
    let text_area = Rect {
        x: area.x,
        y: v_center,
        width: area.width,
        height: 1.min(area.height),
    };

    let para = Paragraph::new(Line::from(msg))
        .style(theme.status_warn)
        .alignment(Alignment::Center);
    frame.render_widget(para, text_area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn split_at_80x24_covers_full_area_without_gaps_or_overlaps() {
        let root = Rect::new(0, 0, 80, 24);
        let areas = split(root);

        assert!(!is_too_small(root));
        assert!(areas.sidebar.width >= MIN_SIDEBAR_WIDTH);
        assert!(areas.list.width > 0);
        assert!(areas.detail.width > 0);
        assert_eq!(areas.status_bar.height, 1);
        assert_eq!(areas.status_bar.width, 80);

        let total_cells = areas.sidebar.area()
            + areas.list.area()
            + areas.detail.area()
            + areas.status_bar.area();
        assert_eq!(total_cells, root.area());
    }

    #[test]
    fn guard_renders_and_recovers_on_degenerate_sizes() {
        let sizes = [(1, 1), (80, 1), (0, 0), (60, 20), (80, 24), (120, 40)];
        let theme = Theme::default();
        for (w, h) in sizes {
            let backend = TestBackend::new(w, h);
            let mut terminal = Terminal::new(backend).unwrap();
            terminal
                .draw(|f| {
                    let area = f.area();
                    if is_too_small(area) {
                        render_too_small(f, area, &theme);
                    }
                })
                .unwrap();
        }
    }

    #[test]
    fn snapshot_too_small_at_60x20() {
        let theme = Theme::default();
        let backend = TestBackend::new(60, 20);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| {
                let area = f.area();
                assert!(is_too_small(area));
                render_too_small(f, area, &theme);
            })
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut found = false;
        for y in 0..20 {
            let row_str: String = (0..60)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            if row_str.contains("terminal too small: need 80x24, have 60x20") {
                found = true;
                break;
            }
        }
        assert!(found, "Expected guard text in 60x20 buffer");
    }
}
