//! Structure add/rename forms for projects, sections, and labels (`T-807`, EPIC-09).

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::api::model::Id;
use crate::structure::command::MAX_NAME_LEN;
use crate::ui::theme::Theme;

/// Target structure entity type & mode for [`StructureForm`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureFormKind {
    AddProject,
    AddSubProject {
        parent_id: Id,
        parent_name: String,
    },
    RenameProject {
        id: Id,
    },
    AddSection {
        project_id: Id,
        project_name: String,
    },
    RenameSection {
        id: Id,
    },
    AddLabel,
    RenameLabel {
        id: Id,
    },
}

/// Intent emitted when the user submits [`StructureForm`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureIntent {
    AddProject { name: String },
    AddSubProject { parent_id: Id, name: String },
    RenameProject { id: Id, name: String },
    AddSection { project_id: Id, name: String },
    RenameSection { id: Id, name: String },
    AddLabel { name: String },
    RenameLabel { id: Id, name: String },
}

/// Event returned by [`StructureForm::handle_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum StructureFormEvent {
    Pending,
    Cancelled,
    Submit(StructureIntent),
}

/// Reusable modal form for adding or renaming a project, section, or label.
#[derive(Debug, Clone)]
pub struct StructureForm {
    pub kind: StructureFormKind,
    pub name: String,
    pub error: Option<String>,
}

impl StructureForm {
    /// Creates an empty Add-mode structure form.
    #[must_use]
    pub fn new(kind: StructureFormKind) -> Self {
        Self {
            kind,
            name: String::new(),
            error: None,
        }
    }

    /// Creates a Rename-mode structure form prefilled with `current_name`.
    #[must_use]
    pub fn rename(kind: StructureFormKind, current_name: impl Into<String>) -> Self {
        Self {
            kind,
            name: current_name.into(),
            error: None,
        }
    }

    /// Sets an inline validation or dispatch error without closing the form.
    pub fn set_error(&mut self, msg: impl Into<String>) {
        self.error = Some(msg.into());
    }

    fn title(&self) -> String {
        match &self.kind {
            StructureFormKind::AddProject => " New Project ".to_string(),
            StructureFormKind::AddSubProject { parent_name, .. } => {
                format!(" New Sub-project under {parent_name} ")
            }
            StructureFormKind::RenameProject { .. } => " Rename Project ".to_string(),
            StructureFormKind::AddSection { project_name, .. } => {
                format!(" New Section in {project_name} ")
            }
            StructureFormKind::RenameSection { .. } => " Rename Section ".to_string(),
            StructureFormKind::AddLabel => " New Label ".to_string(),
            StructureFormKind::RenameLabel { .. } => " Rename Label ".to_string(),
        }
    }

    /// Handles a key event inside the structure form.
    pub fn handle_key(&mut self, key: KeyEvent) -> StructureFormEvent {
        match key.code {
            KeyCode::Esc => StructureFormEvent::Cancelled,
            KeyCode::Enter => self.try_submit(),
            KeyCode::Backspace => {
                self.name.pop();
                self.error = None;
                StructureFormEvent::Pending
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.name.push(c);
                self.error = None;
                StructureFormEvent::Pending
            }
            _ => StructureFormEvent::Pending,
        }
    }

    /// Validates the trimmed name and emits a [`StructureIntent`], or records an inline error.
    pub fn try_submit(&mut self) -> StructureFormEvent {
        let trimmed = self.name.trim();
        if trimmed.is_empty() {
            self.error = Some("name must not be empty".to_string());
            return StructureFormEvent::Pending;
        }
        let len = trimmed.chars().count();
        if len > MAX_NAME_LEN {
            self.error = Some(format!("name is too long: {len} characters"));
            return StructureFormEvent::Pending;
        }

        let name = trimmed.to_string();
        let intent = match &self.kind {
            StructureFormKind::AddProject => StructureIntent::AddProject { name },
            StructureFormKind::AddSubProject { parent_id, .. } => StructureIntent::AddSubProject {
                parent_id: parent_id.clone(),
                name,
            },
            StructureFormKind::RenameProject { id } => StructureIntent::RenameProject {
                id: id.clone(),
                name,
            },
            StructureFormKind::AddSection { project_id, .. } => StructureIntent::AddSection {
                project_id: project_id.clone(),
                name,
            },
            StructureFormKind::RenameSection { id } => StructureIntent::RenameSection {
                id: id.clone(),
                name,
            },
            StructureFormKind::AddLabel => StructureIntent::AddLabel { name },
            StructureFormKind::RenameLabel { id } => StructureIntent::RenameLabel {
                id: id.clone(),
                name,
            },
        };
        StructureFormEvent::Submit(intent)
    }

    /// Renders the centered structure form modal into `area`.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width < 20 || area.height < 8 {
            return;
        }
        let box_w = 58u16.min(area.width.saturating_sub(4));
        let box_h = 8u16.min(area.height.saturating_sub(2));
        let x = area.x + (area.width.saturating_sub(box_w)) / 2;
        let y = area.y + (area.height.saturating_sub(box_h)) / 2;
        let rect = Rect::new(x, y, box_w, box_h);

        frame.render_widget(Clear, rect);
        let block = Block::default()
            .title(self.title())
            .borders(Borders::ALL)
            .border_style(theme.border_focused)
            .style(theme.help_overlay);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let mut lines = vec![
            Line::from(vec![
                Span::styled("Name: ", theme.muted.add_modifier(Modifier::BOLD)),
                Span::styled(self.name.clone(), theme.sidebar_selected),
            ]),
            Line::from(""),
        ];

        if let Some(ref err) = self.error {
            lines.push(Line::from(Span::styled(err.clone(), theme.status_error)));
        } else {
            lines.push(Line::from(Span::styled(
                "Enter: submit | Esc: cancel",
                theme.muted,
            )));
        }

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn empty_name_shows_inline_error_and_renders_cleanly_at_80x24() {
        let mut form = StructureForm::new(StructureFormKind::AddProject);
        let ev = form.try_submit();
        assert_eq!(ev, StructureFormEvent::Pending);
        assert!(form.error.is_some());

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        term.draw(|f| form.render(f, f.area(), &theme)).unwrap();

        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..24 {
            for x in 0..80 {
                out.push_str(buf[(x, y)].symbol());
            }
        }
        assert!(out.contains("New Project"));
        assert!(out.contains("name must not be empty"));
    }
}
