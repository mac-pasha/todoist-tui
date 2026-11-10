//! Task action vocabulary (T-701, EPIC-08).
//!
//! [`TaskAction`] is the single write-path entry point the rest of
//! EPIC-08 dispatches through: one variant per user-initiated action, each
//! carrying its own payload struct. This task only lands the enum/payload
//! *types* plus [`crate::error::ActionError`] (defined in `src/error.rs`);
//! turning a `TaskAction` into a queued write-queue command (EPIC-05) and
//! applying it to the local cache (EPIC-04) is later EPIC-08 work, so
//! there is no HTTP, no SQL and no ratatui widget code here.
//!
//! The [`EditPayload`] fields are `Option<T>` so "the user didn't touch
//! this field" (`None`) stays distinguishable from "the user cleared this
//! field to empty" (`Some(String::new())`). Fields that are already
//! nullable on the wire (section/parent/due) use [`FieldEdit`] instead of
//! `Option<Option<T>>` (clippy's `option_option` lint) to add a third,
//! explicit "clear" state. A partial edit only needs to carry the fields
//! that actually changed.
//!
//! [`commands`] lands the other half of EPIC-08's write path (T-702):
//! [`commands::task_action_to_commands`] turns a `TaskAction` into the
//! Sync API write command(s) (EPIC-05's [`crate::queue::QueuedCommand`]) to
//! enqueue. Applying the action to the local cache (EPIC-04) is still later
//! EPIC-08 work.

// Local-cache application (EPIC-04) is still later EPIC-08 work. `pub`
// alone does not silence `dead_code` in a binary crate, so allow it at
// module scope — same pattern as `error.rs` (T-003) and `src/ui/mod.rs`
// (T-601). `unused_imports` joins for the T-702 re-export, same as
// `src/queue/mod.rs` (T-403).
#![allow(dead_code, unused_imports)]

use crate::api::model::{Id, Priority};

pub mod add;
pub mod commands;
pub mod comment;
pub mod complete;
pub mod dispatch;
pub mod edit;
pub mod errors;
pub mod move_task;
pub mod reorder;

pub use commands::task_action_to_commands;
pub use dispatch::{dispatch, DispatchResult};

/// A single user-initiated task action, queued for execution against the
/// Todoist Sync API (EPIC-05) and applied optimistically to the local
/// cache (EPIC-04).
///
/// `#[non_exhaustive]` so later EPIC-08 tasks can add further actions
/// (e.g. label management) without breaking exhaustive matches elsewhere
/// in the crate.
#[non_exhaustive]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskAction {
    /// Create a new task.
    Add(AddPayload),
    /// Apply a partial edit to an existing task.
    Edit(EditPayload),
    /// Mark a task complete.
    Complete(CompletePayload),
    /// Reopen a previously completed task.
    Uncomplete(UncompletePayload),
    /// Delete a task.
    Delete(DeletePayload),
    /// Move a task to a different project/section/parent.
    Move(MovePayload),
    /// Reorder tasks within a project (or section).
    Reorder(ReorderPayload),
    /// Attach a comment to a task.
    AddComment(AddCommentPayload),
}

/// Payload for [`TaskAction::Add`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddPayload {
    /// The new task's title.
    pub content: String,
    /// Markdown-ish free-text body; empty string when there is none.
    pub description: String,
    /// Project the task is created in.
    pub project: Id,
    /// Section the task is created in, if any.
    pub section: Option<Id>,
    /// Parent task, if this is a subtask.
    pub parent: Option<Id>,
    /// Due date/time, in whatever free-form string the Todoist "quick add"
    /// due-date parser accepts (e.g. `"tomorrow 5pm"`). Parsing/validating
    /// this string is a later EPIC-08 task.
    pub due: Option<String>,
    /// Task priority.
    pub priority: Priority,
    /// Label names attached to the new task.
    pub labels: Vec<String>,
}

/// A change to an already-nullable field in [`EditPayload`].
///
/// Three-state instead of `Option<Option<T>>` (clippy's `option_option`
/// lint, and clearer at call sites): [`FieldEdit::Unchanged`] means the
/// edit does not touch this field at all, [`FieldEdit::Clear`] explicitly
/// sets it to absent, and [`FieldEdit::Set`] gives it a new value.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub enum FieldEdit<T> {
    /// Leave the field exactly as it is.
    #[default]
    Unchanged,
    /// Explicitly clear the field to absent.
    Clear,
    /// Set the field to a new value.
    Set(T),
}

/// Payload for [`TaskAction::Edit`].
///
/// Fields that are mandatory on an [`AddPayload`] (content, description,
/// priority, labels) use `Option<T>` here so a partial edit can express
/// "leave unchanged" (`None`) distinctly from "clear this field to empty"
/// (`Some(String::new())`). Fields that are already nullable
/// (section/parent/due) use [`FieldEdit`] to add that same distinction on
/// top of their existing absent/present states.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EditPayload {
    /// Task being edited.
    pub id: Id,
    /// New title, or `None` to leave unchanged.
    pub content: Option<String>,
    /// New description, or `None` to leave unchanged. `Some(String::new())`
    /// explicitly clears the description.
    pub description: Option<String>,
    /// New project, or `None` to leave unchanged.
    pub project: Option<Id>,
    /// Change to the task's section.
    pub section: FieldEdit<Id>,
    /// Change to the task's parent.
    pub parent: FieldEdit<Id>,
    /// Change to the task's due date/time string.
    pub due: FieldEdit<String>,
    /// New priority, or `None` to leave unchanged.
    pub priority: Option<Priority>,
    /// New label set, or `None` to leave unchanged.
    pub labels: Option<Vec<String>>,
}

/// Payload for [`TaskAction::Complete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompletePayload {
    /// Task being completed.
    pub id: Id,
}

/// Payload for [`TaskAction::Uncomplete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UncompletePayload {
    /// Task being reopened.
    pub id: Id,
}

/// Payload for [`TaskAction::Delete`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DeletePayload {
    /// Task being deleted.
    pub id: Id,
}

/// Payload for [`TaskAction::Move`].
///
/// At least one of `project`/`section`/`parent` is expected to be set by
/// the caller; this scaffold does not enforce that invariant (validation
/// lands with the command-dispatch task that consumes this payload).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MovePayload {
    /// Task being moved.
    pub id: Id,
    /// Destination project, if moving between projects.
    pub project: Option<Id>,
    /// Destination section, if moving between sections.
    pub section: Option<Id>,
    /// Destination parent, if moving between parent tasks.
    pub parent: Option<Id>,
}

/// Payload for [`TaskAction::Reorder`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReorderPayload {
    /// Project the reordered tasks belong to.
    pub project: Id,
    /// Task ids in their new order.
    pub ordered_ids: Vec<Id>,
}

/// Payload for [`TaskAction::AddComment`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AddCommentPayload {
    /// Task the comment is attached to.
    pub item_id: Id,
    /// Comment body.
    pub content: String,
}

#[cfg(test)]
mod tests {
    use super::*;

    /// `None` ("leave unchanged") and `Some(String::new())` ("clear to
    /// empty") must survive a round trip through `EditPayload` as distinct
    /// values, not collapse to the same thing. Likewise `FieldEdit::Unchanged`
    /// must stay distinct from `FieldEdit::Clear`.
    #[test]
    fn edit_payload_distinguishes_unchanged_from_cleared() {
        let unchanged = EditPayload {
            id: Id::new("task-1"),
            content: None,
            description: None,
            project: None,
            section: FieldEdit::Unchanged,
            parent: FieldEdit::Unchanged,
            due: FieldEdit::Unchanged,
            priority: None,
            labels: None,
        };
        let cleared = EditPayload {
            description: Some(String::new()),
            section: FieldEdit::Clear,
            parent: FieldEdit::Clear,
            due: FieldEdit::Clear,
            ..unchanged.clone()
        };

        assert_ne!(unchanged, cleared);

        assert_eq!(unchanged.description, None);
        assert_eq!(cleared.description, Some(String::new()));
        assert_ne!(unchanged.description, cleared.description);

        assert_eq!(unchanged.section, FieldEdit::Unchanged);
        assert_eq!(cleared.section, FieldEdit::Clear);
        assert_ne!(unchanged.section, cleared.section);

        assert_eq!(unchanged.parent, FieldEdit::Unchanged);
        assert_eq!(cleared.parent, FieldEdit::Clear);

        assert_eq!(unchanged.due, FieldEdit::Unchanged);
        assert_eq!(cleared.due, FieldEdit::Clear);

        let set = EditPayload {
            section: FieldEdit::Set(Id::new("section-9")),
            ..unchanged.clone()
        };
        assert_ne!(set.section, unchanged.section);
        assert_ne!(set.section, cleared.section);
    }

    #[test]
    fn task_action_variants_carry_their_own_payload() {
        let actions = [
            TaskAction::Add(AddPayload {
                content: "Buy milk".to_string(),
                description: String::new(),
                project: Id::new("project-1"),
                section: None,
                parent: None,
                due: Some("tomorrow".to_string()),
                priority: Priority::new(1).unwrap(),
                labels: vec!["errand".to_string()],
            }),
            TaskAction::Edit(EditPayload {
                id: Id::new("task-1"),
                content: Some("Buy oat milk".to_string()),
                description: None,
                project: None,
                section: FieldEdit::Unchanged,
                parent: FieldEdit::Unchanged,
                due: FieldEdit::Unchanged,
                priority: None,
                labels: None,
            }),
            TaskAction::Complete(CompletePayload {
                id: Id::new("task-1"),
            }),
            TaskAction::Uncomplete(UncompletePayload {
                id: Id::new("task-1"),
            }),
            TaskAction::Delete(DeletePayload {
                id: Id::new("task-1"),
            }),
            TaskAction::Move(MovePayload {
                id: Id::new("task-1"),
                project: Some(Id::new("project-2")),
                section: None,
                parent: None,
            }),
            TaskAction::Reorder(ReorderPayload {
                project: Id::new("project-1"),
                ordered_ids: vec![Id::new("task-2"), Id::new("task-1")],
            }),
            TaskAction::AddComment(AddCommentPayload {
                item_id: Id::new("task-1"),
                content: "left a note".to_string(),
            }),
        ];

        // Exactly one variant per in-scope action, each distinct.
        assert_eq!(actions.len(), 8);
        for (i, a) in actions.iter().enumerate() {
            for (j, b) in actions.iter().enumerate() {
                if i != j {
                    assert_ne!(a, b);
                }
            }
        }
    }
}
