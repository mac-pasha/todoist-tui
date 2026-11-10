//! Filterable destination picker widget for Move Task (`T-708`, EPIC-08).

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::api::model::Id;
use crate::ui::theme::Theme;

/// Destination category represented by a [`PickerEntry`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DestinationTarget {
    Project(Id),
    Section(Id),
    Parent(Id),
}

/// Single selectable option inside [`DestinationPicker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PickerEntry {
    pub label: String,
    pub target: DestinationTarget,
}

/// Outcome of handling a key in [`DestinationPicker`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PickerEvent {
    Pending,
    Cancelled,
    Selected(DestinationTarget),
}

/// Stateful filterable picker modal.
#[derive(Debug, Clone)]
pub struct DestinationPicker {
    pub entries: Vec<PickerEntry>,
    pub query: String,
    pub selected: usize,
    pub error: Option<String>,
}

impl DestinationPicker {
    /// Creates a picker seeded with `entries`.
    #[must_use]
    pub fn new(entries: Vec<PickerEntry>) -> Self {
        Self {
            entries,
            query: String::new(),
            selected: 0,
            error: None,
        }
    }

    /// Returns entries matching `query` (case-insensitive substring).
    #[must_use]
    pub fn filtered(&self) -> Vec<&PickerEntry> {
        let q = self.query.trim().to_ascii_lowercase();
        if q.is_empty() {
            self.entries.iter().collect()
        } else {
            self.entries
                .iter()
                .filter(|e| e.label.to_ascii_lowercase().contains(&q))
                .collect()
        }
    }

    /// Handles a key event inside the picker.
    pub fn handle_key(&mut self, key: KeyEvent) -> PickerEvent {
        let count = self.filtered().len();
        match key.code {
            KeyCode::Esc => PickerEvent::Cancelled,
            KeyCode::Down | KeyCode::Char('n') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                if count > 0 {
                    self.selected = (self.selected + 1).min(count - 1);
                }
                PickerEvent::Pending
            }
            KeyCode::Down => {
                if count > 0 {
                    self.selected = (self.selected + 1).min(count - 1);
                }
                PickerEvent::Pending
            }
            KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                PickerEvent::Pending
            }
            KeyCode::Enter => {
                let filtered = self.filtered();
                if let Some(entry) = filtered.get(self.selected) {
                    PickerEvent::Selected(entry.target.clone())
                } else {
                    PickerEvent::Pending
                }
            }
            KeyCode::Backspace => {
                self.query.pop();
                self.selected = 0;
                self.error = None;
                PickerEvent::Pending
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.query.push(c);
                self.selected = 0;
                self.error = None;
                PickerEvent::Pending
            }
            _ => PickerEvent::Pending,
        }
    }

    /// Renders the centered picker modal.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width < 20 || area.height < 10 {
            return;
        }
        let box_w = 56u16.min(area.width.saturating_sub(4));
        let box_h = 16u16.min(area.height.saturating_sub(2));
        let x = area.x + (area.width.saturating_sub(box_w)) / 2;
        let y = area.y + (area.height.saturating_sub(box_h)) / 2;
        let rect = Rect::new(x, y, box_w, box_h);

        frame.render_widget(Clear, rect);
        let block = Block::default()
            .title(" Move To Destination ")
            .borders(Borders::ALL)
            .border_style(theme.border_focused)
            .style(theme.help_overlay);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let mut lines = vec![Line::from(vec![
            Span::styled("Filter: ", theme.muted.add_modifier(Modifier::BOLD)),
            Span::styled(self.query.clone(), theme.list_row),
        ])];

        if let Some(ref err) = self.error {
            lines.push(Line::from(Span::styled(err.clone(), theme.status_error)));
        }

        let filtered = self.filtered();
        if filtered.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no matching destination)",
                theme.muted,
            )));
        } else {
            let max_rows = usize::from(inner.height.saturating_sub(2));
            let offset = self.selected.saturating_sub(max_rows.saturating_sub(1));
            for (idx, entry) in filtered.iter().enumerate().skip(offset).take(max_rows) {
                let is_sel = idx == self.selected;
                let style = if is_sel {
                    theme.sidebar_selected
                } else {
                    theme.list_row
                };
                let prefix = if is_sel { "> " } else { "  " };
                lines.push(Line::from(Span::styled(
                    format!("{prefix}{}", entry.label),
                    style,
                )));
            }
        }

        frame.render_widget(Paragraph::new(lines), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn snapshot_picker_empty_and_filtered() {
        let mut picker = DestinationPicker::new(vec![
            PickerEntry {
                label: "Project: Work".to_string(),
                target: DestinationTarget::Project(Id::new("p1")),
            },
            PickerEntry {
                label: "Project: Home".to_string(),
                target: DestinationTarget::Project(Id::new("p2")),
            },
        ]);

        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();

        // Empty filter renders both
        term.draw(|f| picker.render(f, f.area(), &theme)).unwrap();

        // Type "Work" -> filters down to 1 entry
        for ch in "Work".chars() {
            picker.handle_key(KeyEvent::new(KeyCode::Char(ch), KeyModifiers::NONE));
        }
        assert_eq!(picker.filtered().len(), 1);
        term.draw(|f| picker.render(f, f.area(), &theme)).unwrap();
    }
}
