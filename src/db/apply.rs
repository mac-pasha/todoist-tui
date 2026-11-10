//! Transactional application of a Sync API [`SyncResponse`] delta (T-307, EPIC-04).
//!
//! [`apply_sync`] takes a mutable [`rusqlite::Connection`] and a parsed
//! [`SyncResponse`] and applies every resource change — upserts, deletions,
//! optional full-sync table clears, and `meta` cursor/timestamp updates —
//! inside a single SQLite transaction. If any step fails, dropping the
//! uncommitted [`rusqlite::Transaction`] rolls the entire batch back so
//! `sync_token` and all resource rows remain byte-for-row identical to the
//! pre-call state.
//!
//! [`ApplyStats`] summarises how many rows were upserted or deleted per
//! resource table and contains only numeric counters and booleans so its
//! [`Debug`] representation never leaks task content, user fields or tokens.

#![allow(dead_code)]

use rusqlite::Connection;

use crate::api::model::{Filter, Item, Label, Note, Project, Section};
use crate::api::sync::SyncResponse;
use crate::db::meta;
use crate::db::resources::{day_order, filter, item, label, note, project, section, user};
use crate::error::{DbError, Result};

/// Upsert and delete row counts for a single resource table during
/// [`apply_sync`].
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ResourceCounts {
    /// Number of non-deleted resources upserted into the table.
    pub upserted: usize,
    /// Number of `is_deleted: true` resources deleted from the table.
    pub deleted: usize,
}

/// Summary of a completed [`apply_sync`] transaction.
///
/// Contains only numeric counters and boolean flags so formatting with
/// `{:?}` never emits task content, personal data or sync tokens.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ApplyStats {
    /// `true` when the response was applied as a full sync (all resource
    /// tables cleared before upserting).
    pub full_sync: bool,
    /// Counts for the `projects` table.
    pub projects: ResourceCounts,
    /// Counts for the `sections` table.
    pub sections: ResourceCounts,
    /// Counts for the `items` table.
    pub items: ResourceCounts,
    /// Counts for the `labels` table.
    pub labels: ResourceCounts,
    /// Counts for the `notes` table.
    pub notes: ResourceCounts,
    /// Counts for the `filters` table.
    pub filters: ResourceCounts,
    /// `true` when `resp.user` was `Some` and upserted into `user`.
    pub user_upserted: bool,
    /// Number of entries written to `day_orders` (`0` when left untouched).
    pub day_orders_replaced: usize,
}

/// SQL batch executed at the start of a full sync to purge stale rows before
/// inserting the fresh snapshot. Child tables are cleared before parent
/// tables so foreign-key constraints remain satisfied.
const CLEAR_ALL_RESOURCES_SQL: &str = "\
    DELETE FROM notes;
    DELETE FROM items;
    DELETE FROM sections;
    DELETE FROM projects;
    DELETE FROM labels;
    DELETE FROM filters;
    DELETE FROM user;
    DELETE FROM day_orders;";

/// Applies a [`SyncResponse`] delta (or full sync) inside one SQLite
/// transaction and returns per-resource [`ApplyStats`].
///
/// Full-sync detection checks `resp.full_sync || resp.full_sync_date_utc.is_some()`:
/// when either is set, all resource tables are cleared before applying the
/// response's lists so rows deleted remotely while the client was offline do
/// not linger. Resources flagged `is_deleted: true` are removed via their
/// table's `delete` helper; all other resources are written via `upsert`.
///
/// Finally, `sync_token`, `last_sync_at` and (when present)
/// `full_sync_date_utc` are updated in `meta` inside the same transaction
/// before committing.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] or [`DbError::Decode`] (wrapped in
/// [`crate::error::AppError::Db`]) if beginning the transaction, clearing
/// tables, applying any resource, updating `meta` or committing fails. On any
/// error the transaction is rolled back automatically.
pub fn apply_sync(conn: &mut Connection, resp: &SyncResponse) -> Result<ApplyStats> {
    let tx = conn
        .transaction()
        .map_err(|source| DbError::Sqlite { source })?;

    let is_full_sync = resp.full_sync || resp.full_sync_date_utc.is_some();
    if is_full_sync {
        tx.execute_batch(CLEAR_ALL_RESOURCES_SQL)
            .map_err(|source| DbError::Sqlite { source })?;
    }

    let projects = apply_projects(&tx, &resp.projects)?;
    let sections = apply_sections(&tx, &resp.sections)?;
    let items = apply_items(&tx, &resp.items)?;
    let notes = apply_notes(&tx, &resp.notes)?;
    let labels = apply_labels(&tx, &resp.labels)?;
    let filters = apply_filters(&tx, &resp.filters)?;

    let user_upserted = if let Some(ref u) = resp.user {
        user::upsert(&tx, u)?;
        true
    } else {
        false
    };

    let day_orders_replaced = if is_full_sync || !resp.day_orders.is_empty() {
        day_order::replace_all(&tx, &resp.day_orders)?;
        resp.day_orders.len()
    } else {
        0
    };

    meta::set_sync_token(&tx, &resp.sync_token)?;
    let now_utc = current_utc_timestamp(&tx)?;
    meta::set_last_sync_at(&tx, &now_utc)?;
    if let Some(ref full_date) = resp.full_sync_date_utc {
        meta::set_full_sync_date_utc(&tx, full_date)?;
    }

    tx.commit().map_err(|source| DbError::Sqlite { source })?;

    Ok(ApplyStats {
        full_sync: is_full_sync,
        projects,
        sections,
        items,
        labels,
        notes,
        filters,
        user_upserted,
        day_orders_replaced,
    })
}

fn apply_projects(conn: &Connection, list: &[Project]) -> Result<ResourceCounts> {
    let mut counts = ResourceCounts::default();
    for p in list {
        if p.is_deleted {
            project::delete(conn, &p.id)?;
            counts.deleted += 1;
        } else {
            project::upsert(conn, p)?;
            counts.upserted += 1;
        }
    }
    Ok(counts)
}

fn apply_sections(conn: &Connection, list: &[Section]) -> Result<ResourceCounts> {
    let mut counts = ResourceCounts::default();
    for s in list {
        if s.is_deleted {
            section::delete(conn, &s.id)?;
            counts.deleted += 1;
        } else {
            section::upsert(conn, s)?;
            counts.upserted += 1;
        }
    }
    Ok(counts)
}

fn apply_items(conn: &Connection, list: &[Item]) -> Result<ResourceCounts> {
    let mut counts = ResourceCounts::default();
    for it in list {
        if it.is_deleted {
            item::delete(conn, it.id.as_str())?;
            counts.deleted += 1;
        } else {
            item::upsert(conn, it)?;
            counts.upserted += 1;
        }
    }
    Ok(counts)
}

fn apply_notes(conn: &Connection, list: &[Note]) -> Result<ResourceCounts> {
    let mut counts = ResourceCounts::default();
    for n in list {
        if n.is_deleted {
            note::delete(conn, n.id.as_str())?;
            counts.deleted += 1;
        } else {
            note::upsert(conn, n)?;
            counts.upserted += 1;
        }
    }
    Ok(counts)
}

fn apply_labels(conn: &Connection, list: &[Label]) -> Result<ResourceCounts> {
    let mut counts = ResourceCounts::default();
    for l in list {
        if l.is_deleted {
            label::delete(conn, &l.id)?;
            counts.deleted += 1;
        } else {
            label::upsert(conn, l)?;
            counts.upserted += 1;
        }
    }
    Ok(counts)
}

fn apply_filters(conn: &Connection, list: &[Filter]) -> Result<ResourceCounts> {
    let mut counts = ResourceCounts::default();
    for f in list {
        if f.is_deleted {
            filter::delete(conn, &f.id)?;
            counts.deleted += 1;
        } else {
            filter::upsert(conn, f)?;
            counts.upserted += 1;
        }
    }
    Ok(counts)
}

fn current_utc_timestamp(conn: &Connection) -> Result<String> {
    conn.query_row("SELECT strftime('%Y-%m-%dT%H:%M:%SZ', 'now')", [], |row| {
        row.get(0)
    })
    .map_err(|source| DbError::Sqlite { source }.into())
}

#[cfg(test)]
mod tests {
    //! Unit tests for `apply_sync` (T-307, EPIC-04).
    //!
    //! All tokens, ids, emails and task contents are fake fixture values.

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::api::model::{Id, Priority, TzInfo, User};
    use crate::db::migrations;

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        conn
    }

    fn sample_project(id: &str, name: &str, is_deleted: bool) -> Project {
        Project {
            id: Id::new(id),
            name: name.to_string(),
            parent_id: None,
            child_order: 1,
            color: "berry_red".to_string(),
            is_favorite: false,
            is_archived: false,
            is_deleted,
            view_style: "list".to_string(),
        }
    }

    fn sample_section(id: &str, project_id: &str, name: &str, is_deleted: bool) -> Section {
        Section {
            id: Id::new(id),
            name: name.to_string(),
            project_id: Id::new(project_id),
            section_order: 1,
            is_deleted,
            is_archived: false,
        }
    }

    fn sample_item(id: &str, project_id: &str, content: &str, is_deleted: bool) -> Item {
        Item {
            id: Id::new(id),
            content: content.to_string(),
            description: String::new(),
            project_id: Id::new(project_id),
            section_id: None,
            parent_id: None,
            child_order: 1,
            priority: Priority::new(1).unwrap(),
            labels: Vec::new(),
            due: None,
            deadline: None,
            checked: false,
            is_deleted,
            added_at: "2024-05-01T10:00:00Z".to_string(),
            completed_at: None,
            user_id: Id::new("6X4VfakeUsr0001"),
            responsible_uid: None,
        }
    }

    fn sample_label(id: &str, name: &str, is_deleted: bool) -> Label {
        Label {
            id: Id::new(id),
            name: name.to_string(),
            color: "blue".to_string(),
            item_order: 1,
            is_favorite: false,
            is_deleted,
        }
    }

    fn sample_note(id: &str, item_id: &str, content: &str, is_deleted: bool) -> Note {
        Note {
            id: Id::new(id),
            item_id: Some(Id::new(item_id)),
            project_id: None,
            content: content.to_string(),
            posted_at: "2024-05-01T10:00:00Z".to_string(),
            is_deleted,
            file_attachment: None,
        }
    }

    fn sample_filter(id: &str, name: &str, is_deleted: bool) -> Filter {
        Filter {
            id: Id::new(id),
            name: name.to_string(),
            query: "today".to_string(),
            color: "charcoal".to_string(),
            item_order: 1,
            is_favorite: false,
            is_deleted,
        }
    }

    fn sample_user(id: &str) -> User {
        User {
            id: Id::new(id),
            full_name: "Ada Example".to_string(),
            email: "user@example.invalid".to_string(),
            color: "#ff8581".to_string(),
            tz_info: TzInfo::default(),
        }
    }

    fn table_count(conn: &Connection, table: &str) -> i64 {
        conn.query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |r| r.get(0))
            .unwrap()
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn apply_sync_upserts_and_deletes_in_one_response_and_updates_meta() {
        let mut conn = test_conn();

        // Seed initial rows that will be deleted by the next response.
        let seed = SyncResponse {
            sync_token: "fake_sync_tok_001".to_string(),
            full_sync: true,
            full_sync_date_utc: Some("2024-05-01T09:00:00Z".to_string()),
            projects: vec![
                sample_project("6X4VfakePrj0001", "Keep Project", false),
                sample_project("6X4VfakePrj0002", "Doomed Project", false),
            ],
            sections: vec![sample_section(
                "6X4VfakeSec0001",
                "6X4VfakePrj0001",
                "Doomed Section",
                false,
            )],
            items: vec![
                sample_item("6X4VfakeItm0001", "6X4VfakePrj0001", "Keep Item", false),
                sample_item("6X4VfakeItm0002", "6X4VfakePrj0001", "Doomed Item", false),
            ],
            labels: vec![sample_label("6X4VfakeLbl0001", "doomed-label", false)],
            notes: vec![sample_note(
                "6X4VfakeNot0001",
                "6X4VfakeItm0001",
                "Doomed Note",
                false,
            )],
            filters: vec![sample_filter("6X4VfakeFil0001", "Doomed Filter", false)],
            user: Some(sample_user("6X4VfakeUsr0001")),
            day_orders: serde_json::from_value(json!({"6X4VfakeItm0001": 1})).unwrap(),
            ..SyncResponse::default()
        };
        let seed_stats = apply_sync(&mut conn, &seed).unwrap();
        assert!(seed_stats.full_sync);
        assert_eq!(seed_stats.projects.upserted, 2);
        assert_eq!(seed_stats.items.upserted, 2);
        assert!(seed_stats.user_upserted);
        assert_eq!(seed_stats.day_orders_replaced, 1);

        // Incremental response with both upserts and `is_deleted: true` entries.
        let delta = SyncResponse {
            sync_token: "fake_sync_tok_002".to_string(),
            full_sync: false,
            full_sync_date_utc: None,
            projects: vec![
                sample_project("6X4VfakePrj0002", "Doomed Project", true),
                sample_project("6X4VfakePrj0003", "New Project", false),
            ],
            sections: vec![
                sample_section("6X4VfakeSec0001", "6X4VfakePrj0001", "Doomed Section", true),
                sample_section("6X4VfakeSec0002", "6X4VfakePrj0001", "New Section", false),
            ],
            items: vec![
                sample_item("6X4VfakeItm0001", "6X4VfakePrj0001", "Updated Item", false),
                sample_item("6X4VfakeItm0002", "6X4VfakePrj0001", "Doomed Item", true),
                sample_item("6X4VfakeItm0003", "6X4VfakePrj0003", "Added Item", false),
            ],
            labels: vec![
                sample_label("6X4VfakeLbl0001", "doomed-label", true),
                sample_label("6X4VfakeLbl0002", "fresh-label", false),
            ],
            notes: vec![
                sample_note("6X4VfakeNot0001", "6X4VfakeItm0001", "Doomed Note", true),
                sample_note("6X4VfakeNot0002", "6X4VfakeItm0001", "Fresh Note", false),
            ],
            filters: vec![
                sample_filter("6X4VfakeFil0001", "Doomed Filter", true),
                sample_filter("6X4VfakeFil0002", "Fresh Filter", false),
            ],
            user: None,
            day_orders: serde_json::Map::new(),
            ..SyncResponse::default()
        };

        let stats = apply_sync(&mut conn, &delta).unwrap();
        assert!(!stats.full_sync);
        assert_eq!(
            stats.projects,
            ResourceCounts {
                upserted: 1,
                deleted: 1
            }
        );
        assert_eq!(
            stats.sections,
            ResourceCounts {
                upserted: 1,
                deleted: 1
            }
        );
        assert_eq!(
            stats.items,
            ResourceCounts {
                upserted: 2,
                deleted: 1
            }
        );
        assert_eq!(
            stats.labels,
            ResourceCounts {
                upserted: 1,
                deleted: 1
            }
        );
        assert_eq!(
            stats.notes,
            ResourceCounts {
                upserted: 1,
                deleted: 1
            }
        );
        assert_eq!(
            stats.filters,
            ResourceCounts {
                upserted: 1,
                deleted: 1
            }
        );
        assert!(!stats.user_upserted);
        assert_eq!(stats.day_orders_replaced, 0);

        // Deleted rows are gone; kept & new rows exist.
        assert_eq!(table_count(&conn, "projects"), 2);
        assert_eq!(table_count(&conn, "sections"), 1);
        assert_eq!(table_count(&conn, "items"), 2);
        assert_eq!(table_count(&conn, "labels"), 1);
        assert_eq!(table_count(&conn, "notes"), 1);
        assert_eq!(table_count(&conn, "filters"), 1);
        assert_eq!(
            item::fetch(&conn, "6X4VfakeItm0001")
                .unwrap()
                .unwrap()
                .content,
            "Updated Item"
        );
        assert!(item::fetch(&conn, "6X4VfakeItm0002").unwrap().is_none());
        assert!(item::fetch(&conn, "6X4VfakeItm0003").unwrap().is_some());

        // Meta keys updated.
        assert_eq!(
            meta::sync_token(&conn).unwrap().as_deref(),
            Some("fake_sync_tok_002")
        );
        assert!(meta::last_sync_at(&conn).unwrap().is_some());
        assert_eq!(
            meta::full_sync_date_utc(&conn).unwrap().as_deref(),
            Some("2024-05-01T09:00:00Z")
        );
    }

    #[test]
    fn full_sync_date_utc_clears_stale_rows_before_upserting() {
        let mut conn = test_conn();

        // Initial state with stale rows across every resource table.
        let initial = SyncResponse {
            sync_token: "fake_sync_tok_initial".to_string(),
            full_sync: true,
            full_sync_date_utc: Some("2024-05-01T09:00:00Z".to_string()),
            projects: vec![sample_project("6X4VfakePrjStale", "Stale Project", false)],
            sections: vec![sample_section(
                "6X4VfakeSecStale",
                "6X4VfakePrjStale",
                "Stale Section",
                false,
            )],
            items: vec![sample_item(
                "6X4VfakeItmStale",
                "6X4VfakePrjStale",
                "Stale Item",
                false,
            )],
            labels: vec![sample_label("6X4VfakeLblStale", "stale-label", false)],
            notes: vec![sample_note(
                "6X4VfakeNotStale",
                "6X4VfakeItmStale",
                "Stale Note",
                false,
            )],
            filters: vec![sample_filter("6X4VfakeFilStale", "Stale Filter", false)],
            user: Some(sample_user("6X4VfakeUsrStale")),
            day_orders: serde_json::from_value(json!({"6X4VfakeItmStale": 1})).unwrap(),
            ..SyncResponse::default()
        };
        apply_sync(&mut conn, &initial).unwrap();

        // Second full sync (marked by `full_sync_date_utc` even if `full_sync` is false)
        // that omits all the stale resources and replaces them with one new project + item.
        let fresh_full = SyncResponse {
            sync_token: "fake_sync_tok_fresh".to_string(),
            full_sync: false,
            full_sync_date_utc: Some("2024-05-02T12:00:00Z".to_string()),
            projects: vec![sample_project("6X4VfakePrjFresh", "Fresh Project", false)],
            items: vec![sample_item(
                "6X4VfakeItmFresh",
                "6X4VfakePrjFresh",
                "Fresh Item",
                false,
            )],
            ..SyncResponse::default()
        };
        let stats = apply_sync(&mut conn, &fresh_full).unwrap();
        assert!(stats.full_sync);

        assert!(item::fetch(&conn, "6X4VfakeItmStale").unwrap().is_none());
        assert!(item::fetch(&conn, "6X4VfakeItmFresh").unwrap().is_some());
        assert_eq!(table_count(&conn, "projects"), 1);
        assert_eq!(table_count(&conn, "sections"), 0);
        assert_eq!(table_count(&conn, "items"), 1);
        assert_eq!(table_count(&conn, "labels"), 0);
        assert_eq!(table_count(&conn, "notes"), 0);
        assert_eq!(table_count(&conn, "filters"), 0);
        assert_eq!(table_count(&conn, "user"), 0);
        assert!(day_order::fetch(&conn).unwrap().is_empty());
        assert_eq!(
            meta::full_sync_date_utc(&conn).unwrap().as_deref(),
            Some("2024-05-02T12:00:00Z")
        );
    }

    #[test]
    fn incremental_sync_leaves_unmentioned_rows_untouched() {
        let mut conn = test_conn();

        let full = SyncResponse {
            sync_token: "fake_sync_tok_full".to_string(),
            full_sync: true,
            full_sync_date_utc: Some("2024-05-01T09:00:00Z".to_string()),
            projects: vec![sample_project("6X4VfakePrj0001", "Home", false)],
            sections: vec![sample_section(
                "6X4VfakeSec0001",
                "6X4VfakePrj0001",
                "Chores",
                false,
            )],
            items: vec![
                sample_item("6X4VfakeItm0001", "6X4VfakePrj0001", "Unmentioned 1", false),
                sample_item("6X4VfakeItm0002", "6X4VfakePrj0001", "Mentioned 2", false),
            ],
            labels: vec![sample_label("6X4VfakeLbl0001", "errand", false)],
            notes: vec![sample_note(
                "6X4VfakeNot0001",
                "6X4VfakeItm0001",
                "Keep this note",
                false,
            )],
            filters: vec![sample_filter("6X4VfakeFil0001", "Keep filter", false)],
            user: Some(sample_user("6X4VfakeUsr0001")),
            day_orders: serde_json::from_value(json!({"6X4VfakeItm0001": 3})).unwrap(),
            ..SyncResponse::default()
        };
        apply_sync(&mut conn, &full).unwrap();

        let incremental = SyncResponse {
            sync_token: "fake_sync_tok_inc".to_string(),
            full_sync: false,
            full_sync_date_utc: None,
            items: vec![sample_item(
                "6X4VfakeItm0002",
                "6X4VfakePrj0001",
                "Mentioned 2 (edited)",
                false,
            )],
            ..SyncResponse::default()
        };
        apply_sync(&mut conn, &incremental).unwrap();

        assert_eq!(
            item::fetch(&conn, "6X4VfakeItm0001")
                .unwrap()
                .unwrap()
                .content,
            "Unmentioned 1"
        );
        assert_eq!(
            item::fetch(&conn, "6X4VfakeItm0002")
                .unwrap()
                .unwrap()
                .content,
            "Mentioned 2 (edited)"
        );
        assert_eq!(table_count(&conn, "projects"), 1);
        assert_eq!(table_count(&conn, "sections"), 1);
        assert_eq!(table_count(&conn, "labels"), 1);
        assert_eq!(table_count(&conn, "notes"), 1);
        assert_eq!(table_count(&conn, "filters"), 1);
        assert_eq!(table_count(&conn, "user"), 1);
        assert_eq!(
            day_order::fetch(&conn).unwrap().get("6X4VfakeItm0001"),
            Some(&json!(3))
        );
    }

    #[test]
    fn failure_part_way_through_rolls_back_entire_transaction() {
        let mut conn = test_conn();

        let initial = SyncResponse {
            sync_token: "fake_sync_tok_before".to_string(),
            full_sync: true,
            full_sync_date_utc: Some("2024-05-01T09:00:00Z".to_string()),
            projects: vec![sample_project("6X4VfakePrj0001", "Original Project", false)],
            items: vec![sample_item(
                "6X4VfakeItm0001",
                "6X4VfakePrj0001",
                "Original Item",
                false,
            )],
            ..SyncResponse::default()
        };
        apply_sync(&mut conn, &initial).unwrap();
        let last_sync_before = meta::last_sync_at(&conn).unwrap();

        // Inject a database trigger that aborts on a specific bad item id
        // after projects and earlier items in the same batch have already
        // been written inside the transaction.
        conn.execute_batch(
            "CREATE TRIGGER reject_poison_item
             BEFORE INSERT ON items
             WHEN NEW.id = '6X4VfakeItmPoison'
             BEGIN
                 SELECT RAISE(ABORT, 'injected mid-transaction failure');
             END;",
        )
        .unwrap();

        let bad_delta = SyncResponse {
            sync_token: "fake_sync_tok_after_failed".to_string(),
            full_sync: false,
            full_sync_date_utc: None,
            projects: vec![sample_project("6X4VfakePrj0002", "Should Roll Back", false)],
            items: vec![
                sample_item(
                    "6X4VfakeItm0001",
                    "6X4VfakePrj0001",
                    "Mutated Before Failure",
                    false,
                ),
                sample_item(
                    "6X4VfakeItmPoison",
                    "6X4VfakePrj0001",
                    "Trigger Failure",
                    false,
                ),
            ],
            ..SyncResponse::default()
        };

        let err = apply_sync(&mut conn, &bad_delta);
        assert!(err.is_err(), "expected mid-transaction failure to error");

        // Every table and meta key must match the pre-call state.
        assert_eq!(
            meta::sync_token(&conn).unwrap().as_deref(),
            Some("fake_sync_tok_before")
        );
        assert_eq!(meta::last_sync_at(&conn).unwrap(), last_sync_before);
        assert_eq!(table_count(&conn, "projects"), 1);
        assert_eq!(table_count(&conn, "items"), 1);
        assert_eq!(
            item::fetch(&conn, "6X4VfakeItm0001")
                .unwrap()
                .unwrap()
                .content,
            "Original Item"
        );
    }

    #[test]
    fn apply_stats_debug_never_contains_task_content_or_token() {
        let mut conn = test_conn();
        let secret_task_text = "SENSITIVE_FAKE_TASK_CONTENT_DO_NOT_LOG";
        let resp = SyncResponse {
            sync_token: "tok_fake_abc123".to_string(),
            full_sync: true,
            full_sync_date_utc: Some("2024-05-01T09:00:00Z".to_string()),
            projects: vec![sample_project("6X4VfakePrj0001", secret_task_text, false)],
            items: vec![sample_item(
                "6X4VfakeItm0001",
                "6X4VfakePrj0001",
                secret_task_text,
                false,
            )],
            ..SyncResponse::default()
        };

        let stats = apply_sync(&mut conn, &resp).unwrap();
        let debug = format!("{stats:?}");
        assert!(!debug.contains(secret_task_text), "leaked content: {debug}");
        assert!(!debug.contains("tok_fake_abc123"), "leaked token: {debug}");
    }
}
