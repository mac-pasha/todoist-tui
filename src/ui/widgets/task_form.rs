//! Reusable Create / Edit Task form widget (`T-704`, `T-706`, EPIC-08).

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::api::model::{Id, Priority};
use crate::ui::actions::{AddPayload, EditPayload, FieldEdit, TaskAction};
use crate::ui::theme::Theme;

/// Which field currently holds keyboard focus inside [`TaskForm`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FormField {
    Content,
    Description,
    Project,
    Section,
    Parent,
    Due,
    Priority,
    Labels,
}

impl FormField {
    const ALL: [Self; 8] = [
        Self::Content,
        Self::Description,
        Self::Project,
        Self::Section,
        Self::Parent,
        Self::Due,
        Self::Priority,
        Self::Labels,
    ];

    const fn label(self) -> &'static str {
        match self {
            Self::Content => "Content*",
            Self::Description => "Description",
            Self::Project => "Project ID",
            Self::Section => "Section ID",
            Self::Parent => "Parent ID",
            Self::Due => "Due",
            Self::Priority => "Priority (1-4)",
            Self::Labels => "Labels (csv)",
        }
    }

    const fn next(self) -> Self {
        match self {
            Self::Content => Self::Description,
            Self::Description => Self::Project,
            Self::Project => Self::Section,
            Self::Section => Self::Parent,
            Self::Parent => Self::Due,
            Self::Due => Self::Priority,
            Self::Priority => Self::Labels,
            Self::Labels => Self::Content,
        }
    }

    const fn prev(self) -> Self {
        match self {
            Self::Content => Self::Labels,
            Self::Description => Self::Content,
            Self::Project => Self::Description,
            Self::Section => Self::Project,
            Self::Parent => Self::Section,
            Self::Due => Self::Parent,
            Self::Priority => Self::Due,
            Self::Labels => Self::Priority,
        }
    }
}

/// Form mode: adding a new task vs editing an existing task.
#[allow(clippy::large_enum_variant)]
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormMode {
    Add,
    Edit {
        task_id: Id,
        initial: TaskFormValues,
    },
}

/// Snapshot of text values held by [`TaskForm`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskFormValues {
    pub content: String,
    pub description: String,
    pub project: String,
    pub section: String,
    pub parent: String,
    pub due: String,
    pub priority: String,
    pub labels: String,
}

/// Outcome of feeding a key event into [`TaskForm::handle_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormEvent {
    /// Stay open cleanly.
    Continue,
    /// User pressed Esc to cancel without mutating.
    Cancel,
    /// Form validated and produced a ready [`TaskAction`] (or `None` when Edit had zero changes).
    Submit(Option<TaskAction>),
}

/// Stateful modal form widget for creating or editing tasks.
#[derive(Debug, Clone)]
pub struct TaskForm {
    pub mode: FormMode,
    pub values: TaskFormValues,
    pub focused: FormField,
    pub error: Option<String>,
}

impl TaskForm {
    /// Creates an empty Add-mode form prefilled with project/section/parent defaults.
    #[must_use]
    #[allow(clippy::needless_pass_by_value)]
    pub fn new_add(project: Id, section: Option<Id>, parent: Option<Id>) -> Self {
        Self {
            mode: FormMode::Add,
            values: TaskFormValues {
                content: String::new(),
                description: String::new(),
                project: project.as_str().to_string(),
                section: section.map(|s| s.as_str().to_string()).unwrap_or_default(),
                parent: parent.map(|p| p.as_str().to_string()).unwrap_or_default(),
                due: String::new(),
                priority: "1".to_string(),
                labels: String::new(),
            },
            focused: FormField::Content,
            error: None,
        }
    }

    /// Creates an Edit-mode form prefilled from existing task values.
    #[must_use]
    #[allow(clippy::too_many_arguments, clippy::needless_pass_by_value)]
    pub fn new_edit(
        task_id: Id,
        content: String,
        description: String,
        project: Id,
        section: Option<Id>,
        parent: Option<Id>,
        due: Option<String>,
        priority: Priority,
        labels: Vec<String>,
    ) -> Self {
        let initial = TaskFormValues {
            content,
            description,
            project: project.as_str().to_string(),
            section: section.map(|s| s.as_str().to_string()).unwrap_or_default(),
            parent: parent.map(|p| p.as_str().to_string()).unwrap_or_default(),
            due: due.unwrap_or_default(),
            priority: priority.get().to_string(),
            labels: labels.join(", "),
        };
        Self {
            mode: FormMode::Edit {
                task_id,
                initial: initial.clone(),
            },
            values: initial,
            focused: FormField::Content,
            error: None,
        }
    }

    fn focused_buf_mut(&mut self) -> &mut String {
        match self.focused {
            FormField::Content => &mut self.values.content,
            FormField::Description => &mut self.values.description,
            FormField::Project => &mut self.values.project,
            FormField::Section => &mut self.values.section,
            FormField::Parent => &mut self.values.parent,
            FormField::Due => &mut self.values.due,
            FormField::Priority => &mut self.values.priority,
            FormField::Labels => &mut self.values.labels,
        }
    }

    fn field_value(&self, field: FormField) -> &str {
        match field {
            FormField::Content => &self.values.content,
            FormField::Description => &self.values.description,
            FormField::Project => &self.values.project,
            FormField::Section => &self.values.section,
            FormField::Parent => &self.values.parent,
            FormField::Due => &self.values.due,
            FormField::Priority => &self.values.priority,
            FormField::Labels => &self.values.labels,
        }
    }

    /// Processes a single key event inside the form.
    pub fn handle_key(&mut self, key: KeyEvent) -> FormEvent {
        match key.code {
            KeyCode::Esc => FormEvent::Cancel,
            KeyCode::Tab => {
                self.focused = self.focused.next();
                FormEvent::Continue
            }
            KeyCode::BackTab => {
                self.focused = self.focused.prev();
                FormEvent::Continue
            }
            KeyCode::Enter => {
                if self.focused == FormField::Labels
                    || key.modifiers.contains(KeyModifiers::CONTROL)
                {
                    self.try_submit()
                } else {
                    self.focused = self.focused.next();
                    FormEvent::Continue
                }
            }
            KeyCode::Char('s') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.try_submit()
            }
            KeyCode::Backspace => {
                self.focused_buf_mut().pop();
                self.error = None;
                FormEvent::Continue
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.focused_buf_mut().push(c);
                self.error = None;
                FormEvent::Continue
            }
            _ => FormEvent::Continue,
        }
    }

    /// Validates current values and builds a [`TaskAction`], or sets inline error.
    #[allow(clippy::too_many_lines)]
    pub fn try_submit(&mut self) -> FormEvent {
        if self.values.content.trim().is_empty() {
            self.error = Some("Content cannot be empty".to_string());
            return FormEvent::Continue;
        }

        let parsed_prio: u8 = match self.values.priority.trim().parse::<u8>() {
            Ok(v) if (1..=4).contains(&v) => v,
            _ => {
                self.error = Some("Priority must be 1, 2, 3, or 4".to_string());
                return FormEvent::Continue;
            }
        };
        let Ok(priority) = Priority::new(parsed_prio) else {
            self.error = Some("Priority must be 1..=4".to_string());
            return FormEvent::Continue;
        };

        let labels: Vec<String> = self
            .values
            .labels
            .split(',')
            .map(str::trim)
            .filter(|s| !s.is_empty())
            .map(str::to_string)
            .collect();

        match &self.mode {
            FormMode::Add => {
                let payload = AddPayload {
                    content: self.values.content.trim().to_string(),
                    description: self.values.description.trim().to_string(),
                    project: Id::new(self.values.project.trim()),
                    section: (!self.values.section.trim().is_empty())
                        .then(|| Id::new(self.values.section.trim())),
                    parent: (!self.values.parent.trim().is_empty())
                        .then(|| Id::new(self.values.parent.trim())),
                    due: (!self.values.due.trim().is_empty())
                        .then(|| self.values.due.trim().to_string()),
                    priority,
                    labels,
                };
                FormEvent::Submit(Some(TaskAction::Add(payload)))
            }
            FormMode::Edit { task_id, initial } => {
                let content = (self.values.content != initial.content)
                    .then(|| self.values.content.trim().to_string());
                let description = (self.values.description != initial.description)
                    .then(|| self.values.description.trim().to_string());
                let project = (self.values.project != initial.project)
                    .then(|| Id::new(self.values.project.trim()));
                let section = if self.values.section == initial.section {
                    FieldEdit::Unchanged
                } else if self.values.section.trim().is_empty() {
                    FieldEdit::Clear
                } else {
                    FieldEdit::Set(Id::new(self.values.section.trim()))
                };
                let parent = if self.values.parent == initial.parent {
                    FieldEdit::Unchanged
                } else if self.values.parent.trim().is_empty() {
                    FieldEdit::Clear
                } else {
                    FieldEdit::Set(Id::new(self.values.parent.trim()))
                };
                let due = if self.values.due == initial.due {
                    FieldEdit::Unchanged
                } else if self.values.due.trim().is_empty() {
                    FieldEdit::Clear
                } else {
                    FieldEdit::Set(self.values.due.trim().to_string())
                };
                let prio_edit = (self.values.priority != initial.priority).then_some(priority);
                let labels_edit = (self.values.labels != initial.labels).then_some(labels);

                if content.is_none()
                    && description.is_none()
                    && project.is_none()
                    && section == FieldEdit::Unchanged
                    && parent == FieldEdit::Unchanged
                    && due == FieldEdit::Unchanged
                    && prio_edit.is_none()
                    && labels_edit.is_none()
                {
                    return FormEvent::Submit(None);
                }

                let payload = EditPayload {
                    id: task_id.clone(),
                    content,
                    description,
                    project,
                    section,
                    parent,
                    due,
                    priority: prio_edit,
                    labels: labels_edit,
                };
                FormEvent::Submit(Some(TaskAction::Edit(payload)))
            }
        }
    }

    /// Renders the centered form modal into `area`.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width < 20 || area.height < 12 {
            return;
        }
        let box_w = 64u16.min(area.width.saturating_sub(4));
        let box_h = 16u16.min(area.height.saturating_sub(2));
        let x = area.x + (area.width.saturating_sub(box_w)) / 2;
        let y = area.y + (area.height.saturating_sub(box_h)) / 2;
        let rect = Rect::new(x, y, box_w, box_h);

        frame.render_widget(Clear, rect);
        let title = match self.mode {
            FormMode::Add => " Add Task ",
            FormMode::Edit { .. } => " Edit Task ",
        };
        let block = Block::default()
            .title(title)
            .borders(Borders::ALL)
            .border_style(theme.border_focused)
            .style(theme.help_overlay);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let mut lines: Vec<Line<'static>> = Vec::with_capacity(12);
        for field in FormField::ALL {
            let is_focused = field == self.focused;
            let marker = if is_focused { "> " } else { "  " };
            let label = format!("{marker}{:<14}: ", field.label());
            let val = self.field_value(field).to_string();
            let style = if is_focused {
                theme.sidebar_selected
            } else {
                theme.list_row
            };
            lines.push(Line::from(vec![
                Span::styled(label, theme.muted.add_modifier(Modifier::BOLD)),
                Span::styled(val, style),
            ]));
        }

        if let Some(ref err) = self.error {
            lines.push(Line::from(Span::styled(err.clone(), theme.status_error)));
        } else {
            lines.push(Line::from(""));
        }
        lines.push(Line::from(Span::styled(
            "Tab: next field | Enter on last: submit | Esc: cancel",
            theme.muted,
        )));

        let para = Paragraph::new(lines).wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn empty_content_refused_inline_and_emits_no_submit() {
        let mut form = TaskForm::new_add(Id::new("proj_1"), None, None);
        let ev = form.try_submit();
        assert_eq!(ev, FormEvent::Continue);
        assert!(form.error.is_some());
    }

    #[test]
    fn filled_form_submits_add_action_and_renders_at_80x24() {
        let mut form = TaskForm::new_add(Id::new("proj_1"), None, None);
        for c in "Write docs".chars() {
            form.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
        }

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        term.draw(|f| form.render(f, f.area(), &theme)).unwrap();

        let ev = form.try_submit();
        match ev {
            FormEvent::Submit(Some(TaskAction::Add(payload))) => {
                assert_eq!(payload.content, "Write docs");
                assert_eq!(payload.project, Id::new("proj_1"));
            }
            other => panic!("Unexpected: {other:?}"),
        }
    }

    #[test]
    fn edit_mode_returns_none_when_unchanged_and_only_changed_fields_when_modified() {
        let mut form = TaskForm::new_edit(
            Id::new("item_1"),
            "Original".to_string(),
            String::new(),
            Id::new("proj_1"),
            None,
            None,
            None,
            Priority::new(1).unwrap(),
            Vec::new(),
        );

        assert_eq!(form.try_submit(), FormEvent::Submit(None));

        form.focused = FormField::Priority;
        form.values.priority = "4".to_string();

        match form.try_submit() {
            FormEvent::Submit(Some(TaskAction::Edit(edit))) => {
                assert_eq!(edit.priority, Some(Priority::new(4).unwrap()));
                assert_eq!(edit.content, None);
            }
            other => panic!("Unexpected: {other:?}"),
        }
    }
}
