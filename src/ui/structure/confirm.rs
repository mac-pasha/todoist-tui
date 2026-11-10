//! Structure delete/archive confirmation dialogs using cascade previews (`T-807`, EPIC-09).

#![allow(dead_code)]

use crate::api::model::Id;
use crate::structure::cascade::CascadeSummary;
use crate::ui::widgets::confirm::ConfirmDialog;

/// Pending destructive/archival structure action awaiting confirmation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureConfirmAction {
    DeleteProject { id: Id, name: String },
    ArchiveProject { id: Id, name: String },
    DeleteSection { id: Id, name: String },
    DeleteLabel { id: Id, name: String },
}

/// Builds a [`ConfirmDialog`] displaying the affected resource counts from [`CascadeSummary`].
#[must_use]
pub fn build_confirm_dialog(
    action: &StructureConfirmAction,
    preview: CascadeSummary,
) -> ConfirmDialog {
    match action {
        StructureConfirmAction::DeleteProject { name, .. } => {
            let sub_count = preview.projects.saturating_sub(1);
            let msg = if sub_count > 0 {
                format!(
                    "Delete project \"{name}\"? ({} tasks, {sub_count} sub-projects)",
                    preview.items
                )
            } else {
                format!("Delete project \"{name}\"? ({} tasks)", preview.items)
            };
            ConfirmDialog::new("Delete Project", msg)
        }
        StructureConfirmAction::ArchiveProject { name, .. } => ConfirmDialog::new(
            "Archive Project",
            format!("Archive project \"{name}\"? (items retained)"),
        ),
        StructureConfirmAction::DeleteSection { name, .. } => ConfirmDialog::new(
            "Delete Section",
            format!(
                "Delete section \"{name}\"? ({} tasks affected)",
                preview.items
            ),
        ),
        StructureConfirmAction::DeleteLabel { name, .. } => ConfirmDialog::new(
            "Delete Label",
            format!(
                "Delete label \"{name}\"? (removed from {} tasks)",
                preview.items
            ),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::theme::Theme;
    use crate::ui::widgets::ConfirmEvent;
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn snapshot_delete_confirm_with_12_tasks_and_cancel_returns_cancelled() {
        let action = StructureConfirmAction::DeleteProject {
            id: Id::new("p1"),
            name: "Work".to_string(),
        };
        let preview = CascadeSummary {
            projects: 1,
            sections: 2,
            items: 12,
        };
        let mut dlg = build_confirm_dialog(&action, preview);

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        term.draw(|f| dlg.render(f, f.area(), &theme)).unwrap();

        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..24 {
            for x in 0..80 {
                out.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(out.contains("Delete project \"Work\"? (12 tasks)"));

        let ev = dlg.handle_key(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE));
        assert_eq!(ev, ConfirmEvent::Cancelled);
    }
}
