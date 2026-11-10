//! Read-model data port ([`ViewData`]), deterministic in-memory [`FixtureData`],
//! and SQLite-backed [`SqliteViewData`] adapter (T-605, EPIC-07).
//!
//! Widgets and [`crate::ui::app::App`] depend only on the [`ViewData`] trait so
//! tests and early TUI shell runs can operate against [`FixtureData`] with zero
//! database or network I/O, while live sessions can plug in [`SqliteViewData`].

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};

use rusqlite::Connection;

use crate::api::model::{Deadline, Due, Filter, Id, Item, Label, Note, Priority, Project, Section};
use crate::db::queries;
use crate::error::Result;
use crate::filters::{self, FilterItem, FilterResult, NaiveDateLike};

use super::model::{Comment, SectionHeader, SidebarNode, TaskDetail, TaskRow, ViewId};

/// Reference "today" date (`YYYY-MM-DD`) used by [`FixtureData::sample`] and
/// [`FixtureData::generated`] so overdue/today/upcoming classifications are
/// 100% deterministic regardless of the host clock.
pub const SAMPLE_TODAY_DATE: &str = "2024-05-06";

/// Fake Inbox project identifier used in [`FixtureData`].
pub const SAMPLE_INBOX_PROJECT_ID: &str = "6X4VfakePrjInbox";

/// Read-only data port consumed by the TUI reducer and widgets.
pub trait ViewData {
    /// Returns the ordered list of sidebar entries (Inbox, Today, Upcoming,
    /// project tree in depth-first sibling order, personal labels, and saved
    /// filters).
    ///
    /// # Errors
    ///
    /// Returns an [`crate::error::AppError`] if the underlying store fails.
    fn sidebar(&self) -> Result<Vec<SidebarNode>>;

    /// Returns the ordered task rows for `view`.
    ///
    /// For project and inbox views, ungrouped tasks (`section_id: None`) come
    /// first, followed by tasks grouped by section in ascending `section_order`;
    /// within each group, sub-tasks immediately follow their parent task in
    /// `(child_order, id)` order with `depth` set to their nesting level.
    ///
    /// # Errors
    ///
    /// Returns an [`crate::error::AppError`] if the underlying store fails.
    fn tasks(&self, view: &ViewId) -> Result<Vec<TaskRow>>;

    /// Returns the full [`TaskDetail`] (including comments) for `task_id`, or
    /// `Ok(None)` when no task with that id exists.
    ///
    /// # Errors
    ///
    /// Returns an [`crate::error::AppError`] if the underlying store fails.
    fn detail(&self, task_id: &Id) -> Result<Option<TaskDetail>>;

    /// Returns the ordered section headers belonging to `project_id`.
    ///
    /// # Errors
    ///
    /// Returns an [`crate::error::AppError`] if the underlying store fails.
    fn sections(&self, _project_id: &Id) -> Result<Vec<SectionHeader>> {
        Ok(Vec::new())
    }
}

/// Deterministic in-memory implementation of [`ViewData`] for headless tests,
/// snapshots, and benchmarks.
#[derive(Debug, Clone)]
pub struct FixtureData {
    today_date: String,
    inbox_project_id: Id,
    projects: Vec<Project>,
    sections: Vec<Section>,
    labels: Vec<Label>,
    filters: Vec<Filter>,
    items: Vec<Item>,
    comments: HashMap<Id, Vec<Comment>>,
}

impl Default for FixtureData {
    fn default() -> Self {
        Self::sample()
    }
}

impl FixtureData {
    /// Returns the reference "today" date (`YYYY-MM-DD`) used by this fixture.
    #[must_use]
    pub fn today_date(&self) -> &str {
        &self.today_date
    }

    /// Returns the fake Inbox project [`Id`] used by this fixture.
    #[must_use]
    pub const fn inbox_project_id(&self) -> &Id {
        &self.inbox_project_id
    }

    /// Builds a rich, deterministic sample fixture covering:
    /// - Inbox, Today, and Upcoming views
    /// - 3-level nested project hierarchy (`Home > Kitchen`, `Work > Q2 Launch > Backend`)
    /// - Multiple sections (`Errands`, `Planning`, `In Progress`)
    /// - Sub-tasks up to depth 2
    /// - All four Todoist priorities (`1..=4`)
    /// - Overdue, due-today, upcoming, recurring due dates, and deadlines
    /// - Personal labels (`errand`, `home`, `deep-work`, `urgent`)
    /// - Saved filters (`Overdue & Home`, `High Priority`)
    /// - Task comments
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn sample() -> Self {
        let inbox_id = Id::new(SAMPLE_INBOX_PROJECT_ID);
        let prj_home = Id::new("6X4VfakePrjHome");
        let prj_kitchen = Id::new("6X4VfakePrjKitchen");
        let prj_work = Id::new("6X4VfakePrjWork");
        let prj_launch = Id::new("6X4VfakePrjLaunch");
        let prj_backend = Id::new("6X4VfakePrjBackend");

        let projects = vec![
            Project {
                id: inbox_id.clone(),
                name: String::from("Inbox"),
                parent_id: None,
                child_order: 0,
                color: String::from("grey"),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: String::from("list"),
            },
            Project {
                id: prj_home.clone(),
                name: String::from("Home"),
                parent_id: None,
                child_order: 1,
                color: String::from("green"),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: String::from("list"),
            },
            Project {
                id: prj_kitchen.clone(),
                name: String::from("Kitchen"),
                parent_id: Some(prj_home.clone()),
                child_order: 1,
                color: String::from("olive_green"),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: String::from("list"),
            },
            Project {
                id: prj_work.clone(),
                name: String::from("Work"),
                parent_id: None,
                child_order: 2,
                color: String::from("blue"),
                is_favorite: true,
                is_archived: false,
                is_deleted: false,
                view_style: String::from("list"),
            },
            Project {
                id: prj_launch.clone(),
                name: String::from("Q2 Launch"),
                parent_id: Some(prj_work.clone()),
                child_order: 1,
                color: String::from("berry_red"),
                is_favorite: true,
                is_archived: false,
                is_deleted: false,
                view_style: String::from("list"),
            },
            Project {
                id: prj_backend.clone(),
                name: String::from("Backend"),
                parent_id: Some(prj_launch.clone()),
                child_order: 1,
                color: String::from("teal"),
                is_favorite: false,
                is_archived: false,
                is_deleted: false,
                view_style: String::from("list"),
            },
        ];

        let sec_errands = Id::new("6X4VfakeSecErrands");
        let sec_planning = Id::new("6X4VfakeSecPlanning");
        let sec_in_progress = Id::new("6X4VfakeSecInProg");

        let sections = vec![
            Section {
                id: sec_errands.clone(),
                name: String::from("Errands"),
                project_id: inbox_id.clone(),
                section_order: 1,
                is_deleted: false,
                is_archived: false,
            },
            Section {
                id: sec_planning.clone(),
                name: String::from("Planning"),
                project_id: inbox_id.clone(),
                section_order: 2,
                is_deleted: false,
                is_archived: false,
            },
            Section {
                id: sec_in_progress.clone(),
                name: String::from("In Progress"),
                project_id: prj_work.clone(),
                section_order: 1,
                is_deleted: false,
                is_archived: false,
            },
        ];

        let labels = vec![
            Label {
                id: Id::new("6X4VfakeLblErrand"),
                name: String::from("errand"),
                color: String::from("amber"),
                item_order: 1,
                is_favorite: false,
                is_deleted: false,
            },
            Label {
                id: Id::new("6X4VfakeLblHome"),
                name: String::from("home"),
                color: String::from("green"),
                item_order: 2,
                is_favorite: false,
                is_deleted: false,
            },
            Label {
                id: Id::new("6X4VfakeLblDeepWork"),
                name: String::from("deep-work"),
                color: String::from("blue"),
                item_order: 3,
                is_favorite: true,
                is_deleted: false,
            },
            Label {
                id: Id::new("6X4VfakeLblUrgent"),
                name: String::from("urgent"),
                color: String::from("berry_red"),
                item_order: 4,
                is_favorite: true,
                is_deleted: false,
            },
        ];

        let filters = vec![
            Filter {
                id: Id::new("6X4VfakeFltOverdueHome"),
                name: String::from("Overdue & Home"),
                query: String::from("overdue & #Home"),
                color: String::from("berry_red"),
                item_order: 1,
                is_favorite: true,
                is_deleted: false,
            },
            Filter {
                id: Id::new("6X4VfakeFltHighPrio"),
                name: String::from("High Priority"),
                query: String::from("p1 | p2"),
                color: String::from("orange"),
                item_order: 2,
                is_favorite: false,
                is_deleted: false,
            },
        ];

        let items = Self::sample_items(
            &inbox_id,
            &prj_home,
            &prj_kitchen,
            &prj_work,
            &prj_launch,
            &prj_backend,
            &sec_errands,
            &sec_planning,
            &sec_in_progress,
        );

        let mut comments = HashMap::new();
        comments.insert(
            Id::new("6X4VfakeItm001"),
            vec![
                Comment {
                    id: Id::new("6X4VfakeNot001"),
                    posted_at: String::from("2024-05-04T09:15:00Z"),
                    author: String::from("user@example.invalid"),
                    content: String::from("Drafted initial agenda notes for the team review."),
                    attachment_name: None,
                },
                Comment {
                    id: Id::new("6X4VfakeNot002"),
                    posted_at: String::from("2024-05-05T14:30:00Z"),
                    author: String::from("user@example.invalid"),
                    content: String::from("Attached the latency benchmark summary."),
                    attachment_name: Some(String::from("benchmarks.txt")),
                },
            ],
        );
        comments.insert(
            Id::new("6X4VfakeItm002"),
            vec![Comment {
                id: Id::new("6X4VfakeNot003"),
                posted_at: String::from("2024-05-06T08:00:00Z"),
                author: String::from("user@example.invalid"),
                content: String::from("Verified p95 numbers on CI runner."),
                attachment_name: None,
            }],
        );

        Self {
            today_date: String::from(SAMPLE_TODAY_DATE),
            inbox_project_id: inbox_id,
            projects,
            sections,
            labels,
            filters,
            items,
            comments,
        }
    }

    #[allow(clippy::too_many_arguments, clippy::too_many_lines)]
    fn sample_items(
        inbox_id: &Id,
        prj_home: &Id,
        prj_kitchen: &Id,
        prj_work: &Id,
        prj_launch: &Id,
        prj_backend: &Id,
        sec_errands: &Id,
        sec_planning: &Id,
        sec_in_progress: &Id,
    ) -> Vec<Item> {
        let user_id = Id::new("6X4VfakeUsr001");
        let p1 = Priority::try_from(4).unwrap_or(Priority_fallback());
        let p2 = Priority::try_from(3).unwrap_or(Priority_fallback());
        let p3 = Priority::try_from(2).unwrap_or(Priority_fallback());
        let p4 = Priority::try_from(1).unwrap_or(Priority_fallback());

        vec![
            // Inbox ungrouped top-level task (overdue, P1/api=4, deadline, labels, 2 comments)
            Item {
                id: Id::new("6X4VfakeItm001"),
                content: String::from("Finalize quarterly roadmap"),
                description: String::from(
                    "Prepare agenda and metrics deck\nInclude Q2 launch milestones",
                ),
                project_id: inbox_id.clone(),
                section_id: None,
                parent_id: None,
                child_order: 1,
                priority: p1,
                labels: vec![String::from("deep-work"), String::from("urgent")],
                due: Some(Due {
                    date: String::from("2024-05-01"),
                    string: Some(String::from("May 1")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: Some(Deadline {
                    date: String::from("2024-05-05"),
                    lang: Some(String::from("en")),
                }),
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-04-25T10:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Sub-task of 6X4VfakeItm001 (depth 1, due today, P2/api=3)
            Item {
                id: Id::new("6X4VfakeItm002"),
                content: String::from("Collect latency numbers"),
                description: String::from("Run synthetic 5k-item benchmark"),
                project_id: inbox_id.clone(),
                section_id: None,
                parent_id: Some(Id::new("6X4VfakeItm001")),
                child_order: 1,
                priority: p2,
                labels: vec![String::from("deep-work")],
                due: Some(Due {
                    date: String::from("2024-05-06"),
                    string: Some(String::from("today")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-04-26T10:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Sub-sub-task of 6X4VfakeItm002 (depth 2, future due, P3/api=2)
            Item {
                id: Id::new("6X4VfakeItm003"),
                content: String::from("Format p95 table"),
                description: String::new(),
                project_id: inbox_id.clone(),
                section_id: None,
                parent_id: Some(Id::new("6X4VfakeItm002")),
                child_order: 1,
                priority: p3,
                labels: Vec::new(),
                due: Some(Due {
                    date: String::from("2024-05-08"),
                    string: Some(String::from("May 8")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-04-26T11:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Inbox Section "Errands" (order 1): recurring due today, P3/api=2
            Item {
                id: Id::new("6X4VfakeItm004"),
                content: String::from("Pick up coffee beans"),
                description: String::from("Whole bean medium roast"),
                project_id: inbox_id.clone(),
                section_id: Some(sec_errands.clone()),
                parent_id: None,
                child_order: 1,
                priority: p3,
                labels: vec![String::from("errand"), String::from("home")],
                due: Some(Due {
                    date: String::from("2024-05-06"),
                    string: Some(String::from("every monday")),
                    lang: Some(String::from("en")),
                    is_recurring: true,
                    timezone: None,
                }),
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-04-27T09:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Inbox Section "Errands" (order 1): future due, P4/api=1
            Item {
                id: Id::new("6X4VfakeItm005"),
                content: String::from("Drop off recycling"),
                description: String::new(),
                project_id: inbox_id.clone(),
                section_id: Some(sec_errands.clone()),
                parent_id: None,
                child_order: 2,
                priority: p4,
                labels: vec![String::from("errand")],
                due: Some(Due {
                    date: String::from("2024-05-10"),
                    string: Some(String::from("May 10")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-04-28T09:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Inbox Section "Planning" (order 2): deadline + future due, P2/api=3
            Item {
                id: Id::new("6X4VfakeItm006"),
                content: String::from("Draft sprint retrospective"),
                description: String::from("Collect feedback from engineering"),
                project_id: inbox_id.clone(),
                section_id: Some(sec_planning.clone()),
                parent_id: None,
                child_order: 1,
                priority: p2,
                labels: vec![String::from("deep-work")],
                due: Some(Due {
                    date: String::from("2024-05-09"),
                    string: Some(String::from("May 9")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: Some(Deadline {
                    date: String::from("2024-05-12"),
                    lang: Some(String::from("en")),
                }),
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-04-29T09:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Home project task (overdue)
            Item {
                id: Id::new("6X4VfakeItm007"),
                content: String::from("Repair garden faucet"),
                description: String::from("Replace worn washer"),
                project_id: prj_home.clone(),
                section_id: None,
                parent_id: None,
                child_order: 1,
                priority: p2,
                labels: vec![String::from("home")],
                due: Some(Due {
                    date: String::from("2024-05-02"),
                    string: Some(String::from("May 2")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-04-29T12:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Home > Kitchen task
            Item {
                id: Id::new("6X4VfakeItm008"),
                content: String::from("Descale espresso machine"),
                description: String::new(),
                project_id: prj_kitchen.clone(),
                section_id: None,
                parent_id: None,
                child_order: 1,
                priority: p4,
                labels: vec![String::from("home")],
                due: Some(Due {
                    date: String::from("2024-05-11"),
                    string: Some(String::from("May 11")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-04-30T12:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Work project task in "In Progress" section
            Item {
                id: Id::new("6X4VfakeItm009"),
                content: String::from("Review sync engine PR"),
                description: String::from("Verify retry backoff and offline queue drain"),
                project_id: prj_work.clone(),
                section_id: Some(sec_in_progress.clone()),
                parent_id: None,
                child_order: 1,
                priority: p1,
                labels: vec![String::from("deep-work"), String::from("urgent")],
                due: Some(Due {
                    date: String::from("2024-05-06"),
                    string: Some(String::from("today")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-05-01T12:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Work > Q2 Launch task
            Item {
                id: Id::new("6X4VfakeItm010"),
                content: String::from("Publish release notes"),
                description: String::new(),
                project_id: prj_launch.clone(),
                section_id: None,
                parent_id: None,
                child_order: 1,
                priority: p2,
                labels: vec![String::from("deep-work")],
                due: Some(Due {
                    date: String::from("2024-05-12"),
                    string: Some(String::from("May 12")),
                    lang: Some(String::from("en")),
                    is_recurring: false,
                    timezone: None,
                }),
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-05-01T15:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            },
            // Work > Q2 Launch > Backend task
            Item {
                id: Id::new("6X4VfakeItm011"),
                content: String::from("Vacuum SQLite WAL on shutdown"),
                description: String::new(),
                project_id: prj_backend.clone(),
                section_id: None,
                parent_id: None,
                child_order: 1,
                priority: p3,
                labels: vec![String::from("deep-work")],
                due: None,
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-05-02T15:00:00Z"),
                completed_at: None,
                user_id,
                responsible_uid: None,
            },
        ]
    }

    /// Deterministically generates `n_items` fake tasks across projects,
    /// sections, priorities, due dates, and sub-task depths for performance
    /// testing (T-615).
    #[must_use]
    #[allow(clippy::too_many_lines)]
    pub fn generated(n_items: usize) -> Self {
        let mut base = Self::sample();
        base.items.clear();
        base.comments.clear();

        let project_ids: Vec<Id> = base.projects.iter().map(|p| p.id.clone()).collect();
        let inbox_id = base.inbox_project_id.clone();
        let sec_errands = Id::new("6X4VfakeSecErrands");
        let sec_planning = Id::new("6X4VfakeSecPlanning");
        let user_id = Id::new("6X4VfakeUsr001");
        let label_pool = ["errand", "home", "deep-work", "urgent"];
        let due_pool = [
            Some("2024-05-01"),
            Some("2024-05-06"),
            Some("2024-05-08"),
            Some("2024-05-12"),
            None,
        ];

        let mut items = Vec::with_capacity(n_items);
        let mut last_top_level_in_proj: HashMap<Id, Id> = HashMap::new();

        for idx in 0..n_items {
            let item_id = Id::new(format!("6X4VfakeGenItm{idx:05}"));
            // Put half of generated items in Inbox so default-view perf tests
            // at 5k items exercise a large list immediately on startup.
            let project_id = if idx % 2 == 0 || project_ids.is_empty() {
                inbox_id.clone()
            } else {
                project_ids[idx % project_ids.len()].clone()
            };

            let section_id = if project_id == inbox_id {
                match idx % 3 {
                    0 => None,
                    1 => Some(sec_errands.clone()),
                    _ => Some(sec_planning.clone()),
                }
            } else {
                None
            };

            let parent_id = if idx % 7 == 3 {
                last_top_level_in_proj.get(&project_id).cloned()
            } else {
                last_top_level_in_proj.insert(project_id.clone(), item_id.clone());
                None
            };

            #[allow(clippy::cast_possible_truncation)]
            let raw_prio = ((idx % 4) as u8) + 1;
            let priority = Priority::try_from(raw_prio).unwrap_or(Priority_fallback());

            let due = due_pool[idx % due_pool.len()].map(|d| Due {
                date: String::from(d),
                string: None,
                lang: Some(String::from("en")),
                is_recurring: idx % 11 == 0,
                timezone: None,
            });

            let labels = if idx % 3 == 0 {
                vec![String::from(label_pool[idx % label_pool.len()])]
            } else {
                Vec::new()
            };

            let child_order = i64::try_from(idx + 1).unwrap_or(i64::MAX);

            items.push(Item {
                id: item_id,
                content: format!("Synthetic fixture task #{idx:05}"),
                description: format!("Generated description for synthetic task {idx}"),
                project_id,
                section_id,
                parent_id,
                child_order,
                priority,
                labels,
                due,
                deadline: None,
                checked: false,
                is_deleted: false,
                added_at: String::from("2024-05-01T00:00:00Z"),
                completed_at: None,
                user_id: user_id.clone(),
                responsible_uid: None,
            });
        }

        base.items = items;
        base
    }
}

#[allow(non_snake_case)]
fn Priority_fallback() -> Priority {
    match Priority::new(1) {
        Ok(p) => p,
        Err(_) => unreachable!(),
    }
}

impl ViewData for FixtureData {
    fn sidebar(&self) -> Result<Vec<SidebarNode>> {
        let active_items: Vec<&Item> = self
            .items
            .iter()
            .filter(|it| !it.checked && !it.is_deleted)
            .collect();

        Ok(build_sidebar_nodes(
            &self.inbox_project_id,
            &self.today_date,
            &self.projects,
            &self.labels,
            &self.filters,
            &active_items,
        ))
    }

    fn tasks(&self, view: &ViewId) -> Result<Vec<TaskRow>> {
        let project_names: HashMap<&Id, &str> = self
            .projects
            .iter()
            .filter(|p| !p.is_deleted)
            .map(|p| (&p.id, p.name.as_str()))
            .collect();

        let section_map: HashMap<&Id, (&str, i64)> = self
            .sections
            .iter()
            .filter(|s| !s.is_deleted && !s.is_archived)
            .map(|s| (&s.id, (s.name.as_str(), s.section_order)))
            .collect();

        let active_items: Vec<&Item> = self
            .items
            .iter()
            .filter(|it| !it.checked && !it.is_deleted)
            .collect();

        let matching: Vec<&Item> = match view {
            ViewId::Inbox => active_items
                .into_iter()
                .filter(|it| it.project_id == self.inbox_project_id)
                .collect(),
            ViewId::Today => active_items
                .into_iter()
                .filter(|it| is_due_on_or_before(it.due.as_ref(), &self.today_date))
                .collect(),
            ViewId::Upcoming => active_items
                .into_iter()
                .filter(|it| is_due_upcoming(it.due.as_ref(), &self.today_date, 7))
                .collect(),
            ViewId::Project(pid) => active_items
                .into_iter()
                .filter(|it| &it.project_id == pid)
                .collect(),
            ViewId::Label(label_name) => active_items
                .into_iter()
                .filter(|it| it.labels.iter().any(|l| l == label_name))
                .collect(),
            ViewId::Filter(filter_id) => {
                let Some(flt) = self
                    .filters
                    .iter()
                    .find(|f| &f.id == filter_id && !f.is_deleted)
                else {
                    return Ok(Vec::new());
                };
                filter_matching_items(&active_items, &project_names, &flt.query, &self.today_date)
            }
        };

        let group_by_section = matches!(view, ViewId::Inbox | ViewId::Project(_));
        Ok(build_task_rows(
            &matching,
            &project_names,
            &section_map,
            &self.today_date,
            group_by_section,
        ))
    }

    fn detail(&self, task_id: &Id) -> Result<Option<TaskDetail>> {
        let Some(item) = self
            .items
            .iter()
            .find(|it| &it.id == task_id && !it.is_deleted)
        else {
            return Ok(None);
        };

        let project_name = self
            .projects
            .iter()
            .find(|p| p.id == item.project_id && !p.is_deleted)
            .map(|p| p.name.clone());

        let section_name = item.section_id.as_ref().and_then(|sid| {
            self.sections
                .iter()
                .find(|s| &s.id == sid && !s.is_deleted)
                .map(|s| s.name.clone())
        });

        let comments = self.comments.get(task_id).cloned().unwrap_or_default();

        Ok(Some(TaskDetail {
            id: item.id.clone(),
            content: item.content.clone(),
            description: item.description.clone(),
            project_id: item.project_id.clone(),
            project_name,
            section_id: item.section_id.clone(),
            section_name,
            is_overdue: is_overdue_due(item.due.as_ref(), &self.today_date),
            due: item.due.clone(),
            deadline: item.deadline.clone(),
            priority: item.priority,
            labels: item.labels.clone(),
            checked: item.checked,
            comments,
        }))
    }

    fn sections(&self, project_id: &Id) -> Result<Vec<SectionHeader>> {
        let mut secs: Vec<&Section> = self
            .sections
            .iter()
            .filter(|s| &s.project_id == project_id && !s.is_deleted && !s.is_archived)
            .collect();
        secs.sort_by(|a, b| a.section_order.cmp(&b.section_order).then(a.id.cmp(&b.id)));
        Ok(secs
            .into_iter()
            .map(|s| SectionHeader {
                id: s.id.clone(),
                name: s.name.clone(),
                section_order: i32::try_from(s.section_order).unwrap_or(i32::MAX),
            })
            .collect())
    }
}

/// SQLite-backed implementation of [`ViewData`] reading from `crate::db::queries`.
pub struct SqliteViewData<'a> {
    conn: &'a Connection,
    today_date: String,
}

impl<'a> SqliteViewData<'a> {
    /// Wraps an open SQLite [`Connection`] and an injected `today_date` (`YYYY-MM-DD`).
    #[must_use]
    pub fn new(conn: &'a Connection, today_date: impl Into<String>) -> Self {
        Self {
            conn,
            today_date: today_date.into(),
        }
    }
}

impl ViewData for SqliteViewData<'_> {
    fn sidebar(&self) -> Result<Vec<SidebarNode>> {
        let projects = queries::all_projects(self.conn)?;
        let labels = queries::all_labels(self.conn)?;
        let filters = queries::all_filters(self.conn)?;
        let items = queries::all_active_items(self.conn)?;
        let inbox_items = queries::inbox(self.conn)?;

        let inbox_project_id = inbox_items
            .first()
            .map(|it| it.project_id.clone())
            .or_else(|| {
                projects
                    .iter()
                    .find(|p| p.name.eq_ignore_ascii_case("inbox"))
                    .map(|p| p.id.clone())
            })
            .unwrap_or_else(|| Id::new(""));

        let item_refs: Vec<&Item> = items.iter().collect();
        let mut nodes = build_sidebar_nodes(
            &inbox_project_id,
            &self.today_date,
            &projects,
            &labels,
            &filters,
            &item_refs,
        );
        if let Some(first) = nodes.first_mut() {
            first.task_count = inbox_items.len();
        }
        Ok(nodes)
    }

    fn tasks(&self, view: &ViewId) -> Result<Vec<TaskRow>> {
        let projects = queries::all_projects(self.conn)?;
        let sections = queries::all_sections(self.conn)?;

        let project_names: HashMap<&Id, &str> = projects
            .iter()
            .filter(|p| !p.is_deleted)
            .map(|p| (&p.id, p.name.as_str()))
            .collect();

        let section_map: HashMap<&Id, (&str, i64)> = sections
            .iter()
            .filter(|s| !s.is_deleted && !s.is_archived)
            .map(|s| (&s.id, (s.name.as_str(), s.section_order)))
            .collect();

        let items: Vec<Item> = match view {
            ViewId::Inbox => queries::inbox(self.conn)?,
            ViewId::Today => queries::today(self.conn, &self.today_date)?,
            ViewId::Upcoming => queries::upcoming(self.conn, &self.today_date, 7)?,
            ViewId::Project(pid) => queries::items_by_project(self.conn, pid)?,
            ViewId::Label(name) => queries::items_by_label(self.conn, name)?,
            ViewId::Filter(filter_id) => {
                let all_filters = queries::all_filters(self.conn)?;
                let Some(flt) = all_filters.iter().find(|f| &f.id == filter_id) else {
                    return Ok(Vec::new());
                };
                let all_items = queries::all_active_items(self.conn)?;
                let refs: Vec<&Item> = all_items.iter().collect();
                filter_matching_items(&refs, &project_names, &flt.query, &self.today_date)
                    .into_iter()
                    .cloned()
                    .collect()
            }
        };

        let item_refs: Vec<&Item> = items.iter().collect();
        let group_by_section = matches!(view, ViewId::Inbox | ViewId::Project(_));
        Ok(build_task_rows(
            &item_refs,
            &project_names,
            &section_map,
            &self.today_date,
            group_by_section,
        ))
    }

    fn detail(&self, task_id: &Id) -> Result<Option<TaskDetail>> {
        let Some(item) = queries::item_by_id(self.conn, task_id)? else {
            return Ok(None);
        };
        let projects = queries::all_projects(self.conn)?;
        let sections = queries::all_sections(self.conn)?;
        let notes = queries::notes_for_item(self.conn, task_id)?;

        let project_name = projects
            .into_iter()
            .find(|p| p.id == item.project_id)
            .map(|p| p.name);

        let section_name = item
            .section_id
            .as_ref()
            .and_then(|sid| sections.into_iter().find(|s| &s.id == sid).map(|s| s.name));

        let comments = notes.into_iter().map(note_to_comment).collect();

        Ok(Some(TaskDetail {
            id: item.id,
            content: item.content,
            description: item.description,
            project_id: item.project_id,
            project_name,
            section_id: item.section_id,
            section_name,
            is_overdue: is_overdue_due(item.due.as_ref(), &self.today_date),
            due: item.due,
            deadline: item.deadline,
            priority: item.priority,
            labels: item.labels,
            checked: item.checked,
            comments,
        }))
    }

    fn sections(&self, project_id: &Id) -> Result<Vec<SectionHeader>> {
        let secs = queries::sections_by_project(self.conn, project_id)?;
        Ok(secs
            .into_iter()
            .filter(|s| !s.is_deleted && !s.is_archived)
            .map(|s| SectionHeader {
                id: s.id,
                name: s.name,
                section_order: i32::try_from(s.section_order).unwrap_or(i32::MAX),
            })
            .collect())
    }
}

fn note_to_comment(note: Note) -> Comment {
    let attachment_name = note.file_attachment.and_then(|att| att.file_name);
    Comment {
        id: note.id,
        posted_at: note.posted_at,
        author: String::from("user"),
        content: note.content,
        attachment_name,
    }
}

#[allow(clippy::too_many_lines)]
fn build_sidebar_nodes(
    inbox_project_id: &Id,
    today_date: &str,
    projects: &[Project],
    labels: &[Label],
    filters: &[Filter],
    active_items: &[&Item],
) -> Vec<SidebarNode> {
    let mut project_counts: HashMap<&Id, usize> = HashMap::new();
    let mut label_counts: HashMap<&str, usize> = HashMap::new();
    let mut today_count = 0usize;
    let mut upcoming_count = 0usize;

    for item in active_items {
        *project_counts.entry(&item.project_id).or_insert(0) += 1;
        for lbl in &item.labels {
            *label_counts.entry(lbl.as_str()).or_insert(0) += 1;
        }
        if is_due_on_or_before(item.due.as_ref(), today_date) {
            today_count += 1;
        }
        if is_due_upcoming(item.due.as_ref(), today_date, 7) {
            upcoming_count += 1;
        }
    }

    let inbox_count = project_counts.get(inbox_project_id).copied().unwrap_or(0);
    let mut nodes = vec![
        SidebarNode::inbox(inbox_count),
        SidebarNode::today(today_count),
        SidebarNode::upcoming(upcoming_count),
    ];

    let live_projects: Vec<&Project> = projects
        .iter()
        .filter(|p| !p.is_deleted && !p.is_archived && &p.id != inbox_project_id)
        .collect();

    let live_ids: HashSet<&Id> = live_projects.iter().map(|p| &p.id).collect();
    let mut children_by_parent: HashMap<Option<&Id>, Vec<&Project>> = HashMap::new();
    for proj in &live_projects {
        let parent_key = proj.parent_id.as_ref().filter(|pid| live_ids.contains(pid));
        children_by_parent
            .entry(parent_key)
            .or_default()
            .push(*proj);
    }
    for siblings in children_by_parent.values_mut() {
        siblings.sort_by(|a, b| a.child_order.cmp(&b.child_order).then(a.id.cmp(&b.id)));
    }

    append_project_subtree(
        None,
        0,
        &children_by_parent,
        &project_counts,
        &mut nodes,
        &mut HashSet::new(),
    );

    let mut sorted_labels: Vec<&Label> = labels.iter().filter(|l| !l.is_deleted).collect();
    sorted_labels.sort_by(|a, b| a.item_order.cmp(&b.item_order).then(a.id.cmp(&b.id)));
    for lbl in sorted_labels {
        let count = label_counts.get(lbl.name.as_str()).copied().unwrap_or(0);
        let color = (!lbl.color.is_empty()).then(|| lbl.color.clone());
        nodes.push(
            SidebarNode::label_node(lbl.id.clone(), lbl.name.clone(), count)
                .with_favorite(lbl.is_favorite)
                .with_color(color),
        );
    }

    let project_names: HashMap<&Id, &str> = projects
        .iter()
        .filter(|p| !p.is_deleted)
        .map(|p| (&p.id, p.name.as_str()))
        .collect();

    let mut sorted_filters: Vec<&Filter> = filters.iter().filter(|f| !f.is_deleted).collect();
    sorted_filters.sort_by(|a, b| a.item_order.cmp(&b.item_order).then(a.id.cmp(&b.id)));
    for flt in sorted_filters {
        let count =
            filter_matching_items(active_items, &project_names, &flt.query, today_date).len();
        let color = (!flt.color.is_empty()).then(|| flt.color.clone());
        nodes.push(
            SidebarNode::filter_node(flt.id.clone(), flt.name.clone(), flt.query.clone(), count)
                .with_favorite(flt.is_favorite)
                .with_color(color),
        );
    }

    nodes
}

fn append_project_subtree<'a>(
    parent_id: Option<&'a Id>,
    depth: usize,
    children_by_parent: &HashMap<Option<&'a Id>, Vec<&'a Project>>,
    project_counts: &HashMap<&'a Id, usize>,
    out: &mut Vec<SidebarNode>,
    visited: &mut HashSet<&'a Id>,
) {
    let Some(children) = children_by_parent.get(&parent_id) else {
        return;
    };

    for proj in children {
        if !visited.insert(&proj.id) {
            continue;
        }
        let has_children = children_by_parent
            .get(&Some(&proj.id))
            .is_some_and(|c| !c.is_empty());
        let count = project_counts.get(&proj.id).copied().unwrap_or(0);
        let color = (!proj.color.is_empty()).then(|| proj.color.clone());
        out.push(
            SidebarNode::project(
                proj.id.clone(),
                proj.name.clone(),
                proj.parent_id.clone(),
                depth,
                false,
                has_children,
                count,
            )
            .with_favorite(proj.is_favorite)
            .with_color(color),
        );
        append_project_subtree(
            Some(&proj.id),
            depth + 1,
            children_by_parent,
            project_counts,
            out,
            visited,
        );
    }
}

fn build_task_rows(
    items: &[&Item],
    project_names: &HashMap<&Id, &str>,
    section_map: &HashMap<&Id, (&str, i64)>,
    today_date: &str,
    group_by_section: bool,
) -> Vec<TaskRow> {
    if items.is_empty() {
        return Vec::new();
    }

    // Group items by section key: None (ungrouped) first, then ordered sections.
    let mut section_groups: Vec<(Option<&Id>, i64, Vec<&Item>)> = Vec::new();

    if group_by_section {
        let mut by_sec: HashMap<Option<&Id>, Vec<&Item>> = HashMap::new();
        for item in items {
            let sec_key = item
                .section_id
                .as_ref()
                .filter(|sid| section_map.contains_key(sid));
            by_sec.entry(sec_key).or_default().push(*item);
        }

        if let Some(ungrouped) = by_sec.remove(&None) {
            section_groups.push((None, i64::MIN, ungrouped));
        }

        let mut sec_entries: Vec<(&Id, i64, Vec<&Item>)> = by_sec
            .into_iter()
            .filter_map(|(sid, group)| {
                let sid = sid?;
                let order = section_map.get(sid).map_or(i64::MAX, |(_, ord)| *ord);
                Some((sid, order, group))
            })
            .collect();

        sec_entries.sort_by(|(id_a, ord_a, _), (id_b, ord_b, _)| {
            ord_a.cmp(ord_b).then_with(|| id_a.cmp(id_b))
        });

        for (sid, order, group) in sec_entries {
            section_groups.push((Some(sid), order, group));
        }
    } else {
        section_groups.push((None, i64::MIN, items.to_vec()));
    }

    let mut out = Vec::with_capacity(items.len());
    for (_, _, group_items) in section_groups {
        append_ordered_task_group(
            &group_items,
            project_names,
            section_map,
            today_date,
            &mut out,
        );
    }
    out
}

fn append_ordered_task_group(
    group_items: &[&Item],
    project_names: &HashMap<&Id, &str>,
    section_map: &HashMap<&Id, (&str, i64)>,
    today_date: &str,
    out: &mut Vec<TaskRow>,
) {
    let group_ids: HashSet<&Id> = group_items.iter().map(|it| &it.id).collect();
    let mut children_by_parent: HashMap<Option<&Id>, Vec<&Item>> = HashMap::new();

    for item in group_items {
        let parent_key = item
            .parent_id
            .as_ref()
            .filter(|pid| group_ids.contains(pid));
        children_by_parent
            .entry(parent_key)
            .or_default()
            .push(*item);
    }

    for siblings in children_by_parent.values_mut() {
        siblings.sort_by(|a, b| a.child_order.cmp(&b.child_order).then(a.id.cmp(&b.id)));
    }

    let mut visited = HashSet::new();
    append_task_subtree(
        None,
        0,
        &children_by_parent,
        project_names,
        section_map,
        today_date,
        out,
        &mut visited,
    );
}

#[allow(clippy::too_many_arguments)]
fn append_task_subtree<'a>(
    parent_id: Option<&'a Id>,
    depth: usize,
    children_by_parent: &HashMap<Option<&'a Id>, Vec<&'a Item>>,
    project_names: &HashMap<&Id, &str>,
    section_map: &HashMap<&Id, (&str, i64)>,
    today_date: &str,
    out: &mut Vec<TaskRow>,
    visited: &mut HashSet<&'a Id>,
) {
    let Some(children) = children_by_parent.get(&parent_id) else {
        return;
    };

    for item in children {
        if !visited.insert(&item.id) {
            continue;
        }
        let project_name = project_names
            .get(&item.project_id)
            .map(|name| (*name).to_owned());
        let section_name = item
            .section_id
            .as_ref()
            .and_then(|sid| section_map.get(sid).map(|(name, _)| (*name).to_owned()));

        out.push(TaskRow {
            id: item.id.clone(),
            content: item.content.clone(),
            description: item.description.clone(),
            project_id: item.project_id.clone(),
            project_name,
            parent_id: item.parent_id.clone(),
            depth,
            priority: item.priority,
            is_overdue: is_overdue_due(item.due.as_ref(), today_date),
            due: item.due.clone(),
            deadline: item.deadline.clone(),
            labels: item.labels.clone(),
            section_id: item.section_id.clone(),
            section_name,
            child_order: i32::try_from(item.child_order).unwrap_or(i32::MAX),
            checked: item.checked,
        });

        append_task_subtree(
            Some(&item.id),
            depth + 1,
            children_by_parent,
            project_names,
            section_map,
            today_date,
            out,
            visited,
        );
    }
}

fn filter_matching_items<'a>(
    active_items: &[&'a Item],
    project_names: &HashMap<&Id, &str>,
    query: &str,
    today_date: &str,
) -> Vec<&'a Item> {
    let Some(today) = parse_naive_date(today_date) else {
        return Vec::new();
    };

    let filter_items: Vec<FilterItem> = active_items
        .iter()
        .map(|it| {
            // Todoist user-facing priority is inverted relative to Sync API integer:
            // API 4 -> p1, API 3 -> p2, API 2 -> p3, API 1 -> p4.
            let user_prio = 5u8.saturating_sub(it.priority.get());
            FilterItem {
                id: it.id.as_str().to_owned(),
                project: project_names
                    .get(&it.project_id)
                    .map(|name| (*name).to_owned()),
                labels: it.labels.clone(),
                priority: user_prio,
                due: it.due.as_ref().map(|d| d.date.clone()),
                is_completed: it.checked,
                is_deleted: it.is_deleted,
            }
        })
        .collect();

    match filters::resolve(query, &filter_items, today) {
        FilterResult::Ok(matching_ids) => {
            let id_set: HashSet<&str> = matching_ids.iter().map(String::as_str).collect();
            active_items
                .iter()
                .copied()
                .filter(|it| id_set.contains(it.id.as_str()))
                .collect()
        }
        FilterResult::Unsupported { .. } => Vec::new(),
    }
}

fn parse_naive_date(date_str: &str) -> Option<NaiveDateLike> {
    let prefix = date_str.get(..10)?;
    let mut parts = prefix.split('-');
    let year: i32 = parts.next()?.parse().ok()?;
    let month: u8 = parts.next()?.parse().ok()?;
    let day: u8 = parts.next()?.parse().ok()?;
    NaiveDateLike::from_ymd(year, month, day)
}

fn due_date_prefix(due: Option<&Due>) -> Option<&str> {
    let raw = due?.date.as_str();
    let prefix = raw.get(..10)?;
    let valid = prefix.bytes().enumerate().all(|(i, b)| match i {
        4 | 7 => b == b'-',
        _ => b.is_ascii_digit(),
    });
    valid.then_some(prefix)
}

fn is_overdue_due(due: Option<&Due>, today_date: &str) -> bool {
    let Some(due_prefix) = due_date_prefix(due) else {
        return false;
    };
    let today_prefix = today_date.get(..10).unwrap_or(today_date);
    due_prefix < today_prefix
}

fn is_due_on_or_before(due: Option<&Due>, today_date: &str) -> bool {
    let Some(due_prefix) = due_date_prefix(due) else {
        return false;
    };
    let today_prefix = today_date.get(..10).unwrap_or(today_date);
    due_prefix <= today_prefix
}

fn is_due_upcoming(due: Option<&Due>, today_date: &str, days: u32) -> bool {
    let Some(due_prefix) = due_date_prefix(due) else {
        return false;
    };
    let today_prefix = today_date.get(..10).unwrap_or(today_date);
    let Some(end_prefix) = add_days_iso(today_prefix, days) else {
        return false;
    };
    due_prefix > today_prefix && due_prefix <= end_prefix.as_str()
}

#[allow(clippy::similar_names)]
fn add_days_iso(date_str: &str, days: u32) -> Option<String> {
    let prefix = date_str.get(..10)?;
    let mut parts = prefix.split('-');
    let y: i64 = parts.next()?.parse().ok()?;
    let m: i64 = parts.next()?.parse().ok()?;
    let d: i64 = parts.next()?.parse().ok()?;
    if parts.next().is_some() || !(1..=12).contains(&m) || !(1..=31).contains(&d) {
        return None;
    }

    let y_adj = if m <= 2 { y - 1 } else { y };
    let era = if y_adj >= 0 { y_adj } else { y_adj - 399 } / 400;
    let yoe = y_adj - era * 400;
    let mp = if m > 2 { m - 3 } else { m + 9 };
    let doy = (153 * mp + 2) / 5 + d - 1;
    let doe = yoe * 365 + yoe / 4 - yoe / 100 + doy;
    let total_days = era * 146_097 + doe - 719_468 + i64::from(days);

    let z = total_days + 719_468;
    let era2 = if z >= 0 { z } else { z - 146_096 } / 146_097;
    let doe2 = z - era2 * 146_097;
    let yoe2 = (doe2 - doe2 / 1_460 + doe2 / 36_524 - doe2 / 146_096) / 365;
    let ny = yoe2 + era2 * 400;
    let doy2 = doe2 - (365 * yoe2 + yoe2 / 4 - yoe2 / 100);
    let mp2 = (5 * doy2 + 2) / 153;
    let nd = doy2 - (153 * mp2 + 2) / 5 + 1;
    let nm = if mp2 < 10 { mp2 + 3 } else { mp2 - 9 };
    let ny = if nm <= 2 { ny + 1 } else { ny };

    Some(format!("{ny:04}-{nm:02}-{nd:02}"))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::model::SidebarKind;

    #[test]
    fn sample_sidebar_nesting_depth_and_order_are_stable_and_deterministic() {
        let data = FixtureData::sample();
        let first = data.sidebar().expect("sidebar should succeed");
        let second = data.sidebar().expect("sidebar should be deterministic");
        assert_eq!(first, second);

        let summary: Vec<(&str, usize, bool)> = first
            .iter()
            .map(|n| (n.title(), n.depth(), n.has_children))
            .collect();

        assert_eq!(
            summary,
            vec![
                ("Inbox", 0, false),
                ("Today", 0, false),
                ("Upcoming", 0, false),
                ("Home", 0, true),
                ("Kitchen", 1, false),
                ("Work", 0, true),
                ("Q2 Launch", 1, true),
                ("Backend", 2, false),
                ("errand", 0, false),
                ("home", 0, false),
                ("deep-work", 0, false),
                ("urgent", 0, false),
                ("Overdue & Home", 0, false),
                ("High Priority", 0, false),
            ]
        );

        assert!(matches!(
            first[3].kind,
            SidebarKind::Project { depth: 0, .. }
        ));
        assert!(matches!(
            first[4].kind,
            SidebarKind::Project { depth: 1, .. }
        ));
        assert!(matches!(
            first[6].kind,
            SidebarKind::Project { depth: 1, .. }
        ));
        assert!(matches!(
            first[7].kind,
            SidebarKind::Project { depth: 2, .. }
        ));
    }

    #[test]
    fn sample_inbox_tasks_preserve_section_grouping_and_subtask_indentation() {
        let data = FixtureData::sample();
        let rows = data.tasks(&ViewId::Inbox).expect("inbox tasks should load");
        let rows_again = data
            .tasks(&ViewId::Inbox)
            .expect("inbox tasks should be deterministic");
        assert_eq!(rows, rows_again);

        let shape: Vec<(&str, Option<&str>, usize, u8, bool)> = rows
            .iter()
            .map(|r| {
                (
                    r.id.as_str(),
                    r.section_name.as_deref(),
                    r.depth,
                    r.priority.get(),
                    r.is_overdue,
                )
            })
            .collect();

        assert_eq!(
            shape,
            vec![
                // Ungrouped first, with nested sub-tasks indented at depth 1 and 2
                ("6X4VfakeItm001", None, 0, 4, true),
                ("6X4VfakeItm002", None, 1, 3, false),
                ("6X4VfakeItm003", None, 2, 2, false),
                // Then Section "Errands" (section_order = 1)
                ("6X4VfakeItm004", Some("Errands"), 0, 2, false),
                ("6X4VfakeItm005", Some("Errands"), 0, 1, false),
                // Then Section "Planning" (section_order = 2)
                ("6X4VfakeItm006", Some("Planning"), 0, 3, false),
            ]
        );
    }

    #[test]
    fn sample_detail_includes_description_deadline_labels_and_comments() {
        let data = FixtureData::sample();
        let detail = data
            .detail(&Id::new("6X4VfakeItm001"))
            .expect("detail query should succeed")
            .expect("task 001 should exist");

        assert_eq!(detail.content, "Finalize quarterly roadmap");
        assert!(detail.description.contains("Q2 launch milestones"));
        assert!(detail.is_overdue);
        assert_eq!(
            detail.deadline.as_ref().map(|d| d.date.as_str()),
            Some("2024-05-05")
        );
        assert_eq!(detail.labels, vec!["deep-work", "urgent"]);
        assert_eq!(detail.comments.len(), 2);
        assert_eq!(detail.comments[0].id.as_str(), "6X4VfakeNot001");
        assert_eq!(detail.comments[1].id.as_str(), "6X4VfakeNot002");

        let missing = data
            .detail(&Id::new("6X4VfakeItmNonexistent"))
            .expect("missing detail should not error");
        assert_eq!(missing, None);
    }

    #[test]
    fn sample_today_upcoming_label_and_filter_views_resolve_expected_tasks() {
        let data = FixtureData::sample();

        let today_rows = data.tasks(&ViewId::Today).unwrap();
        let today_ids: Vec<&str> = today_rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            today_ids,
            vec![
                "6X4VfakeItm001",
                "6X4VfakeItm002",
                "6X4VfakeItm004",
                "6X4VfakeItm007",
                "6X4VfakeItm009",
            ]
        );

        let upcoming_rows = data.tasks(&ViewId::Upcoming).unwrap();
        let upcoming_ids: Vec<&str> = upcoming_rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            upcoming_ids,
            vec![
                "6X4VfakeItm003",
                "6X4VfakeItm006",
                "6X4VfakeItm008",
                "6X4VfakeItm010",
                "6X4VfakeItm005",
            ]
        );

        let home_label_rows = data.tasks(&ViewId::Label(String::from("home"))).unwrap();
        let home_label_ids: Vec<&str> = home_label_rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(
            home_label_ids,
            vec!["6X4VfakeItm004", "6X4VfakeItm007", "6X4VfakeItm008"]
        );

        let filter_rows = data
            .tasks(&ViewId::Filter(Id::new("6X4VfakeFltOverdueHome")))
            .unwrap();
        let filter_ids: Vec<&str> = filter_rows.iter().map(|r| r.id.as_str()).collect();
        assert_eq!(filter_ids, vec!["6X4VfakeItm007"]);
    }

    #[test]
    fn generated_fixture_produces_exact_count_deterministically() {
        let gen_a = FixtureData::generated(120);
        let gen_b = FixtureData::generated(120);
        assert_eq!(gen_a.items.len(), 120);
        assert_eq!(gen_a.items, gen_b.items);

        let inbox_tasks = gen_a.tasks(&ViewId::Inbox).unwrap();
        assert_ne!(inbox_tasks, Vec::<TaskRow>::new());
    }
}
