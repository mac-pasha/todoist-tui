//! Read queries for the TUI views and one-shot CLI commands (T-308, EPIC-04).
//!
//! Every item view query reconstructs complete [`Item`] instances from the
//! `items.raw_json` payload so fields not projected into dedicated SQLite
//! columns (such as full `due`/`deadline` objects, `completed_at` and
//! `responsible_uid`) survive a round trip without loss.
//!
//! Ordering contract: all item view queries order by `child_order ASC, id ASC`
//! so equal `child_order` values always break ties deterministically.
//!
//! Retention contract (REQ-001 D3): `checked: true` items remain in the
//! `items` table so recent completions can be undone via [`item_by_id`],
//! while all active-task view queries filter them out (`checked = 0 AND
//! is_deleted = 0`).

#![allow(dead_code)]

use std::collections::HashMap;

use rusqlite::{params, Connection, OptionalExtension};
use serde::de::DeserializeOwned;
use serde_json::Value;

use crate::api::model::{Filter, Id, Item, Label, Note, Project, Section};
use crate::db::resources::day_order;
use crate::error::{DbError, Result};

/// Returns all active (`checked = 0 AND is_deleted = 0`) items belonging to
/// `project_id`, ordered by `child_order ASC, id ASC`.
///
/// Covered by `idx_items_project_order ON items (project_id, child_order)`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into an [`Item`].
pub fn items_by_project(conn: &Connection, project_id: &Id) -> Result<Vec<Item>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM items
         WHERE project_id = ?1 AND checked = 0 AND is_deleted = 0
         ORDER BY child_order ASC, id ASC",
        params![project_id.as_str()],
    )
}

/// Returns all active (`checked = 0 AND is_deleted = 0`) items belonging to
/// `section_id`, ordered by `child_order ASC, id ASC`.
///
/// Covered by `idx_items_section_order ON items (section_id, child_order)`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into an [`Item`].
pub fn items_by_section(conn: &Connection, section_id: &Id) -> Result<Vec<Item>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM items
         WHERE section_id = ?1 AND checked = 0 AND is_deleted = 0
         ORDER BY child_order ASC, id ASC",
        params![section_id.as_str()],
    )
}

/// Returns all active (`checked = 0 AND is_deleted = 0`) direct subtasks of
/// `parent_id`, ordered by `child_order ASC, id ASC`.
///
/// Covered by `idx_items_parent_order ON items (parent_id, child_order)`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into an [`Item`].
pub fn subtasks_of(conn: &Connection, parent_id: &Id) -> Result<Vec<Item>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM items
         WHERE parent_id = ?1 AND checked = 0 AND is_deleted = 0
         ORDER BY child_order ASC, id ASC",
        params![parent_id.as_str()],
    )
}

/// Returns all active (`checked = 0 AND is_deleted = 0`) items whose
/// `due_date` is on or before `today_date` (including overdue items and
/// items due today that carry a datetime suffix such as
/// `YYYY-MM-DDTHH:MM:SSZ`), ordered by `child_order ASC, id ASC`.
///
/// Covered by `idx_items_due_date ON items (due_date)`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into an [`Item`].
pub fn today(conn: &Connection, today_date: &str) -> Result<Vec<Item>> {
    let prefix = date_prefix(today_date);
    let upper_bound = format!("{prefix}~");
    query_decoded(
        conn,
        "SELECT raw_json FROM items
         WHERE due_date IS NOT NULL
           AND due_date <= ?1
           AND checked = 0
           AND is_deleted = 0
         ORDER BY child_order ASC, id ASC",
        params![upper_bound],
    )
}

/// Returns all active (`checked = 0 AND is_deleted = 0`) items due strictly
/// after `today_date` and within the next `days` calendar days inclusive,
/// ordered by `child_order ASC, id ASC`.
///
/// When `days == 0`, returns `Ok(Vec::new())` without querying the database.
/// Covered by `idx_items_due_date ON items (due_date)`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into an [`Item`].
pub fn upcoming(conn: &Connection, today_date: &str, days: u32) -> Result<Vec<Item>> {
    if days == 0 {
        return Ok(Vec::new());
    }
    let prefix = date_prefix(today_date);
    let Some(end_date) = add_days_ymd(prefix, days) else {
        return Ok(Vec::new());
    };
    let lower_bound = format!("{prefix}~");
    let upper_bound = format!("{end_date}~");
    query_decoded(
        conn,
        "SELECT raw_json FROM items
         WHERE due_date IS NOT NULL
           AND due_date > ?1
           AND due_date <= ?2
           AND checked = 0
           AND is_deleted = 0
         ORDER BY child_order ASC, id ASC",
        params![lower_bound, upper_bound],
    )
}

/// Returns all active (`checked = 0 AND is_deleted = 0`) items carrying
/// `label_name` in their `labels` JSON array, ordered by
/// `child_order ASC, id ASC`.
///
/// Covered by `idx_items_checked ON items (checked)`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into an [`Item`].
pub fn items_by_label(conn: &Connection, label_name: &str) -> Result<Vec<Item>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM items
         WHERE checked = 0
           AND is_deleted = 0
           AND EXISTS (
               SELECT 1 FROM json_each(items.labels) WHERE json_each.value = ?1
           )
         ORDER BY child_order ASC, id ASC",
        params![label_name],
    )
}

/// Returns all active (`checked = 0 AND is_deleted = 0`) items in the
/// authenticated user's Inbox project, ordered by `child_order ASC, id ASC`.
///
/// Resolves the inbox project id from the cached `user` row's
/// `inbox_project_id` field (or, when the `user` row is present without that
/// field, from a non-deleted project flagged `inbox_project` or named
/// `"Inbox"`). When no `user` row is cached in the database, returns
/// `Ok(vec![])` rather than an error.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if a database query fails or
/// [`DbError::Decode`] if a stored `raw_json` payload cannot be decoded.
pub fn inbox(conn: &Connection) -> Result<Vec<Item>> {
    let Some(inbox_id) = resolve_inbox_project_id(conn)? else {
        return Ok(Vec::new());
    };
    items_by_project(conn, &inbox_id)
}

/// Fetches a single non-deleted item by `id`, including completed
/// (`checked: true`) items so recent completions can be undone (REQ-001 D3).
///
/// Returns `Ok(None)` when no non-deleted row exists for `id`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// the stored `raw_json` cannot be deserialised into an [`Item`].
pub fn item_by_id(conn: &Connection, id: &Id) -> Result<Option<Item>> {
    let raw_json: Option<String> = conn
        .query_row(
            "SELECT raw_json FROM items WHERE id = ?1 AND is_deleted = 0",
            params![id.as_str()],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| DbError::Sqlite { source })?;

    raw_json.as_deref().map(decode_json).transpose()
}

/// Returns all non-deleted projects, ordered by `child_order ASC, id ASC`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into a [`Project`].
pub fn all_projects(conn: &Connection) -> Result<Vec<Project>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM projects
         WHERE is_deleted = 0
         ORDER BY child_order ASC, id ASC",
        [],
    )
}

/// Returns all non-deleted sections belonging to `project_id`, ordered by
/// `section_order ASC, id ASC`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into a [`Section`].
pub fn sections_by_project(conn: &Connection, project_id: &Id) -> Result<Vec<Section>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM sections
         WHERE project_id = ?1 AND is_deleted = 0
         ORDER BY section_order ASC, id ASC",
        params![project_id.as_str()],
    )
}

/// Returns all non-deleted sections across all projects, ordered by
/// `section_order ASC, id ASC`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into a [`Section`].
pub fn all_sections(conn: &Connection) -> Result<Vec<Section>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM sections
         WHERE is_deleted = 0
         ORDER BY section_order ASC, id ASC",
        [],
    )
}

/// Returns all non-deleted labels, ordered by `item_order ASC, id ASC`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into a [`Label`].
pub fn all_labels(conn: &Connection) -> Result<Vec<Label>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM labels
         WHERE is_deleted = 0
         ORDER BY item_order ASC, id ASC",
        [],
    )
}

/// Returns all non-deleted filters, ordered by `item_order ASC, id ASC`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into a [`Filter`].
pub fn all_filters(conn: &Connection) -> Result<Vec<Filter>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM filters
         WHERE is_deleted = 0
         ORDER BY item_order ASC, id ASC",
        [],
    )
}

/// Returns all active (`checked = 0 AND is_deleted = 0`) items across all
/// projects, ordered by `child_order ASC, id ASC`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into an [`Item`].
pub fn all_active_items(conn: &Connection) -> Result<Vec<Item>> {
    query_decoded(
        conn,
        "SELECT raw_json FROM items
         WHERE checked = 0 AND is_deleted = 0
         ORDER BY child_order ASC, id ASC",
        [],
    )
}

/// Returns all non-deleted notes attached to `item_id`, ordered by
/// `posted_at ASC, id ASC`.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// a stored `raw_json` payload cannot be deserialised into a [`Note`].
pub fn notes_for_item(conn: &Connection, item_id: &Id) -> Result<Vec<Note>> {
    if item_id.as_str().is_empty() {
        return Ok(Vec::new());
    }
    query_decoded(
        conn,
        "SELECT raw_json FROM notes
         WHERE item_id = ?1 AND is_deleted = 0
         ORDER BY posted_at ASC, id ASC",
        params![item_id.as_str()],
    )
}

/// Returns the cached `day_orders` map as `HashMap<Id, i32>`.
///
/// Non-integer values (or integers outside `i32` range) in the tolerant
/// JSON map are skipped rather than failing the read.
///
/// # Errors
///
/// Returns [`DbError::Sqlite`] if the query fails or [`DbError::Decode`] if
/// the stored `day_orders` JSON is malformed.
pub fn day_orders(conn: &Connection) -> Result<HashMap<Id, i32>> {
    let raw_map = day_order::fetch(conn)?;
    let mut out = HashMap::with_capacity(raw_map.len());
    for (key, value) in raw_map {
        if let Some(order) = value.as_i64().and_then(|n| i32::try_from(n).ok()) {
            out.insert(Id::new(key), order);
        }
    }
    Ok(out)
}

fn resolve_inbox_project_id(conn: &Connection) -> Result<Option<Id>> {
    let user_raw: Option<String> = conn
        .query_row("SELECT raw_json FROM user LIMIT 1", [], |row| row.get(0))
        .optional()
        .map_err(|source| DbError::Sqlite { source })?;

    let Some(user_raw) = user_raw else {
        return Ok(None);
    };

    let user_val: Value =
        serde_json::from_str(&user_raw).map_err(|source| DbError::Decode { source })?;

    if let Some(inbox_val) = user_val.get("inbox_project_id") {
        if let Some(id_str) = inbox_val.as_str().filter(|s| !s.is_empty()) {
            return Ok(Some(Id::new(id_str)));
        }
        if let Some(id_num) = inbox_val.as_i64() {
            return Ok(Some(Id::new(id_num.to_string())));
        }
    }

    let fallback_id: Option<String> = conn
        .query_row(
            "SELECT id FROM projects
             WHERE is_deleted = 0
               AND (
                   json_extract(raw_json, '$.inbox_project') = 1
                   OR json_extract(raw_json, '$.is_inbox_project') = 1
                   OR LOWER(name) = 'inbox'
               )
             ORDER BY
               CASE
                   WHEN json_extract(raw_json, '$.inbox_project') = 1
                     OR json_extract(raw_json, '$.is_inbox_project') = 1
                   THEN 0
                   ELSE 1
               END ASC,
               child_order ASC,
               id ASC
             LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(|source| DbError::Sqlite { source })?;

    Ok(fallback_id.map(Id::new))
}

fn query_decoded<T, P>(conn: &Connection, sql: &str, params: P) -> Result<Vec<T>>
where
    T: DeserializeOwned,
    P: rusqlite::Params,
{
    let mut stmt = conn
        .prepare_cached(sql)
        .map_err(|source| DbError::Sqlite { source })?;
    let rows = stmt
        .query_map(params, |row| row.get::<_, String>(0))
        .map_err(|source| DbError::Sqlite { source })?;

    let mut out = Vec::new();
    for row in rows {
        let raw_json = row.map_err(|source| DbError::Sqlite { source })?;
        out.push(decode_json(&raw_json)?);
    }
    Ok(out)
}

fn decode_json<T: DeserializeOwned>(raw_json: &str) -> Result<T> {
    serde_json::from_str::<T>(raw_json).map_err(|source| DbError::Decode { source }.into())
}

fn date_prefix(date_str: &str) -> &str {
    date_str.get(..10).unwrap_or(date_str)
}

fn parse_ymd(s: &str) -> Option<(i64, i64, i64)> {
    let prefix = s.get(..10)?;
    let mut parts = prefix.split('-');
    let year: i64 = parts.next()?.parse().ok()?;
    let month: i64 = parts.next()?.parse().ok()?;
    let day: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&month) || !(1..=31).contains(&day) {
        return None;
    }
    Some((year, month, day))
}

fn days_from_civil(y: i64, m: i64, d: i64) -> i64 {
    let y = if m <= 2 { y - 1 } else { y };
    let era = if y >= 0 { y } else { y - 399 } / 400;
    let yoe = y - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    era * 146_097 + doe - 719_468
}

fn civil_from_days(z: i64) -> (i64, i64, i64) {
    let z = z + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = z - era * 146_097;
    let yoe = (doe - doe / 1_460 + doe / 36_524 - doe / 146_096) / 365;
    let y = yoe + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100);
    let mp = (5 * doy + 2) / 153;
    let d = doy - (153 * mp + 2) / 5 + 1;
    let m = if mp < 10 { mp + 3 } else { mp - 9 };
    let y = if m <= 2 { y + 1 } else { y };
    (y, m, d)
}

fn add_days_ymd(date_str: &str, days: u32) -> Option<String> {
    let (y, m, d) = parse_ymd(date_str)?;
    let total = days_from_civil(y, m, d) + i64::from(days);
    let (ny, nm, nd) = civil_from_days(total);
    Some(format!("{ny:04}-{nm:02}-{nd:02}"))
}

#[cfg(test)]
mod tests {
    //! Unit tests for `queries` (T-308, EPIC-04).
    //!
    //! All fixture data uses fake ids and emails (`6X4Vfake...`,
    //! `user@example.invalid`).

    use rusqlite::Connection;
    use serde_json::json;

    use super::*;
    use crate::api::model::{Deadline, Due, Priority};
    use crate::db::migrations;
    use crate::db::resources::{day_order, filter, item, label, note, project, section, user};

    fn test_conn() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        conn
    }

    fn make_item(id: &str, project_id: &str, child_order: i64) -> Item {
        Item {
            id: Id::new(id),
            content: format!("Task {id}"),
            description: String::new(),
            project_id: Id::new(project_id),
            section_id: None,
            parent_id: None,
            child_order,
            priority: Priority::new(2).unwrap(),
            labels: Vec::new(),
            due: None,
            deadline: None,
            checked: false,
            is_deleted: false,
            added_at: "2024-05-01T10:00:00Z".to_string(),
            completed_at: None,
            user_id: Id::new("6X4VfakeUsr0001"),
            responsible_uid: None,
        }
    }

    fn ids_of(items: &[Item]) -> Vec<&str> {
        items.iter().map(|it| it.id.as_str()).collect()
    }

    #[test]
    fn items_by_project_orders_by_child_order_then_id_and_excludes_checked_or_deleted() {
        let conn = test_conn();
        let p1 = Id::new("6X4VfakePrj0001");
        let p2 = Id::new("6X4VfakePrj0002");

        // Equal `child_order = 2` on b and a to test deterministic `id` tiebreak.
        let it_b = make_item("6X4VfakeItmB", p1.as_str(), 2);
        let it_a = make_item("6X4VfakeItmA", p1.as_str(), 2);
        let it_first = make_item("6X4VfakeItmZ", p1.as_str(), 1);
        let mut it_checked = make_item("6X4VfakeItmChecked", p1.as_str(), 0);
        it_checked.checked = true;
        let mut it_deleted = make_item("6X4VfakeItmDeleted", p1.as_str(), 0);
        it_deleted.is_deleted = true;
        let it_other_proj = make_item("6X4VfakeItmOther", p2.as_str(), 1);

        for it in [
            &it_b,
            &it_a,
            &it_first,
            &it_checked,
            &it_deleted,
            &it_other_proj,
        ] {
            item::upsert(&conn, it).unwrap();
        }

        let got = items_by_project(&conn, &p1).unwrap();
        assert_eq!(
            ids_of(&got),
            vec!["6X4VfakeItmZ", "6X4VfakeItmA", "6X4VfakeItmB"]
        );
    }

    #[test]
    fn items_by_section_and_subtasks_of_order_and_filter_correctly() {
        let conn = test_conn();
        let sec = Id::new("6X4VfakeSec0001");
        let parent = Id::new("6X4VfakeItmParent");

        let mut s_b = make_item("6X4VfakeItmSecB", "6X4VfakePrj0001", 5);
        s_b.section_id = Some(sec.clone());
        let mut s_a = make_item("6X4VfakeItmSecA", "6X4VfakePrj0001", 5);
        s_a.section_id = Some(sec.clone());
        let mut s_checked = make_item("6X4VfakeItmSecChk", "6X4VfakePrj0001", 1);
        s_checked.section_id = Some(sec.clone());
        s_checked.checked = true;

        let mut sub_2 = make_item("6X4VfakeItmSub2", "6X4VfakePrj0001", 2);
        sub_2.parent_id = Some(parent.clone());
        let mut sub_1 = make_item("6X4VfakeItmSub1", "6X4VfakePrj0001", 1);
        sub_1.parent_id = Some(parent.clone());

        for it in [&s_b, &s_a, &s_checked, &sub_2, &sub_1] {
            item::upsert(&conn, it).unwrap();
        }

        let by_sec = items_by_section(&conn, &sec).unwrap();
        assert_eq!(ids_of(&by_sec), vec!["6X4VfakeItmSecA", "6X4VfakeItmSecB"]);

        let subs = subtasks_of(&conn, &parent).unwrap();
        assert_eq!(ids_of(&subs), vec!["6X4VfakeItmSub1", "6X4VfakeItmSub2"]);
    }

    #[test]
    fn today_and_upcoming_filter_dates_and_exclude_checked_items() {
        let conn = test_conn();

        let with_due = |id: &str, order: i64, date: &str, checked: bool| {
            let mut it = make_item(id, "6X4VfakePrj0001", order);
            it.due = Some(Due {
                date: date.to_string(),
                string: None,
                lang: None,
                is_recurring: false,
                timezone: None,
            });
            it.checked = checked;
            it
        };

        let overdue = with_due("6X4VfakeItmOverdue", 2, "2024-05-01", false);
        let due_today = with_due("6X4VfakeItmToday", 1, "2024-05-06", false);
        let due_today_dt = with_due("6X4VfakeItmTodayDt", 3, "2024-05-06T18:30:00Z", false);
        let due_today_checked = with_due("6X4VfakeItmTodayChk", 0, "2024-05-06", true);
        let tomorrow = with_due("6X4VfakeItmDay1", 2, "2024-05-07", false);
        let day_7 = with_due("6X4VfakeItmDay7", 1, "2024-05-13T09:00:00Z", false);
        let day_7_checked = with_due("6X4VfakeItmDay7Chk", 0, "2024-05-13", true);
        let day_8 = with_due("6X4VfakeItmDay8", 1, "2024-05-14", false);
        let no_due = make_item("6X4VfakeItmNoDue", "6X4VfakePrj0001", 1);

        for it in [
            &overdue,
            &due_today,
            &due_today_dt,
            &due_today_checked,
            &tomorrow,
            &day_7,
            &day_7_checked,
            &day_8,
            &no_due,
        ] {
            item::upsert(&conn, it).unwrap();
        }

        let today_items = today(&conn, "2024-05-06").unwrap();
        assert_eq!(
            ids_of(&today_items),
            vec![
                "6X4VfakeItmToday",
                "6X4VfakeItmOverdue",
                "6X4VfakeItmTodayDt"
            ]
        );

        let next_7 = upcoming(&conn, "2024-05-06", 7).unwrap();
        assert_eq!(ids_of(&next_7), vec!["6X4VfakeItmDay7", "6X4VfakeItmDay1"]);

        assert_eq!(
            upcoming(&conn, "2024-05-06", 0).unwrap(),
            Vec::<Item>::new()
        );
    }

    #[test]
    fn items_by_label_matches_exact_label_and_orders_deterministically() {
        let conn = test_conn();

        let mut it1 = make_item("6X4VfakeItm1", "6X4VfakePrj0001", 2);
        it1.labels = vec!["errand".to_string(), "home".to_string()];

        let mut it2 = make_item("6X4VfakeItm2", "6X4VfakePrj0001", 1);
        it2.labels = vec!["errand".to_string()];

        let mut it3 = make_item("6X4VfakeItm3", "6X4VfakePrj0001", 0);
        it3.labels = vec!["errands".to_string()];

        let mut it_checked = make_item("6X4VfakeItmChk", "6X4VfakePrj0001", 0);
        it_checked.labels = vec!["errand".to_string()];
        it_checked.checked = true;

        for it in [&it1, &it2, &it3, &it_checked] {
            item::upsert(&conn, it).unwrap();
        }

        let got = items_by_label(&conn, "errand").unwrap();
        assert_eq!(ids_of(&got), vec!["6X4VfakeItm2", "6X4VfakeItm1"]);
    }

    #[test]
    fn inbox_returns_empty_when_no_user_row_and_resolves_inbox_project_id_when_present() {
        let conn = test_conn();
        let inbox_proj = Id::new("6X4VfakePrjInbox");
        let other_proj = Id::new("6X4VfakePrjOther");

        let it_inbox = make_item("6X4VfakeItmInbox", inbox_proj.as_str(), 1);
        let it_other = make_item("6X4VfakeItmOther", other_proj.as_str(), 1);
        item::upsert(&conn, &it_inbox).unwrap();
        item::upsert(&conn, &it_other).unwrap();

        // No user row cached -> Ok(vec![])
        assert_eq!(inbox(&conn).unwrap(), Vec::<Item>::new());

        // Cache a user row carrying `inbox_project_id` in its `raw_json`.
        conn.execute(
            "INSERT INTO user (id, full_name, email, raw_json) VALUES (?1, ?2, ?3, ?4)",
            params![
                "6X4VfakeUsr0001",
                "Ada Example",
                "user@example.invalid",
                json!({
                    "id": "6X4VfakeUsr0001",
                    "full_name": "Ada Example",
                    "email": "user@example.invalid",
                    "inbox_project_id": "6X4VfakePrjInbox"
                })
                .to_string(),
            ],
        )
        .unwrap();

        let got = inbox(&conn).unwrap();
        assert_eq!(got, vec![it_inbox]);
    }

    #[test]
    fn full_item_round_trips_losslessly_and_checked_item_is_retrievable_by_id() {
        let conn = test_conn();

        let rich_item = Item {
            id: Id::new("6X4VfakeItmFull"),
            content: "Review architecture".to_string(),
            description: "Detailed markdown notes".to_string(),
            project_id: Id::new("6X4VfakePrj0001"),
            section_id: Some(Id::new("6X4VfakeSec0001")),
            parent_id: Some(Id::new("6X4VfakeItmParent")),
            child_order: 7,
            priority: Priority::new(4).unwrap(),
            labels: vec!["deep-work".to_string(), "q2".to_string()],
            due: Some(Due {
                date: "2024-05-06T09:00:00Z".to_string(),
                string: Some("every weekday".to_string()),
                lang: Some("en".to_string()),
                is_recurring: true,
                timezone: Some("Europe/Lisbon".to_string()),
            }),
            deadline: Some(Deadline {
                date: "2024-05-10".to_string(),
                lang: Some("en".to_string()),
            }),
            checked: false,
            is_deleted: false,
            added_at: "2024-04-28T08:15:00Z".to_string(),
            completed_at: None,
            user_id: Id::new("6X4VfakeUsr0001"),
            responsible_uid: Some(Id::new("6X4VfakeUsr0002")),
        };
        item::upsert(&conn, &rich_item).unwrap();

        let queried = items_by_project(&conn, &rich_item.project_id).unwrap();
        assert_eq!(queried, vec![rich_item.clone()]);

        // Transition item to checked = true: excluded from view queries,
        // but still returned by `item_by_id` (REQ-001 D3).
        let mut completed = rich_item;
        completed.checked = true;
        completed.completed_at = Some("2024-05-06T11:00:00Z".to_string());
        item::upsert(&conn, &completed).unwrap();

        assert_eq!(
            items_by_project(&conn, &completed.project_id).unwrap(),
            Vec::<Item>::new()
        );
        assert_eq!(today(&conn, "2024-05-06").unwrap(), Vec::<Item>::new());
        assert_eq!(item_by_id(&conn, &completed.id).unwrap(), Some(completed));
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn resource_list_helpers_return_non_deleted_rows_in_order() {
        let conn = test_conn();

        project::upsert(
            &conn,
            &Project {
                id: Id::new("6X4VfakePrj2"),
                name: "Work".to_string(),
                parent_id: None,
                child_order: 2,
                color: "blue".to_string(),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: "list".to_string(),
            },
        )
        .unwrap();
        project::upsert(
            &conn,
            &Project {
                id: Id::new("6X4VfakePrj1"),
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

        section::upsert(
            &conn,
            &Section {
                id: Id::new("6X4VfakeSec1"),
                name: "Backlog".to_string(),
                project_id: Id::new("6X4VfakePrj1"),
                section_order: 1,
                is_deleted: false,
                is_archived: false,
            },
        )
        .unwrap();

        label::upsert(
            &conn,
            &Label {
                id: Id::new("6X4VfakeLbl1"),
                name: "errand".to_string(),
                color: "green".to_string(),
                item_order: 1,
                is_favorite: false,
                is_deleted: false,
            },
        )
        .unwrap();

        filter::upsert(
            &conn,
            &Filter {
                id: Id::new("6X4VfakeFil1"),
                name: "Focus".to_string(),
                query: "today".to_string(),
                color: "red".to_string(),
                item_order: 1,
                is_favorite: true,
                is_deleted: false,
            },
        )
        .unwrap();

        note::upsert(
            &conn,
            &Note {
                id: Id::new("6X4VfakeNot1"),
                item_id: Some(Id::new("6X4VfakeItm1")),
                project_id: None,
                content: "Note body".to_string(),
                posted_at: "2024-05-01T10:00:00Z".to_string(),
                is_deleted: false,
                file_attachment: None,
            },
        )
        .unwrap();

        user::upsert(
            &conn,
            &crate::api::model::User {
                id: Id::new("6X4VfakeUsr0001"),
                full_name: "Ada Example".to_string(),
                email: "user@example.invalid".to_string(),
                color: "#ff8581".to_string(),
                tz_info: crate::api::model::TzInfo::default(),
            },
        )
        .unwrap();

        let mut map = serde_json::Map::new();
        map.insert("6X4VfakeItm1".to_string(), json!(4));
        day_order::replace_all(&conn, &map).unwrap();

        assert_eq!(all_projects(&conn).unwrap().len(), 2);
        assert_eq!(
            sections_by_project(&conn, &Id::new("6X4VfakePrj1"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(all_sections(&conn).unwrap().len(), 1);
        assert_eq!(all_labels(&conn).unwrap().len(), 1);
        assert_eq!(all_filters(&conn).unwrap().len(), 1);
        assert_eq!(
            notes_for_item(&conn, &Id::new("6X4VfakeItm1"))
                .unwrap()
                .len(),
            1
        );
        assert_eq!(
            day_orders(&conn).unwrap().get(&Id::new("6X4VfakeItm1")),
            Some(&4)
        );
    }
}
