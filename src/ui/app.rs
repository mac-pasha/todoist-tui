//! Application state and pure navigation reducer for the TUI shell (T-606, EPIC-07).
//!
//! [`App`] owns the UI focus (`Sidebar | List | Detail`), current selection
//! indices, collapsed project set, and modal flags (`show_help`,
//! `search_active`, `should_quit`, `sync_triggered`). [`App::apply`] is a pure
//! state reducer over [`Action`] + [`ViewData`] with zero terminal or network
//! I/O so navigation sequences can be unit-tested headlessly.

#![allow(dead_code)]

use std::collections::HashSet;

use crossterm::event::{KeyCode, KeyEvent};

use crate::api::model::Id;
use crate::config::DefaultView;
use crate::error::Result;

use super::data::ViewData;
use super::keymap::{Action, Keymap};
use super::model::{SidebarKind, SidebarNode, TaskDetail, TaskRow, ViewId};

/// Focusable pane in the three-pane TUI layout.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum Pane {
    /// Left navigation sidebar (Inbox, Today, Upcoming, projects, labels, filters).
    #[default]
    Sidebar,
    /// Middle task list for the active sidebar view.
    List,
    /// Right task detail and comments view for the selected task.
    Detail,
}

/// Complete state of the TUI shell.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone)]
pub struct App {
    /// Currently focused pane.
    pub focus: Pane,
    /// Selected row index within [`App::sidebar_nodes`].
    pub sidebar_index: usize,
    /// Selected row index within [`App::tasks`].
    pub list_index: usize,
    /// Vertical scroll offset within the detail pane.
    pub detail_scroll: usize,
    /// Set of project [`Id`]s whose child subtrees are collapsed in the sidebar.
    pub collapsed_projects: HashSet<Id>,
    /// Whether the `?` help overlay is currently visible.
    pub show_help: bool,
    /// Whether the `/` search prompt is currently active.
    pub search_active: bool,
    /// Whether the user has requested to quit the application (`q`).
    pub should_quit: bool,
    /// Whether an immediate manual sync (`r`) was requested.
    pub sync_triggered: bool,
    all_sidebar_nodes: Vec<SidebarNode>,
    visible_sidebar_nodes: Vec<SidebarNode>,
    task_rows: Vec<TaskRow>,
    current_detail: Option<TaskDetail>,
    active_view: ViewId,
}

impl App {
    /// Initialises [`App`] from `data` with [`DefaultView::Inbox`] selected.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by `data.sidebar()`, `data.tasks()`, or
    /// `data.detail()`.
    pub fn new(data: &dyn ViewData) -> Result<Self> {
        Self::with_default_view(data, DefaultView::Inbox)
    }

    /// Initialises [`App`] from `data` starting on `default_view`.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by `data.sidebar()`, `data.tasks()`, or
    /// `data.detail()`.
    pub fn with_default_view(data: &dyn ViewData, default_view: DefaultView) -> Result<Self> {
        let all_sidebar_nodes = data.sidebar()?;
        let collapsed_projects = HashSet::new();
        let visible_sidebar_nodes =
            compute_visible_sidebar(&all_sidebar_nodes, &collapsed_projects);

        let target_view = match default_view {
            DefaultView::Inbox => ViewId::Inbox,
            DefaultView::Today => ViewId::Today,
            DefaultView::Upcoming => ViewId::Upcoming,
        };

        let sidebar_index = visible_sidebar_nodes
            .iter()
            .position(|n| n.id == target_view)
            .unwrap_or(0);

        let active_view = visible_sidebar_nodes
            .get(sidebar_index)
            .map_or(target_view, SidebarNode::view_id);

        let task_rows = data.tasks(&active_view)?;
        let current_detail = match task_rows.first() {
            Some(first) => data.detail(&first.id)?,
            None => None,
        };

        Ok(Self {
            focus: Pane::Sidebar,
            sidebar_index,
            list_index: 0,
            detail_scroll: 0,
            collapsed_projects,
            show_help: false,
            search_active: false,
            should_quit: false,
            sync_triggered: false,
            all_sidebar_nodes,
            visible_sidebar_nodes,
            task_rows,
            current_detail,
            active_view,
        })
    }

    /// Returns the currently focused [`Pane`].
    #[must_use]
    pub const fn focus(&self) -> Pane {
        self.focus
    }

    /// Returns the visible sidebar nodes (with descendants of collapsed
    /// projects filtered out and collapsed project nodes marked
    /// `collapsed = true`).
    #[must_use]
    pub fn sidebar_nodes(&self) -> &[SidebarNode] {
        &self.visible_sidebar_nodes
    }

    /// Returns the unfiltered sidebar nodes as provided by [`ViewData::sidebar`].
    #[must_use]
    pub fn all_sidebar_nodes(&self) -> &[SidebarNode] {
        &self.all_sidebar_nodes
    }

    /// Returns the currently selected [`SidebarNode`], if any.
    #[must_use]
    pub fn selected_sidebar_node(&self) -> Option<&SidebarNode> {
        self.visible_sidebar_nodes.get(self.sidebar_index)
    }

    /// Returns the active [`ViewId`].
    #[must_use]
    pub const fn active_view(&self) -> &ViewId {
        &self.active_view
    }

    /// Returns the task rows for the active view.
    #[must_use]
    pub fn tasks(&self) -> &[TaskRow] {
        &self.task_rows
    }

    /// Returns the currently selected [`TaskRow`] in the task list, if any.
    #[must_use]
    pub fn selected_task(&self) -> Option<&TaskRow> {
        self.task_rows.get(self.list_index)
    }

    /// Returns the [`TaskDetail`] for the currently selected task, or `None`
    /// when the task list is empty.
    #[must_use]
    pub const fn detail(&self) -> Option<&TaskDetail> {
        self.current_detail.as_ref()
    }

    /// Returns the current sidebar selection index.
    #[must_use]
    pub const fn sidebar_index(&self) -> usize {
        self.sidebar_index
    }

    /// Returns the current task list selection index.
    #[must_use]
    pub const fn list_index(&self) -> usize {
        self.list_index
    }

    /// Returns the current vertical scroll offset in the detail pane.
    #[must_use]
    pub const fn detail_scroll(&self) -> usize {
        self.detail_scroll
    }

    /// Returns `true` if `project_id` is currently collapsed in the sidebar.
    #[must_use]
    pub fn is_project_collapsed(&self, project_id: &Id) -> bool {
        self.collapsed_projects.contains(project_id)
    }

    /// Dismisses the help overlay if it is open, returning `true` if it was open.
    pub fn dismiss_help(&mut self) -> bool {
        let was_open = self.show_help;
        self.show_help = false;
        was_open
    }

    /// Clears and returns the `sync_triggered` flag.
    pub fn take_sync_triggered(&mut self) -> bool {
        std::mem::take(&mut self.sync_triggered)
    }

    /// Selects `view` in the sidebar (expanding collapsed project ancestors if
    /// needed) and optionally selects `task_id` in the task list.
    ///
    /// # Errors
    ///
    /// Propagates errors returned by `data.tasks()` or `data.detail()`.
    pub fn select_view_and_task(
        &mut self,
        view: &ViewId,
        task_id: Option<&Id>,
        data: &dyn ViewData,
    ) -> Result<()> {
        if let Some(pos) = self
            .visible_sidebar_nodes
            .iter()
            .position(|n| &n.view_id() == view)
        {
            self.set_sidebar_index(pos, data)?;
        } else if matches!(view, ViewId::Project(_)) {
            self.collapsed_projects.clear();
            self.recompute_visible_sidebar();
            if let Some(pos) = self
                .visible_sidebar_nodes
                .iter()
                .position(|n| &n.view_id() == view)
            {
                self.set_sidebar_index(pos, data)?;
            }
        }

        if let Some(tid) = task_id {
            if let Some(task_pos) = self.task_rows.iter().position(|t| &t.id == tid) {
                self.set_list_index(task_pos, data)?;
            }
        }

        Ok(())
    }

    /// Reloads sidebar, task list, and detail pane from `data`, clamping
    /// selection indices into the new bounds.
    ///
    /// # Errors
    ///
    /// Propagates any error returned by `data`.
    pub fn refresh(&mut self, data: &dyn ViewData) -> Result<()> {
        self.all_sidebar_nodes = data.sidebar()?;
        self.recompute_visible_sidebar();
        if let Some(node) = self.visible_sidebar_nodes.get(self.sidebar_index) {
            self.active_view = node.view_id();
        }
        self.task_rows = data.tasks(&self.active_view)?;
        if self.task_rows.is_empty() {
            self.list_index = 0;
            self.current_detail = None;
        } else {
            let max_idx = self.task_rows.len() - 1;
            if self.list_index > max_idx {
                self.list_index = max_idx;
            }
            self.reload_detail(data)?;
        }
        Ok(())
    }

    /// Applies a single user [`Action`] against the current state and `data`.
    ///
    /// # Errors
    ///
    /// Propagates any error from `data.tasks()` or `data.detail()` when a
    /// selection change triggers a view or detail reload.
    pub fn apply(&mut self, action: Action, data: &dyn ViewData) -> Result<()> {
        match action {
            Action::ToggleHelp => {
                self.show_help = !self.show_help;
                return Ok(());
            }
            Action::OpenSearch => {
                self.show_help = false;
                self.search_active = true;
                return Ok(());
            }
            Action::Quit => {
                self.show_help = false;
                self.should_quit = true;
                return Ok(());
            }
            Action::SyncNow => {
                self.show_help = false;
                self.sync_triggered = true;
                return Ok(());
            }
            _ => {
                if self.show_help {
                    self.show_help = false;
                    return Ok(());
                }
            }
        }

        match action {
            Action::Down => self.move_down(data)?,
            Action::Up => self.move_up(data)?,
            Action::First => self.move_first(data)?,
            Action::Last => self.move_last(data)?,
            Action::Left => self.move_left(data)?,
            Action::Right => self.move_right(data)?,
            _ => {}
        }

        Ok(())
    }

    fn move_down(&mut self, data: &dyn ViewData) -> Result<()> {
        match self.focus {
            Pane::Sidebar => {
                if self.visible_sidebar_nodes.is_empty() {
                    return Ok(());
                }
                let max_idx = self.visible_sidebar_nodes.len() - 1;
                let next_idx = (self.sidebar_index + 1).min(max_idx);
                self.set_sidebar_index(next_idx, data)?;
            }
            Pane::List => {
                if self.task_rows.is_empty() {
                    return Ok(());
                }
                let max_idx = self.task_rows.len() - 1;
                let next_idx = (self.list_index + 1).min(max_idx);
                self.set_list_index(next_idx, data)?;
            }
            Pane::Detail => {
                self.detail_scroll = self.detail_scroll.saturating_add(1);
            }
        }
        Ok(())
    }

    fn move_up(&mut self, data: &dyn ViewData) -> Result<()> {
        match self.focus {
            Pane::Sidebar => {
                let next_idx = self.sidebar_index.saturating_sub(1);
                self.set_sidebar_index(next_idx, data)?;
            }
            Pane::List => {
                if self.task_rows.is_empty() {
                    return Ok(());
                }
                let next_idx = self.list_index.saturating_sub(1);
                self.set_list_index(next_idx, data)?;
            }
            Pane::Detail => {
                self.detail_scroll = self.detail_scroll.saturating_sub(1);
            }
        }
        Ok(())
    }

    fn move_first(&mut self, data: &dyn ViewData) -> Result<()> {
        match self.focus {
            Pane::Sidebar => {
                self.set_sidebar_index(0, data)?;
            }
            Pane::List => {
                if !self.task_rows.is_empty() {
                    self.set_list_index(0, data)?;
                }
            }
            Pane::Detail => {
                self.detail_scroll = 0;
            }
        }
        Ok(())
    }

    fn move_last(&mut self, data: &dyn ViewData) -> Result<()> {
        match self.focus {
            Pane::Sidebar => {
                if !self.visible_sidebar_nodes.is_empty() {
                    let last = self.visible_sidebar_nodes.len() - 1;
                    self.set_sidebar_index(last, data)?;
                }
            }
            Pane::List => {
                if !self.task_rows.is_empty() {
                    let last = self.task_rows.len() - 1;
                    self.set_list_index(last, data)?;
                }
            }
            Pane::Detail => {
                self.detail_scroll = usize::MAX;
            }
        }
        Ok(())
    }

    fn move_left(&mut self, data: &dyn ViewData) -> Result<()> {
        match self.focus {
            Pane::Sidebar => {
                if let Some(proj_id) = self
                    .selected_sidebar_node()
                    .and_then(SidebarNode::project_id)
                    .cloned()
                {
                    if self.collapsed_projects.insert(proj_id) {
                        let prev_view = self.active_view.clone();
                        self.recompute_visible_sidebar();
                        if self.active_view != prev_view {
                            self.reload_tasks_and_detail(data)?;
                        }
                    }
                }
            }
            Pane::List => {
                self.focus = Pane::Sidebar;
            }
            Pane::Detail => {
                self.focus = Pane::List;
            }
        }
        Ok(())
    }

    fn move_right(&mut self, data: &dyn ViewData) -> Result<()> {
        match self.focus {
            Pane::Sidebar => {
                if let Some(proj_id) = self
                    .selected_sidebar_node()
                    .and_then(SidebarNode::project_id)
                    .cloned()
                {
                    if self.collapsed_projects.remove(&proj_id) {
                        let prev_view = self.active_view.clone();
                        self.recompute_visible_sidebar();
                        if self.active_view != prev_view {
                            self.reload_tasks_and_detail(data)?;
                        }
                        return Ok(());
                    }
                }
                self.focus = Pane::List;
            }
            Pane::List | Pane::Detail => {
                self.focus = Pane::Detail;
            }
        }
        Ok(())
    }

    fn set_sidebar_index(&mut self, new_index: usize, data: &dyn ViewData) -> Result<()> {
        if self.visible_sidebar_nodes.is_empty() {
            return Ok(());
        }
        let clamped = new_index.min(self.visible_sidebar_nodes.len() - 1);
        if clamped == self.sidebar_index {
            return Ok(());
        }
        self.sidebar_index = clamped;
        self.active_view = self.visible_sidebar_nodes[clamped].view_id();
        self.reload_tasks_and_detail(data)
    }

    fn set_list_index(&mut self, new_index: usize, data: &dyn ViewData) -> Result<()> {
        if self.task_rows.is_empty() {
            self.list_index = 0;
            self.current_detail = None;
            return Ok(());
        }
        let clamped = new_index.min(self.task_rows.len() - 1);
        if clamped == self.list_index {
            return Ok(());
        }
        self.list_index = clamped;
        self.detail_scroll = 0;
        self.reload_detail(data)
    }

    fn reload_tasks_and_detail(&mut self, data: &dyn ViewData) -> Result<()> {
        self.task_rows = data.tasks(&self.active_view)?;
        self.list_index = 0;
        self.detail_scroll = 0;
        self.reload_detail(data)
    }

    fn reload_detail(&mut self, data: &dyn ViewData) -> Result<()> {
        self.current_detail = match self.task_rows.get(self.list_index) {
            Some(row) => data.detail(&row.id)?,
            None => None,
        };
        Ok(())
    }

    fn recompute_visible_sidebar(&mut self) {
        let current_view = self
            .visible_sidebar_nodes
            .get(self.sidebar_index)
            .map(SidebarNode::view_id);

        self.visible_sidebar_nodes =
            compute_visible_sidebar(&self.all_sidebar_nodes, &self.collapsed_projects);

        if self.visible_sidebar_nodes.is_empty() {
            self.sidebar_index = 0;
            return;
        }

        if let Some(view) = current_view {
            if let Some(pos) = self.visible_sidebar_nodes.iter().position(|n| n.id == view) {
                self.sidebar_index = pos;
                self.active_view = self.visible_sidebar_nodes[pos].view_id();
                return;
            }
        }

        let max_idx = self.visible_sidebar_nodes.len() - 1;
        if self.sidebar_index > max_idx {
            self.sidebar_index = max_idx;
        }
        self.active_view = self.visible_sidebar_nodes[self.sidebar_index].view_id();
    }
}

fn compute_visible_sidebar(
    all_nodes: &[SidebarNode],
    collapsed_projects: &HashSet<Id>,
) -> Vec<SidebarNode> {
    let mut visible = Vec::with_capacity(all_nodes.len());
    let mut collapsed_ancestor_depth: Option<usize> = None;

    for node in all_nodes {
        if let SidebarKind::Project { id, depth, .. } = &node.kind {
            if let Some(anc_depth) = collapsed_ancestor_depth {
                if *depth > anc_depth {
                    continue;
                }
                collapsed_ancestor_depth = None;
            }
            let is_collapsed = collapsed_projects.contains(id);
            if is_collapsed {
                collapsed_ancestor_depth = Some(*depth);
            }
            visible.push(node.clone().with_collapsed(is_collapsed));
        } else {
            collapsed_ancestor_depth = None;
            visible.push(node.clone());
        }
    }

    visible
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::ui::data::FixtureData;

    #[test]
    fn key_sequence_j_j_k_cap_g_g_l_h_updates_selection_and_focus_at_each_step() {
        let data = FixtureData::sample();
        let mut app = App::new(&data).expect("app init should succeed");

        assert_eq!(app.focus(), Pane::Sidebar);
        assert_eq!(app.sidebar_index(), 0);
        assert_eq!(app.active_view(), &ViewId::Inbox);

        // 1. `j` -> Down to row 1 (Today)
        app.apply(Action::Down, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        assert_eq!(app.sidebar_index(), 1);
        assert_eq!(app.active_view(), &ViewId::Today);

        // 2. `j` -> Down to row 2 (Upcoming)
        app.apply(Action::Down, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        assert_eq!(app.sidebar_index(), 2);
        assert_eq!(app.active_view(), &ViewId::Upcoming);

        // 3. `k` -> Up to row 1 (Today)
        app.apply(Action::Up, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        assert_eq!(app.sidebar_index(), 1);
        assert_eq!(app.active_view(), &ViewId::Today);

        // 4. `G` -> Last row in sidebar
        let last_sidebar_idx = app.sidebar_nodes().len() - 1;
        app.apply(Action::Last, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        assert_eq!(app.sidebar_index(), last_sidebar_idx);
        assert_eq!(
            app.active_view(),
            &ViewId::Filter(Id::new("6X4VfakeFltHighPrio"))
        );

        // 5. `g` -> First row in sidebar (Inbox)
        app.apply(Action::First, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        assert_eq!(app.sidebar_index(), 0);
        assert_eq!(app.active_view(), &ViewId::Inbox);

        // 6. `l` -> Right moves focus from Sidebar to List
        app.apply(Action::Right, &data).unwrap();
        assert_eq!(app.focus(), Pane::List);
        assert_eq!(app.list_index(), 0);

        // 7. `h` -> Left moves focus from List back to Sidebar
        app.apply(Action::Left, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        assert_eq!(app.sidebar_index(), 0);
    }

    #[test]
    fn down_and_up_clamp_at_bounds_without_wrapping_in_all_panes() {
        let data = FixtureData::sample();
        let mut app = App::new(&data).unwrap();

        // Clamping at top of Sidebar
        app.apply(Action::Up, &data).unwrap();
        assert_eq!(app.sidebar_index(), 0);

        // Clamping at bottom of Sidebar
        let sidebar_len = app.sidebar_nodes().len();
        app.apply(Action::Last, &data).unwrap();
        assert_eq!(app.sidebar_index(), sidebar_len - 1);
        app.apply(Action::Down, &data).unwrap();
        assert_eq!(app.sidebar_index(), sidebar_len - 1);

        // Return to Inbox and move focus to List
        app.apply(Action::First, &data).unwrap();
        app.apply(Action::Right, &data).unwrap();
        assert_eq!(app.focus(), Pane::List);

        // Clamping at top of List
        app.apply(Action::Up, &data).unwrap();
        assert_eq!(app.list_index(), 0);

        // Move through List and clamp at bottom
        let list_len = app.tasks().len();
        assert!(list_len > 2);
        app.apply(Action::Down, &data).unwrap();
        assert_eq!(app.list_index(), 1);
        app.apply(Action::Last, &data).unwrap();
        assert_eq!(app.list_index(), list_len - 1);
        app.apply(Action::Down, &data).unwrap();
        assert_eq!(app.list_index(), list_len - 1);
        app.apply(Action::First, &data).unwrap();
        assert_eq!(app.list_index(), 0);

        // Focus Detail pane and test clamping of Right/Left and scroll
        app.apply(Action::Right, &data).unwrap();
        assert_eq!(app.focus(), Pane::Detail);
        app.apply(Action::Right, &data).unwrap();
        assert_eq!(app.focus(), Pane::Detail);
        app.apply(Action::Up, &data).unwrap();
        assert_eq!(app.detail_scroll(), 0);
        app.apply(Action::Down, &data).unwrap();
        assert_eq!(app.detail_scroll(), 1);
        app.apply(Action::Left, &data).unwrap();
        assert_eq!(app.focus(), Pane::List);
    }

    #[test]
    fn changing_sidebar_selection_resets_list_index_to_zero_and_updates_detail() {
        let data = FixtureData::sample();
        let mut app = App::new(&data).unwrap();

        // Move into List and select task index 2
        app.apply(Action::Right, &data).unwrap();
        app.apply(Action::Down, &data).unwrap();
        app.apply(Action::Down, &data).unwrap();
        assert_eq!(app.list_index(), 2);
        assert_eq!(app.detail().map(|d| d.id.as_str()), Some("6X4VfakeItm003"));

        // Return to Sidebar and move down to Today
        app.apply(Action::Left, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        app.apply(Action::Down, &data).unwrap();
        assert_eq!(app.active_view(), &ViewId::Today);
        assert_eq!(app.list_index(), 0);

        let expected_first_today = app.tasks()[0].id.clone();
        assert_eq!(app.detail().map(|d| &d.id), Some(&expected_first_today));
    }

    #[test]
    fn left_and_right_on_project_node_collapse_and_expand_subtree() {
        let data = FixtureData::sample();
        let mut app = App::new(&data).unwrap();

        let initial_len = app.sidebar_nodes().len();
        // Navigate to "Work" project (index 5: Inbox=0, Today=1, Upcoming=2, Home=3, Kitchen=4, Work=5)
        for _ in 0..5 {
            app.apply(Action::Down, &data).unwrap();
        }
        let work_id = Id::new("6X4VfakePrjWork");
        assert_eq!(
            app.selected_sidebar_node()
                .and_then(SidebarNode::project_id),
            Some(&work_id)
        );
        assert!(!app.is_project_collapsed(&work_id));

        // Press Left (`h`) on "Work": collapses "Q2 Launch" and "Backend" (2 descendants)
        app.apply(Action::Left, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        assert!(app.is_project_collapsed(&work_id));
        assert!(app.selected_sidebar_node().unwrap().is_collapsed());
        assert_eq!(app.sidebar_nodes().len(), initial_len - 2);
        assert!(!app
            .sidebar_nodes()
            .iter()
            .any(|n| n.title() == "Q2 Launch" || n.title() == "Backend"));

        // Press Right (`l`) on collapsed "Work": expands subtree, stays in Sidebar
        app.apply(Action::Right, &data).unwrap();
        assert_eq!(app.focus(), Pane::Sidebar);
        assert!(!app.is_project_collapsed(&work_id));
        assert!(!app.selected_sidebar_node().unwrap().is_collapsed());
        assert_eq!(app.sidebar_nodes().len(), initial_len);

        // Press Right (`l`) again on expanded "Work": moves focus to List
        app.apply(Action::Right, &data).unwrap();
        assert_eq!(app.focus(), Pane::List);
    }

    #[test]
    fn toggle_help_open_search_sync_now_and_quit_update_flags() {
        let data = FixtureData::sample();
        let mut app = App::new(&data).unwrap();

        assert!(!app.show_help);
        app.apply(Action::ToggleHelp, &data).unwrap();
        assert!(app.show_help);
        app.apply(Action::ToggleHelp, &data).unwrap();
        assert!(!app.show_help);

        assert!(!app.search_active);
        app.apply(Action::OpenSearch, &data).unwrap();
        assert!(app.search_active);

        assert!(!app.sync_triggered);
        app.apply(Action::SyncNow, &data).unwrap();
        assert!(app.sync_triggered);
        assert!(app.take_sync_triggered());
        assert!(!app.sync_triggered);

        assert!(!app.should_quit);
        app.apply(Action::Quit, &data).unwrap();
        assert!(app.should_quit);
    }

    struct EmptyViewData {
        inner: FixtureData,
    }

    impl ViewData for EmptyViewData {
        fn sidebar(&self) -> Result<Vec<SidebarNode>> {
            self.inner.sidebar()
        }

        fn tasks(&self, _view: &ViewId) -> Result<Vec<TaskRow>> {
            Ok(Vec::new())
        }

        fn detail(&self, _task_id: &Id) -> Result<Option<TaskDetail>> {
            Ok(None)
        }
    }

    #[test]
    fn empty_task_list_navigation_is_noop_and_detail_is_none() {
        let empty_data = EmptyViewData {
            inner: FixtureData::sample(),
        };
        let mut app = App::new(&empty_data).unwrap();

        assert_eq!(app.tasks(), Vec::<TaskRow>::new());
        assert_eq!(app.selected_task(), None);
        assert_eq!(app.detail(), None);

        app.apply(Action::Right, &empty_data).unwrap();
        assert_eq!(app.focus(), Pane::List);

        for action in [Action::Down, Action::Up, Action::Last, Action::First] {
            app.apply(action, &empty_data).unwrap();
            assert_eq!(app.list_index(), 0);
            assert_eq!(app.detail(), None);
        }
    }
}
