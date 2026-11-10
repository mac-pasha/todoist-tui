//! `today` view projection model, local day-boundary clock seam, and
//! deterministic sorting (`T-1005`).
//!
//! # Definition of "Today"
//!
//! A task belongs in the `today` view when:
//! 1. It is neither completed (`!item.checked`) nor deleted (`!item.is_deleted`).
//! 2. It has a `due` date (`item.due.is_some()`).
//! 3. The leading `YYYY-MM-DD` date prefix of `due.date` is less than or equal
//!    to the user's local calendar date (`due_day <= today_date`), which
//!    includes both tasks due today and overdue tasks (`due_day < today_date`).
//!
//! # Deterministic Ordering
//!
//! Returned [`TodayRow`] items are ordered by:
//! 1. `priority` descending (`4` = P1 highest priority first, down to `1` = P4)
//! 2. `due` string ascending (earlier dates/timestamps first)
//! 3. `child_order` ascending
//! 4. `id` ascending (final deterministic tie-breaker)

#![allow(dead_code)]

use std::collections::HashMap;
use std::time::{SystemTime, UNIX_EPOCH};

use rusqlite::Connection;

use crate::api::model::{Item, Project, User};
use crate::db::queries;
use crate::error::Result;

/// Environment variable allowing tests and callers to pin the local calendar
/// day (`YYYY-MM-DD`) without relying on the host wall clock (`T-1005`, `T-1008`).
pub const TODAY_DATE_ENV_VAR: &str = "TODOIST_TUI_TODAY";

/// Optional environment variable overriding the local timezone offset in
/// seconds east of UTC when computing today's date from the wall clock.
pub const TZ_OFFSET_SECS_ENV_VAR: &str = "TODOIST_TUI_TZ_OFFSET_SECS";

/// Flat, owned view model row representing a single task in `todoist-tui today`
/// (`T-1005`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TodayRow {
    /// Stable task identifier (`Item::id`).
    pub id: String,
    /// Task title/content.
    pub content: String,
    /// Owning project identifier (`Item::project_id`).
    pub project_id: String,
    /// Resolved human-readable project name (falls back to `"Inbox"` if the
    /// project row is absent from the local cache).
    pub project_name: String,
    /// Todoist API priority (`1..=4`, where `4` is P1 / urgent and `1` is P4 / normal).
    pub priority: u8,
    /// Raw due date or datetime string (`YYYY-MM-DD` or `YYYY-MM-DDTHH:MM:SS`), if set.
    pub due: Option<String>,
    /// `true` when the task's due date prefix (`YYYY-MM-DD`) is strictly before
    /// the local calendar date (`due_day < today_date`).
    pub is_overdue: bool,
    /// Task label names in cache order.
    pub labels: Vec<String>,
    /// Completion flag (`false` for all rows returned by [`load_today_rows`]).
    pub checked: bool,
    /// Position of the task among siblings (`Item::child_order`).
    pub child_order: i32,
}

/// Clock + timezone seam used to resolve the user's current local calendar
/// date (`YYYY-MM-DD`) deterministically (`T-1005`, `T-1008`).
pub trait TodayClock: Send + Sync {
    /// Return the current local calendar date formatted as `YYYY-MM-DD`.
    fn today_date(&self, cached_tz_offset_secs: Option<i64>) -> String;
}

/// Fixed clock for unit and integration tests.
///
/// Can be constructed either directly from a `YYYY-MM-DD` date string or from a
/// UTC Unix timestamp plus a local timezone offset in seconds east of UTC.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FixedClock {
    today: String,
}

impl FixedClock {
    /// Pin "today" directly to the given `YYYY-MM-DD` string.
    #[must_use]
    pub fn from_ymd(today: impl Into<String>) -> Self {
        Self {
            today: today.into(),
        }
    }

    /// Compute the local `YYYY-MM-DD` date from a UTC Unix timestamp (in
    /// seconds) and a timezone offset (in seconds east of UTC).
    #[must_use]
    pub fn from_utc_and_offset(utc_secs: i64, offset_secs: i64) -> Self {
        Self {
            today: unix_secs_to_ymd(utc_secs.saturating_add(offset_secs)),
        }
    }

    /// Return the pinned `YYYY-MM-DD` date string.
    #[must_use]
    pub fn date_str(&self) -> &str {
        &self.today
    }
}

impl TodayClock for FixedClock {
    fn today_date(&self, _cached_tz_offset_secs: Option<i64>) -> String {
        self.today.clone()
    }
}

/// Production clock that checks [`TODAY_DATE_ENV_VAR`] (`TODOIST_TUI_TODAY`)
/// first, then [`TZ_OFFSET_SECS_ENV_VAR`], then the cached Todoist user
/// `tz_info` offset, and finally falls back to UTC wall-clock time.
#[derive(Debug, Clone, Copy, Default)]
pub struct SystemClock;

impl TodayClock for SystemClock {
    fn today_date(&self, cached_tz_offset_secs: Option<i64>) -> String {
        if let Ok(override_date) = std::env::var(TODAY_DATE_ENV_VAR) {
            let trimmed = override_date.trim();
            if is_valid_ymd_prefix(trimmed) {
                return trimmed[..10].to_owned();
            }
        }

        let env_offset = std::env::var(TZ_OFFSET_SECS_ENV_VAR)
            .ok()
            .and_then(|raw| raw.trim().parse::<i64>().ok());

        let offset_secs = env_offset.or(cached_tz_offset_secs).unwrap_or(0);
        let now_utc_secs = SystemTime::now()
            .duration_since(UNIX_EPOCH)
            .map_or(0_i64, |d| i64::try_from(d.as_secs()).unwrap_or(i64::MAX));

        unix_secs_to_ymd(now_utc_secs.saturating_add(offset_secs))
    }
}

/// Read the cached user's timezone offset (in seconds east of UTC) from the
/// `user` table if present.
#[must_use]
pub fn cached_user_tz_offset_secs(conn: &Connection) -> Option<i64> {
    let raw_json: String = conn
        .query_row("SELECT raw_json FROM user LIMIT 1", [], |row| row.get(0))
        .ok()?;
    let user: User = serde_json::from_str(&raw_json).ok()?;
    Some(user.tz_info.hours * 3600 + user.tz_info.minutes * 60)
}

/// Resolve the active `YYYY-MM-DD` local date using `clock` and the cached user
/// timezone in `conn`.
#[must_use]
pub fn resolve_today_date<C: TodayClock>(conn: &Connection, clock: &C) -> String {
    let tz_offset = cached_user_tz_offset_secs(conn);
    clock.today_date(tz_offset)
}

/// Load and sort all tasks due today or overdue as of `today_date` (`YYYY-MM-DD`)
/// from the SQLite cache (`T-1005`).
///
/// Makes zero network calls and reads exclusively from `conn`.
///
/// # Errors
///
/// Propagates [`crate::error::DbError`] if querying `items` or `projects` fails.
pub fn load_today_rows(conn: &Connection, today_date: &str) -> Result<Vec<TodayRow>> {
    let items = queries::today(conn, today_date)?;
    let projects = queries::all_projects(conn)?;
    Ok(build_today_rows(&items, &projects, today_date))
}

/// Load and sort all tasks due today or overdue using the provided [`TodayClock`]
/// (`T-1005`).
///
/// # Errors
///
/// Propagates [`crate::error::DbError`] if querying `items` or `projects` fails.
pub fn load_today_rows_with_clock<C: TodayClock>(
    conn: &Connection,
    clock: &C,
) -> Result<Vec<TodayRow>> {
    let today_date = resolve_today_date(conn, clock);
    load_today_rows(conn, &today_date)
}

/// Pure projection and filtering from domain [`Item`] and [`Project`] slices
/// into deterministically sorted [`TodayRow`] values.
#[must_use]
pub fn build_today_rows(items: &[Item], projects: &[Project], today_date: &str) -> Vec<TodayRow> {
    let today_day = due_day_prefix(today_date);
    let project_names: HashMap<&str, &str> = projects
        .iter()
        .filter(|p| !p.is_deleted)
        .map(|p| (p.id.as_str(), p.name.as_str()))
        .collect();

    let mut rows: Vec<TodayRow> = items
        .iter()
        .filter(|item| !item.checked && !item.is_deleted)
        .filter_map(|item| {
            let due = item.due.as_ref()?;
            let due_day = due_day_prefix(&due.date);
            if due_day.is_empty() || due_day > today_day {
                return None;
            }
            let is_overdue = due_day < today_day;
            let project_name = project_names
                .get(item.project_id.as_str())
                .copied()
                .unwrap_or("Inbox")
                .to_owned();
            let child_order = i32::try_from(item.child_order).unwrap_or(i32::MAX);

            Some(TodayRow {
                id: item.id.as_str().to_owned(),
                content: item.content.clone(),
                project_id: item.project_id.as_str().to_owned(),
                project_name,
                priority: item.priority.get(),
                due: Some(due.date.clone()),
                is_overdue,
                labels: item.labels.clone(),
                checked: item.checked,
                child_order,
            })
        })
        .collect();

    sort_today_rows(&mut rows);
    rows
}

/// Sort [`TodayRow`] items in-place according to the `T-1005` deterministic
/// ordering rule:
/// `priority` DESC -> `due` ASC -> `child_order` ASC -> `id` ASC.
pub fn sort_today_rows(rows: &mut [TodayRow]) {
    rows.sort_by(|a, b| {
        b.priority
            .cmp(&a.priority)
            .then_with(|| {
                a.due
                    .as_deref()
                    .unwrap_or("")
                    .cmp(b.due.as_deref().unwrap_or(""))
            })
            .then_with(|| a.child_order.cmp(&b.child_order))
            .then_with(|| a.id.cmp(&b.id))
    });
}

fn due_day_prefix(date_str: &str) -> &str {
    let bytes = date_str.as_bytes();
    if bytes.len() >= 10 && date_str.is_char_boundary(10) {
        &date_str[..10]
    } else {
        date_str
    }
}

fn is_valid_ymd_prefix(s: &str) -> bool {
    let b = s.as_bytes();
    if b.len() < 10 {
        return false;
    }
    b[0].is_ascii_digit()
        && b[1].is_ascii_digit()
        && b[2].is_ascii_digit()
        && b[3].is_ascii_digit()
        && b[4] == b'-'
        && b[5].is_ascii_digit()
        && b[6].is_ascii_digit()
        && b[7] == b'-'
        && b[8].is_ascii_digit()
        && b[9].is_ascii_digit()
}

/// Convert a Unix timestamp in seconds to a civil `YYYY-MM-DD` string using
/// Howard Hinnant's civil-from-days algorithm.
#[must_use]
#[allow(clippy::cast_sign_loss, clippy::cast_possible_wrap)]
pub fn unix_secs_to_ymd(secs: i64) -> String {
    let days = secs.div_euclid(86_400);
    let z = days + 719_468;
    let era = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe = (z - era * 146_097) as u64; // [0, 146096]
    let yoe = (doe - doe / 1460 + doe / 36524 - doe / 146_096) / 365; // [0, 399]
    let y = (yoe as i64) + era * 400;
    let doy = doe - (365 * yoe + yoe / 4 - yoe / 100); // [0, 365]
    let mp = (5 * doy + 2) / 153; // [0, 11]
    let d = doy - (153 * mp + 2) / 5 + 1; // [1, 31]
    let m = if mp < 10 { mp + 3 } else { mp - 9 }; // [1, 12]
    let year = if m <= 2 { y + 1 } else { y };
    format!("{year:04}-{m:02}-{d:02}")
}

#[cfg(test)]
#[allow(
    clippy::unwrap_used,
    clippy::expect_used,
    clippy::too_many_arguments,
    clippy::useless_vec
)]
mod tests {
    use super::*;
    use crate::api::model::{Due, Id, Priority};
    use crate::db::migrations;
    use crate::db::resources::{item as item_repo, project as project_repo};

    fn make_project(id: &str, name: &str) -> Project {
        Project {
            id: Id::from(id),
            name: name.to_owned(),
            parent_id: None,
            child_order: 1,
            color: "charcoal".to_owned(),
            is_favorite: false,
            is_archived: false,
            is_deleted: false,
            view_style: "list".to_owned(),
        }
    }

    fn make_item(
        id: &str,
        content: &str,
        project_id: &str,
        priority: u8,
        due_date: Option<&str>,
        child_order: i64,
        checked: bool,
        is_deleted: bool,
    ) -> Item {
        Item {
            id: Id::from(id),
            content: content.to_owned(),
            description: String::new(),
            project_id: Id::from(project_id),
            section_id: None,
            parent_id: None,
            child_order,
            priority: Priority::new(priority).unwrap(),
            labels: vec!["work".to_owned()],
            due: due_date.map(|d| Due {
                date: d.to_owned(),
                string: None,
                lang: None,
                is_recurring: false,
                timezone: None,
            }),
            deadline: None,
            checked,
            is_deleted,
            added_at: "2026-04-01T10:00:00Z".to_owned(),
            completed_at: None,
            user_id: Id::from("6X4VfakeUser1"),
            responsible_uid: None,
        }
    }

    #[test]
    #[allow(clippy::too_many_lines)]
    fn today_includes_overdue_and_due_today_and_excludes_tomorrow_no_due_completed_and_deleted() {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        let tx = conn.transaction().unwrap();
        project_repo::upsert(&tx, &make_project("6X4VfakeProj1", "Backend")).unwrap();

        let fixtures = [
            make_item(
                "6X4VfakeOverdue",
                "Overdue task",
                "6X4VfakeProj1",
                2,
                Some("2026-04-16"),
                1,
                false,
                false,
            ),
            make_item(
                "6X4VfakeTodayDate",
                "Due today date",
                "6X4VfakeProj1",
                3,
                Some("2026-04-17"),
                2,
                false,
                false,
            ),
            make_item(
                "6X4VfakeTodayTime",
                "Due today datetime",
                "6X4VfakeProj1",
                3,
                Some("2026-04-17T14:30:00"),
                3,
                false,
                false,
            ),
            make_item(
                "6X4VfakeTomorrow",
                "Due tomorrow",
                "6X4VfakeProj1",
                4,
                Some("2026-04-18"),
                1,
                false,
                false,
            ),
            make_item(
                "6X4VfakeNoDue",
                "No due date",
                "6X4VfakeProj1",
                4,
                None,
                1,
                false,
                false,
            ),
            make_item(
                "6X4VfakeChecked",
                "Completed today",
                "6X4VfakeProj1",
                4,
                Some("2026-04-17"),
                1,
                true,
                false,
            ),
            make_item(
                "6X4VfakeDeleted",
                "Deleted today",
                "6X4VfakeProj1",
                4,
                Some("2026-04-17"),
                1,
                false,
                true,
            ),
        ];

        for item in &fixtures {
            item_repo::upsert(&tx, item).unwrap();
        }
        tx.commit().unwrap();

        let clock = FixedClock::from_ymd("2026-04-17");
        let rows = load_today_rows_with_clock(&conn, &clock).unwrap();

        let ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            ids,
            vec!["6X4VfakeTodayDate", "6X4VfakeTodayTime", "6X4VfakeOverdue"]
        );

        assert!(!rows[0].is_overdue);
        assert!(!rows[1].is_overdue);
        assert!(rows[2].is_overdue);
        assert_eq!(rows[0].project_name, "Backend");
    }

    #[test]
    fn fixed_clock_timezone_offset_shifts_day_boundary_deterministically() {
        // 2026-04-17T22:30:00Z is still 2026-04-17 in UTC, but 2026-04-18 in UTC+3 (+10_800s).
        let utc_secs = 1_776_465_000_i64; // 2026-04-17T22:30:00Z
        let clock_utc = FixedClock::from_utc_and_offset(utc_secs, 0);
        let clock_plus_3 = FixedClock::from_utc_and_offset(utc_secs, 3 * 3600);

        assert_eq!(clock_utc.date_str(), "2026-04-17");
        assert_eq!(clock_plus_3.date_str(), "2026-04-18");
    }

    #[test]
    fn deterministic_ordering_sorts_by_priority_desc_due_asc_child_order_asc_id_asc() {
        let projects = vec![make_project("6X4VfakeProj1", "Work")];
        let canonical = vec![
            // 1. P1 (priority 4), earlier due
            make_item(
                "6X4VfakeA",
                "First",
                "6X4VfakeProj1",
                4,
                Some("2026-04-16"),
                5,
                false,
                false,
            ),
            // 2. P1 (priority 4), later due, lower child_order
            make_item(
                "6X4VfakeB",
                "Second",
                "6X4VfakeProj1",
                4,
                Some("2026-04-17"),
                1,
                false,
                false,
            ),
            // 3. P1 (priority 4), same due, higher child_order, lower id
            make_item(
                "6X4VfakeC1",
                "Third",
                "6X4VfakeProj1",
                4,
                Some("2026-04-17"),
                2,
                false,
                false,
            ),
            // 4. P1 (priority 4), same due, same child_order, higher id
            make_item(
                "6X4VfakeC2",
                "Fourth",
                "6X4VfakeProj1",
                4,
                Some("2026-04-17"),
                2,
                false,
                false,
            ),
            // 5. P3 (priority 2)
            make_item(
                "6X4VfakeD",
                "Fifth",
                "6X4VfakeProj1",
                2,
                Some("2026-04-15"),
                1,
                false,
                false,
            ),
        ];

        let expected_ids = vec![
            "6X4VfakeA",
            "6X4VfakeB",
            "6X4VfakeC1",
            "6X4VfakeC2",
            "6X4VfakeD",
        ];

        // Test multiple permutations to prove input order never affects output order.
        let permutations: [&[usize]; 3] = [&[4, 3, 2, 1, 0], &[2, 0, 4, 1, 3], &[1, 4, 0, 3, 2]];

        for perm in permutations {
            let shuffled: Vec<Item> = perm.iter().map(|&idx| canonical[idx].clone()).collect();
            let rows = build_today_rows(&shuffled, &projects, "2026-04-17");
            let actual_ids: Vec<&str> = rows.iter().map(|r| r.id.as_str()).collect();
            assert_eq!(actual_ids, expected_ids);
        }
    }
}
