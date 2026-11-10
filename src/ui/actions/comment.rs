//! Comment controller wiring `CommentsModal` to `dispatch(AddComment)` (`T-710`, EPIC-08).

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::Id;
use crate::db::queries;
use crate::error::{ActionError, Result};
use crate::ui::actions::dispatch::{dispatch, DispatchResult};
use crate::ui::actions::{AddCommentPayload, TaskAction};
use crate::ui::model::Comment;
use crate::ui::widgets::comments::CommentsModal;

/// Opens [`CommentsModal`] populated with cached notes oldest-first for `task_id`.
///
/// # Errors
///
/// Propagates SQLite query errors.
pub fn open_comments_modal(
    conn: &Connection,
    task_id: Id,
    task_title: impl Into<String>,
) -> Result<CommentsModal> {
    let notes = queries::notes_for_item(conn, &task_id)?;
    let comments: Vec<Comment> = notes
        .into_iter()
        .map(|n| Comment {
            id: n.id,
            posted_at: n.posted_at,
            author: "user".to_string(),
            content: n.content,
            attachment_name: n.file_attachment.and_then(|f| f.file_name),
        })
        .collect();
    Ok(CommentsModal::new(task_id, task_title, comments))
}

/// Validates non-empty content and dispatches `TaskAction::AddComment`.
///
/// Works seamlessly when `item_id` is still a temporary ID (`tmp_...`), so EPIC-05
/// `temp_id_mapping` rewrites `note_add.args.item_id` when the parent task flushes.
///
/// # Errors
///
/// Returns [`ActionError::InvalidInput`] on empty content, or propagates dispatch errors.
pub fn submit_comment(
    conn: &mut Connection,
    authenticated: bool,
    item_id: Id,
    content: &str,
) -> Result<DispatchResult> {
    let trimmed = content.trim();
    if trimmed.is_empty() {
        return Err(ActionError::InvalidInput {
            field: "content",
            reason: "comment content cannot be empty".to_string(),
        }
        .into());
    }
    dispatch(
        conn,
        authenticated,
        &TaskAction::AddComment(AddCommentPayload {
            item_id,
            content: trimmed.to_string(),
        }),
    )
}
