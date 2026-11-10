//! TUI frame rendering & widget modules (`T-607`..`T-612`, EPIC-07).

#![allow(dead_code)]

pub mod detail;
pub mod help;
pub mod layout;
pub mod sidebar;
pub mod status_bar;
pub mod task_list;

use std::time::SystemTime;

use ratatui::Frame;

use crate::ui::app::App;
use crate::ui::keymap::Keymap;
use crate::ui::status::StatusModel;
use crate::ui::theme::Theme;

/// Renders the entire TUI frame (or the terminal-too-small guard if below 80x24).
pub fn render_frame(
    frame: &mut Frame<'_>,
    app: &App,
    status: &StatusModel,
    keymap: &Keymap,
    now: SystemTime,
    theme: &Theme,
) {
    let area = frame.area();
    if layout::is_too_small(area) {
        layout::render_too_small(frame, area, theme);
        return;
    }

    let areas = layout::split(area);
    sidebar::render(frame, areas.sidebar, app, theme);
    task_list::render(frame, areas.list, app, theme);
    detail::render(frame, areas.detail, app, theme);
    status_bar::render(
        frame,
        areas.status_bar,
        status,
        app.search_active,
        now,
        theme,
    );

    if app.show_help {
        help::render(frame, area, keymap, theme);
    }
}

/// Convenience wrapper matching `render(frame, &App, &Theme)` for static snapshots.
pub fn render(frame: &mut Frame<'_>, app: &App, theme: &Theme) {
    let status = StatusModel::default();
    let keymap = Keymap::default();
    render_frame(frame, app, &status, &keymap, SystemTime::now(), theme);
}
