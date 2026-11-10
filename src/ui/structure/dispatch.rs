//! UI structure dispatcher: context gating, cascade preview confirmation,
//! mutation dispatch, and error surfacing (`T-809`, EPIC-09).

#![allow(dead_code)]

use std::time::{Duration, SystemTime};

use rusqlite::Connection;

use crate::api::model::Id;
use crate::db::queries;
use crate::error::{AppError, Result};
use crate::structure::cascade::{
    preview_label_delete, preview_project_archive, preview_project_delete, preview_section_delete,
    CascadeSummary,
};
use crate::structure::{label, project, section};
use crate::ui::app::{App, Pane};
use crate::ui::data::SqliteViewData;
use crate::ui::keymap::Action;
use crate::ui::model::{SidebarKind, ViewId};
use crate::ui::structure::confirm::{build_confirm_dialog, StructureConfirmAction};
use crate::ui::structure::form::{StructureForm, StructureFormKind, StructureIntent};
use crate::ui::structure::sidebar::refresh_after_structure_change;
use crate::ui::widgets::{ConfirmDialog, ErrorToast};

/// Default TTL for structure error toasts.
pub const STRUCTURE_TOAST_TTL: Duration = Duration::from_secs(4);

/// Outcome returned when evaluating a structure key action against current [`App`] context.
#[derive(Debug, Clone)]
pub enum StructurePrompt {
    /// Action is not valid in the current pane or selection context.
    Ignored,
    /// Open the interactive [`StructureForm`] modal.
    Form(StructureForm),
    /// Open a cascade-preview [`ConfirmDialog`] bound to a pending [`StructureConfirmAction`].
    Confirm {
        action: StructureConfirmAction,
        dialog: ConfirmDialog,
    },
}

/// Evaluates `action` (`StructureNew`, `StructureEdit`, `StructureDelete`) against the
/// current [`App`] focus and selection, enforcing pane & resource context gating.
///
/// - In [`Pane::Detail`], all structure actions return [`StructurePrompt::Ignored`].
/// - In [`Pane::Sidebar`], actions target the highlighted [`SidebarKind`] node.
/// - In [`Pane::List`] inside a [`ViewId::Project`], `StructureNew` targets a new section;
///   `StructureEdit`/`StructureDelete` target the selected task's section (if any).
///
/// # Errors
///
/// Propagates database lookup errors when computing cascade previews or reading sections.
pub fn prompt_for_action(conn: &Connection, app: &App, action: Action) -> Result<StructurePrompt> {
    if !matches!(
        action,
        Action::StructureNew | Action::StructureEdit | Action::StructureDelete
    ) {
        return Ok(StructurePrompt::Ignored);
    }

    match app.focus {
        Pane::Detail => Ok(StructurePrompt::Ignored),
        Pane::Sidebar => prompt_in_sidebar(conn, app, action),
        Pane::List => prompt_in_list(conn, app, action),
    }
}

/// Builds an Archive-Project confirmation prompt when a user project is selected in Sidebar.
///
/// Separate from [`Action::StructureDelete`] so archive (retains tasks) and delete
/// (cascades to tasks/sub-projects) remain distinct operations.
///
/// # Errors
///
/// Propagates [`crate::error::StructureError`] or SQLite errors if preview lookup fails.
pub fn prompt_archive_selected_project(conn: &Connection, app: &App) -> Result<StructurePrompt> {
    if app.focus != Pane::Sidebar {
        return Ok(StructurePrompt::Ignored);
    }
    let Some(node) = app.selected_sidebar_node() else {
        return Ok(StructurePrompt::Ignored);
    };
    let SidebarKind::Project { id, .. } = &node.kind else {
        return Ok(StructurePrompt::Ignored);
    };

    let confirm_action = StructureConfirmAction::ArchiveProject {
        id: id.clone(),
        name: node.label.clone(),
    };
    let preview = preview_project_archive(conn, id)?;
    let dialog = build_confirm_dialog(&confirm_action, preview);
    Ok(StructurePrompt::Confirm {
        action: confirm_action,
        dialog,
    })
}

fn prompt_in_sidebar(conn: &Connection, app: &App, action: Action) -> Result<StructurePrompt> {
    let Some(node) = app.selected_sidebar_node() else {
        return Ok(StructurePrompt::Ignored);
    };

    match (&node.kind, action) {
        (
            SidebarKind::Inbox
            | SidebarKind::Today
            | SidebarKind::Upcoming
            | SidebarKind::Filter { .. },
            Action::StructureNew,
        ) => Ok(StructurePrompt::Form(StructureForm::new(
            StructureFormKind::AddProject,
        ))),
        (SidebarKind::Project { id, .. }, Action::StructureNew) => Ok(StructurePrompt::Form(
            StructureForm::new(StructureFormKind::AddSubProject {
                parent_id: id.clone(),
                parent_name: node.label.clone(),
            }),
        )),
        (SidebarKind::Label { .. }, Action::StructureNew) => Ok(StructurePrompt::Form(
            StructureForm::new(StructureFormKind::AddLabel),
        )),

        (SidebarKind::Project { id, .. }, Action::StructureEdit) => {
            Ok(StructurePrompt::Form(StructureForm::rename(
                StructureFormKind::RenameProject { id: id.clone() },
                node.label.clone(),
            )))
        }
        (SidebarKind::Label { id, name }, Action::StructureEdit) => {
            Ok(StructurePrompt::Form(StructureForm::rename(
                StructureFormKind::RenameLabel { id: id.clone() },
                name.clone(),
            )))
        }

        (SidebarKind::Project { id, .. }, Action::StructureDelete) => {
            let confirm_action = StructureConfirmAction::DeleteProject {
                id: id.clone(),
                name: node.label.clone(),
            };
            let preview = preview_project_delete(conn, id)?;
            let dialog = build_confirm_dialog(&confirm_action, preview);
            Ok(StructurePrompt::Confirm {
                action: confirm_action,
                dialog,
            })
        }
        (SidebarKind::Label { id, name }, Action::StructureDelete) => {
            let confirm_action = StructureConfirmAction::DeleteLabel {
                id: id.clone(),
                name: name.clone(),
            };
            let preview = preview_label_delete(conn, id)?;
            let dialog = build_confirm_dialog(&confirm_action, preview);
            Ok(StructurePrompt::Confirm {
                action: confirm_action,
                dialog,
            })
        }

        _ => Ok(StructurePrompt::Ignored),
    }
}

fn prompt_in_list(conn: &Connection, app: &App, action: Action) -> Result<StructurePrompt> {
    let ViewId::Project(project_id) = app.active_view() else {
        return Ok(StructurePrompt::Ignored);
    };

    match action {
        Action::StructureNew => {
            let project_name = queries::all_projects(conn)?
                .into_iter()
                .find(|p| &p.id == project_id)
                .map_or_else(|| "Project".to_string(), |p| p.name);
            Ok(StructurePrompt::Form(StructureForm::new(
                StructureFormKind::AddSection {
                    project_id: project_id.clone(),
                    project_name,
                },
            )))
        }
        Action::StructureEdit | Action::StructureDelete => {
            let Some(task) = app.selected_task() else {
                return Ok(StructurePrompt::Ignored);
            };
            let Some(sec_id) = &task.section_id else {
                return Ok(StructurePrompt::Ignored);
            };
            let Some(sec) = queries::all_sections(conn)?
                .into_iter()
                .find(|s| &s.id == sec_id)
            else {
                return Ok(StructurePrompt::Ignored);
            };

            if action == Action::StructureEdit {
                Ok(StructurePrompt::Form(StructureForm::rename(
                    StructureFormKind::RenameSection { id: sec.id },
                    sec.name,
                )))
            } else {
                let confirm_action = StructureConfirmAction::DeleteSection {
                    id: sec.id.clone(),
                    name: sec.name,
                };
                let preview = preview_section_delete(conn, &sec.id)?;
                let dialog = build_confirm_dialog(&confirm_action, preview);
                Ok(StructurePrompt::Confirm {
                    action: confirm_action,
                    dialog,
                })
            }
        }
        _ => Ok(StructurePrompt::Ignored),
    }
}

/// Dispatches a validated [`StructureIntent`] into `crate::structure::*` and returns the
/// created resource [`Id`] when applicable.
///
/// # Errors
///
/// Returns [`crate::error::StructureError`] for invalid names, duplicate labels, or unknown IDs.
pub fn dispatch_intent(conn: &mut Connection, intent: &StructureIntent) -> Result<Option<Id>> {
    match intent {
        StructureIntent::AddProject { name } => Ok(Some(project::add(conn, name)?)),
        StructureIntent::AddSubProject { parent_id, name } => {
            Ok(Some(project::add_child(conn, name, parent_id)?))
        }
        StructureIntent::RenameProject { id, name } => {
            project::rename(conn, id, name)?;
            Ok(None)
        }
        StructureIntent::AddSection { project_id, name } => {
            Ok(Some(section::add(conn, name, project_id)?))
        }
        StructureIntent::RenameSection { id, name } => {
            section::rename(conn, id, name)?;
            Ok(None)
        }
        StructureIntent::AddLabel { name } => Ok(Some(label::add(conn, name)?)),
        StructureIntent::RenameLabel { id, name } => {
            label::rename(conn, id, name)?;
            Ok(None)
        }
    }
}

/// Submits `intent` from `form`, refreshes `app` on success, or surfaces [`AppError::Structure`]
/// inline via [`StructureForm::set_error`] while keeping the form open.
///
/// Returns `Ok(true)` if the mutation succeeded and the form may close; `Ok(false)` if a
/// domain structure validation/lookup error was surfaced onto `form`.
///
/// # Errors
///
/// Propagates unexpected non-structure errors (such as SQLite failures).
pub fn submit_form(
    conn: &mut Connection,
    app: &mut App,
    today_date: &str,
    form: &mut StructureForm,
    intent: &StructureIntent,
) -> Result<bool> {
    match dispatch_intent(conn, intent) {
        Ok(_) => {
            let data = SqliteViewData::new(conn, today_date);
            refresh_after_structure_change(app, &data, None)?;
            Ok(true)
        }
        Err(AppError::Structure(err)) => {
            form.set_error(err.to_string());
            Ok(false)
        }
        Err(err) => Err(err),
    }
}

/// Dispatches a confirmed [`StructureConfirmAction`] (delete or archive) into `crate::structure::*`.
///
/// # Errors
///
/// Returns [`crate::error::StructureError`] or SQLite errors.
pub fn dispatch_confirm(
    conn: &mut Connection,
    action: &StructureConfirmAction,
) -> Result<CascadeSummary> {
    match action {
        StructureConfirmAction::DeleteProject { id, .. } => project::delete(conn, id),
        StructureConfirmAction::ArchiveProject { id, .. } => {
            project::archive(conn, id)?;
            Ok(CascadeSummary {
                projects: 1,
                sections: 0,
                items: 0,
            })
        }
        StructureConfirmAction::DeleteSection { id, .. } => {
            section::delete(conn, id)?;
            Ok(CascadeSummary {
                projects: 0,
                sections: 1,
                items: 0,
            })
        }
        StructureConfirmAction::DeleteLabel { id, .. } => label::delete(conn, id),
    }
}

/// Executes a confirmed [`StructureConfirmAction`] and refreshes `app` (repairing sidebar
/// selection to `preferred_parent_id` or Inbox if the active view vanished).
///
/// # Errors
///
/// Propagates [`AppError`] from the underlying deletion/archive or UI refresh.
pub fn execute_confirm(
    conn: &mut Connection,
    app: &mut App,
    today_date: &str,
    action: &StructureConfirmAction,
    preferred_parent_id: Option<&Id>,
) -> Result<CascadeSummary> {
    let summary = dispatch_confirm(conn, action)?;
    let data = SqliteViewData::new(conn, today_date);
    refresh_after_structure_change(app, &data, preferred_parent_id)?;
    Ok(summary)
}

/// Converts any structure/app error into a transient [`ErrorToast`] for UI presentation.
#[must_use]
pub fn error_to_toast(err: &AppError, now: SystemTime) -> ErrorToast {
    ErrorToast::new(err.to_string(), now, STRUCTURE_TOAST_TTL)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Item, Priority, Project};
    use crate::db::{migrations, resources};
    use crate::error::StructureError;
    use crate::queue::store::CommandStore;
    use crate::ui::data::SqliteViewData;
    use crate::ui::keymap::Action;
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

    fn make_item(id: &str, project_id: Id, content: &str) -> Item {
        Item {
            id: Id::new(id),
            user_id: Id::new("u1"),
            project_id,
            section_id: None,
            parent_id: None,
            content: content.to_string(),
            description: String::new(),
            priority: Priority::new(1).unwrap(),
            due: None,
            deadline: None,
            labels: Vec::new(),
            child_order: 1,
            is_deleted: false,
            checked: false,
            completed_at: None,
            added_at: "2026-11-10T00:00:00Z".to_string(),
            responsible_uid: None,
        }
    }

    #[test]
    fn detail_pane_ignores_all_structure_actions() {
        let mut conn = test_conn();
        let pid = project::add(&mut conn, "Work").unwrap();
        resources::item::upsert(&conn, &make_item("i1", pid, "Task")).unwrap();

        let data = SqliteViewData::new(&conn, "2026-11-10");
        let mut app = App::new(&data).unwrap();
        app.focus = Pane::Detail;

        for act in [
            Action::StructureNew,
            Action::StructureEdit,
            Action::StructureDelete,
        ] {
            let prompt = prompt_for_action(&conn, &app, act).unwrap();
            assert!(matches!(prompt, StructurePrompt::Ignored));
        }
    }

    #[test]
    fn duplicate_label_error_surfaces_inline_in_form_without_closing() {
        let mut conn = test_conn();
        let _ = label::add(&mut conn, "urgent").unwrap();

        let mut app = {
            let data = SqliteViewData::new(&conn, "2026-11-10");
            App::new(&data).unwrap()
        };
        let mut form = StructureForm::new(StructureFormKind::AddLabel);

        let closed = submit_form(
            &mut conn,
            &mut app,
            "2026-11-10",
            &mut form,
            &StructureIntent::AddLabel {
                name: "URGENT".to_string(),
            },
        )
        .unwrap();

        assert!(!closed);
        assert!(form
            .error
            .as_deref()
            .unwrap_or("")
            .to_ascii_lowercase()
            .contains("urgent"));
    }

    #[test]
    fn archive_vs_delete_project_separation_and_queue_enqueues() {
        let mut conn = test_conn();
        let p_archive = project::add(&mut conn, "ArchiveMe").unwrap();
        let p_delete = project::add(&mut conn, "DeleteMe").unwrap();

        resources::item::upsert(&conn, &make_item("t0", p_archive.clone(), "Task 0")).unwrap();
        resources::item::upsert(&conn, &make_item("t1", p_delete.clone(), "Task 1")).unwrap();

        let mut app = {
            let data = SqliteViewData::new(&conn, "2026-11-10");
            App::new(&data).unwrap()
        };

        // Archive keeps items
        let archive_act = StructureConfirmAction::ArchiveProject {
            id: p_archive.clone(),
            name: "ArchiveMe".to_string(),
        };
        execute_confirm(&mut conn, &mut app, "2026-11-10", &archive_act, None).unwrap();
        let archived_items = queries::items_by_project(&conn, &p_archive).unwrap();
        assert_eq!(archived_items.len(), 1);

        // Delete cascades to items
        let delete_act = StructureConfirmAction::DeleteProject {
            id: p_delete.clone(),
            name: "DeleteMe".to_string(),
        };
        execute_confirm(&mut conn, &mut app, "2026-11-10", &delete_act, None).unwrap();
        let deleted_items = queries::items_by_project(&conn, &p_delete).unwrap();
        assert_eq!(deleted_items.len(), 0);

        let pending = CommandStore::new(&conn).pending(100).unwrap();
        let types: Vec<&str> = pending.iter().map(|c| c.kind.as_str()).collect();
        assert!(types.contains(&"project_archive"));
        assert!(types.contains(&"project_delete"));
    }

    #[test]
    fn error_toast_captures_structure_error_cleanly() {
        let err: AppError = StructureError::EmptyName.into();
        let toast = error_to_toast(&err, SystemTime::UNIX_EPOCH);
        assert_ne!(toast.message.len(), 0);
        assert!(!toast.is_expired(SystemTime::UNIX_EPOCH + Duration::from_secs(1)));
        assert!(toast.is_expired(SystemTime::UNIX_EPOCH + Duration::from_secs(5)));
    }
}
