//! Local optimistic cascade rules matching `docs/manual/epic-09-cascade-semantics.md` (`T-806`, EPIC-09).

#![allow(dead_code)]

use std::collections::HashSet;

use rusqlite::{Connection, Transaction};

use crate::api::model::{Id, Item, Project};
use crate::db::{queries, resources};
use crate::error::{DbError, Result, StructureError};

/// Counts of resources affected by a structure delete or archive operation.
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub struct CascadeSummary {
    /// Number of project rows deleted or archived.
    pub projects: usize,
    /// Number of section rows deleted or affected.
    pub sections: usize,
    /// Number of task item rows deleted or modified.
    pub items: usize,
}

fn collect_descendant_project_ids(conn: &Connection, root_id: &Id) -> Result<Vec<Id>> {
    let projects = queries::all_projects(conn)?;
    let mut visited = HashSet::new();
    let mut result = Vec::new();

    if !projects.iter().any(|p| &p.id == root_id) {
        return Err(StructureError::UnknownProject {
            id: root_id.as_str().to_string(),
        }
        .into());
    }

    visited.insert(root_id.clone());
    result.push(root_id.clone());

    let mut added = true;
    while added {
        added = false;
        for p in &projects {
            if let Some(ref parent) = p.parent_id {
                if visited.contains(parent) && visited.insert(p.id.clone()) {
                    result.push(p.id.clone());
                    added = true;
                }
            }
        }
    }

    Ok(result)
}

/// Computes the affected project, section, and item counts for deleting `project_id`
/// without modifying the database.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] when `project_id` does not exist in cache.
pub fn preview_project_delete(conn: &Connection, project_id: &Id) -> Result<CascadeSummary> {
    let proj_ids = collect_descendant_project_ids(conn, project_id)?;
    let id_set: HashSet<&Id> = proj_ids.iter().collect();

    let sections = queries::all_sections(conn)?
        .into_iter()
        .filter(|s| id_set.contains(&s.project_id))
        .count();

    let items = queries::all_active_items(conn)?
        .into_iter()
        .filter(|it| id_set.contains(&it.project_id))
        .count();

    Ok(CascadeSummary {
        projects: proj_ids.len(),
        sections,
        items,
    })
}

/// Applies `on project_delete` optimistically on `tx` per rule 3.1 of
/// `docs/manual/epic-09-cascade-semantics.md`:
/// deletes the project row, all transitive child sub-projects, their sections, and their items.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] or SQLite errors.
pub fn on_project_delete(tx: &Transaction<'_>, project_id: &Id) -> Result<CascadeSummary> {
    let proj_ids = collect_descendant_project_ids(tx, project_id)?;
    let id_set: HashSet<&Id> = proj_ids.iter().collect();

    let sections: Vec<_> = queries::all_sections(tx)?
        .into_iter()
        .filter(|s| id_set.contains(&s.project_id))
        .collect();

    let items: Vec<Item> = queries::all_active_items(tx)?
        .into_iter()
        .filter(|it| id_set.contains(&it.project_id))
        .collect();

    for it in &items {
        resources::item::delete(tx, it.id.as_str())?;
    }
    for sec in &sections {
        resources::section::delete(tx, &sec.id)?;
    }
    for pid in &proj_ids {
        resources::project::delete(tx, pid)?;
    }

    Ok(CascadeSummary {
        projects: proj_ids.len(),
        sections: sections.len(),
        items: items.len(),
    })
}

/// Computes affected counts for archiving `project_id` without mutating the database.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] when `project_id` does not exist.
pub fn preview_project_archive(conn: &Connection, project_id: &Id) -> Result<CascadeSummary> {
    let exists = queries::all_projects(conn)?
        .into_iter()
        .any(|p| &p.id == project_id);
    if !exists {
        return Err(StructureError::UnknownProject {
            id: project_id.as_str().to_string(),
        }
        .into());
    }
    Ok(CascadeSummary {
        projects: 1,
        sections: 0,
        items: 0,
    })
}

/// Applies `on project_archive` optimistically on `tx` per rule 3.2 and `## Unverified`
/// item 3 of `docs/manual/epic-09-cascade-semantics.md`:
/// marks only the targeted project's `is_archived = true`, leaving items, sections, and
/// sub-projects untouched locally.
///
/// # Errors
///
/// Returns [`StructureError::UnknownProject`] or SQLite errors.
pub fn on_project_archive(tx: &Transaction<'_>, project_id: &Id) -> Result<CascadeSummary> {
    let projects = queries::all_projects(tx)?;
    let Some(mut proj) = projects.into_iter().find(|p| &p.id == project_id) else {
        return Err(StructureError::UnknownProject {
            id: project_id.as_str().to_string(),
        }
        .into());
    };
    proj.is_archived = true;
    resources::project::upsert(tx, &proj)?;
    Ok(CascadeSummary {
        projects: 1,
        sections: 0,
        items: 0,
    })
}

/// Computes affected section/item counts for deleting `section_id` without mutating cache.
///
/// # Errors
///
/// Returns [`StructureError::UnknownSection`] if `section_id` is absent.
pub fn preview_section_delete(conn: &Connection, section_id: &Id) -> Result<CascadeSummary> {
    let exists = queries::all_sections(conn)?
        .into_iter()
        .any(|s| &s.id == section_id);
    if !exists {
        return Err(StructureError::UnknownSection {
            id: section_id.as_str().to_string(),
        }
        .into());
    }
    let affected_items = queries::items_by_section(conn, section_id)?.len();
    Ok(CascadeSummary {
        projects: 0,
        sections: 1,
        items: affected_items,
    })
}

/// Applies `on section_delete` optimistically on `tx` per rule 3.4 and `## Unverified`
/// item 2 of `docs/manual/epic-09-cascade-semantics.md`:
/// deletes ONLY the section row locally; items carrying `section_id` are left untouched
/// to be reconciled by the next sync delta.
///
/// # Errors
///
/// Returns [`StructureError::UnknownSection`] or SQLite errors.
pub fn on_section_delete(tx: &Transaction<'_>, section_id: &Id) -> Result<CascadeSummary> {
    let preview = preview_section_delete(tx, section_id)?;
    resources::section::delete(tx, section_id)?;
    Ok(preview)
}

/// Computes affected item count for deleting `label_id` without mutating cache.
///
/// # Errors
///
/// Returns [`StructureError::UnknownLabel`] if `label_id` is absent.
pub fn preview_label_delete(conn: &Connection, label_id: &Id) -> Result<CascadeSummary> {
    let Some(lbl) = queries::all_labels(conn)?
        .into_iter()
        .find(|l| &l.id == label_id)
    else {
        return Err(StructureError::UnknownLabel {
            id: label_id.as_str().to_string(),
        }
        .into());
    };
    let affected_items = queries::all_active_items(conn)?
        .into_iter()
        .filter(|it| it.labels.iter().any(|name| name == &lbl.name))
        .count();

    Ok(CascadeSummary {
        projects: 0,
        sections: 0,
        items: affected_items,
    })
}

/// Applies `on label_delete` optimistically on `tx` per rule 3.5 of
/// `docs/manual/epic-09-cascade-semantics.md`:
/// deletes the label row and strips that label's name from every cached item's labels array.
///
/// # Errors
///
/// Returns [`StructureError::UnknownLabel`] or SQLite errors.
pub fn on_label_delete(tx: &Transaction<'_>, label_id: &Id) -> Result<CascadeSummary> {
    let Some(lbl) = queries::all_labels(tx)?
        .into_iter()
        .find(|l| &l.id == label_id)
    else {
        return Err(StructureError::UnknownLabel {
            id: label_id.as_str().to_string(),
        }
        .into());
    };

    let items = queries::all_active_items(tx)?;
    let mut updated_items = 0usize;
    for mut it in items {
        let orig_len = it.labels.len();
        it.labels.retain(|name| name != &lbl.name);
        if it.labels.len() != orig_len {
            resources::item::upsert(tx, &it)?;
            updated_items += 1;
        }
    }

    resources::label::delete(tx, label_id)?;

    Ok(CascadeSummary {
        projects: 0,
        sections: 0,
        items: updated_items,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Item, Label, Priority, Project, Section};
    use crate::db::migrations;

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        conn
    }

    fn seed_proj(conn: &Connection, id: &str, parent_id: Option<&str>) {
        resources::project::upsert(
            conn,
            &Project {
                id: Id::new(id),
                name: format!("Proj {id}"),
                parent_id: parent_id.map(Id::new),
                child_order: 1,
                color: "blue".to_string(),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: "list".to_string(),
            },
        )
        .unwrap();
    }

    fn seed_item(conn: &Connection, id: &str, project_id: &str, labels: Vec<String>) {
        resources::item::upsert(
            conn,
            &Item {
                id: Id::new(id),
                content: format!("Task {id}"),
                description: String::new(),
                project_id: Id::new(project_id),
                section_id: None,
                parent_id: None,
                child_order: 1,
                priority: Priority::new(1).unwrap(),
                labels,
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
    fn project_delete_cascades_recursively_and_preview_does_not_mutate() {
        let mut conn = test_conn();
        seed_proj(&conn, "root", None);
        seed_proj(&conn, "child", Some("root"));
        seed_proj(&conn, "grandchild", Some("child"));
        seed_proj(&conn, "unrelated", None);

        seed_item(&conn, "t1", "root", Vec::new());
        seed_item(&conn, "t2", "child", Vec::new());
        seed_item(&conn, "t3", "grandchild", Vec::new());
        seed_item(&conn, "t4", "unrelated", Vec::new());

        let preview = preview_project_delete(&conn, &Id::new("root")).unwrap();
        assert_eq!(preview.projects, 3);
        assert_eq!(preview.items, 3);
        // Unmutated by preview
        assert_eq!(queries::all_projects(&conn).unwrap().len(), 4);

        let tx = conn.transaction().unwrap();
        let actual = on_project_delete(&tx, &Id::new("root")).unwrap();
        tx.commit().unwrap();

        assert_eq!(actual, preview);
        let remaining_projs = queries::all_projects(&conn).unwrap();
        assert_eq!(remaining_projs.len(), 1);
        assert_eq!(remaining_projs[0].id, Id::new("unrelated"));
        assert_eq!(queries::all_active_items(&conn).unwrap().len(), 1);
    }

    #[test]
    fn label_delete_strips_only_deleted_label_name_from_items() {
        let mut conn = test_conn();
        seed_proj(&conn, "p1", None);
        resources::label::upsert(
            &conn,
            &Label {
                id: Id::new("l1"),
                name: "urgent".to_string(),
                color: "red".to_string(),
                item_order: 1,
                is_deleted: false,
                is_favorite: false,
            },
        )
        .unwrap();
        seed_item(
            &conn,
            "t1",
            "p1",
            vec!["urgent".to_string(), "home".to_string()],
        );

        let tx = conn.transaction().unwrap();
        let summary = on_label_delete(&tx, &Id::new("l1")).unwrap();
        tx.commit().unwrap();

        assert_eq!(summary.items, 1);
        let item = queries::item_by_id(&conn, &Id::new("t1")).unwrap().unwrap();
        assert_eq!(item.labels, vec!["home".to_string()]);
    }
}
