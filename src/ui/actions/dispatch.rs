//! Action dispatcher: atomic command enqueue plus optimistic SQLite cache apply
//! (T-703, EPIC-08).
//!
//! [`dispatch`] is the single write-path seam every user-initiated [`TaskAction`]
//! flows through:
//! 1. Verifies the caller is authenticated (returning
//!    [`ActionError::NotAuthenticated`] without touching the database when not).
//! 2. Translates the [`TaskAction`] into one or more [`QueuedCommand`]s via
//!    [`super::commands::task_action_to_commands`].
//! 3. Invokes [`crate::queue::enqueue::enqueue_many`] so the command insert(s)
//!    and the optimistic local cache mutation execute inside a single
//!    [`rusqlite::Transaction`]. If either step fails, the transaction rolls
//!    back and neither a queued command nor a mutated cache row remains.
//!
//! This module is completely UI-free (no `ratatui` types) and performs no
//! network I/O.

use rusqlite::{params, Connection, OptionalExtension, Transaction};
use uuid::Uuid;

use super::commands::task_action_to_commands;
use super::{
    AddCommentPayload, AddPayload, CompletePayload, DeletePayload, EditPayload, FieldEdit,
    MovePayload, ReorderPayload, TaskAction, UncompletePayload,
};
use crate::api::model::{Due, Id, Item, Note};
use crate::auth::TokenStore;
use crate::db::resources::{item, note};
use crate::error::{ActionError, DbError, Result};
use crate::queue::enqueue::enqueue_many;
use crate::queue::QueuedCommand;

/// Placeholder ISO-8601 timestamp used for optimistically created items and
/// notes before the first sync delta replaces them with server-assigned
/// timestamps.
const OPTIMISTIC_TIMESTAMP: &str = "1970-01-01T00:00:00Z";

/// Placeholder user ID stored on optimistically created items until the next
/// sync delta arrives.
const OPTIMISTIC_USER_ID: &str = "me";

/// Identifiers produced by [`dispatch`] so callers can track optimistic rows
/// and correlate later per-command `sync_status` failures.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DispatchResult {
    /// UUIDs of the commands enqueued for this action, in slice order.
    pub command_uuids: Vec<Uuid>,
    /// Client-generated `temp_id` when the action created a new resource
    /// (`Add` or `AddComment`), or `None` for non-creating actions.
    pub temp_id: Option<String>,
    /// Primary resource ID affected by the action (for creating actions, the
    /// newly minted `temp_id` wrapped in [`Id`]; for item mutations, the item
    /// [`Id`]; for [`TaskAction::Reorder`], the project [`Id`]).
    pub target_id: Id,
}

/// Dispatches `action` by checking authentication in `token_store`, enqueuing
/// the corresponding Sync API write command(s), and applying the optimistic
/// cache mutation in a single SQLite transaction.
///
/// # Errors
///
/// Returns [`ActionError::NotAuthenticated`] when `token_store` holds no token,
/// [`ActionError::InvalidInput`] when the action payload is invalid or names an
/// unknown cached item, or the underlying database/queue error if the
/// transaction fails.
pub fn dispatch_with_token_store(
    conn: &mut Connection,
    token_store: &TokenStore,
    action: &TaskAction,
) -> Result<DispatchResult> {
    let authenticated = token_store.load()?.is_some();
    dispatch(conn, authenticated, action)
}

/// Dispatches `action` atomically into the command queue and local SQLite
/// cache when `authenticated` is `true`.
///
/// Never performs network I/O and returns immediately without waiting for a
/// background sync cycle.
///
/// # Errors
///
/// Returns [`ActionError::NotAuthenticated`] when `authenticated` is `false`
/// (enqueuing and mutating nothing), [`ActionError::InvalidInput`] when
/// `action` is invalid or targets a missing item, or any SQLite/queue error
/// raised while committing the transaction.
pub fn dispatch(
    conn: &mut Connection,
    authenticated: bool,
    action: &TaskAction,
) -> Result<DispatchResult> {
    dispatch_with_hook(conn, authenticated, action, |_| Ok(()))
}

/// Dispatches `action` like [`dispatch`], invoking `after_apply` inside the
/// same SQLite transaction after the optimistic cache mutation has run.
///
/// Exposed for tests that force a mid-transaction error to verify atomic
/// rollback of both the `commands` table and the resource tables.
///
/// # Errors
///
/// Returns any error from authentication check, command construction,
/// optimistic cache mutation, `after_apply`, or transaction commit.
pub fn dispatch_with_hook<F>(
    conn: &mut Connection,
    authenticated: bool,
    action: &TaskAction,
    after_apply: F,
) -> Result<DispatchResult>
where
    F: FnOnce(&Transaction<'_>) -> Result<()>,
{
    if !authenticated {
        return Err(ActionError::NotAuthenticated.into());
    }

    let commands = task_action_to_commands(action)?;
    let command_uuids: Vec<Uuid> = commands.iter().map(|cmd| cmd.uuid).collect();
    let temp_id = commands.first().and_then(|cmd| cmd.temp_id.clone());
    let target_id = resolve_target_id(action, temp_id.as_deref());

    enqueue_many(conn, &commands, |tx| {
        apply_action_locally(tx, action, &commands)?;
        after_apply(tx)
    })?;

    Ok(DispatchResult {
        command_uuids,
        temp_id,
        target_id,
    })
}

/// Rolls back the optimistic local creation for a rejected `item_add` or
/// `note_add` command when [`crate::queue::status::ErrorClass::should_rollback`]
/// is `true`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if removing the optimistic row fails.
pub fn rollback_task_command(tx: &Transaction<'_>, cmd: &QueuedCommand) -> Result<()> {
    match cmd.kind.as_str() {
        "item_add" => {
            if let Some(tid) = cmd.temp_id.as_deref() {
                item::delete(tx, tid)?;
            }
        }
        "note_add" => {
            if let Some(tid) = cmd.temp_id.as_deref() {
                note::delete(tx, tid)?;
            }
        }
        _ => {}
    }
    Ok(())
}

fn resolve_target_id(action: &TaskAction, temp_id: Option<&str>) -> Id {
    match action {
        TaskAction::Add(_) | TaskAction::AddComment(_) => {
            Id::new(temp_id.unwrap_or_default().to_owned())
        }
        TaskAction::Edit(p) => p.id.clone(),
        TaskAction::Complete(p) => p.id.clone(),
        TaskAction::Uncomplete(p) => p.id.clone(),
        TaskAction::Delete(p) => p.id.clone(),
        TaskAction::Move(p) => p.id.clone(),
        TaskAction::Reorder(p) => p.project.clone(),
    }
}

/// Applies the optimistic SQLite cache mutation for `action` inside the
/// caller's transaction `tx`.
pub fn apply_action_locally(
    tx: &Transaction<'_>,
    action: &TaskAction,
    commands: &[QueuedCommand],
) -> Result<()> {
    match action {
        TaskAction::Add(payload) => apply_add(tx, payload, commands),
        TaskAction::Edit(payload) => apply_edit(tx, payload),
        TaskAction::Complete(payload) => apply_complete(tx, payload),
        TaskAction::Uncomplete(payload) => apply_uncomplete(tx, payload),
        TaskAction::Delete(payload) => apply_delete(tx, payload),
        TaskAction::Move(payload) => apply_move(tx, payload),
        TaskAction::Reorder(payload) => apply_reorder(tx, payload),
        TaskAction::AddComment(payload) => apply_add_comment(tx, payload, commands),
    }
}

fn apply_add(tx: &Transaction<'_>, payload: &AddPayload, commands: &[QueuedCommand]) -> Result<()> {
    let temp_id = commands
        .first()
        .and_then(|c| c.temp_id.as_deref())
        .ok_or_else(|| ActionError::InvalidInput {
            field: "temp_id",
            reason: "item_add command must carry a temp_id".to_string(),
        })?;

    let next_order: i64 = tx
        .query_row(
            "SELECT COALESCE(MAX(child_order), 0) + 1 FROM items \
             WHERE project_id = ?1 AND is_deleted = 0",
            params![payload.project.as_str()],
            |row| row.get(0),
        )
        .map_err(|source| DbError::Sqlite { source })?;

    let new_item = Item {
        id: Id::new(temp_id),
        content: payload.content.clone(),
        description: payload.description.clone(),
        project_id: payload.project.clone(),
        section_id: payload.section.clone(),
        parent_id: payload.parent.clone(),
        child_order: next_order,
        priority: payload.priority,
        labels: payload.labels.clone(),
        due: payload
            .due
            .as_deref()
            .map(|due_str| build_optimistic_due(due_str, None)),
        deadline: None,
        checked: false,
        is_deleted: false,
        added_at: OPTIMISTIC_TIMESTAMP.to_string(),
        completed_at: None,
        user_id: Id::new(OPTIMISTIC_USER_ID),
        responsible_uid: None,
    };

    item::upsert(tx, &new_item)
}

fn apply_edit(tx: &Transaction<'_>, payload: &EditPayload) -> Result<()> {
    let mut existing = require_item(tx, &payload.id)?;

    if let Some(content) = &payload.content {
        existing.content.clone_from(content);
    }
    if let Some(description) = &payload.description {
        existing.description.clone_from(description);
    }
    if let Some(project) = &payload.project {
        existing.project_id = project.clone();
    }
    match &payload.section {
        FieldEdit::Unchanged => {}
        FieldEdit::Clear => existing.section_id = None,
        FieldEdit::Set(section_id) => existing.section_id = Some(section_id.clone()),
    }
    match &payload.parent {
        FieldEdit::Unchanged => {}
        FieldEdit::Clear => existing.parent_id = None,
        FieldEdit::Set(parent_id) => existing.parent_id = Some(parent_id.clone()),
    }
    match &payload.due {
        FieldEdit::Unchanged => {}
        FieldEdit::Clear => existing.due = None,
        FieldEdit::Set(due_str) => {
            existing.due = Some(build_optimistic_due(due_str, existing.due.as_ref()));
        }
    }
    if let Some(priority) = payload.priority {
        existing.priority = priority;
    }
    if let Some(labels) = &payload.labels {
        existing.labels.clone_from(labels);
    }

    item::upsert(tx, &existing)
}

fn apply_complete(tx: &Transaction<'_>, payload: &CompletePayload) -> Result<()> {
    let mut existing = require_item(tx, &payload.id)?;
    existing.checked = true;
    item::upsert(tx, &existing)
}

fn apply_uncomplete(tx: &Transaction<'_>, payload: &UncompletePayload) -> Result<()> {
    let mut existing = require_item(tx, &payload.id)?;
    existing.checked = false;
    existing.completed_at = None;
    item::upsert(tx, &existing)
}

fn apply_delete(tx: &Transaction<'_>, payload: &DeletePayload) -> Result<()> {
    item::delete(tx, payload.id.as_str())
}

fn apply_move(tx: &Transaction<'_>, payload: &MovePayload) -> Result<()> {
    let mut existing = require_item(tx, &payload.id)?;

    if let Some(project_id) = &payload.project {
        existing.project_id = project_id.clone();
        existing.section_id = None;
        existing.parent_id = None;
    } else if let Some(section_id) = &payload.section {
        let section_project: Option<String> = tx
            .query_row(
                "SELECT project_id FROM sections WHERE id = ?1 AND is_deleted = 0",
                params![section_id.as_str()],
                |row| row.get(0),
            )
            .optional()
            .map_err(|source| DbError::Sqlite { source })?;
        if let Some(proj_id) = section_project {
            existing.project_id = Id::new(proj_id);
        }
        existing.section_id = Some(section_id.clone());
        existing.parent_id = None;
    } else if let Some(parent_id) = &payload.parent {
        if let Some(parent_item) = item::fetch(tx, parent_id.as_str())? {
            existing.project_id = parent_item.project_id;
            existing.section_id = parent_item.section_id;
        }
        existing.parent_id = Some(parent_id.clone());
    }

    item::upsert(tx, &existing)
}

fn apply_reorder(tx: &Transaction<'_>, payload: &ReorderPayload) -> Result<()> {
    for (index, id) in payload.ordered_ids.iter().enumerate() {
        let mut existing = require_item(tx, id)?;
        let order = i64::try_from(index + 1).unwrap_or(i64::MAX);
        existing.child_order = order;
        item::upsert(tx, &existing)?;
    }
    Ok(())
}

fn apply_add_comment(
    tx: &Transaction<'_>,
    payload: &AddCommentPayload,
    commands: &[QueuedCommand],
) -> Result<()> {
    let temp_id = commands
        .first()
        .and_then(|c| c.temp_id.as_deref())
        .ok_or_else(|| ActionError::InvalidInput {
            field: "temp_id",
            reason: "note_add command must carry a temp_id".to_string(),
        })?;

    let new_note = Note {
        id: Id::new(temp_id),
        item_id: Some(payload.item_id.clone()),
        project_id: None,
        content: payload.content.clone(),
        posted_at: OPTIMISTIC_TIMESTAMP.to_string(),
        is_deleted: false,
        file_attachment: None,
    };

    note::upsert(tx, &new_note)
}

fn require_item(tx: &Transaction<'_>, id: &Id) -> Result<Item> {
    item::fetch(tx, id.as_str())?.ok_or_else(|| {
        ActionError::InvalidInput {
            field: "id",
            reason: format!("unknown item: {id}"),
        }
        .into()
    })
}

fn build_optimistic_due(due_str: &str, existing: Option<&Due>) -> Due {
    let date = if looks_like_iso_date_prefix(due_str) {
        due_str.to_string()
    } else if let Some(prev) = existing {
        prev.date.clone()
    } else {
        due_str.to_string()
    };

    Due {
        date,
        string: Some(due_str.to_string()),
        lang: existing.and_then(|d| d.lang.clone()),
        is_recurring: existing.is_some_and(|d| d.is_recurring),
        timezone: existing.and_then(|d| d.timezone.clone()),
    }
}

fn looks_like_iso_date_prefix(s: &str) -> bool {
    let bytes = s.as_bytes();
    bytes.len() >= 10
        && bytes[0].is_ascii_digit()
        && bytes[1].is_ascii_digit()
        && bytes[2].is_ascii_digit()
        && bytes[3].is_ascii_digit()
        && bytes[4] == b'-'
        && bytes[5].is_ascii_digit()
        && bytes[6].is_ascii_digit()
        && bytes[7] == b'-'
        && bytes[8].is_ascii_digit()
        && bytes[9].is_ascii_digit()
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::expect_used, clippy::too_many_lines)]
mod tests {
    //! Unit tests for T-703 action dispatcher (`dispatch.rs`).
    //!
    //! Runs against an in-memory SQLite database with fake fixture data only.
    use rusqlite::Connection;

    use super::*;
    use crate::api::model::{Priority, Project, Section};
    use crate::auth::{MemoryTokenSource, Token, TokenSource, TokenStore};
    use crate::db::{migrations, queries, resources};
    use crate::error::AppError;
    use crate::queue::CommandStore;

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().expect("open in-memory db");
        migrations::run(&mut conn).expect("run migrations");
        conn
    }

    fn seed_project(conn: &Connection, id: &str, name: &str) {
        resources::project::upsert(
            conn,
            &Project {
                id: Id::new(id),
                name: name.to_string(),
                parent_id: None,
                child_order: 1,
                color: "charcoal".to_string(),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: "list".to_string(),
            },
        )
        .expect("seed project");
    }

    fn seed_section(conn: &Connection, id: &str, project_id: &str, name: &str) {
        resources::section::upsert(
            conn,
            &Section {
                id: Id::new(id),
                name: name.to_string(),
                project_id: Id::new(project_id),
                section_order: 1,
                is_deleted: false,
                is_archived: false,
            },
        )
        .expect("seed section");
    }

    fn seed_item(conn: &Connection, id: &str, project_id: &str, order: i64) -> Item {
        let it = Item {
            id: Id::new(id),
            content: format!("Task {id}"),
            description: "initial desc".to_string(),
            project_id: Id::new(project_id),
            section_id: None,
            parent_id: None,
            child_order: order,
            priority: Priority::new(1).expect("p1"),
            labels: vec!["tag1".to_string()],
            due: None,
            deadline: None,
            checked: false,
            is_deleted: false,
            added_at: "2025-01-01T00:00:00Z".to_string(),
            completed_at: None,
            user_id: Id::new("user_fake_1"),
            responsible_uid: None,
        };
        resources::item::upsert(conn, &it).expect("seed item");
        it
    }

    fn count_commands(conn: &Connection) -> usize {
        CommandStore::new(conn)
            .pending(100)
            .expect("pending commands")
            .len()
    }

    #[test]
    fn unauthenticated_dispatch_returns_not_authenticated_and_enqueues_nothing() {
        let mut conn = test_conn();
        seed_project(&conn, "proj_fake_1", "Inbox");

        let action = TaskAction::Add(AddPayload {
            content: "Should not be added".to_string(),
            description: String::new(),
            project: Id::new("proj_fake_1"),
            section: None,
            parent: None,
            due: None,
            priority: Priority::new(1).unwrap(),
            labels: Vec::new(),
        });

        let err = dispatch(&mut conn, false, &action).expect_err("must reject unauthenticated");
        assert!(matches!(
            err,
            AppError::Action(ActionError::NotAuthenticated)
        ));
        assert_eq!(count_commands(&conn), 0);
        assert_eq!(
            queries::items_by_project(&conn, &Id::new("proj_fake_1")).unwrap(),
            Vec::<Item>::new()
        );

        // Also verify via empty TokenStore.
        let empty_store = TokenStore::with_sources(
            Box::new(MemoryTokenSource::new()),
            Box::new(MemoryTokenSource::new()),
        );
        let err2 = dispatch_with_token_store(&mut conn, &empty_store, &action)
            .expect_err("empty token store must reject");
        assert!(matches!(
            err2,
            AppError::Action(ActionError::NotAuthenticated)
        ));
        assert_eq!(count_commands(&conn), 0);
    }

    #[test]
    fn authenticated_token_store_dispatches_without_network_io() {
        let mut conn = test_conn();
        seed_project(&conn, "proj_fake_1", "Inbox");

        let primary = MemoryTokenSource::new();
        primary
            .store(&Token::new("tok_fake_abc123".to_string()).unwrap())
            .unwrap();
        let store = TokenStore::with_sources(Box::new(primary), Box::new(MemoryTokenSource::new()));

        let action = TaskAction::Add(AddPayload {
            content: "Offline local task".to_string(),
            description: "body".to_string(),
            project: Id::new("proj_fake_1"),
            section: None,
            parent: None,
            due: Some("2025-05-10".to_string()),
            priority: Priority::new(3).unwrap(),
            labels: vec!["work".to_string()],
        });

        // No API client is passed or constructed; dispatch completes purely in-memory.
        let result =
            dispatch_with_token_store(&mut conn, &store, &action).expect("dispatch succeeds");
        assert_eq!(result.command_uuids.len(), 1);
        let temp_id = result.temp_id.expect("add returns temp_id");
        assert_eq!(result.target_id.as_str(), temp_id);

        let items = queries::items_by_project(&conn, &Id::new("proj_fake_1")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_str(), temp_id);
        assert_eq!(items[0].content, "Offline local task");
        assert_eq!(items[0].description, "body");
        assert_eq!(items[0].priority.get(), 3);
        assert_eq!(items[0].labels, vec!["work".to_string()]);
        assert_eq!(
            items[0].due.as_ref().and_then(|d| d.string.as_deref()),
            Some("2025-05-10")
        );
    }

    #[test]
    fn forced_rollback_leaves_neither_queued_command_nor_mutated_row() {
        let mut conn = test_conn();
        seed_project(&conn, "proj_fake_1", "Inbox");
        seed_item(&conn, "item_fake_1", "proj_fake_1", 1);

        let add_action = TaskAction::Add(AddPayload {
            content: "Rolled back task".to_string(),
            description: String::new(),
            project: Id::new("proj_fake_1"),
            section: None,
            parent: None,
            due: None,
            priority: Priority::new(1).unwrap(),
            labels: Vec::new(),
        });

        // 1. Force rollback via the after_apply hook.
        let res = dispatch_with_hook(&mut conn, true, &add_action, |_tx| {
            Err(DbError::Sqlite {
                source: rusqlite::Error::InvalidQuery,
            }
            .into())
        });
        assert!(res.is_err());
        assert_eq!(count_commands(&conn), 0);
        let items = queries::items_by_project(&conn, &Id::new("proj_fake_1")).unwrap();
        assert_eq!(items.len(), 1);
        assert_eq!(items[0].id.as_str(), "item_fake_1");

        // 2. Force rollback via a SQLite BEFORE INSERT trigger on `commands`.
        conn.execute_batch(
            "CREATE TRIGGER abort_commands BEFORE INSERT ON commands \
             BEGIN \
                 SELECT RAISE(ABORT, 'simulated queue write failure'); \
             END;",
        )
        .unwrap();

        let edit_action = TaskAction::Edit(EditPayload {
            id: Id::new("item_fake_1"),
            content: Some("Mutated title".to_string()),
            description: None,
            project: None,
            section: FieldEdit::Unchanged,
            parent: FieldEdit::Unchanged,
            due: FieldEdit::Unchanged,
            priority: None,
            labels: None,
        });

        let res2 = dispatch(&mut conn, true, &edit_action);
        assert!(res2.is_err());
        assert_eq!(count_commands(&conn), 0);
        let unchanged = queries::item_by_id(&conn, &Id::new("item_fake_1"))
            .unwrap()
            .unwrap();
        assert_eq!(unchanged.content, "Task item_fake_1");
    }

    #[test]
    fn optimistic_apply_covers_all_eight_actions() {
        let mut conn = test_conn();
        seed_project(&conn, "proj_fake_1", "Inbox");
        seed_project(&conn, "proj_fake_2", "Work");
        seed_section(&conn, "sec_fake_1", "proj_fake_2", "Sprint");

        // 1. Add
        let add_res = dispatch(
            &mut conn,
            true,
            &TaskAction::Add(AddPayload {
                content: "First task".to_string(),
                description: "desc 1".to_string(),
                project: Id::new("proj_fake_1"),
                section: None,
                parent: None,
                due: Some("tomorrow".to_string()),
                priority: Priority::new(2).unwrap(),
                labels: vec!["home".to_string()],
            }),
        )
        .unwrap();
        let item1_id = add_res.target_id.clone();

        let add_res2 = dispatch(
            &mut conn,
            true,
            &TaskAction::Add(AddPayload {
                content: "Second task".to_string(),
                description: String::new(),
                project: Id::new("proj_fake_1"),
                section: None,
                parent: None,
                due: None,
                priority: Priority::new(1).unwrap(),
                labels: Vec::new(),
            }),
        )
        .unwrap();
        let item2_id = add_res2.target_id.clone();

        // 2. Edit
        dispatch(
            &mut conn,
            true,
            &TaskAction::Edit(EditPayload {
                id: item1_id.clone(),
                content: Some("First task edited".to_string()),
                description: Some(String::new()),
                project: None,
                section: FieldEdit::Set(Id::new("sec_fake_1")),
                parent: FieldEdit::Unchanged,
                due: FieldEdit::Clear,
                priority: Some(Priority::new(4).unwrap()),
                labels: Some(vec!["urgent".to_string()]),
            }),
        )
        .unwrap();

        let edited = queries::item_by_id(&conn, &item1_id).unwrap().unwrap();
        assert_eq!(edited.content, "First task edited");
        assert_eq!(edited.description, "");
        assert_eq!(edited.section_id, Some(Id::new("sec_fake_1")));
        assert_eq!(edited.due, None);
        assert_eq!(edited.priority.get(), 4);
        assert_eq!(edited.labels, vec!["urgent".to_string()]);

        // 3. Complete
        dispatch(
            &mut conn,
            true,
            &TaskAction::Complete(CompletePayload {
                id: item1_id.clone(),
            }),
        )
        .unwrap();
        let completed = queries::item_by_id(&conn, &item1_id).unwrap().unwrap();
        assert!(completed.checked);
        // Active list excludes completed item.
        let active = queries::items_by_project(&conn, &Id::new("proj_fake_1")).unwrap();
        assert_eq!(active.len(), 1);
        assert_eq!(active[0].id, item2_id);

        // 4. Uncomplete
        dispatch(
            &mut conn,
            true,
            &TaskAction::Uncomplete(UncompletePayload {
                id: item1_id.clone(),
            }),
        )
        .unwrap();
        let reopened = queries::item_by_id(&conn, &item1_id).unwrap().unwrap();
        assert!(!reopened.checked);
        assert_eq!(
            queries::items_by_project(&conn, &Id::new("proj_fake_1"))
                .unwrap()
                .len(),
            2
        );

        // 5. Reorder (flip item2 before item1)
        dispatch(
            &mut conn,
            true,
            &TaskAction::Reorder(ReorderPayload {
                project: Id::new("proj_fake_1"),
                ordered_ids: vec![item2_id.clone(), item1_id.clone()],
            }),
        )
        .unwrap();
        let reordered = queries::items_by_project(&conn, &Id::new("proj_fake_1")).unwrap();
        assert_eq!(reordered[0].id, item2_id);
        assert_eq!(reordered[0].child_order, 1);
        assert_eq!(reordered[1].id, item1_id);
        assert_eq!(reordered[1].child_order, 2);

        // 6. Move (move item1 to proj_fake_2)
        dispatch(
            &mut conn,
            true,
            &TaskAction::Move(MovePayload {
                id: item1_id.clone(),
                project: Some(Id::new("proj_fake_2")),
                section: None,
                parent: None,
            }),
        )
        .unwrap();
        let moved = queries::item_by_id(&conn, &item1_id).unwrap().unwrap();
        assert_eq!(moved.project_id, Id::new("proj_fake_2"));
        assert_eq!(moved.section_id, None);
        assert_eq!(moved.parent_id, None);

        // 7. AddComment
        let comment_res = dispatch(
            &mut conn,
            true,
            &TaskAction::AddComment(AddCommentPayload {
                item_id: item1_id.clone(),
                content: "A note on item 1".to_string(),
            }),
        )
        .unwrap();
        let notes = queries::notes_for_item(&conn, &item1_id).unwrap();
        assert_eq!(notes.len(), 1);
        assert_eq!(notes[0].id, comment_res.target_id);
        assert_eq!(notes[0].content, "A note on item 1");

        // 8. Delete
        dispatch(
            &mut conn,
            true,
            &TaskAction::Delete(DeletePayload {
                id: item2_id.clone(),
            }),
        )
        .unwrap();
        assert_eq!(queries::item_by_id(&conn, &item2_id).unwrap(), None);

        // 9 commands total were enqueued in order (2 adds + 7 actions).
        assert_eq!(count_commands(&conn), 9);
    }
}
