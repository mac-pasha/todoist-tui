//! Personal label actions: add, rename, delete (`T-805`, EPIC-09).

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::{Id, Label};
use crate::db::{queries, resources};
use crate::error::{Result, StructureError};
use crate::queue::enqueue::enqueue;
use crate::structure::cascade::{on_label_delete, CascadeSummary};
use crate::structure::command;

fn find_label(conn: &Connection, label_id: &Id) -> Result<Label> {
    queries::all_labels(conn)?
        .into_iter()
        .find(|l| &l.id == label_id)
        .ok_or_else(|| {
            StructureError::UnknownLabel {
                id: label_id.as_str().to_string(),
            }
            .into()
        })
}

/// Creates a personal label optimistically and enqueues `label_add`.
///
/// Rejects duplicate label names (case-insensitive) with [`StructureError::DuplicateLabel`]
/// before enqueuing anything.
///
/// # Errors
///
/// Returns [`StructureError`] or storage errors.
pub fn add(conn: &mut Connection, name: &str) -> Result<Id> {
    let cmd = command::label_add(name)?;
    let trimmed = name.trim();
    let lower = trimmed.to_ascii_lowercase();

    let existing = queries::all_labels(conn)?;
    if existing
        .iter()
        .any(|l| l.name.to_ascii_lowercase() == lower)
    {
        return Err(StructureError::DuplicateLabel {
            name: trimmed.to_string(),
        }
        .into());
    }

    let next_order = existing.iter().map(|l| l.item_order).max().unwrap_or(0) + 1;
    let temp_id = Id::new(cmd.temp_id.clone().unwrap_or_else(|| cmd.uuid.to_string()));
    let lbl = Label {
        id: temp_id.clone(),
        name: trimmed.to_string(),
        color: "charcoal".to_string(),
        item_order: next_order,
        is_deleted: false,
        is_favorite: false,
    };

    enqueue(conn, &cmd, |tx| resources::label::upsert(tx, &lbl))?;
    Ok(temp_id)
}

/// Renames `label_id` optimistically (including updating every cached item carrying the
/// old label name) and enqueues `label_update`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownLabel`] or validation/storage errors.
pub fn rename(conn: &mut Connection, label_id: &Id, new_name: &str) -> Result<()> {
    let mut lbl = find_label(conn, label_id)?;
    let old_name = lbl.name.clone();
    let cmd = command::label_update(label_id.as_str(), new_name)?;
    let trimmed = new_name.trim().to_string();
    lbl.name.clone_from(&trimmed);

    let items = queries::all_active_items(conn)?;
    enqueue(conn, &cmd, |tx| {
        resources::label::upsert(tx, &lbl)?;
        for mut it in items {
            let mut changed = false;
            for l in &mut it.labels {
                if *l == old_name {
                    trimmed.clone_into(l);
                    changed = true;
                }
            }
            if changed {
                resources::item::upsert(tx, &it)?;
            }
        }
        Ok(())
    })?;

    Ok(())
}

/// Deletes `label_id` via `cascade::on_label_delete` and enqueues `label_delete`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownLabel`] or storage errors.
pub fn delete(conn: &mut Connection, label_id: &Id) -> Result<CascadeSummary> {
    let _ = find_label(conn, label_id)?;
    let cmd = command::label_delete(label_id.as_str());
    let mut summary = CascadeSummary::default();
    enqueue(conn, &cmd, |tx| {
        summary = on_label_delete(tx, label_id)?;
        Ok(())
    })?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Item, Priority, Project};
    use crate::db::migrations;
    use crate::queue::store::CommandStore;

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

    #[test]
    fn duplicate_case_insensitive_add_rejected_without_enqueue() {
        let mut conn = test_conn();
        add(&mut conn, "Errand").unwrap();
        let err = add(&mut conn, "errand").unwrap_err();
        assert!(matches!(
            err,
            crate::error::AppError::Structure(StructureError::DuplicateLabel { .. })
        ));
        assert_eq!(CommandStore::new(&conn).pending(10).unwrap().len(), 1);
    }

    #[test]
    fn rename_fans_out_to_cached_items_and_delete_strips_from_items() {
        let mut conn = test_conn();
        let l1 = add(&mut conn, "focus").unwrap();

        for idx in ["t1", "t2"] {
            resources::item::upsert(
                &conn,
                &Item {
                    id: Id::new(idx),
                    content: format!("Task {idx}"),
                    description: String::new(),
                    project_id: Id::new("p1"),
                    section_id: None,
                    parent_id: None,
                    child_order: 1,
                    priority: Priority::new(1).unwrap(),
                    labels: vec!["focus".to_string(), "work".to_string()],
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

        rename(&mut conn, &l1, "deep-focus").unwrap();
        for idx in ["t1", "t2"] {
            let it = queries::item_by_id(&conn, &Id::new(idx)).unwrap().unwrap();
            assert_eq!(
                it.labels,
                vec!["deep-focus".to_string(), "work".to_string()]
            );
        }

        delete(&mut conn, &l1).unwrap();
        for idx in ["t1", "t2"] {
            let it = queries::item_by_id(&conn, &Id::new(idx)).unwrap().unwrap();
            assert_eq!(it.labels, vec!["work".to_string()]);
        }
    }
}
