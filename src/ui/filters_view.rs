//! Read-only filter view status rendering and mutating-action guard (`T-909`, EPIC-10).

#![allow(dead_code)]

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph, Wrap};
use ratatui::Frame;

use crate::filters;
use crate::ui::app::App;
use crate::ui::keymap::Action;
use crate::ui::model::{SidebarKind, ViewId};
use crate::ui::theme::Theme;

/// Visual state of a saved filter view when active in the task-list pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FilterViewState {
    /// Active view is not a saved filter.
    NotFilter,
    /// Query parsed locally and yielded one or more matching tasks.
    Populated(usize),
    /// Query parsed locally and matched zero cached tasks.
    Empty,
    /// Query uses constructs outside the local filter grammar subset.
    Unsupported { reason: String },
}

/// Returns `true` if `action` mutates tasks or workspace structure.
#[must_use]
pub const fn is_mutating_action(action: Action) -> bool {
    matches!(
        action,
        Action::AddTask
            | Action::AddSubtask
            | Action::EditTask
            | Action::QuickPriority
            | Action::QuickDue
            | Action::ToggleComplete
            | Action::DeleteTask
            | Action::MoveTask
            | Action::ReorderUp
            | Action::ReorderDown
            | Action::Comments
            | Action::StructureNew
            | Action::StructureEdit
            | Action::StructureDelete
    )
}

/// Returns `false` when `active_view` is a read-only [`ViewId::Filter`] and `action` is mutating.
#[must_use]
pub const fn allow_action_in_view(active_view: &ViewId, action: Action) -> bool {
    !(matches!(active_view, ViewId::Filter(_)) && is_mutating_action(action))
}

/// Extracts the raw filter query string for `app`'s active [`ViewId::Filter`], if any.
#[must_use]
pub fn active_filter_query(app: &App) -> Option<&str> {
    let ViewId::Filter(active_id) = app.active_view() else {
        return None;
    };
    app.sidebar_nodes().iter().find_map(|node| {
        if let SidebarKind::Filter { id, query } = &node.kind {
            if id == active_id {
                return Some(query.as_str());
            }
        }
        None
    })
}

/// Inspects the active filter view (if any) to determine whether it is populated,
/// empty, or unsupported locally.
#[must_use]
pub fn evaluate_filter_state(app: &App) -> FilterViewState {
    let Some(query) = active_filter_query(app) else {
        return FilterViewState::NotFilter;
    };
    match filters::parse(query) {
        Err(unsupported) => FilterViewState::Unsupported {
            reason: unsupported.reason,
        },
        Ok(_) => {
            if app.tasks().is_empty() {
                FilterViewState::Empty
            } else {
                FilterViewState::Populated(app.tasks().len())
            }
        }
    }
}

/// Renders the read-only empty or unsupported state for a saved filter view into `area`.
pub fn render_status_pane(
    frame: &mut Frame<'_>,
    area: Rect,
    title: &str,
    focused: bool,
    state: &FilterViewState,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    let border_style = if focused {
        theme.border_focused
    } else {
        theme.border
    };

    let block = Block::default()
        .title(format!(" {title} "))
        .borders(Borders::ALL)
        .border_style(border_style);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let lines = match state {
        FilterViewState::Unsupported { reason } => vec![
            Line::from(Span::styled(
                "server-defined filter - cannot be evaluated locally",
                theme.status_error,
            )),
            Line::from(Span::styled(format!("Reason: {reason}"), theme.muted)),
        ],
        FilterViewState::Empty => vec![Line::from(Span::styled("no matching tasks", theme.muted))],
        FilterViewState::NotFilter | FilterViewState::Populated(_) => Vec::new(),
    };

    let para = Paragraph::new(lines).wrap(Wrap { trim: false });
    frame.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Filter, Id, Item, Priority, Project};
    use crate::db::{migrations, resources};
    use crate::queue::store::CommandStore;
    use crate::ui::data::SqliteViewData;
    use crate::ui::keymap::Keymap;
    use crate::ui::view;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use rusqlite::Connection;

    fn render_buffer(app: &App) -> String {
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        term.draw(|f| view::render(f, app, &theme)).unwrap();

        let area = term.size().unwrap();
        let list_rect =
            view::layout::split(ratatui::layout::Rect::new(0, 0, area.width, area.height)).list;
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in (list_rect.y + 1)..(list_rect.y + list_rect.height.saturating_sub(1)) {
            for x in (list_rect.x + 1)..(list_rect.x + list_rect.width.saturating_sub(1)) {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push(' ');
        }
        out
    }

    fn seed_filter_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        resources::project::upsert(
            &conn,
            &Project {
                id: Id::new("inbox"),
                name: "Inbox".to_string(),
                parent_id: None,
                child_order: 0,
                color: "grey".to_string(),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: "list".to_string(),
            },
        )
        .unwrap();

        resources::item::upsert(
            &conn,
            &Item {
                id: Id::new("t1"),
                content: "Urgent task".to_string(),
                description: String::new(),
                project_id: Id::new("inbox"),
                section_id: None,
                parent_id: None,
                child_order: 1,
                priority: Priority::new(4).unwrap(),
                labels: vec!["ops".to_string()],
                due: None,
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: "2026-11-10T00:00:00Z".to_string(),
                completed_at: None,
                user_id: Id::new("u1"),
                responsible_uid: None,
            },
        )
        .unwrap();

        for (id, name, query, order) in [
            ("f_pop", "High Prio", "p1", 1),
            ("f_empty", "Missing Label", "@nonexistent", 2),
            ("f_unsup", "Assigned Server", "assigned to: me", 3),
        ] {
            resources::filter::upsert(
                &conn,
                &Filter {
                    id: Id::new(id),
                    name: name.to_string(),
                    query: query.to_string(),
                    color: "blue".to_string(),
                    item_order: order,
                    is_deleted: false,
                    is_favorite: false,
                },
            )
            .unwrap();
        }
        conn
    }

    fn navigate_to_filter(app: &mut App, data: &SqliteViewData<'_>, filter_id: &str) {
        let pos = app
            .sidebar_nodes()
            .iter()
            .position(|n| n.view_id() == ViewId::Filter(Id::new(filter_id)))
            .expect("filter node missing from sidebar");
        app.apply(Action::First, data).unwrap();
        for _ in 0..pos {
            app.apply(Action::Down, data).unwrap();
        }
    }

    #[test]
    fn snapshots_populated_empty_and_unsupported_filter_views() {
        let conn = seed_filter_db();
        let data = SqliteViewData::new(&conn, "2026-11-10");
        let mut app = App::new(&data).unwrap();

        // 1. Populated filter
        navigate_to_filter(&mut app, &data, "f_pop");
        assert_eq!(evaluate_filter_state(&app), FilterViewState::Populated(1));
        let pop_out = render_buffer(&app);
        assert!(pop_out.contains("Urgent task"));

        // 2. Empty filter
        navigate_to_filter(&mut app, &data, "f_empty");
        assert_eq!(evaluate_filter_state(&app), FilterViewState::Empty);
        let empty_out = render_buffer(&app);
        assert!(empty_out.contains("no matching tasks"));

        // 3. Unsupported filter
        navigate_to_filter(&mut app, &data, "f_unsup");
        assert!(matches!(
            evaluate_filter_state(&app),
            FilterViewState::Unsupported { .. }
        ));
        let unsup_norm: String = render_buffer(&app)
            .split_whitespace()
            .collect::<Vec<_>>()
            .join(" ");
        assert!(unsup_norm.contains("server-defined filter - cannot be evaluated locally"));
        assert!(!unsup_norm.contains("Urgent task"));
    }

    #[test]
    fn filter_view_rejects_every_mutating_key_without_enqueueing() {
        let conn = seed_filter_db();
        let data = SqliteViewData::new(&conn, "2026-11-10");
        let mut app = App::new(&data).unwrap();
        navigate_to_filter(&mut app, &data, "f_pop");

        let keymap = Keymap::default();
        for (_, action, _) in keymap.bindings() {
            if is_mutating_action(action) {
                assert!(
                    !allow_action_in_view(app.active_view(), action),
                    "mutating action {action:?} should be rejected in Filter view"
                );
            }
        }
        assert_eq!(CommandStore::new(&conn).pending(20).unwrap().len(), 0);
    }
}
