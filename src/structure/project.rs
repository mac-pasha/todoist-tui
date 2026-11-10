//! Project structure actions: `add`, `add_child`, `rename`, `archive`, `unarchive`, `delete` (`T-803`, EPIC-09).

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::{Id, Project};
use crate::db::{queries, resources};
use crate::error::{Result, StructureError};
use crate::queue::enqueue::enqueue;
use crate::structure::cascade::{on_project_archive, on_project_delete, CascadeSummary};
use crate::structure::command;

fn next_child_order(conn: &Connection, parent_id: Option<&Id>) -> Result<i64> {
    let max_order = queries::all_projects(conn)?
        .into_iter()
        .filter(|p| p.parent_id.as_ref() == parent_id)
        .map(|p| p.child_order)
        .max()
        .unwrap_or(0);
    Ok(max_order + 1)
}

fn find_project(conn: &Connection, id: &Id) -> Result<Project> {
    queries::all_projects(conn)?
        .into_iter()
        .find(|p| &p.id == id)
        .ok_or_else(|| {
            StructureError::UnknownProject {
                id: id.as_str().to_string(),
            }
            .into()
        })
}

/// Creates a top-level project optimistically and enqueues `project_add`.
///
/// # Errors
///
/// Returns [`StructureError::EmptyName`] / [`StructureError::NameTooLong`] or storage errors.
pub fn add(conn: &mut Connection, name: &str) -> Result<Id> {
    let cmd = command::project_add(name, None)?;
    let temp_id = Id::new(cmd.temp_id.clone().unwrap_or_else(|| cmd.uuid.to_string()));
    let trimmed_name = name.trim().to_string();
    let order = next_child_order(conn, None)?;

    let proj = Project {
        id: temp_id.clone(),
        name: trimmed_name,
        parent_id: None,
        child_order: order,
        color: "charcoal".to_string(),
        is_favorite: false,
        is_archived: false,
        is_deleted: false,
        view_style: "list".to_string(),
    };

    enqueue(conn, &cmd, |tx| resources::project::upsert(tx, &proj))?;
    Ok(temp_id)
}

/// Creates a child sub-project under `parent_id` and enqueues `project_add`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] when `parent_id` does not exist in cache.
pub fn add_child(conn: &mut Connection, name: &str, parent_id: &Id) -> Result<Id> {
    let _parent = find_project(conn, parent_id)?;
    let cmd = command::project_add(name, Some(parent_id.as_str()))?;
    let temp_id = Id::new(cmd.temp_id.clone().unwrap_or_else(|| cmd.uuid.to_string()));
    let trimmed_name = name.trim().to_string();
    let order = next_child_order(conn, Some(parent_id))?;

    let proj = Project {
        id: temp_id.clone(),
        name: trimmed_name,
        parent_id: Some(parent_id.clone()),
        child_order: order,
        color: "charcoal".to_string(),
        is_favorite: false,
        is_archived: false,
        is_deleted: false,
        view_style: "list".to_string(),
    };

    enqueue(conn, &cmd, |tx| resources::project::upsert(tx, &proj))?;
    Ok(temp_id)
}

/// Renames an existing project optimistically and enqueues `project_update`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] or validation/storage errors.
pub fn rename(conn: &mut Connection, project_id: &Id, new_name: &str) -> Result<()> {
    let mut proj = find_project(conn, project_id)?;
    let cmd = command::project_update(project_id.as_str(), new_name)?;
    proj.name = new_name.trim().to_string();

    enqueue(conn, &cmd, |tx| resources::project::upsert(tx, &proj))?;
    Ok(())
}

/// Archives `project_id` optimistically (keeping its items intact) and enqueues `project_archive`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] or storage errors.
pub fn archive(conn: &mut Connection, project_id: &Id) -> Result<()> {
    let _ = find_project(conn, project_id)?;
    let cmd = command::project_archive(project_id.as_str());
    enqueue(conn, &cmd, |tx| {
        let _ = on_project_archive(tx, project_id)?;
        Ok(())
    })?;
    Ok(())
}

/// Unarchives `project_id` optimistically and enqueues `project_unarchive`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] or storage errors.
pub fn unarchive(conn: &mut Connection, project_id: &Id) -> Result<()> {
    let mut proj = find_project(conn, project_id)?;
    let cmd = command::project_unarchive(project_id.as_str());
    proj.is_archived = false;
    enqueue(conn, &cmd, |tx| resources::project::upsert(tx, &proj))?;
    Ok(())
}

/// Deletes `project_id` and its local cascade (sub-projects, sections, items) atomically
/// and enqueues `project_delete`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] or storage errors.
pub fn delete(conn: &mut Connection, project_id: &Id) -> Result<CascadeSummary> {
    let _ = find_project(conn, project_id)?;
    let cmd = command::project_delete(project_id.as_str());
    let mut summary = CascadeSummary::default();
    enqueue(conn, &cmd, |tx| {
        summary = on_project_delete(tx, project_id)?;
        Ok(())
    })?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::db::migrations;
    use crate::queue::store::CommandStore;

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        conn
    }

    #[test]
    fn project_lifecycle_add_child_rename_archive_unarchive_delete() {
        let mut conn = test_conn();

        let root_id = add(&mut conn, "Root").unwrap();
        let child_id = add_child(&mut conn, "Child", &root_id).unwrap();
        rename(&mut conn, &child_id, "Child Renamed").unwrap();

        let projs = queries::all_projects(&conn).unwrap();
        assert_eq!(projs.len(), 2);
        let child = projs.iter().find(|p| p.id == child_id).unwrap();
        assert_eq!(child.name, "Child Renamed");
        assert_eq!(child.parent_id.as_ref(), Some(&root_id));

        archive(&mut conn, &root_id).unwrap();
        let archived = find_project(&conn, &root_id).unwrap();
        assert!(archived.is_archived);

        unarchive(&mut conn, &root_id).unwrap();
        let unarchived = find_project(&conn, &root_id).unwrap();
        assert!(!unarchived.is_archived);

        let summary = delete(&mut conn, &root_id).unwrap();
        assert_eq!(summary.projects, 2);
        assert!(queries::all_projects(&conn).unwrap().is_empty());

        let cmds = CommandStore::new(&conn).pending(20).unwrap();
        assert_eq!(cmds.len(), 6);
    }

    #[test]
    fn unknown_parent_or_rename_returns_unknown_project_and_enqueues_nothing() {
        let mut conn = test_conn();
        let err1 = add_child(&mut conn, "X", &Id::new("missing")).unwrap_err();
        assert!(matches!(
            err1,
            crate::error::AppError::Structure(StructureError::UnknownProject { .. })
        ));
        let err2 = rename(&mut conn, &Id::new("missing"), "Y").unwrap_err();
        assert!(matches!(
            err2,
            crate::error::AppError::Structure(StructureError::UnknownProject { .. })
        ));
        assert_eq!(CommandStore::new(&conn).pending(10).unwrap().len(), 0);
    }
}
