//! Edit-task action controller & quick inline priority/due edits (`T-706`, EPIC-08).

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::{Id, Priority};
use crate::error::Result;
use crate::ui::actions::dispatch::{dispatch, DispatchResult};
use crate::ui::actions::{EditPayload, FieldEdit, TaskAction};
use crate::ui::model::TaskDetail;
use crate::ui::widgets::task_form::TaskForm;

/// Opens a [`TaskForm`] in Edit mode prefilled from `detail`.
#[must_use]
pub fn open_edit_form(detail: &TaskDetail) -> TaskForm {
    TaskForm::new_edit(
        detail.id.clone(),
        detail.content.clone(),
        detail.description.clone(),
        detail.project_id.clone(),
        detail.section_id.clone(),
        None,
        detail
            .due
            .as_ref()
            .map(|d| d.string.clone().unwrap_or_else(|| d.date.clone())),
        detail.priority,
        detail.labels.clone(),
    )
}

/// Dispatches a partial edit action (`None` means user made no changes -> no-op).
///
/// # Errors
///
/// Propagates dispatch errors.
pub fn submit_edit(
    conn: &mut Connection,
    authenticated: bool,
    action: Option<&TaskAction>,
) -> Result<Option<DispatchResult>> {
    match action {
        Some(act) => Ok(Some(dispatch(conn, authenticated, act)?)),
        None => Ok(None),
    }
}

/// Quick inline priority update (`p` followed by 1..4) without opening the full form.
///
/// # Errors
///
/// Propagates priority validation or dispatch errors.
pub fn quick_priority(
    conn: &mut Connection,
    authenticated: bool,
    task_id: Id,
    priority_val: u8,
) -> Result<DispatchResult> {
    let priority =
        Priority::new(priority_val).map_err(|_| crate::error::ActionError::InvalidInput {
            field: "priority",
            reason: format!("priority must be 1..=4, got {priority_val}"),
        })?;
    let payload = EditPayload {
        id: task_id,
        content: None,
        description: None,
        project: None,
        section: FieldEdit::Unchanged,
        parent: FieldEdit::Unchanged,
        due: FieldEdit::Unchanged,
        priority: Some(priority),
        labels: None,
    };
    dispatch(conn, authenticated, &TaskAction::Edit(payload))
}

/// Quick inline due-date edit (`d` followed by due text) without opening the full form.
///
/// Empty string clears the due date; non-empty sends the raw string verbatim.
///
/// # Errors
///
/// Propagates dispatch errors.
pub fn quick_due(
    conn: &mut Connection,
    authenticated: bool,
    task_id: Id,
    due_text: &str,
) -> Result<DispatchResult> {
    let due_edit = if due_text.trim().is_empty() {
        FieldEdit::Clear
    } else {
        FieldEdit::Set(due_text.trim().to_string())
    };
    let payload = EditPayload {
        id: task_id,
        content: None,
        description: None,
        project: None,
        section: FieldEdit::Unchanged,
        parent: FieldEdit::Unchanged,
        due: due_edit,
        priority: None,
        labels: None,
    };
    dispatch(conn, authenticated, &TaskAction::Edit(payload))
}
