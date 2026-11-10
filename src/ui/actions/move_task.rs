//! Move-task controller with cycle prevention (`T-708`, EPIC-08).

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::Id;
use crate::db::queries;
use crate::error::{ActionError, Result};
use crate::ui::actions::dispatch::{dispatch, DispatchResult};
use crate::ui::actions::{MovePayload, TaskAction};
use crate::ui::widgets::picker::{DestinationPicker, DestinationTarget, PickerEntry};

/// Builds a [`DestinationPicker`] populated from cached projects, sections, and tasks.
///
/// # Errors
///
/// Propagates SQLite query errors.
pub fn build_move_picker(conn: &Connection, exclude_task_id: &Id) -> Result<DestinationPicker> {
    let mut entries = Vec::new();
    let projects = queries::all_projects(conn)?;

    for proj in &projects {
        entries.push(PickerEntry {
            label: format!("Project: {}", proj.name),
            target: DestinationTarget::Project(proj.id.clone()),
        });

        let sections = queries::sections_by_project(conn, &proj.id)?;
        for sec in sections {
            entries.push(PickerEntry {
                label: format!("Section: {} / {}", proj.name, sec.name),
                target: DestinationTarget::Section(sec.id.clone()),
            });
        }
    }

    let items = queries::all_active_items(conn)?;
    for it in items {
        if &it.id != exclude_task_id {
            entries.push(PickerEntry {
                label: format!("Parent Task: {}", it.content),
                target: DestinationTarget::Parent(it.id.clone()),
            });
        }
    }

    Ok(DestinationPicker::new(entries))
}

/// Returns `true` if `candidate_parent` is `task_id` or a descendant of `task_id`.
///
/// # Errors
///
/// Propagates SQLite query errors.
pub fn is_self_or_descendant(
    conn: &Connection,
    task_id: &Id,
    candidate_parent: &Id,
) -> Result<bool> {
    if candidate_parent == task_id {
        return Ok(true);
    }

    let mut cursor = Some(candidate_parent.clone());
    let mut visited = std::collections::HashSet::new();
    while let Some(curr_id) = cursor {
        if !visited.insert(curr_id.clone()) {
            break;
        }
        if let Some(item) = queries::item_by_id(conn, &curr_id)? {
            if let Some(ref parent_id) = item.parent_id {
                if parent_id == task_id {
                    return Ok(true);
                }
                cursor = Some(parent_id.clone());
            } else {
                cursor = None;
            }
        } else {
            cursor = None;
        }
    }

    Ok(false)
}

/// Validates destination (refusing self/descendant cycle) and dispatches `TaskAction::Move`.
///
/// # Errors
///
/// Returns [`ActionError::InvalidInput`] on a cycle, or propagates dispatch/DB errors.
pub fn move_task(
    conn: &mut Connection,
    authenticated: bool,
    task_id: Id,
    target: DestinationTarget,
) -> Result<DispatchResult> {
    let payload = match target {
        DestinationTarget::Project(pid) => MovePayload {
            id: task_id,
            project: Some(pid),
            section: None,
            parent: None,
        },
        DestinationTarget::Section(sid) => MovePayload {
            id: task_id,
            project: None,
            section: Some(sid),
            parent: None,
        },
        DestinationTarget::Parent(parent_id) => {
            if is_self_or_descendant(conn, &task_id, &parent_id)? {
                return Err(ActionError::InvalidInput {
                    field: "parent",
                    reason: "cannot move a task under itself or its own descendant".to_string(),
                }
                .into());
            }
            MovePayload {
                id: task_id,
                project: None,
                section: None,
                parent: Some(parent_id),
            }
        }
    };

    dispatch(conn, authenticated, &TaskAction::Move(payload))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Item, Priority, Project};
    use crate::db::{migrations, resources};

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        conn
    }

    fn seed_item(conn: &Connection, id: &str, parent_id: Option<&str>) {
        let item = Item {
            id: Id::new(id),
            content: format!("Task {id}"),
            description: String::new(),
            project_id: Id::new("p1"),
            section_id: None,
            parent_id: parent_id.map(Id::new),
            child_order: 1,
            priority: Priority::new(1).unwrap(),
            labels: Vec::new(),
            due: None,
            deadline: None,
            checked: false,
            is_deleted: false,
            added_at: "2026-01-01T00:00:00Z".to_string(),
            completed_at: None,
            user_id: Id::new("u1"),
            responsible_uid: None,
        };
        resources::item::upsert(conn, &item).unwrap();
    }

    #[test]
    fn refuses_moving_task_under_itself_or_descendant_in_3_level_tree() {
        let mut conn = test_conn();
        resources::project::upsert(
            &conn,
            &Project {
                id: Id::new("p1"),
                name: "Inbox".to_string(),
                parent_id: None,
                child_order: 1,
                color: "grey".to_string(),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: "list".to_string(),
            },
        )
        .unwrap();

        seed_item(&conn, "root", None);
        seed_item(&conn, "child", Some("root"));
        seed_item(&conn, "grandchild", Some("child"));

        // Moving root -> self or child or grandchild must error with zero queued commands
        for target_id in ["root", "child", "grandchild"] {
            let err = move_task(
                &mut conn,
                true,
                Id::new("root"),
                DestinationTarget::Parent(Id::new(target_id)),
            )
            .unwrap_err();
            assert!(err.to_string().contains("descendant"));
        }

        let cmd_count: i64 = conn
            .query_row("SELECT COUNT(*) FROM commands", [], |r| r.get(0))
            .unwrap();
        assert_eq!(cmd_count, 0);
    }
}
