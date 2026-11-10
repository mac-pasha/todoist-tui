//! Sibling-scoped task reordering (`T-709`, EPIC-08).

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::{Id, Item};
use crate::db::queries;
use crate::error::Result;
use crate::ui::actions::dispatch::{dispatch, DispatchResult};
use crate::ui::actions::{ReorderPayload, TaskAction};

/// Direction to move a task among its siblings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReorderDirection {
    Up,
    Down,
}

/// Reorders `task_id` one step `dir` within its exact sibling group
/// (`same project_id, same section_id, same parent_id`).
///
/// Moving the first sibling up or last sibling down is a clean no-op returning
/// `Ok(None)` without writing to the queue or cache.
///
/// # Errors
///
/// Propagates SQLite query or dispatch errors.
pub fn reorder_task(
    conn: &mut Connection,
    authenticated: bool,
    task_id: &Id,
    dir: ReorderDirection,
) -> Result<Option<DispatchResult>> {
    let Some(target) = queries::item_by_id(conn, task_id)? else {
        return Ok(None);
    };

    let mut siblings: Vec<Item> = queries::items_by_project(conn, &target.project_id)?
        .into_iter()
        .filter(|it| it.section_id == target.section_id && it.parent_id == target.parent_id)
        .collect();

    let Some(pos) = siblings.iter().position(|it| &it.id == task_id) else {
        return Ok(None);
    };

    let swap_idx = match dir {
        ReorderDirection::Up => {
            if pos == 0 {
                return Ok(None);
            }
            pos - 1
        }
        ReorderDirection::Down => {
            if pos + 1 >= siblings.len() {
                return Ok(None);
            }
            pos + 1
        }
    };

    siblings.swap(pos, swap_idx);
    let ordered_ids: Vec<Id> = siblings.into_iter().map(|it| it.id).collect();

    let res = dispatch(
        conn,
        authenticated,
        &TaskAction::Reorder(ReorderPayload {
            project: target.project_id,
            ordered_ids,
        }),
    )?;

    Ok(Some(res))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Priority, Project};
    use crate::db::{migrations, resources};

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
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
        conn
    }

    fn seed(conn: &Connection, id: &str, order: i64, section_id: Option<&str>) {
        resources::item::upsert(
            conn,
            &Item {
                id: Id::new(id),
                content: format!("Item {id}"),
                description: String::new(),
                project_id: Id::new("p1"),
                section_id: section_id.map(Id::new),
                parent_id: None,
                child_order: order,
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
            },
        )
        .unwrap();
    }

    #[test]
    fn reorders_within_sibling_group_and_noops_at_bounds() {
        let mut conn = test_conn();
        for i in 1..=5 {
            seed(&conn, &format!("t{i}"), i, None);
        }
        // Different section sibling must keep its own order untouched
        seed(&conn, "other_sec", 99, Some("sec_1"));

        // Moving first up is a no-op
        assert!(
            reorder_task(&mut conn, true, &Id::new("t1"), ReorderDirection::Up)
                .unwrap()
                .is_none()
        );

        // Moving last down is a no-op
        assert!(
            reorder_task(&mut conn, true, &Id::new("t5"), ReorderDirection::Down)
                .unwrap()
                .is_none()
        );

        // Move t2 up -> order becomes t2(1), t1(2), t3(3), t4(4), t5(5)
        let res = reorder_task(&mut conn, true, &Id::new("t2"), ReorderDirection::Up)
            .unwrap()
            .expect("reorder dispatched");
        assert_eq!(res.command_uuids.len(), 1);

        let all = queries::items_by_project(&conn, &Id::new("p1")).unwrap();
        let ungrouped: Vec<String> = all
            .iter()
            .filter(|i| i.section_id.is_none())
            .map(|i| format!("{}:{}", i.id, i.child_order))
            .collect();
        assert_eq!(ungrouped, vec!["t2:1", "t1:2", "t3:3", "t4:4", "t5:5"]);

        let other = queries::item_by_id(&conn, &Id::new("other_sec"))
            .unwrap()
            .unwrap();
        assert_eq!(other.child_order, 99);
    }
}
