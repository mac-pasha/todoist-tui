//! Section structure actions: add, rename, reorder, delete (`T-804`, EPIC-09).

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::{Id, Section};
use crate::db::{queries, resources};
use crate::error::{Result, StructureError};
use crate::queue::enqueue::enqueue;
use crate::structure::cascade::{on_section_delete, CascadeSummary};
use crate::structure::command;

fn project_exists(conn: &Connection, project_id: &Id) -> Result<()> {
    let found = queries::all_projects(conn)?
        .into_iter()
        .any(|p| &p.id == project_id);
    if !found {
        return Err(StructureError::UnknownProject {
            id: project_id.as_str().to_string(),
        }
        .into());
    }
    Ok(())
}

fn find_section(conn: &Connection, section_id: &Id) -> Result<Section> {
    queries::all_sections(conn)?
        .into_iter()
        .find(|s| &s.id == section_id)
        .ok_or_else(|| {
            StructureError::UnknownSection {
                id: section_id.as_str().to_string(),
            }
            .into()
        })
}

/// Adds a new section inside `project_id` and enqueues `section_add`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] when `project_id` is absent, or validation/storage errors.
pub fn add(conn: &mut Connection, name: &str, project_id: &Id) -> Result<Id> {
    project_exists(conn, project_id)?;
    let cmd = command::section_add(name, project_id.as_str())?;
    let temp_id = Id::new(cmd.temp_id.clone().unwrap_or_else(|| cmd.uuid.to_string()));
    let trimmed_name = name.trim().to_string();

    let next_order = queries::sections_by_project(conn, project_id)?
        .into_iter()
        .map(|s| s.section_order)
        .max()
        .unwrap_or(0)
        + 1;

    let sec = Section {
        id: temp_id.clone(),
        project_id: project_id.clone(),
        name: trimmed_name,
        section_order: next_order,
        is_archived: false,
        is_deleted: false,
    };

    enqueue(conn, &cmd, |tx| resources::section::upsert(tx, &sec))?;
    Ok(temp_id)
}

/// Renames an existing section optimistically and enqueues `section_update`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownSection`] or validation/storage errors.
pub fn rename(conn: &mut Connection, section_id: &Id, new_name: &str) -> Result<()> {
    let mut sec = find_section(conn, section_id)?;
    let cmd = command::section_update(section_id.as_str(), new_name)?;
    sec.name = new_name.trim().to_string();

    enqueue(conn, &cmd, |tx| resources::section::upsert(tx, &sec))?;
    Ok(())
}

/// Reorders `section_id` to 0-based `new_index` among sibling sections of its project.
///
/// Idempotent when `new_index` equals the section's current position (still emits the
/// normalized ordering command safely). Updates every section's `section_order` in
/// the same transaction as the `section_reorder` enqueue.
///
/// # Errors
///
/// Returns [`StructureError::UnknownSection`] or storage errors.
pub fn reorder(conn: &mut Connection, section_id: &Id, new_index: usize) -> Result<()> {
    let target = find_section(conn, section_id)?;
    let mut siblings = queries::sections_by_project(conn, &target.project_id)?;

    let Some(cur_idx) = siblings.iter().position(|s| &s.id == section_id) else {
        return Err(StructureError::UnknownSection {
            id: section_id.as_str().to_string(),
        }
        .into());
    };

    let clamped = new_index.min(siblings.len().saturating_sub(1));
    if cur_idx != clamped {
        let moved = siblings.remove(cur_idx);
        siblings.insert(clamped, moved);
    }

    for (idx, sec) in siblings.iter_mut().enumerate() {
        sec.section_order = i64::try_from(idx + 1).unwrap_or(i64::MAX);
    }

    let pairs: Vec<(&str, i64)> = siblings
        .iter()
        .map(|s| (s.id.as_str(), s.section_order))
        .collect();
    let cmd = command::section_reorder(&pairs);

    enqueue(conn, &cmd, |tx| {
        for sec in &siblings {
            resources::section::upsert(tx, sec)?;
        }
        Ok(())
    })?;

    Ok(())
}

/// Deletes `section_id` via `cascade::on_section_delete` and enqueues `section_delete`.
///
/// # Errors
///
/// Returns [`StructureError::UnknownSection`] or storage errors.
pub fn delete(conn: &mut Connection, section_id: &Id) -> Result<CascadeSummary> {
    let _ = find_section(conn, section_id)?;
    let cmd = command::section_delete(section_id.as_str());
    let mut summary = CascadeSummary::default();
    enqueue(conn, &cmd, |tx| {
        summary = on_section_delete(tx, section_id)?;
        Ok(())
    })?;
    Ok(summary)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::Project;
    use crate::db::migrations;
    use crate::queue::store::CommandStore;

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        resources::project::upsert(
            &conn,
            &Project {
                id: Id::new("p1"),
                name: "Proj".to_string(),
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
    fn section_add_rename_reorder_delete() {
        let mut conn = test_conn();
        let s1 = add(&mut conn, "Alpha", &Id::new("p1")).unwrap();
        let s2 = add(&mut conn, "Beta", &Id::new("p1")).unwrap();
        rename(&mut conn, &s2, "Beta Renamed").unwrap();
        reorder(&mut conn, &s2, 0).unwrap();

        let secs = queries::sections_by_project(&conn, &Id::new("p1")).unwrap();
        assert_eq!(secs[0].id, s2);
        assert_eq!(secs[0].name, "Beta Renamed");
        assert_eq!(secs[0].section_order, 1);
        assert_eq!(secs[1].id, s1);
        assert_eq!(secs[1].section_order, 2);

        delete(&mut conn, &s1).unwrap();
        assert_eq!(
            queries::sections_by_project(&conn, &Id::new("p1"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(CommandStore::new(&conn).pending(10).unwrap().len(), 5);
    }
}
