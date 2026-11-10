//! Add-task action controller wiring `TaskForm` to `dispatch` (`T-705`, EPIC-08).

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::Id;
use crate::error::Result;
use crate::ui::actions::dispatch::{dispatch, DispatchResult};
use crate::ui::actions::TaskAction;
use crate::ui::model::{TaskRow, ViewId};
use crate::ui::widgets::task_form::TaskForm;

/// Opens an add-task form prefilled from `active_view` (and optionally a parent task for sub-task add).
#[must_use]
pub fn open_add_form(
    active_view: &ViewId,
    default_inbox_id: &Id,
    selected_task: Option<&TaskRow>,
    as_subtask: bool,
) -> TaskForm {
    if as_subtask {
        if let Some(parent) = selected_task {
            return TaskForm::new_add(
                parent.project_id.clone(),
                parent.section_id.clone(),
                Some(parent.id.clone()),
            );
        }
    }

    let project_id = match active_view {
        ViewId::Project(id) => id.clone(),
        _ => default_inbox_id.clone(),
    };
    let section_id = selected_task.and_then(|t| t.section_id.clone());

    TaskForm::new_add(project_id, section_id, None)
}

/// Dispatches a validated `TaskAction::Add` action atomically to queue & cache.
///
/// # Errors
///
/// Propagates validation, authentication, or storage errors from [`dispatch`].
pub fn submit_add(
    conn: &mut Connection,
    authenticated: bool,
    action: &TaskAction,
) -> Result<DispatchResult> {
    dispatch(conn, authenticated, action)
}
