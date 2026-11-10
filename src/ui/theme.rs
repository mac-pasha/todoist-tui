//! Semantic UI colour theme and terminal colour-depth detection (T-604, EPIC-07).
//!
//! [`Theme`] exposes named semantic [`ratatui::style::Style`] fields consumed
//! by the sidebar, task list, detail pane, status bar, and help overlay.
//! [`ColorDepth::detect`] inspects injected `COLORTERM` / `TERM` values and
//! [`Theme::for_depth`] downgrades 24-bit `Color::Rgb` colours to the nearest
//! xterm-256 (`Color::Indexed`) or basic 16-colour ANSI [`Color`].

// Ships ahead of its widget consumers in EPIC-07; allow `dead_code` at module
// scope just like `src/error.rs` and `src/ui/mod.rs`.
#![allow(dead_code)]

use ratatui::style::{Color, Modifier, Style};

/// Name of the built-in default theme matching `DEFAULT_THEME` in
/// `src/config/model.rs`.
pub const DEFAULT_THEME_NAME: &str = "default";

/// Terminal colour capability level.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ColorDepth {
    /// 24-bit RGB colour (`COLORTERM=truecolor` or `COLORTERM=24bit`).
    #[default]
    TrueColor,
    /// 8-bit 256-colour palette (`TERM` contains `256color`).
    Ansi256,
    /// Basic 16-colour ANSI palette (fallback when neither indicator is set).
    Ansi16,
}

impl ColorDepth {
    /// Detects terminal colour capability from an injected environment lookup.
    ///
    /// Detection rules (in priority order):
    /// 1. If `COLORTERM` (case-insensitive) contains `"truecolor"` or `"24bit"`,
    ///    returns [`ColorDepth::TrueColor`].
    /// 2. Otherwise, if `TERM` (case-insensitive) contains `"256color"`,
    ///    returns [`ColorDepth::Ansi256`].
    /// 3. Otherwise, returns [`ColorDepth::Ansi16`].
    #[must_use]
    pub fn detect<F>(mut env: F) -> Self
    where
        F: FnMut(&str) -> Option<String>,
    {
        if let Some(colorterm) = env("COLORTERM") {
            let lower = colorterm.to_ascii_lowercase();
            if lower.contains("truecolor") || lower.contains("24bit") {
                return Self::TrueColor;
            }
        }

        if let Some(term) = env("TERM") {
            let lower = term.to_ascii_lowercase();
            if lower.contains("256color") {
                return Self::Ansi256;
            }
        }

        Self::Ansi16
    }

    /// Detects terminal colour capability from the current process environment.
    #[must_use]
    pub fn from_env() -> Self {
        Self::detect(|key| std::env::var(key).ok())
    }
}

/// Semantic style palette for the TUI shell.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Theme {
    /// Unselected row in the left sidebar.
    pub sidebar: Style,
    /// Active/selected row in the left sidebar.
    pub sidebar_selected: Style,
    /// Unselected row in the task list.
    pub list_row: Style,
    /// Active/selected row in the task list.
    pub list_row_selected: Style,
    /// Todoist P1 (urgent / red) indicator style.
    pub priority_p1: Style,
    /// Todoist P2 (high / orange) indicator style.
    pub priority_p2: Style,
    /// Todoist P3 (medium / blue) indicator style.
    pub priority_p3: Style,
    /// Todoist P4 (normal / muted) indicator style.
    pub priority_p4: Style,
    /// Overdue due-date / deadline badge style.
    pub overdue: Style,
    /// Task `@label` badge style.
    pub label: Style,
    /// Status-bar `Synced` / healthy indicator style.
    pub status_ok: Style,
    /// Status-bar `Syncing` / `Offline` / `Pending` warning indicator style.
    pub status_warn: Style,
    /// Status-bar error indicator style.
    pub status_error: Style,
    /// Help overlay box and text style.
    pub help_overlay: Style,
    /// Unfocused pane border style.
    pub border: Style,
    /// Focused pane border style.
    pub border_focused: Style,
    /// Dimmed secondary text / placeholder style.
    pub muted: Style,
}

impl Default for Theme {
    /// Built-in `"default"` truecolor palette.
    fn default() -> Self {
        Self {
            sidebar: Style::default().fg(Color::Rgb(212, 212, 216)),
            sidebar_selected: Style::default()
                .fg(Color::Rgb(255, 255, 255))
                .bg(Color::Rgb(59, 66, 82))
                .add_modifier(Modifier::BOLD),
            list_row: Style::default().fg(Color::Rgb(229, 233, 240)),
            list_row_selected: Style::default()
                .fg(Color::Rgb(255, 255, 255))
                .bg(Color::Rgb(67, 76, 94))
                .add_modifier(Modifier::BOLD),
            priority_p1: Style::default()
                .fg(Color::Rgb(220, 50, 47))
                .add_modifier(Modifier::BOLD),
            priority_p2: Style::default()
                .fg(Color::Rgb(235, 148, 58))
                .add_modifier(Modifier::BOLD),
            priority_p3: Style::default()
                .fg(Color::Rgb(64, 130, 230))
                .add_modifier(Modifier::BOLD),
            priority_p4: Style::default().fg(Color::Rgb(140, 148, 165)),
            overdue: Style::default()
                .fg(Color::Rgb(235, 87, 87))
                .add_modifier(Modifier::BOLD),
            label: Style::default().fg(Color::Rgb(163, 190, 140)),
            status_ok: Style::default().fg(Color::Rgb(130, 201, 138)),
            status_warn: Style::default()
                .fg(Color::Rgb(235, 203, 139))
                .add_modifier(Modifier::BOLD),
            status_error: Style::default()
                .fg(Color::Rgb(240, 96, 96))
                .add_modifier(Modifier::BOLD),
            help_overlay: Style::default()
                .fg(Color::Rgb(236, 239, 244))
                .bg(Color::Rgb(46, 52, 64)),
            border: Style::default().fg(Color::Rgb(92, 99, 112)),
            border_focused: Style::default()
                .fg(Color::Rgb(136, 192, 208))
                .add_modifier(Modifier::BOLD),
            muted: Style::default().fg(Color::Rgb(124, 132, 148)),
        }
    }
}

impl Theme {
    /// Resolves a theme by name from `Config::theme`.
    ///
    /// # Fallback Behaviour
    ///
    /// Recognises the built-in `"default"` theme name (case-insensitive, surrounding
    /// whitespace ignored). Unknown theme names fall back deterministically to
    /// [`Theme::default`] without raising an error so a custom or future theme
    /// name in `config.toml` never prevents the TUI from starting.
    #[must_use]
    pub fn by_name(name: &str) -> Self {
        let normalized = name.trim().to_ascii_lowercase();
        let _ = normalized == DEFAULT_THEME_NAME;
        Self::default()
    }

    /// Resolves a theme by name and downgrades its colours to `depth`.
    #[must_use]
    pub fn by_name_for_depth(name: &str, depth: ColorDepth) -> Self {
        Self::by_name(name).with_depth(depth)
    }

    /// Returns the default theme adapted to `depth`.
    #[must_use]
    pub fn for_depth(depth: ColorDepth) -> Self {
        Self::default().with_depth(depth)
    }

    /// Downgrades all styles in `self` to match `depth`.
    #[must_use]
    pub fn with_depth(self, depth: ColorDepth) -> Self {
        match depth {
            ColorDepth::TrueColor => self,
            ColorDepth::Ansi256 | ColorDepth::Ansi16 => Self {
                sidebar: downgrade_style(self.sidebar, depth),
                sidebar_selected: downgrade_style(self.sidebar_selected, depth),
                list_row: downgrade_style(self.list_row, depth),
                list_row_selected: downgrade_style(self.list_row_selected, depth),
                priority_p1: downgrade_style(self.priority_p1, depth),
                priority_p2: downgrade_style(self.priority_p2, depth),
                priority_p3: downgrade_style(self.priority_p3, depth),
                priority_p4: downgrade_style(self.priority_p4, depth),
                overdue: downgrade_style(self.overdue, depth),
                label: downgrade_style(self.label, depth),
                status_ok: downgrade_style(self.status_ok, depth),
                status_warn: downgrade_style(self.status_warn, depth),
                status_error: downgrade_style(self.status_error, depth),
                help_overlay: downgrade_style(self.help_overlay, depth),
                border: downgrade_style(self.border, depth),
                border_focused: downgrade_style(self.border_focused, depth),
                muted: downgrade_style(self.muted, depth),
            },
        }
    }

    /// Returns every named semantic style in this [`Theme`] for inspection and
    /// testing.
    #[must_use]
    pub const fn styles(&self) -> [(&'static str, Style); 17] {
        [
            ("sidebar", self.sidebar),
            ("sidebar_selected", self.sidebar_selected),
            ("list_row", self.list_row),
            ("list_row_selected", self.list_row_selected),
            ("priority_p1", self.priority_p1),
            ("priority_p2", self.priority_p2),
            ("priority_p3", self.priority_p3),
            ("priority_p4", self.priority_p4),
            ("overdue", self.overdue),
            ("label", self.label),
            ("status_ok", self.status_ok),
            ("status_warn", self.status_warn),
            ("status_error", self.status_error),
            ("help_overlay", self.help_overlay),
            ("border", self.border),
            ("border_focused", self.border_focused),
            ("muted", self.muted),
        ]
    }
}

fn downgrade_style(mut style: Style, depth: ColorDepth) -> Style {
    if let Some(fg) = style.fg {
        style = style.fg(downgrade_color(fg, depth));
    }
    if let Some(bg) = style.bg {
        style = style.bg(downgrade_color(bg, depth));
    }
    style
}

/// Converts a single [`Color`] to the requested [`ColorDepth`].
#[must_use]
pub fn downgrade_color(color: Color, depth: ColorDepth) -> Color {
    match depth {
        ColorDepth::TrueColor => color,
        ColorDepth::Ansi256 => match color {
            Color::Rgb(r, g, b) => Color::Indexed(rgb_to_ansi256(r, g, b)),
            other => other,
        },
        ColorDepth::Ansi16 => match color {
            Color::Rgb(r, g, b) => rgb_to_ansi16(r, g, b),
            Color::Indexed(idx) => {
                let (r, g, b) = ansi256_to_rgb(idx);
                rgb_to_ansi16(r, g, b)
            }
            other => other,
        },
    }
}

const CUBE_LEVELS: [u8; 6] = [0, 95, 135, 175, 215, 255];

const ANSI16_PALETTE: [(Color, (u8, u8, u8)); 16] = [
    (Color::Black, (0, 0, 0)),
    (Color::Red, (170, 0, 0)),
    (Color::Green, (0, 170, 0)),
    (Color::Yellow, (170, 85, 0)),
    (Color::Blue, (0, 0, 170)),
    (Color::Magenta, (170, 0, 170)),
    (Color::Cyan, (0, 170, 170)),
    (Color::Gray, (170, 170, 170)),
    (Color::DarkGray, (85, 85, 85)),
    (Color::LightRed, (255, 85, 85)),
    (Color::LightGreen, (85, 255, 85)),
    (Color::LightYellow, (255, 255, 85)),
    (Color::LightBlue, (85, 85, 255)),
    (Color::LightMagenta, (255, 85, 255)),
    (Color::LightCyan, (85, 255, 255)),
    (Color::White, (255, 255, 255)),
];

/// Maps an RGB triple to the nearest xterm-256 palette index (`16..=255`).
#[must_use]
pub fn rgb_to_ansi256(r: u8, g: u8, b: u8) -> u8 {
    let mut best_idx = 16u8;
    let mut best_dist = u32::MAX;

    for idx in 16u8..=255u8 {
        let (pr, pg, pb) = ansi256_to_rgb(idx);
        let dist = color_distance_sq(r, g, b, pr, pg, pb);
        if dist < best_dist {
            best_dist = dist;
            best_idx = idx;
            if dist == 0 {
                break;
            }
        }
    }

    best_idx
}

/// Maps an RGB triple to the nearest basic 16-colour ANSI [`Color`].
#[must_use]
pub fn rgb_to_ansi16(r: u8, g: u8, b: u8) -> Color {
    let mut best_color = Color::White;
    let mut best_dist = u32::MAX;

    for &(candidate, (pr, pg, pb)) in &ANSI16_PALETTE {
        let dist = color_distance_sq(r, g, b, pr, pg, pb);
        if dist < best_dist {
            best_dist = dist;
            best_color = candidate;
            if dist == 0 {
                break;
            }
        }
    }

    best_color
}

fn ansi256_to_rgb(idx: u8) -> (u8, u8, u8) {
    if idx < 16 {
        return ANSI16_PALETTE[usize::from(idx)].1;
    }
    if idx <= 231 {
        let offset = usize::from(idx - 16);
        let r = CUBE_LEVELS[offset / 36];
        let g = CUBE_LEVELS[(offset / 6) % 6];
        let b = CUBE_LEVELS[offset % 6];
        return (r, g, b);
    }
    let step = idx - 232;
    let level = 8 + step * 10;
    (level, level, level)
}

fn color_distance_sq(r1: u8, g1: u8, b1: u8, r2: u8, g2: u8, b2: u8) -> u32 {
    let dr = u32::from(r1.abs_diff(r2));
    let dg = u32::from(g1.abs_diff(g2));
    let db = u32::from(b1.abs_diff(b2));
    dr * dr + dg * dg + db * db
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn color_depth_detects_truecolor_from_colorterm() {
        let depth = ColorDepth::detect(|key| match key {
            "COLORTERM" => Some(String::from("truecolor")),
            "TERM" => Some(String::from("xterm-256color")),
            _ => None,
        });
        assert_eq!(depth, ColorDepth::TrueColor);

        let depth_24bit = ColorDepth::detect(|key| match key {
            "COLORTERM" => Some(String::from("24BIT")),
            _ => None,
        });
        assert_eq!(depth_24bit, ColorDepth::TrueColor);
    }

    #[test]
    fn color_depth_detects_ansi256_from_term_when_colorterm_absent_or_other() {
        let depth = ColorDepth::detect(|key| match key {
            "TERM" => Some(String::from("tmux-256color")),
            _ => None,
        });
        assert_eq!(depth, ColorDepth::Ansi256);

        let depth_with_non_truecolor = ColorDepth::detect(|key| match key {
            "COLORTERM" => Some(String::from("rxvt")),
            "TERM" => Some(String::from("xterm-256color")),
            _ => None,
        });
        assert_eq!(depth_with_non_truecolor, ColorDepth::Ansi256);
    }

    #[test]
    fn color_depth_falls_back_to_ansi16() {
        let depth = ColorDepth::detect(|key| match key {
            "TERM" => Some(String::from("xterm")),
            _ => None,
        });
        assert_eq!(depth, ColorDepth::Ansi16);

        let empty = ColorDepth::detect(|_| None);
        assert_eq!(empty, ColorDepth::Ansi16);
    }

    #[test]
    fn theme_for_depth_leaves_no_rgb_in_ansi256_or_ansi16() {
        let truecolor = Theme::for_depth(ColorDepth::TrueColor);
        assert!(
            truecolor
                .styles()
                .iter()
                .any(|(_, s)| matches!(s.fg, Some(Color::Rgb(..)))),
            "TrueColor theme should use Color::Rgb"
        );

        let ansi256 = Theme::for_depth(ColorDepth::Ansi256);
        for (name, style) in ansi256.styles() {
            assert!(
                !matches!(style.fg, Some(Color::Rgb(..))),
                "Ansi256 style `{name}` fg still has Color::Rgb: {:?}",
                style.fg
            );
            assert!(
                !matches!(style.bg, Some(Color::Rgb(..))),
                "Ansi256 style `{name}` bg still has Color::Rgb: {:?}",
                style.bg
            );
            assert!(
                matches!(style.fg, Some(Color::Indexed(_))),
                "Ansi256 style `{name}` fg should be Color::Indexed, got: {:?}",
                style.fg
            );
        }

        let ansi16 = Theme::for_depth(ColorDepth::Ansi16);
        for (name, style) in ansi16.styles() {
            assert!(
                !matches!(style.fg, Some(Color::Rgb(..) | Color::Indexed(_))),
                "Ansi16 style `{name}` fg still has Rgb/Indexed: {:?}",
                style.fg
            );
            assert!(
                !matches!(style.bg, Some(Color::Rgb(..) | Color::Indexed(_))),
                "Ansi16 style `{name}` bg still has Rgb/Indexed: {:?}",
                style.bg
            );
        }
    }

    #[test]
    fn theme_by_name_resolves_default_and_falls_back_on_unknown_name() {
        let default_theme = Theme::by_name("default");
        assert_eq!(default_theme, Theme::default());

        let upper_default = Theme::by_name("  DEFAULT ");
        assert_eq!(upper_default, Theme::default());

        let unknown = Theme::by_name("solarized-nonexistent");
        assert_eq!(unknown, Theme::default());

        let unknown_256 = Theme::by_name_for_depth("nonexistent", ColorDepth::Ansi256);
        assert_eq!(unknown_256, Theme::for_depth(ColorDepth::Ansi256));
    }
}
