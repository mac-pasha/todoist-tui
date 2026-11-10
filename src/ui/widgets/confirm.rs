//! Reusable confirmation dialog widget (`T-707`, EPIC-08).

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Alignment, Rect};
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::ui::theme::Theme;

/// Key-handling result inside [`ConfirmDialog`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ConfirmEvent {
    Pending,
    Confirmed,
    Cancelled,
}

/// Generic confirmation modal (`title`, `message`, `[Yes] / [No]`).
#[derive(Debug, Clone)]
pub struct ConfirmDialog {
    pub title: String,
    pub message: String,
    pub confirm_focused: bool,
}

impl ConfirmDialog {
    /// Creates a confirmation dialog defaulting focus to Cancel (`false`) for safety.
    #[must_use]
    pub fn new(title: impl Into<String>, message: impl Into<String>) -> Self {
        Self {
            title: title.into(),
            message: message.into(),
            confirm_focused: false,
        }
    }

    /// Handles navigation and confirm/cancel key presses.
    pub fn handle_key(&mut self, key: KeyEvent) -> ConfirmEvent {
        match key.code {
            KeyCode::Esc | KeyCode::Char('n' | 'N') => ConfirmEvent::Cancelled,
            KeyCode::Char('y' | 'Y') => ConfirmEvent::Confirmed,
            KeyCode::Left | KeyCode::Right | KeyCode::Tab | KeyCode::Char('h' | 'l') => {
                self.confirm_focused = !self.confirm_focused;
                ConfirmEvent::Pending
            }
            KeyCode::Enter => {
                if self.confirm_focused {
                    ConfirmEvent::Confirmed
                } else {
                    ConfirmEvent::Cancelled
                }
            }
            _ => ConfirmEvent::Pending,
        }
    }

    /// Renders the centered confirmation dialog.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width < 20 || area.height < 8 {
            return;
        }
        let box_w = 54u16.min(area.width.saturating_sub(4));
        let box_h = 8u16.min(area.height.saturating_sub(2));
        let x = area.x + (area.width.saturating_sub(box_w)) / 2;
        let y = area.y + (area.height.saturating_sub(box_h)) / 2;
        let rect = Rect::new(x, y, box_w, box_h);

        frame.render_widget(Clear, rect);
        let block = Block::default()
            .title(format!(" {} ", self.title))
            .borders(Borders::ALL)
            .border_style(theme.border_focused)
            .style(theme.help_overlay);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let yes_style = if self.confirm_focused {
            theme.sidebar_selected
        } else {
            theme.muted
        };
        let no_style = if self.confirm_focused {
            theme.muted
        } else {
            theme.sidebar_selected
        };

        let lines = vec![
            Line::from(Span::styled(
                self.message.clone(),
                theme.list_row.add_modifier(Modifier::BOLD),
            )),
            Line::from(""),
            Line::from(vec![
                Span::styled("  [ Yes (y) ]  ", yes_style),
                Span::raw("   "),
                Span::styled("  [ No (n) ]  ", no_style),
            ]),
        ];

        let para = Paragraph::new(lines)
            .alignment(Alignment::Center)
            .wrap(Wrap { trim: false });
        frame.render_widget(para, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crossterm::event::KeyModifiers;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn snapshot_confirm_dialog_at_80x24() {
        let mut dlg = ConfirmDialog::new("Delete Task", "Delete 'Buy oat milk'?");
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();

        term.draw(|f| dlg.render(f, f.area(), &theme)).unwrap();
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..24 {
            let row: String = (0..80)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            out.push_str(&row);
        }
        assert!(out.contains("Delete Task"));
        assert!(out.contains("Buy oat milk"));

        assert_eq!(
            dlg.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE)),
            ConfirmEvent::Cancelled
        );
        assert_eq!(
            dlg.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)),
            ConfirmEvent::Confirmed
        );
    }
}
