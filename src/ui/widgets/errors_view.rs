//! Failed-commands errors modal & transient toast widget (`T-711`, EPIC-08).

#![allow(dead_code)]

use std::time::{Duration, SystemTime};

use crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::queue::status::FailedCommand;
use crate::ui::theme::Theme;

/// Transient error toast shown near the bottom of the screen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ErrorToast {
    pub message: String,
    pub shown_at: SystemTime,
    pub ttl: Duration,
}

impl ErrorToast {
    /// Creates a toast that expires after `ttl`.
    #[must_use]
    pub fn new(message: impl Into<String>, shown_at: SystemTime, ttl: Duration) -> Self {
        Self {
            message: message.into(),
            shown_at,
            ttl,
        }
    }

    /// Returns `true` if `now` is past `shown_at + ttl`.
    #[must_use]
    pub fn is_expired(&self, now: SystemTime) -> bool {
        now.duration_since(self.shown_at)
            .is_ok_and(|elapsed| elapsed >= self.ttl)
    }
}

/// Event produced by [`ErrorsModal::handle_key`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ErrorsEvent {
    Pending,
    Close,
    Dismiss(String),
}

/// Modal listing failed write-queue commands (`commands.state = 'failed'`).
#[derive(Debug, Clone)]
pub struct ErrorsModal {
    pub items: Vec<FailedCommand>,
    pub selected: usize,
}

impl ErrorsModal {
    /// Creates an errors view over `items`.
    #[must_use]
    pub fn new(items: Vec<FailedCommand>) -> Self {
        Self { items, selected: 0 }
    }

    /// Processes a key press in the errors modal.
    pub fn handle_key(&mut self, key: KeyEvent) -> ErrorsEvent {
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => ErrorsEvent::Close,
            KeyCode::Down | KeyCode::Char('j') => {
                if !self.items.is_empty() {
                    self.selected = (self.selected + 1).min(self.items.len() - 1);
                }
                ErrorsEvent::Pending
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                ErrorsEvent::Pending
            }
            KeyCode::Char('d' | 'x') | KeyCode::Enter => {
                if let Some(item) = self.items.get(self.selected) {
                    let uuid = item.uuid.clone();
                    ErrorsEvent::Dismiss(uuid)
                } else {
                    ErrorsEvent::Pending
                }
            }
            _ => ErrorsEvent::Pending,
        }
    }

    /// Renders the centered errors modal.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width < 20 || area.height < 10 {
            return;
        }
        let box_w = 68u16.min(area.width.saturating_sub(4));
        let box_h = 16u16.min(area.height.saturating_sub(2));
        let x = area.x + (area.width.saturating_sub(box_w)) / 2;
        let y = area.y + (area.height.saturating_sub(box_h)) / 2;
        let rect = Rect::new(x, y, box_w, box_h);

        frame.render_widget(Clear, rect);
        let block = Block::default()
            .title(format!(" Sync Errors ({}) ", self.items.len()))
            .borders(Borders::ALL)
            .border_style(theme.status_error)
            .style(theme.help_overlay);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let mut lines = Vec::new();
        if self.items.is_empty() {
            lines.push(Line::from(Span::styled(
                "(no failed commands)",
                theme.muted,
            )));
        } else {
            for (idx, fc) in self.items.iter().enumerate() {
                let sel = idx == self.selected;
                let style = if sel {
                    theme.sidebar_selected
                } else {
                    theme.status_error
                };
                let prefix = if sel { "> " } else { "  " };
                lines.push(Line::from(Span::styled(
                    format!("{prefix}[{}] {}", fc.cmd_type, fc.error),
                    style,
                )));
            }
        }
        lines.push(Line::from(""));
        lines.push(Line::from(Span::styled(
            "d/Enter: dismiss entry | Esc: close",
            theme.muted.add_modifier(Modifier::BOLD),
        )));

        frame.render_widget(Paragraph::new(lines).wrap(Wrap { trim: false }), inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn toast_expires_on_injected_clock() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(100);
        let toast = ErrorToast::new("command failed", t0, Duration::from_secs(5));
        assert!(!toast.is_expired(t0 + Duration::from_secs(4)));
        assert!(toast.is_expired(t0 + Duration::from_secs(5)));
    }
}
