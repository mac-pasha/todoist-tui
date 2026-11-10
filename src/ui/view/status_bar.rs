//! Bottom 1-row status bar widget (`T-611`, EPIC-07).

#![allow(dead_code)]

use std::time::SystemTime;

use ratatui::layout::Rect;
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;
use ratatui::Frame;

use crate::ui::status::{StatusModel, SyncEvent};
use crate::ui::theme::Theme;

/// Renders the bottom status bar row using injected clock `now`.
pub fn render(
    frame: &mut Frame<'_>,
    area: Rect,
    status: &StatusModel,
    search_active: bool,
    now: SystemTime,
    theme: &Theme,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }

    if search_active {
        let prompt = Span::styled("/ Search (press Esc to cancel)", theme.status_warn);
        frame.render_widget(Paragraph::new(Line::from(prompt)), area);
        return;
    }

    let (label_text, style) = match &status.state {
        SyncEvent::Synced { .. } => ("synced".to_string(), theme.status_ok),
        SyncEvent::Syncing => ("syncing...".to_string(), theme.status_warn),
        SyncEvent::Offline => ("offline".to_string(), theme.status_warn),
        SyncEvent::Pending { count } => (format!("pending ({count})"), theme.status_warn),
        SyncEvent::Error { message } => {
            let max_msg = usize::from(area.width).saturating_sub(24);
            let clipped: String = message.chars().take(max_msg).collect();
            (format!("error: {clipped}"), theme.status_error)
        }
    };

    let last_sync_str = format!(" | last sync: {}", status.relative_last_sync(now));
    let help_hint = " | ?: help | q: quit";

    let mut spans = vec![
        Span::styled(format!(" [{label_text}]"), style),
        Span::styled(last_sync_str, theme.muted),
    ];

    if status.failed_count > 0 {
        spans.push(Span::styled(
            format!(" | {} failed (E)", status.failed_count),
            theme.status_error,
        ));
    }

    if let Some(ref notice) = status.notice {
        spans.push(Span::styled(format!(" | {notice}"), theme.status_warn));
    }

    let left_len: usize = spans.iter().map(|s| s.content.chars().count()).sum();
    if left_len + help_hint.len() <= usize::from(area.width) {
        spans.push(Span::styled(help_hint, theme.muted));
    }

    frame.render_widget(Paragraph::new(Line::from(spans)), area);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::time::Duration;

    #[test]
    fn renders_all_five_states_in_sequence() {
        let t0 = SystemTime::UNIX_EPOCH + Duration::from_secs(1_700_000_000);
        let theme = Theme::default();

        let states = vec![
            SyncEvent::Syncing,
            SyncEvent::Synced { at: Some(t0) },
            SyncEvent::Pending { count: 3 },
            SyncEvent::Offline,
            SyncEvent::Error {
                message: "network timeout".to_string(),
            },
        ];

        for st in states {
            let mut model = StatusModel::new();
            model.apply(st.clone(), t0);

            let backend = TestBackend::new(80, 1);
            let mut term = Terminal::new(backend).unwrap();
            term.draw(|f| render(f, f.area(), &model, false, t0, &theme))
                .unwrap();

            let buf = term.backend().buffer();
            let row: String = (0..80)
                .map(|x| buf[(x, 0)].symbol().chars().next().unwrap_or(' '))
                .collect();

            match st {
                SyncEvent::Syncing => assert!(row.contains("syncing...")),
                SyncEvent::Synced { .. } => assert!(row.contains("synced")),
                SyncEvent::Pending { .. } => assert!(row.contains("pending (3)")),
                SyncEvent::Offline => assert!(row.contains("offline")),
                SyncEvent::Error { .. } => assert!(row.contains("error: network timeout")),
            }
        }
    }
}
