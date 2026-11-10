//! Help overlay generated dynamically from `Keymap::bindings()` (`T-612`, EPIC-07).

#![allow(dead_code)]

use ratatui::layout::Rect;
use ratatui::style::Modifier;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, Paragraph};
use ratatui::Frame;

use crate::ui::keymap::Keymap;
use crate::ui::theme::Theme;

/// Renders the centered keybindings help overlay on top of the current frame.
#[allow(clippy::too_many_lines)]
pub fn render(frame: &mut Frame<'_>, area: Rect, keymap: &Keymap, theme: &Theme) {
    if area.width < 10 || area.height < 6 {
        return;
    }

    let bindings = keymap.bindings();
    let max_overlay_h = area.height.saturating_sub(2);
    let max_inner_rows = usize::from(max_overlay_h.saturating_sub(2));
    let use_two_cols = bindings.len() + 2 > max_inner_rows && area.width >= 72;

    let rows_needed = if use_two_cols {
        bindings.len().div_ceil(2) + 2
    } else {
        bindings.len() + 2
    };

    let box_w = if use_two_cols {
        78u16.min(area.width.saturating_sub(2))
    } else {
        56u16.min(area.width.saturating_sub(4))
    };
    let desired_h = u16::try_from(rows_needed + 2).unwrap_or(22);
    let box_h = desired_h.min(max_overlay_h);

    let x = area.x + (area.width.saturating_sub(box_w)) / 2;
    let y = area.y + (area.height.saturating_sub(box_h)) / 2;
    let overlay_rect = Rect::new(x, y, box_w, box_h);

    frame.render_widget(Clear, overlay_rect);

    let block = Block::default()
        .title(" Keybindings ")
        .borders(Borders::ALL)
        .border_style(theme.border_focused)
        .style(theme.help_overlay);

    let inner = block.inner(overlay_rect);
    frame.render_widget(block, overlay_rect);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let max_w = usize::from(inner.width);
    let mut lines: Vec<Line<'static>> = Vec::with_capacity(rows_needed);
    if use_two_cols {
        let half = bindings.len().div_ceil(2);
        let col_w = max_w / 2;
        let max_key = bindings
            .iter()
            .map(|(k, _, _)| k.chars().count())
            .max()
            .unwrap_or(1)
            .min(6);
        let format_col = |key_str: &str, desc: &str| -> Vec<Span<'static>> {
            let key_col = format!(" {key_str:<max_key$} ");
            let used = key_col.chars().count();
            let desc_budget = col_w.saturating_sub(used);
            let clipped_desc: String = desc.chars().take(desc_budget).collect();
            let pad = col_w.saturating_sub(used + clipped_desc.chars().count());
            vec![
                Span::styled(key_col, theme.help_overlay.add_modifier(Modifier::BOLD)),
                Span::styled(clipped_desc, theme.help_overlay),
                Span::styled(" ".repeat(pad), theme.help_overlay),
            ]
        };

        for row_idx in 0..half {
            let mut spans = Vec::new();
            if let Some((k, _, d)) = bindings.get(row_idx) {
                spans.extend(format_col(k, d));
            }
            if let Some((k, _, d)) = bindings.get(row_idx + half) {
                spans.extend(format_col(k, d));
            }
            lines.push(Line::from(spans));
        }
    } else {
        for (key_str, _action, desc) in &bindings {
            let key_col = format!(" {key_str:<10} ");
            let desc_budget = max_w.saturating_sub(key_col.chars().count());
            let clipped_desc: String = desc.chars().take(desc_budget).collect();
            lines.push(Line::from(vec![
                Span::styled(key_col, theme.help_overlay.add_modifier(Modifier::BOLD)),
                Span::styled(clipped_desc, theme.help_overlay),
            ]));
        }
    }

    let footer = " Press Esc or ? to close";
    lines.push(Line::from(""));
    lines.push(Line::from(Span::styled(
        footer.chars().take(max_w).collect::<String>(),
        theme.muted,
    )));

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use std::collections::HashMap;

    #[test]
    fn help_overlay_matches_keymap_bindings_with_zero_drift() {
        let keymap = Keymap::default();
        let theme = Theme::default();
        let backend = TestBackend::new(80, 24);
        let mut terminal = Terminal::new(backend).unwrap();

        terminal
            .draw(|f| render(f, f.area(), &keymap, &theme))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut text = String::new();
        for y in 0..24 {
            let row: String = (0..80)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            text.push_str(&row);
            text.push('\n');
        }

        for (key_str, _action, desc) in keymap.bindings() {
            assert!(text.contains(&key_str), "Missing key {key_str}");
            assert!(text.contains(desc), "Missing desc {desc}");
        }
    }

    #[test]
    fn help_overlay_reflects_custom_rebound_keymap() {
        let mut overrides = HashMap::new();
        overrides.insert("down".to_string(), "n".to_string());
        let keymap = Keymap::from_config(&overrides).unwrap();
        let theme = Theme::default();

        let backend = TestBackend::new(100, 40);
        let mut terminal = Terminal::new(backend).unwrap();
        terminal
            .draw(|f| render(f, f.area(), &keymap, &theme))
            .unwrap();

        let buf = terminal.backend().buffer();
        let mut lines = Vec::new();
        for y in 0..40 {
            let row: String = (0..100)
                .map(|x| buf[(x, y)].symbol().chars().next().unwrap_or(' '))
                .collect();
            lines.push(row);
        }
        let full = lines.join("\n");

        // Line containing "Move selection down" should have "n" and NOT "j"
        let down_line = lines
            .iter()
            .find(|l| l.contains("Move selection down"))
            .expect("must find down description");
        assert!(down_line.contains(" n "));
        assert!(!down_line.contains(" j "));
        assert!(full.contains("Keybindings"));
    }
}
