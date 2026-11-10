//! UI read-model view types rendered by the TUI widgets (T-605, EPIC-07).
//!
//! These structs decouple the ratatui widgets (`src/ui/view/`) and navigation
//! reducer (`src/ui/app.rs`) from raw Sync API wire shapes and SQLite tables.

#![allow(dead_code)]

use crate::api::model::{Deadline, Due, Id, Priority};

/// Logical view identifier selected in the left sidebar.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Default)]
pub enum ViewId {
    /// Built-in Inbox view.
    #[default]
    Inbox,
    /// Built-in Today view (overdue + due today).
    Today,
    /// Built-in Upcoming view (due within the next 7 days).
    Upcoming,
    /// A specific project view by project [`Id`].
    Project(Id),
    /// A personal label view by label name.
    Label(String),
    /// A saved filter view by filter [`Id`].
    Filter(Id),
}

impl ViewId {
    /// Returns the project [`Id`] when this view is [`ViewId::Project`].
    #[must_use]
    pub const fn as_project_id(&self) -> Option<&Id> {
        match self {
            Self::Project(id) => Some(id),
            _ => None,
        }
    }
}

/// Discriminant carrying kind-specific metadata for a [`SidebarNode`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SidebarKind {
    /// Built-in Inbox entry.
    Inbox,
    /// Built-in Today entry.
    Today,
    /// Built-in Upcoming entry.
    Upcoming,
    /// User project node within the hierarchical project tree.
    Project {
        /// Unique project identifier.
        id: Id,
        /// Optional parent project identifier (`None` for root projects).
        parent_id: Option<Id>,
        /// Nesting depth in the project tree (`0` for root projects).
        depth: usize,
        /// Whether this project's child subtree is currently collapsed.
        collapsed: bool,
        /// Whether this project has one or more child sub-projects.
        has_children: bool,
    },
    /// Personal label entry.
    Label {
        /// Unique label identifier.
        id: Id,
        /// Label display/lookup name.
        name: String,
    },
    /// Saved Todoist filter entry.
    Filter {
        /// Unique filter identifier.
        id: Id,
        /// Raw Todoist filter query expression.
        query: String,
    },
}

/// Type alias for [`SidebarKind`] used by callers referring to `SidebarNodeKind`.
pub type SidebarNodeKind = SidebarKind;

/// A single selectable row in the left sidebar.
#[allow(clippy::struct_excessive_bools)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SidebarNode {
    /// Target [`ViewId`] activated when this sidebar node is selected.
    pub id: ViewId,
    /// Display title rendered in the sidebar row.
    pub label: String,
    /// Kind-specific metadata (project hierarchy, label name, filter query).
    pub kind: SidebarKind,
    /// Indentation depth (`0` for top-level entries, `1+` for sub-projects).
    pub depth: usize,
    /// Whether this node's children are currently collapsed.
    pub collapsed: bool,
    /// Whether this node has child nodes in the project tree.
    pub has_children: bool,
    /// Number of active tasks associated with this entry.
    pub task_count: usize,
    /// Whether the underlying project/label/filter is marked as a favourite.
    pub is_favorite: bool,
    /// Optional Todoist colour name (e.g. `"berry_red"`, `"blue"`).
    pub color: Option<String>,
}

impl SidebarNode {
    /// Constructs the built-in Inbox sidebar node.
    #[must_use]
    pub fn inbox(task_count: usize) -> Self {
        Self {
            id: ViewId::Inbox,
            label: String::from("Inbox"),
            kind: SidebarKind::Inbox,
            depth: 0,
            collapsed: false,
            has_children: false,
            task_count,
            is_favorite: false,
            color: None,
        }
    }

    /// Constructs the built-in Today sidebar node.
    #[must_use]
    pub fn today(task_count: usize) -> Self {
        Self {
            id: ViewId::Today,
            label: String::from("Today"),
            kind: SidebarKind::Today,
            depth: 0,
            collapsed: false,
            has_children: false,
            task_count,
            is_favorite: false,
            color: None,
        }
    }

    /// Constructs the built-in Upcoming sidebar node.
    #[must_use]
    pub fn upcoming(task_count: usize) -> Self {
        Self {
            id: ViewId::Upcoming,
            label: String::from("Upcoming"),
            kind: SidebarKind::Upcoming,
            depth: 0,
            collapsed: false,
            has_children: false,
            task_count,
            is_favorite: false,
            color: None,
        }
    }

    /// Constructs a project tree sidebar node.
    #[must_use]
    pub fn project(
        id: Id,
        label: impl Into<String>,
        parent_id: Option<Id>,
        depth: usize,
        collapsed: bool,
        has_children: bool,
        task_count: usize,
    ) -> Self {
        let view_id = ViewId::Project(id.clone());
        Self {
            id: view_id,
            label: label.into(),
            kind: SidebarKind::Project {
                id,
                parent_id,
                depth,
                collapsed,
                has_children,
            },
            depth,
            collapsed,
            has_children,
            task_count,
            is_favorite: false,
            color: None,
        }
    }

    /// Constructs a personal label sidebar node.
    #[must_use]
    pub fn label_node(id: Id, name: impl Into<String>, task_count: usize) -> Self {
        let name_str = name.into();
        Self {
            id: ViewId::Label(name_str.clone()),
            label: name_str.clone(),
            kind: SidebarKind::Label { id, name: name_str },
            depth: 0,
            collapsed: false,
            has_children: false,
            task_count,
            is_favorite: false,
            color: None,
        }
    }

    /// Constructs a saved filter sidebar node.
    #[must_use]
    pub fn filter_node(
        id: Id,
        label: impl Into<String>,
        query: impl Into<String>,
        task_count: usize,
    ) -> Self {
        let view_id = ViewId::Filter(id.clone());
        Self {
            id: view_id,
            label: label.into(),
            kind: SidebarKind::Filter {
                id,
                query: query.into(),
            },
            depth: 0,
            collapsed: false,
            has_children: false,
            task_count,
            is_favorite: false,
            color: None,
        }
    }

    /// Sets `is_favorite` on this node.
    #[must_use]
    pub const fn with_favorite(mut self, is_favorite: bool) -> Self {
        self.is_favorite = is_favorite;
        self
    }

    /// Sets `color` on this node.
    #[must_use]
    pub fn with_color(mut self, color: Option<String>) -> Self {
        self.color = color;
        self
    }

    /// Returns a copy of this node with `collapsed` updated on both the node
    /// and its inner [`SidebarKind::Project`] variant.
    #[must_use]
    pub fn with_collapsed(mut self, collapsed: bool) -> Self {
        self.collapsed = collapsed;
        if let SidebarKind::Project {
            collapsed: ref mut inner,
            ..
        } = self.kind
        {
            *inner = collapsed;
        }
        self
    }

    /// Returns the [`ViewId`] activated when this node is selected.
    #[must_use]
    pub fn view_id(&self) -> ViewId {
        self.id.clone()
    }

    /// Returns the project [`Id`] if this node is a [`SidebarKind::Project`].
    #[must_use]
    pub const fn project_id(&self) -> Option<&Id> {
        match &self.kind {
            SidebarKind::Project { id, .. } => Some(id),
            _ => None,
        }
    }

    /// Returns the parent project [`Id`] if this node is a child sub-project.
    #[must_use]
    pub const fn parent_project_id(&self) -> Option<&Id> {
        match &self.kind {
            SidebarKind::Project { parent_id, .. } => parent_id.as_ref(),
            _ => None,
        }
    }

    /// Returns the nesting depth of this node (`0` for root entries).
    #[must_use]
    pub const fn depth(&self) -> usize {
        self.depth
    }

    /// Returns whether this node is currently marked collapsed.
    #[must_use]
    pub const fn is_collapsed(&self) -> bool {
        self.collapsed
    }

    /// Returns the display title of this sidebar node.
    #[must_use]
    pub fn title(&self) -> &str {
        &self.label
    }
}

/// Section header metadata used when grouping tasks within a project view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SectionHeader {
    /// Unique section identifier.
    pub id: Id,
    /// Section display title.
    pub name: String,
    /// Manual position among sibling sections in the project.
    pub section_order: i32,
}

/// A single task row rendered in the middle task-list pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    /// Unique task identifier.
    pub id: Id,
    /// Task title / primary text.
    pub content: String,
    /// Markdown-ish free-text description (empty when absent).
    pub description: String,
    /// Owning project identifier.
    pub project_id: Id,
    /// Resolved project display name, when known.
    pub project_name: Option<String>,
    /// Optional parent task identifier (`Some` for sub-tasks).
    pub parent_id: Option<Id>,
    /// Sub-task indentation depth (`0` for top-level tasks).
    pub depth: usize,
    /// Validated Todoist priority (`1..=4`, where `4` is highest / P1 in UI).
    pub priority: Priority,
    /// Optional due date / recurrence metadata.
    pub due: Option<Due>,
    /// Whether `due` is strictly before the current reference date.
    pub is_overdue: bool,
    /// Optional deadline date separate from `due`.
    pub deadline: Option<Deadline>,
    /// Attached personal label names.
    pub labels: Vec<String>,
    /// Optional section identifier within the project.
    pub section_id: Option<Id>,
    /// Resolved section display name, when the task belongs to a section.
    pub section_name: Option<String>,
    /// Sibling order position (`child_order` from Sync API).
    pub child_order: i32,
    /// Whether the task is completed.
    pub checked: bool,
}

/// A read-only comment ("note" in Sync API terminology) rendered in the detail pane.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Comment {
    /// Unique comment identifier.
    pub id: Id,
    /// ISO-8601 timestamp when the comment was posted.
    pub posted_at: String,
    /// Display name or handle of the comment's author.
    pub author: String,
    /// Comment body text.
    pub content: String,
    /// Optional attached file name, if the note has a `file_attachment`.
    pub attachment_name: Option<String>,
}

/// Full detail model rendered in the right-hand detail pane for the selected task.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskDetail {
    /// Unique task identifier.
    pub id: Id,
    /// Task title / primary text.
    pub content: String,
    /// Markdown-ish free-text description (empty when absent).
    pub description: String,
    /// Owning project identifier.
    pub project_id: Id,
    /// Resolved project display name, when known.
    pub project_name: Option<String>,
    /// Optional section identifier within the project.
    pub section_id: Option<Id>,
    /// Resolved section display name, when the task belongs to a section.
    pub section_name: Option<String>,
    /// Optional due date / recurrence metadata.
    pub due: Option<Due>,
    /// Whether `due` is strictly before the current reference date.
    pub is_overdue: bool,
    /// Optional deadline date separate from `due`.
    pub deadline: Option<Deadline>,
    /// Validated Todoist priority (`1..=4`).
    pub priority: Priority,
    /// Attached personal label names.
    pub labels: Vec<String>,
    /// Whether the task is completed.
    pub checked: bool,
    /// Chronological (oldest-first / newest-last) comments attached to the task.
    pub comments: Vec<Comment>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sidebar_node_constructors_and_accessors_round_trip() {
        let inbox = SidebarNode::inbox(4);
        assert_eq!(inbox.view_id(), ViewId::Inbox);
        assert_eq!(inbox.title(), "Inbox");
        assert_eq!(inbox.depth(), 0);
        assert!(!inbox.is_collapsed());
        assert_eq!(inbox.project_id(), None);

        let proj_id = Id::new("6X4VfakePrj001");
        let parent_id = Id::new("6X4VfakePrjRoot");
        let proj = SidebarNode::project(
            proj_id.clone(),
            "Backend",
            Some(parent_id.clone()),
            2,
            false,
            true,
            7,
        )
        .with_favorite(true)
        .with_color(Some(String::from("blue")));

        assert_eq!(proj.view_id(), ViewId::Project(proj_id.clone()));
        assert_eq!(proj.project_id(), Some(&proj_id));
        assert_eq!(proj.parent_project_id(), Some(&parent_id));
        assert_eq!(proj.depth(), 2);
        assert!(!proj.is_collapsed());
        assert!(proj.is_favorite);
        assert_eq!(proj.color.as_deref(), Some("blue"));

        let collapsed = proj.with_collapsed(true);
        assert!(collapsed.is_collapsed());
        assert!(matches!(
            collapsed.kind,
            SidebarKind::Project {
                collapsed: true,
                ..
            }
        ));
    }
}
