//! Complete, uncomplete, and delete action controllers (`T-707`, EPIC-08).
//!
//! # Recurring-Task Completion While Offline (Open Question 6)
//!
//! Completing a recurring task locally enqueues `item_complete` and sets `checked = true`
//! in the SQLite cache without fabricating a local next-occurrence date. Once online,
//! the Todoist server computes the next recurrence and pushes the updated task back in
//! the sync response. See `docs/manual/epic-08-task-actions.md`.

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::Id;
use crate::error::Result;
use crate::ui::actions::dispatch::{dispatch, DispatchResult};
use crate::ui::actions::{CompletePayload, DeletePayload, TaskAction, UncompletePayload};
use crate::ui::widgets::confirm::ConfirmDialog;

/// Toggles completion: dispatches `Complete` when `!currently_checked`, or `Uncomplete` when `currently_checked`.
///
/// For recurring tasks, this enqueues `item_complete` cleanly without inventing local
/// recurrence dates; the server updates the next due date on sync.
///
/// # Errors
///
/// Propagates dispatch errors.
pub fn toggle_complete(
    conn: &mut Connection,
    authenticated: bool,
    task_id: Id,
    currently_checked: bool,
) -> Result<DispatchResult> {
    let action = if currently_checked {
        TaskAction::Uncomplete(UncompletePayload { id: task_id })
    } else {
        TaskAction::Complete(CompletePayload { id: task_id })
    };
    dispatch(conn, authenticated, &action)
}

/// Builds a reusable confirmation dialog for task deletion (`D`).
#[must_use]
pub fn open_delete_confirm(task_content: &str) -> ConfirmDialog {
    ConfirmDialog::new("Delete Task", format!("Delete '{task_content}'?"))
}

/// Executes task deletion after confirmation.
///
/// # Errors
///
/// Propagates dispatch errors.
pub fn confirm_delete(
    conn: &mut Connection,
    authenticated: bool,
    task_id: Id,
) -> Result<DispatchResult> {
    dispatch(
        conn,
        authenticated,
        &TaskAction::Delete(DeletePayload { id: task_id }),
    )
}
