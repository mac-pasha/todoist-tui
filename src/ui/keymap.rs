//! Keyboard action vocabulary, default bindings and `[keymap]` configuration
//! overrides (T-603, EPIC-07).
//!
//! This module is pure data and parsing logic — no terminal I/O and no
//! rendering — so both the event loop (`src/ui/run.rs`) and the help overlay
//! (`src/ui/view/help.rs`) can consume a single source of truth without drift.

// Ships ahead of its event-loop and widget consumers in EPIC-07; allow
// `dead_code` at module scope just like `src/error.rs` and `src/ui/mod.rs`.
#![allow(dead_code)]

use std::collections::{BTreeMap, HashMap};
use std::fmt;
use std::str::FromStr;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::error::{Result, UiError};

/// User-level actions dispatched by the TUI shell.
///
/// Variant declaration order matches the canonical table order in `README.md`
/// (`<!-- BEGIN KEYMAP -->`) so [`Keymap::bindings`] yields rows in a stable,
/// deterministic sequence.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Action {
    /// Move selection down in the focused list/sidebar pane (`j`).
    Down,
    /// Move selection up in the focused list/sidebar pane (`k`).
    Up,
    /// Move focus left or collapse the selected sidebar project (`h`).
    Left,
    /// Move focus right or expand the selected sidebar project (`l`).
    Right,
    /// Jump to the first item (`g`).
    First,
    /// Jump to the last item (`G`).
    Last,
    /// Open the search prompt (`/`).
    OpenSearch,
    /// Trigger an immediate sync (`r`).
    SyncNow,
    /// Toggle the help overlay (`?`).
    ToggleHelp,
    /// Quit the application (`q`).
    Quit,
    /// Open add-task form (`a`).
    AddTask,
    /// Open add-subtask form (`A`).
    AddSubtask,
    /// Open edit-task form (`e`).
    EditTask,
    /// Quick-edit priority (`p`).
    QuickPriority,
    /// Quick-edit due date (`d`).
    QuickDue,
    /// Toggle completion of selected task (`x`).
    ToggleComplete,
    /// Delete selected task with confirmation (`D`).
    DeleteTask,
    /// Move selected task (`m`).
    MoveTask,
    /// Reorder task up (`K`).
    ReorderUp,
    /// Reorder task down (`J`).
    ReorderDown,
    /// Open comments pane/modal (`c`).
    Comments,
    /// Open failed-commands errors view (`E`).
    ViewErrors,
    /// Open create structure (project/section/label) modal (`N`).
    StructureNew,
    /// Open rename/edit structure modal (`M`).
    StructureEdit,
    /// Open archive/delete structure confirmation (`X`).
    StructureDelete,
    /// Toggle search scope between current view and all tasks (`Tab`).
    ToggleSearchScope,
}

impl Action {
    /// Every [`Action`] variant in canonical display order.
    pub const ALL: [Self; 26] = [
        Self::Down,
        Self::Up,
        Self::Left,
        Self::Right,
        Self::First,
        Self::Last,
        Self::OpenSearch,
        Self::SyncNow,
        Self::ToggleHelp,
        Self::Quit,
        Self::AddTask,
        Self::AddSubtask,
        Self::EditTask,
        Self::QuickPriority,
        Self::QuickDue,
        Self::ToggleComplete,
        Self::DeleteTask,
        Self::MoveTask,
        Self::ReorderUp,
        Self::ReorderDown,
        Self::Comments,
        Self::ViewErrors,
        Self::StructureNew,
        Self::StructureEdit,
        Self::StructureDelete,
        Self::ToggleSearchScope,
    ];

    /// Canonical `snake_case` identifier used in `config.toml`'s `[keymap]` table.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Down => "down",
            Self::Up => "up",
            Self::Left => "left",
            Self::Right => "right",
            Self::First => "first",
            Self::Last => "last",
            Self::OpenSearch => "open_search",
            Self::SyncNow => "sync_now",
            Self::ToggleHelp => "toggle_help",
            Self::Quit => "quit",
            Self::AddTask => "add_task",
            Self::AddSubtask => "add_subtask",
            Self::EditTask => "edit_task",
            Self::QuickPriority => "quick_priority",
            Self::QuickDue => "quick_due",
            Self::ToggleComplete => "toggle_complete",
            Self::DeleteTask => "delete_task",
            Self::MoveTask => "move_task",
            Self::ReorderUp => "reorder_up",
            Self::ReorderDown => "reorder_down",
            Self::Comments => "comments",
            Self::ViewErrors => "view_errors",
            Self::StructureNew => "structure_new",
            Self::StructureEdit => "structure_edit",
            Self::StructureDelete => "structure_delete",
            Self::ToggleSearchScope => "toggle_search_scope",
        }
    }

    /// Concise human-readable description rendered in the help overlay.
    #[must_use]
    pub const fn description(self) -> &'static str {
        match self {
            Self::Down => "Move selection down",
            Self::Up => "Move selection up",
            Self::Left => "Move focus left / collapse",
            Self::Right => "Move focus right / expand",
            Self::First => "Jump to first item",
            Self::Last => "Jump to last item",
            Self::OpenSearch => "Open search prompt",
            Self::SyncNow => "Trigger immediate sync",
            Self::ToggleHelp => "Toggle help overlay",
            Self::Quit => "Quit application",
            Self::AddTask => "Add task",
            Self::AddSubtask => "Add sub-task",
            Self::EditTask => "Edit selected task",
            Self::QuickPriority => "Quick-edit task priority",
            Self::QuickDue => "Quick-edit task due date",
            Self::ToggleComplete => "Complete / uncomplete task",
            Self::DeleteTask => "Delete selected task",
            Self::MoveTask => "Move selected task",
            Self::ReorderUp => "Move task up in list",
            Self::ReorderDown => "Move task down in list",
            Self::Comments => "View / add comments",
            Self::ViewErrors => "Open sync errors view",
            Self::StructureNew => "Create project / section / label",
            Self::StructureEdit => "Rename project / section / label",
            Self::StructureDelete => "Archive / delete structure item",
            Self::ToggleSearchScope => "Toggle search scope (view/all)",
        }
    }

    /// Pane context label matching the `Context` column of `README.md`.
    #[must_use]
    pub const fn context(self) -> &'static str {
        match self {
            Self::Down | Self::Up => "Sidebar, task list",
            Self::First
            | Self::Last
            | Self::EditTask
            | Self::QuickPriority
            | Self::QuickDue
            | Self::ToggleComplete
            | Self::DeleteTask
            | Self::MoveTask
            | Self::ReorderUp
            | Self::ReorderDown
            | Self::Comments => "Task list",
            Self::StructureNew | Self::StructureEdit | Self::StructureDelete => "Sidebar",
            Self::ToggleSearchScope => "Search",
            Self::Left
            | Self::Right
            | Self::OpenSearch
            | Self::SyncNow
            | Self::ToggleHelp
            | Self::Quit
            | Self::AddTask
            | Self::AddSubtask
            | Self::ViewErrors => "Global",
        }
    }

    /// Built-in default [`KeyBinding`] for this action.
    #[must_use]
    pub const fn default_key(self) -> KeyBinding {
        match self {
            Self::Down => KeyBinding::char('j'),
            Self::Up => KeyBinding::char('k'),
            Self::Left => KeyBinding::char('h'),
            Self::Right => KeyBinding::char('l'),
            Self::First => KeyBinding::char('g'),
            Self::Last => KeyBinding::char('G'),
            Self::OpenSearch => KeyBinding::char('/'),
            Self::SyncNow => KeyBinding::char('r'),
            Self::ToggleHelp => KeyBinding::char('?'),
            Self::Quit => KeyBinding::char('q'),
            Self::AddTask => KeyBinding::char('a'),
            Self::AddSubtask => KeyBinding::char('A'),
            Self::EditTask => KeyBinding::char('e'),
            Self::QuickPriority => KeyBinding::char('p'),
            Self::QuickDue => KeyBinding::char('d'),
            Self::ToggleComplete => KeyBinding::char('x'),
            Self::DeleteTask => KeyBinding::char('D'),
            Self::MoveTask => KeyBinding::char('m'),
            Self::ReorderUp => KeyBinding::char('K'),
            Self::ReorderDown => KeyBinding::char('J'),
            Self::Comments => KeyBinding::char('c'),
            Self::ViewErrors => KeyBinding::char('E'),
            Self::StructureNew => KeyBinding::char('N'),
            Self::StructureEdit => KeyBinding::char('M'),
            Self::StructureDelete => KeyBinding::char('X'),
            Self::ToggleSearchScope => KeyBinding {
                code: KeyCode::Tab,
                modifiers: KeyModifiers::NONE,
            },
        }
    }

    /// Resolves an action name from `config.toml`.
    ///
    /// Accepts canonical `snake_case` names (`"down"`, `"open_search"`),
    /// `kebab-case` (`"open-search"`), and `PascalCase` (`"OpenSearch"`).
    ///
    /// # Errors
    ///
    /// Returns [`UiError::Keymap`] wrapped in [`crate::error::AppError`] when
    /// `name` does not match any known action.
    pub fn from_name(name: &str) -> Result<Self> {
        let trimmed = name.trim();
        let normalized = trimmed.to_ascii_lowercase().replace('-', "_");
        match normalized.as_str() {
            "down" => Ok(Self::Down),
            "up" => Ok(Self::Up),
            "left" => Ok(Self::Left),
            "right" => Ok(Self::Right),
            "first" => Ok(Self::First),
            "last" => Ok(Self::Last),
            "open_search" | "opensearch" => Ok(Self::OpenSearch),
            "sync_now" | "syncnow" => Ok(Self::SyncNow),
            "toggle_help" | "togglehelp" => Ok(Self::ToggleHelp),
            "quit" => Ok(Self::Quit),
            "add_task" | "addtask" => Ok(Self::AddTask),
            "add_subtask" | "addsubtask" => Ok(Self::AddSubtask),
            "edit_task" | "edittask" => Ok(Self::EditTask),
            "quick_priority" | "quickpriority" => Ok(Self::QuickPriority),
            "quick_due" | "quickdue" => Ok(Self::QuickDue),
            "toggle_complete" | "togglecomplete" => Ok(Self::ToggleComplete),
            "delete_task" | "deletetask" => Ok(Self::DeleteTask),
            "move_task" | "movetask" => Ok(Self::MoveTask),
            "reorder_up" | "reorderup" => Ok(Self::ReorderUp),
            "reorder_down" | "reorderdown" => Ok(Self::ReorderDown),
            "comments" => Ok(Self::Comments),
            "view_errors" | "viewerrors" => Ok(Self::ViewErrors),
            "structure_new" | "structurenew" => Ok(Self::StructureNew),
            "structure_edit" | "structureedit" => Ok(Self::StructureEdit),
            "structure_delete" | "structuredelete" => Ok(Self::StructureDelete),
            "toggle_search_scope" | "togglesearchscope" => Ok(Self::ToggleSearchScope),
            _ => Err(UiError::Keymap {
                message: format!("unknown action `{name}`"),
            }
            .into()),
        }
    }
}

impl fmt::Display for Action {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(self.as_str())
    }
}

impl FromStr for Action {
    type Err = crate::error::AppError;

    fn from_str(s: &str) -> Result<Self> {
        Self::from_name(s)
    }
}

/// Normalized key combination (key code + modifiers) used as a lookup key in
/// [`Keymap`].
///
/// Character keys normalize away `KeyModifiers::SHIFT` because the shifted
/// character itself (e.g. `'G'` vs `'g'`, `'?'` vs `'/'`) already encodes case;
/// this ensures both `KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT)`
/// (emitted by terminals) and `KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE)`
/// (often constructed in tests) resolve identically.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct KeyBinding {
    /// Normalized key code.
    pub code: KeyCode,
    /// Normalized key modifiers.
    pub modifiers: KeyModifiers,
}

impl KeyBinding {
    /// Constructs a normalized [`KeyBinding`] from a `crossterm` code and
    /// modifier set.
    #[must_use]
    pub fn new(code: KeyCode, modifiers: KeyModifiers) -> Self {
        let mut clean_mods =
            modifiers & (KeyModifiers::CONTROL | KeyModifiers::ALT | KeyModifiers::SHIFT);
        let clean_code = match code {
            KeyCode::Char(c) => {
                // `Char` already carries case/symbol shifts; strip SHIFT so
                // terminal events with SHIFT and synthetic events without it
                // compare equal.
                clean_mods.remove(KeyModifiers::SHIFT);
                if clean_mods.intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) {
                    KeyCode::Char(c.to_ascii_lowercase())
                } else {
                    KeyCode::Char(c)
                }
            }
            KeyCode::BackTab => {
                clean_mods.remove(KeyModifiers::SHIFT);
                KeyCode::BackTab
            }
            other => other,
        };
        Self {
            code: clean_code,
            modifiers: clean_mods,
        }
    }

    /// Constructs an unmodified character [`KeyBinding`].
    #[must_use]
    pub const fn char(c: char) -> Self {
        Self {
            code: KeyCode::Char(c),
            modifiers: KeyModifiers::NONE,
        }
    }

    /// Normalizes a `crossterm` [`KeyEvent`] for lookup in [`Keymap`].
    #[must_use]
    pub fn from_event(event: KeyEvent) -> Self {
        Self::new(event.code, event.modifiers)
    }
}

impl fmt::Display for KeyBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&render_key(self))
    }
}

/// Parses a human-readable key specification (such as `"j"`, `"G"`, `"/"`,
/// `"Ctrl+r"`, `"Alt+Enter"`, or `"Space"`) into a normalized [`KeyBinding`].
///
/// # Errors
///
/// Returns [`UiError::Keymap`] wrapped in [`crate::error::AppError`] if `raw`
/// is empty, names an unknown modifier, or names an unrecognised key.
pub fn parse_key(raw: &str) -> Result<KeyBinding> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(UiError::Keymap {
            message: format!("unparsable key `{raw}`: key string must not be empty"),
        }
        .into());
    }

    // A single character (including `+`, `/`, `?`, `G`) is always a plain
    // character binding with no modifiers.
    let mut chars = trimmed.chars();
    if let (Some(single), None) = (chars.next(), chars.next()) {
        return Ok(KeyBinding::char(single));
    }

    let mut modifiers = KeyModifiers::NONE;
    let parts: Vec<&str> = trimmed.split('+').collect();
    if parts.iter().any(|p| p.trim().is_empty()) {
        return Err(UiError::Keymap {
            message: format!("unparsable key `{raw}`: empty modifier or key token"),
        }
        .into());
    }

    let (mod_parts, key_part) = parts.split_at(parts.len() - 1);
    for mod_token in mod_parts {
        let flag = match mod_token.trim().to_ascii_lowercase().as_str() {
            "ctrl" | "control" | "c" => KeyModifiers::CONTROL,
            "alt" | "meta" | "a" | "m" => KeyModifiers::ALT,
            "shift" | "s" => KeyModifiers::SHIFT,
            _ => {
                return Err(UiError::Keymap {
                    message: format!("unparsable key `{raw}`: unknown modifier `{mod_token}`"),
                }
                .into());
            }
        };
        if modifiers.contains(flag) {
            return Err(UiError::Keymap {
                message: format!("unparsable key `{raw}`: duplicate modifier `{mod_token}`"),
            }
            .into());
        }
        modifiers.insert(flag);
    }

    let mut code = parse_base_key(key_part[0].trim(), raw)?;
    if modifiers.contains(KeyModifiers::SHIFT) {
        if let KeyCode::Char(c) = code {
            code = KeyCode::Char(c.to_ascii_uppercase());
        }
    }

    Ok(KeyBinding::new(code, modifiers))
}

fn parse_base_key(token: &str, raw: &str) -> Result<KeyCode> {
    let mut chars = token.chars();
    if let (Some(single), None) = (chars.next(), chars.next()) {
        return Ok(KeyCode::Char(single));
    }

    match token.to_ascii_lowercase().as_str() {
        "enter" | "return" | "cr" => Ok(KeyCode::Enter),
        "esc" | "escape" => Ok(KeyCode::Esc),
        "tab" => Ok(KeyCode::Tab),
        "backtab" => Ok(KeyCode::BackTab),
        "backspace" | "bs" => Ok(KeyCode::Backspace),
        "space" | "spc" => Ok(KeyCode::Char(' ')),
        "up" => Ok(KeyCode::Up),
        "down" => Ok(KeyCode::Down),
        "left" => Ok(KeyCode::Left),
        "right" => Ok(KeyCode::Right),
        "home" => Ok(KeyCode::Home),
        "end" => Ok(KeyCode::End),
        "pageup" | "pgup" => Ok(KeyCode::PageUp),
        "pagedown" | "pgdn" => Ok(KeyCode::PageDown),
        "delete" | "del" => Ok(KeyCode::Delete),
        "insert" | "ins" => Ok(KeyCode::Insert),
        _ => Err(UiError::Keymap {
            message: format!("unparsable key `{raw}`: unknown key `{token}`"),
        }
        .into()),
    }
}

/// Formats a [`KeyBinding`] back into its canonical string representation.
///
/// Satisfies `parse_key(&render_key(&k)) == Ok(k)` for all supported bindings.
#[must_use]
pub fn render_key(binding: &KeyBinding) -> String {
    let mut out = String::new();
    if binding.modifiers.contains(KeyModifiers::CONTROL) {
        out.push_str("Ctrl+");
    }
    if binding.modifiers.contains(KeyModifiers::ALT) {
        out.push_str("Alt+");
    }
    if binding.modifiers.contains(KeyModifiers::SHIFT)
        && !matches!(binding.code, KeyCode::Char(_) | KeyCode::BackTab)
    {
        out.push_str("Shift+");
    }

    match binding.code {
        KeyCode::Char(' ') => out.push_str("Space"),
        KeyCode::Char(c) => out.push(c),
        KeyCode::Enter => out.push_str("Enter"),
        KeyCode::Esc => out.push_str("Esc"),
        KeyCode::Tab => out.push_str("Tab"),
        KeyCode::BackTab => out.push_str("BackTab"),
        KeyCode::Backspace => out.push_str("Backspace"),
        KeyCode::Up => out.push_str("Up"),
        KeyCode::Down => out.push_str("Down"),
        KeyCode::Left => out.push_str("Left"),
        KeyCode::Right => out.push_str("Right"),
        KeyCode::Home => out.push_str("Home"),
        KeyCode::End => out.push_str("End"),
        KeyCode::PageUp => out.push_str("PageUp"),
        KeyCode::PageDown => out.push_str("PageDown"),
        KeyCode::Delete => out.push_str("Delete"),
        KeyCode::Insert => out.push_str("Insert"),
        KeyCode::F(n) => {
            out.push('F');
            out.push_str(&n.to_string());
        }
        _ => out.push_str("Unknown"),
    }

    out
}

/// Active keybinding table mapping terminal key events to [`Action`]s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Keymap {
    action_to_key: BTreeMap<Action, KeyBinding>,
    key_to_action: HashMap<KeyBinding, Action>,
}

impl Default for Keymap {
    /// Builds the default keymap specified by EPIC-07 (`j`, `k`, `h`, `l`,
    /// `g`, `G`, `/`, `r`, `?`, `q`).
    fn default() -> Self {
        let mut action_to_key = BTreeMap::new();
        let mut key_to_action = HashMap::with_capacity(Action::ALL.len());
        for action in Action::ALL {
            let key = action.default_key();
            action_to_key.insert(action, key);
            key_to_action.insert(key, action);
        }
        Self {
            action_to_key,
            key_to_action,
        }
    }
}

impl Keymap {
    /// Applies user overrides from `Config::keymap` (`HashMap<String, String>`)
    /// on top of [`Keymap::default`].
    ///
    /// Supported entry forms:
    /// 1. `action_name = "new_key"` (e.g. `"down" = "n"`): binds `Action::Down`
    ///    to `n` and unbinds its previous key (`j`).
    /// 2. `default_key = "new_key"` (e.g. `"j" = "n"`): looks up the action
    ///    bound to `j` in [`Keymap::default`] (`Action::Down`), rebinds it to
    ///    `n`, and leaves `j` unbound.
    /// 3. `"new_key" = action_name` (e.g. `"n" = "down"`): binds `Action::Down`
    ///    to `n` and unbinds its previous key (`j`).
    ///
    /// # Errors
    ///
    /// Returns [`UiError::Keymap`] wrapped in [`crate::error::AppError`] if any
    /// entry contains an unknown action name or an unparsable key string,
    /// naming the offending entry in the error message.
    pub fn from_config(overrides: &HashMap<String, String>) -> Result<Self> {
        let defaults = Self::default();
        let mut sorted_entries: Vec<(&String, &String)> = overrides.iter().collect();
        sorted_entries.sort_by_key(|(ka, _)| *ka);

        let mut resolved: Vec<(Action, KeyBinding)> = Vec::with_capacity(sorted_entries.len());
        for (raw_key, raw_val) in sorted_entries {
            let pair = resolve_override_entry(&defaults, raw_key, raw_val)?;
            resolved.push(pair);
        }

        let mut action_to_key = defaults.action_to_key;
        for &(action, _) in &resolved {
            action_to_key.remove(&action);
        }
        for (action, new_key) in resolved {
            action_to_key.retain(|_, existing_key| *existing_key != new_key);
            action_to_key.insert(action, new_key);
        }

        let key_to_action = action_to_key
            .iter()
            .map(|(&action, &key)| (key, action))
            .collect();

        Ok(Self {
            action_to_key,
            key_to_action,
        })
    }

    /// Resolves a `crossterm` [`KeyEvent`] to its bound [`Action`], if any.
    #[must_use]
    pub fn lookup(&self, event: KeyEvent) -> Option<Action> {
        let binding = KeyBinding::from_event(event);
        self.key_to_action.get(&binding).copied()
    }

    /// Returns the active [`KeyBinding`] for `action`, if bound.
    #[must_use]
    pub fn key_for(&self, action: Action) -> Option<KeyBinding> {
        self.action_to_key.get(&action).copied()
    }

    /// Returns a deterministic, sorted list of `(key_string, Action, description)`
    /// tuples for every bound action in canonical [`Action`] order.
    ///
    /// The help overlay (`src/ui/view/help.rs`) and documentation drift tests
    /// consume this directly so help text never drifts from the active keymap.
    #[must_use]
    pub fn bindings(&self) -> Vec<(String, Action, &'static str)> {
        self.action_to_key
            .iter()
            .map(|(&action, key)| (render_key(key), action, action.description()))
            .collect()
    }
}

fn resolve_override_entry(
    defaults: &Keymap,
    raw_key: &str,
    raw_val: &str,
) -> Result<(Action, KeyBinding)> {
    // Form 1: action_name -> key_string (e.g. "down" = "n").
    if let Ok(action) = Action::from_name(raw_key) {
        let binding = parse_key(raw_val).map_err(|_| UiError::Keymap {
            message: format!("invalid key `{raw_val}` for action `{raw_key}`"),
        })?;
        return Ok((action, binding));
    }

    // Form 2: default_key -> key_string (e.g. "j" = "n").
    if let Ok(source_key) = parse_key(raw_key) {
        if let Some(&default_action) = defaults.key_to_action.get(&source_key) {
            // Check whether raw_val is an action name first (e.g. "j" = "up")
            // or a replacement key string (e.g. "j" = "n").
            if let Ok(target_action) = Action::from_name(raw_val) {
                return Ok((target_action, source_key));
            }
            let new_key = parse_key(raw_val).map_err(|_| UiError::Keymap {
                message: format!("invalid key `{raw_val}` in keymap override `{raw_key}`"),
            })?;
            return Ok((default_action, new_key));
        }
    }

    // Form 3: key_string -> action_name (e.g. "n" = "down").
    if let Ok(action) = Action::from_name(raw_val) {
        let binding = parse_key(raw_key).map_err(|_| UiError::Keymap {
            message: format!("invalid key `{raw_key}` for action `{raw_val}`"),
        })?;
        return Ok((action, binding));
    }

    Err(UiError::Keymap {
        message: format!("unknown action or invalid keymap entry `{raw_key}` = `{raw_val}`"),
    }
    .into())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::error::AppError;

    #[test]
    fn default_keymap_resolves_all_ten_epic_07_bindings() {
        let km = Keymap::default();
        let cases = [
            (
                KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE),
                Action::Down,
            ),
            (
                KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE),
                Action::Up,
            ),
            (
                KeyEvent::new(KeyCode::Char('h'), KeyModifiers::NONE),
                Action::Left,
            ),
            (
                KeyEvent::new(KeyCode::Char('l'), KeyModifiers::NONE),
                Action::Right,
            ),
            (
                KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE),
                Action::First,
            ),
            (
                KeyEvent::new(KeyCode::Char('G'), KeyModifiers::NONE),
                Action::Last,
            ),
            (
                KeyEvent::new(KeyCode::Char('G'), KeyModifiers::SHIFT),
                Action::Last,
            ),
            (
                KeyEvent::new(KeyCode::Char('/'), KeyModifiers::NONE),
                Action::OpenSearch,
            ),
            (
                KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE),
                Action::SyncNow,
            ),
            (
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::NONE),
                Action::ToggleHelp,
            ),
            (
                KeyEvent::new(KeyCode::Char('?'), KeyModifiers::SHIFT),
                Action::ToggleHelp,
            ),
            (
                KeyEvent::new(KeyCode::Char('q'), KeyModifiers::NONE),
                Action::Quit,
            ),
        ];

        for (event, expected) in cases {
            assert_eq!(
                km.lookup(event),
                Some(expected),
                "failed lookup for {event:?}"
            );
        }

        assert_eq!(
            km.lookup(KeyEvent::new(KeyCode::Char('z'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn key_string_parsing_round_trips_for_every_default_binding_and_modifiers() {
        let km = Keymap::default();
        for (rendered, action, _) in km.bindings() {
            let parsed = parse_key(&rendered).expect("default rendered key should parse");
            assert_eq!(parsed, action.default_key());
            assert_eq!(render_key(&parsed), rendered);
        }

        let extra_keys = [
            "Ctrl+r",
            "Alt+j",
            "Ctrl+Alt+q",
            "Enter",
            "Esc",
            "Tab",
            "BackTab",
            "Backspace",
            "Space",
            "Up",
            "Down",
            "Left",
            "Right",
            "Home",
            "End",
            "PageUp",
            "PageDown",
            "Delete",
            "Insert",
            "Shift+Up",
        ];
        for sample in extra_keys {
            let parsed =
                parse_key(sample).unwrap_or_else(|e| panic!("failed to parse {sample}: {e}"));
            let round = render_key(&parsed);
            assert_eq!(round, sample, "render_key mismatch for {sample}");
            assert_eq!(parse_key(&round).unwrap(), parsed);
        }
    }

    #[test]
    fn from_config_rebinds_j_to_n_and_leaves_j_unbound() {
        let mut overrides = HashMap::new();
        overrides.insert(String::from("j"), String::from("n"));

        let km = Keymap::from_config(&overrides).expect("rebinding j -> n should succeed");
        assert_eq!(
            km.lookup(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
            Some(Action::Down)
        );
        assert_eq!(
            km.lookup(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            None
        );

        let bindings = km.bindings();
        assert!(bindings
            .iter()
            .any(|(k, a, _)| k == "n" && *a == Action::Down));
        assert!(!bindings.iter().any(|(k, _, _)| k == "j"));
    }

    #[test]
    fn from_config_supports_action_name_to_key_override() {
        let mut overrides = HashMap::new();
        overrides.insert(String::from("down"), String::from("n"));
        overrides.insert(String::from("sync_now"), String::from("Ctrl+r"));

        let km = Keymap::from_config(&overrides).expect("action name overrides should succeed");
        assert_eq!(
            km.lookup(KeyEvent::new(KeyCode::Char('n'), KeyModifiers::NONE)),
            Some(Action::Down)
        );
        assert_eq!(
            km.lookup(KeyEvent::new(KeyCode::Char('j'), KeyModifiers::NONE)),
            None
        );
        assert_eq!(
            km.lookup(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::CONTROL)),
            Some(Action::SyncNow)
        );
        assert_eq!(
            km.lookup(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE)),
            None
        );
    }

    #[test]
    fn from_config_rejects_unknown_action_names_and_unparsable_keys() {
        let mut bad_action = HashMap::new();
        bad_action.insert(String::from("fly_to_moon"), String::from("x"));
        let err = Keymap::from_config(&bad_action).expect_err("unknown action must fail");
        assert!(matches!(err, AppError::Ui(UiError::Keymap { .. })));
        assert!(
            err.to_string().contains("fly_to_moon"),
            "error should name offending entry, got: {err}"
        );

        let mut bad_key = HashMap::new();
        bad_key.insert(String::from("down"), String::from("Super+Hyper+Nope"));
        let err = Keymap::from_config(&bad_key).expect_err("unparsable key must fail");
        assert!(matches!(err, AppError::Ui(UiError::Keymap { .. })));
        let msg = err.to_string();
        assert!(
            msg.contains("Super+Hyper+Nope") && msg.contains("down"),
            "error should name offending entry, got: {msg}"
        );

        let mut empty_key = HashMap::new();
        empty_key.insert(String::from("j"), String::from("   "));
        let err = Keymap::from_config(&empty_key).expect_err("empty key must fail");
        assert!(matches!(err, AppError::Ui(UiError::Keymap { .. })));
    }

    #[test]
    fn bindings_returns_deterministic_sorted_list_matching_actions() {
        let km = Keymap::default();
        let bindings = km.bindings();
        assert_eq!(bindings.len(), Action::ALL.len());

        let expected = [
            ("j", Action::Down, "Move selection down"),
            ("k", Action::Up, "Move selection up"),
            ("h", Action::Left, "Move focus left / collapse"),
            ("l", Action::Right, "Move focus right / expand"),
            ("g", Action::First, "Jump to first item"),
            ("G", Action::Last, "Jump to last item"),
            ("/", Action::OpenSearch, "Open search prompt"),
            ("r", Action::SyncNow, "Trigger immediate sync"),
            ("?", Action::ToggleHelp, "Toggle help overlay"),
            ("q", Action::Quit, "Quit application"),
        ];

        for (actual, (exp_key, exp_action, exp_desc)) in bindings.iter().zip(expected.iter()) {
            assert_eq!(actual.0, *exp_key);
            assert_eq!(actual.1, *exp_action);
            assert_eq!(actual.2, *exp_desc);
        }
    }
}
