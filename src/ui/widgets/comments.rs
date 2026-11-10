//! Interactive comments modal widget with oldest-first listing & text input (`T-710`, EPIC-08).

#![allow(dead_code)]

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph, Wrap};
use ratatui::Frame;

use crate::api::model::Id;
use crate::ui::model::Comment;
use crate::ui::theme::Theme;

/// Outcome of handling a key inside [`CommentsModal`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum CommentsEvent {
    Pending,
    Close,
    AddComment { item_id: Id, content: String },
}

/// Modal viewing cached comments oldest-first and allowing text entry for a new note.
#[derive(Debug, Clone)]
pub struct CommentsModal {
    pub item_id: Id,
    pub task_title: String,
    pub comments: Vec<Comment>,
    pub scroll: usize,
    pub composing: bool,
    pub draft: String,
    pub error: Option<String>,
}

impl CommentsModal {
    /// Creates a comments modal for `item_id` with chronological `comments`.
    #[must_use]
    pub fn new(item_id: Id, task_title: impl Into<String>, comments: Vec<Comment>) -> Self {
        Self {
            item_id,
            task_title: task_title.into(),
            comments,
            scroll: 0,
            composing: false,
            draft: String::new(),
            error: None,
        }
    }

    /// Processes a single key event inside the comments modal.
    pub fn handle_key(&mut self, key: KeyEvent) -> CommentsEvent {
        if self.composing {
            match key.code {
                KeyCode::Esc => {
                    self.composing = false;
                    self.error = None;
                    CommentsEvent::Pending
                }
                KeyCode::Enter => {
                    let trimmed = self.draft.trim().to_string();
                    if trimmed.is_empty() {
                        self.error = Some("Comment cannot be empty".to_string());
                        CommentsEvent::Pending
                    } else {
                        self.draft.clear();
                        self.composing = false;
                        self.error = None;
                        CommentsEvent::AddComment {
                            item_id: self.item_id.clone(),
                            content: trimmed,
                        }
                    }
                }
                KeyCode::Backspace => {
                    self.draft.pop();
                    self.error = None;
                    CommentsEvent::Pending
                }
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    self.draft.push(c);
                    self.error = None;
                    CommentsEvent::Pending
                }
                _ => CommentsEvent::Pending,
            }
        } else {
            match key.code {
                KeyCode::Esc | KeyCode::Char('q') => CommentsEvent::Close,
                KeyCode::Char('j') | KeyCode::Down => {
                    self.scroll = self.scroll.saturating_add(1);
                    CommentsEvent::Pending
                }
                KeyCode::Char('k') | KeyCode::Up => {
                    self.scroll = self.scroll.saturating_sub(1);
                    CommentsEvent::Pending
                }
                KeyCode::Char('i' | 'a' | 'n') => {
                    self.composing = true;
                    self.error = None;
                    CommentsEvent::Pending
                }
                _ => CommentsEvent::Pending,
            }
        }
    }

    /// Renders the centered comments modal.
    pub fn render(&self, frame: &mut Frame<'_>, area: Rect, theme: &Theme) {
        if area.width < 20 || area.height < 10 {
            return;
        }
        let box_w = 64u16.min(area.width.saturating_sub(4));
        let box_h = 18u16.min(area.height.saturating_sub(2));
        let x = area.x + (area.width.saturating_sub(box_w)) / 2;
        let y = area.y + (area.height.saturating_sub(box_h)) / 2;
        let rect = Rect::new(x, y, box_w, box_h);

        frame.render_widget(Clear, rect);
        let block = Block::default()
            .title(format!(" Comments: {} ", self.task_title))
            .borders(Borders::ALL)
            .border_style(theme.border_focused)
            .style(theme.help_overlay);
        let inner = block.inner(rect);
        frame.render_widget(block, rect);

        let mut lines: Vec<Line<'static>> = Vec::new();
        if self.comments.is_empty() {
            lines.push(Line::from(Span::styled("(no comments yet)", theme.muted)));
        } else {
            for c in &self.comments {
                lines.push(Line::from(vec![
                    Span::styled(
                        format!("{} ", c.author),
                        theme.status_ok.add_modifier(Modifier::BOLD),
                    ),
                    Span::styled(format!("({})", c.posted_at), theme.muted),
                ]));
                lines.push(Line::from(Span::styled(
                    format!("  {}", c.content),
                    theme.list_row,
                )));
            }
        }

        lines.push(Line::from(""));
        if self.composing {
            lines.push(Line::from(vec![
                Span::styled("New comment: ", theme.status_warn),
                Span::styled(self.draft.clone(), theme.list_row),
            ]));
        } else {
            lines.push(Line::from(Span::styled(
                "Press 'i' to add comment | j/k: scroll | Esc: close",
                theme.muted,
            )));
        }
        if let Some(ref err) = self.error {
            lines.push(Line::from(Span::styled(err.clone(), theme.status_error)));
        }

        #[allow(clippy::cast_possible_truncation)]
        let scroll_y = self.scroll.min(usize::from(u16::MAX)) as u16;
        let para = Paragraph::new(lines)
            .wrap(Wrap { trim: false })
            .scroll((scroll_y, 0));
        frame.render_widget(para, inner);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;

    #[test]
    fn snapshot_comments_zero_one_and_many_at_80x24() {
        let theme = Theme::default();
        let backend = TestBackend::new(80, 24);
        let mut term = Terminal::new(backend).unwrap();

        let empty = CommentsModal::new(Id::new("t1"), "Task 1", Vec::new());
        term.draw(|f| empty.render(f, f.area(), &theme)).unwrap();

        let one = CommentsModal::new(
            Id::new("t1"),
            "Task 1",
            vec![Comment {
                id: Id::new("c1"),
                posted_at: "2026-11-01T10:00:00Z".to_string(),
                author: "me".to_string(),
                content: "First note".to_string(),
                attachment_name: None,
            }],
        );
        term.draw(|f| one.render(f, f.area(), &theme)).unwrap();

        let many: Vec<Comment> = (0..10)
            .map(|i| Comment {
                id: Id::new(format!("c{i}")),
                posted_at: "2026-11-01T10:00:00Z".to_string(),
                author: "me".to_string(),
                content: format!("Note {i}"),
                attachment_name: None,
            })
            .collect();
        let mut modal = CommentsModal::new(Id::new("t1"), "Task 1", many);
        // Refuses empty comment
        modal.handle_key(KeyEvent::new(KeyCode::Char('i'), KeyModifiers::NONE));
        assert_eq!(
            modal.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)),
            CommentsEvent::Pending
        );
        assert!(modal.error.is_some());

        term.draw(|f| modal.render(f, f.area(), &theme)).unwrap();
    }
}
