//! Post-structure-mutation sidebar refresh and selection repair (`T-808`, EPIC-09).

#![allow(dead_code)]

use crate::api::model::Id;
use crate::error::Result;
use crate::ui::app::App;
use crate::ui::data::ViewData;
use crate::ui::keymap::Action;
use crate::ui::model::ViewId;

/// Refreshes `app` after a structure mutation, repairing sidebar selection if the
/// previously active view vanished (falling back to `preferred_parent_id` or Inbox).
///
/// # Errors
///
/// Propagates data reload errors from `data`.
pub fn refresh_after_structure_change(
    app: &mut App,
    data: &dyn ViewData,
    preferred_parent_id: Option<&Id>,
) -> Result<()> {
    let prev_view = app.active_view().clone();
    app.refresh(data)?;

    // Check whether previous view is still present in visible sidebar nodes
    let still_exists = app
        .sidebar_nodes()
        .iter()
        .any(|node| node.view_id() == prev_view);

    if !still_exists {
        let mut target_idx = 0usize; // Default fallback: Inbox (first node)
        if let Some(parent_id) = preferred_parent_id {
            if let Some(pos) = app
                .sidebar_nodes()
                .iter()
                .position(|n| n.view_id() == ViewId::Project(parent_id.clone()))
            {
                target_idx = pos;
            }
        }

        // Navigate safely using App actions to ensure tasks & detail reload
        app.apply(Action::First, data)?;
        for _ in 0..target_idx {
            app.apply(Action::Down, data)?;
        }
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Item, Priority, Project, Section};
    use crate::db::{migrations, resources};
    use crate::structure::{label, project, section};
    use crate::ui::data::SqliteViewData;
    use crate::ui::view::sidebar;
    use crate::ui::Theme;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use rusqlite::Connection;

    fn test_conn() -> Connection {
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
        conn
    }

    #[test]
    fn nested_subproject_archived_hidden_and_selection_repairs_on_delete() {
        let mut conn = test_conn();
        let parent_id = project::add(&mut conn, "Parent").unwrap();
        let child_id = project::add_child(&mut conn, "Child", &parent_id).unwrap();

        let mut app = {
            let data = SqliteViewData::new(&conn, "2026-11-10");
            App::new(&data).unwrap()
        };

        // Navigate to Child in sidebar (Inbox, Today, Upcoming, Parent, Child => index 4)
        {
            let data = SqliteViewData::new(&conn, "2026-11-10");
            for _ in 0..4 {
                app.apply(Action::Down, &data).unwrap();
            }
            assert_eq!(app.active_view(), &ViewId::Project(child_id.clone()));
        }

        // Render snapshot verifying nested project
        {
            let backend = TestBackend::new(28, 20);
            let mut term = Terminal::new(backend).unwrap();
            let theme = Theme::default();
            term.draw(|f| sidebar::render(f, f.area(), &app, &theme))
                .unwrap();
        }

        // Delete Child -> selection repairs cleanly to Parent
        project::delete(&mut conn, &child_id).unwrap();
        {
            let data = SqliteViewData::new(&conn, "2026-11-10");
            refresh_after_structure_change(&mut app, &data, Some(&parent_id)).unwrap();
            assert_eq!(app.active_view(), &ViewId::Project(parent_id.clone()));
        }

        // Archive Parent -> hidden from sidebar, selection falls back to Inbox
        project::archive(&mut conn, &parent_id).unwrap();
        {
            let data = SqliteViewData::new(&conn, "2026-11-10");
            refresh_after_structure_change(&mut app, &data, None).unwrap();
            assert_eq!(app.active_view(), &ViewId::Inbox);
            assert!(
                !app.sidebar_nodes().iter().any(|n| n.title() == "Parent"),
                "archived project must be hidden"
            );
        }
    }
}
