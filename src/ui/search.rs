//! TUI incremental `/` search prompt, highlighted results list, and jump-to-task
//! (`T-907`, EPIC-10).

#![allow(dead_code)]

use std::collections::{HashMap, HashSet};
use std::ops::Range;

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Paragraph};
use ratatui::Frame;
use rusqlite::Connection;

use crate::api::model::Id;
use crate::db::queries;
use crate::error::Result;
use crate::search::query::{search, MatchedField, SearchItem, SearchScope};
use crate::search::state::{SessionScope, ViewId as SearchViewId, ViewSnapshot};
use crate::ui::app::App;
use crate::ui::data::ViewData;
use crate::ui::keymap::{Action, Keymap};
use crate::ui::model::ViewId as UiViewId;
use crate::ui::theme::Theme;

/// Owned search hit prepared for TUI display.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchResultRow {
    pub id: String,
    pub content: String,
    pub description: String,
    pub project_id: String,
    pub project_name: String,
    pub field: MatchedField,
    pub ranges: Vec<Range<usize>>,
    pub score: i64,
}

/// Event emitted when processing a key press inside [`SearchOverlay`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchUiEvent {
    Pending,
    Cancelled(ViewSnapshot),
    JumpToTask { project_id: Id, task_id: Id },
}

/// Encodes a UI [`UiViewId`] into [`SearchViewId`] for `SearchSession` snapshots.
#[must_use]
pub fn encode_view_id(view: &UiViewId) -> SearchViewId {
    match view {
        UiViewId::Inbox => SearchViewId::new("builtin:inbox"),
        UiViewId::Today => SearchViewId::new("builtin:today"),
        UiViewId::Upcoming => SearchViewId::new("builtin:upcoming"),
        UiViewId::Project(id) => SearchViewId::new(format!("project:{}", id.as_str())),
        UiViewId::Label(name) => SearchViewId::new(format!("label:{name}")),
        UiViewId::Filter(id) => SearchViewId::new(format!("filter:{}", id.as_str())),
    }
}

/// Decodes a [`SearchViewId`] snapshot back into [`UiViewId`].
#[must_use]
pub fn decode_view_id(view: &SearchViewId) -> UiViewId {
    let raw = view.as_str();
    if raw == "builtin:today" {
        UiViewId::Today
    } else if raw == "builtin:upcoming" {
        UiViewId::Upcoming
    } else if let Some(id) = raw.strip_prefix("project:") {
        UiViewId::Project(Id::new(id))
    } else if let Some(name) = raw.strip_prefix("label:") {
        UiViewId::Label(name.to_string())
    } else if let Some(id) = raw.strip_prefix("filter:") {
        UiViewId::Filter(Id::new(id))
    } else {
        UiViewId::Inbox
    }
}

/// Loads active searchable items and a `project_id -> project_name` lookup table from SQLite.
///
/// # Errors
///
/// Propagates SQLite query failures.
pub fn load_search_items_from_db(
    conn: &Connection,
) -> Result<(Vec<SearchItem>, HashMap<String, String>)> {
    let projects = queries::all_projects(conn)?;
    let mut project_names = HashMap::new();
    for p in projects {
        if !p.is_deleted {
            project_names.insert(p.id.as_str().to_string(), p.name);
        }
    }

    let items = queries::all_active_items(conn)?
        .into_iter()
        .map(|it| SearchItem {
            id: it.id.as_str().to_string(),
            content: it.content,
            description: it.description,
            project_id: it.project_id.as_str().to_string(),
            section_id: it.section_id.map(|s| s.as_str().to_string()),
        })
        .collect();

    Ok((items, project_names))
}

/// Interactive incremental `/` search overlay backed by [`search`].
#[derive(Debug, Clone)]
pub struct SearchOverlay {
    items: Vec<SearchItem>,
    item_index: HashMap<String, usize>,
    project_names: HashMap<String, String>,
    current_view_ids: HashSet<String>,
    snapshot: ViewSnapshot,
    query: String,
    scope: SessionScope,
    results: Vec<SearchResultRow>,
    highlighted: usize,
}

impl SearchOverlay {
    /// Opens a search overlay snapshotting `app`'s current view and selected task.
    #[must_use]
    pub fn open(app: &App, items: Vec<SearchItem>, project_names: HashMap<String, String>) -> Self {
        let current_view_ids: HashSet<String> = app
            .tasks()
            .iter()
            .map(|t| t.id.as_str().to_string())
            .collect();
        let previous_view = encode_view_id(app.active_view());
        let previous_selection = app.selected_task().map(|t| t.id.as_str().to_string());
        let item_index = items
            .iter()
            .enumerate()
            .map(|(i, it)| (it.id.clone(), i))
            .collect();

        let mut overlay = Self {
            items,
            item_index,
            project_names,
            current_view_ids,
            snapshot: ViewSnapshot {
                previous_view,
                previous_selection,
            },
            query: String::new(),
            scope: SessionScope::CurrentView,
            results: Vec::new(),
            highlighted: 0,
        };
        overlay.recompute();
        overlay
    }

    /// Current query text.
    #[must_use]
    pub fn query(&self) -> &str {
        &self.query
    }

    /// Current search scope (`CurrentView` or `All`).
    #[must_use]
    pub const fn scope(&self) -> SessionScope {
        self.scope
    }

    /// Ranked search results for the current query and scope.
    #[must_use]
    pub fn results(&self) -> &[SearchResultRow] {
        &self.results
    }

    /// Highlighted result row index, if any results exist.
    #[must_use]
    pub const fn highlighted_index(&self) -> Option<usize> {
        if self.results.is_empty() {
            None
        } else {
            Some(self.highlighted)
        }
    }

    /// Snapshots captured when search opened.
    #[must_use]
    pub const fn snapshot(&self) -> &ViewSnapshot {
        &self.snapshot
    }

    /// Appends one character and updates results immediately.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.recompute();
    }

    /// Removes the last character and updates results immediately.
    pub fn pop_char(&mut self) {
        self.query.pop();
        self.recompute();
    }

    /// Toggles scope between current view and all cached tasks, preserving query.
    pub fn toggle_scope(&mut self) {
        self.scope = match self.scope {
            SessionScope::CurrentView => SessionScope::All,
            SessionScope::All => SessionScope::CurrentView,
        };
        self.recompute();
    }

    /// Sets scope explicitly and recomputes results.
    pub fn set_scope(&mut self, scope: SessionScope) {
        self.scope = scope;
        self.recompute();
    }

    /// Processes a terminal key press using `keymap` to detect [`Action::ToggleSearchScope`].
    pub fn handle_key(&mut self, key: KeyEvent, keymap: &Keymap) -> SearchUiEvent {
        if keymap.lookup(key) == Some(Action::ToggleSearchScope) {
            self.toggle_scope();
            return SearchUiEvent::Pending;
        }

        match key.code {
            KeyCode::Esc => SearchUiEvent::Cancelled(self.snapshot.clone()),
            KeyCode::Enter => {
                if let Some(hit) = self.results.get(self.highlighted) {
                    SearchUiEvent::JumpToTask {
                        project_id: Id::new(hit.project_id.clone()),
                        task_id: Id::new(hit.id.clone()),
                    }
                } else {
                    SearchUiEvent::Pending
                }
            }
            KeyCode::Down => {
                if !self.results.is_empty() && self.highlighted + 1 < self.results.len() {
                    self.highlighted += 1;
                }
                SearchUiEvent::Pending
            }
            KeyCode::Up => {
                self.highlighted = self.highlighted.saturating_sub(1);
                SearchUiEvent::Pending
            }
            KeyCode::Backspace => {
                self.pop_char();
                SearchUiEvent::Pending
            }
            KeyCode::Char(c)
                if !key
                    .modifiers
                    .intersects(KeyModifiers::CONTROL | KeyModifiers::ALT) =>
            {
                self.push_char(c);
                SearchUiEvent::Pending
            }
            _ => SearchUiEvent::Pending,
        }
    }

    fn recompute(&mut self) {
        let scope = match self.scope {
            SessionScope::CurrentView => SearchScope::CurrentView {
                ids: &self.current_view_ids,
            },
            SessionScope::All => SearchScope::All,
        };
        let hits = search(&self.items, &self.query, &scope);
        self.results = hits
            .into_iter()
            .filter_map(|h| {
                let &idx = self.item_index.get(h.id)?;
                let item = &self.items[idx];
                let project_name = self
                    .project_names
                    .get(&item.project_id)
                    .cloned()
                    .unwrap_or_else(|| item.project_id.clone());
                Some(SearchResultRow {
                    id: item.id.clone(),
                    content: item.content.clone(),
                    description: item.description.clone(),
                    project_id: item.project_id.clone(),
                    project_name,
                    field: h.field,
                    ranges: h.ranges,
                    score: h.score,
                })
            })
            .collect();

        if self.results.is_empty() {
            self.highlighted = 0;
        } else if self.highlighted >= self.results.len() {
            self.highlighted = self.results.len() - 1;
        }
    }
}

/// Restores `app` to the pre-search view and selection captured in `snapshot`.
///
/// # Errors
///
/// Propagates data errors from `data`.
pub fn apply_cancel(app: &mut App, snapshot: &ViewSnapshot, data: &dyn ViewData) -> Result<()> {
    app.search_active = false;
    let view = decode_view_id(&snapshot.previous_view);
    let task_id = snapshot.previous_selection.as_deref().map(Id::new);
    app.select_view_and_task(&view, task_id.as_ref(), data)
}

/// Jumps `app` to `(ViewId::Project(project_id), Some(task_id))` when accepting a search hit.
///
/// # Errors
///
/// Propagates data errors from `data`.
pub fn apply_accept(
    app: &mut App,
    project_id: &Id,
    task_id: &Id,
    data: &dyn ViewData,
) -> Result<()> {
    app.search_active = false;
    app.select_view_and_task(&UiViewId::Project(project_id.clone()), Some(task_id), data)
}

/// Splits `text` into normal and highlighted spans covering byte `ranges`.
#[must_use]
pub fn highlight_spans(
    text: &str,
    ranges: &[Range<usize>],
    normal: Style,
    matched: Style,
) -> Vec<Span<'static>> {
    if ranges.is_empty() {
        return vec![Span::styled(text.to_string(), normal)];
    }
    let mut sorted: Vec<Range<usize>> = ranges
        .iter()
        .filter(|r| r.start < r.end && r.end <= text.len())
        .filter(|r| text.is_char_boundary(r.start) && text.is_char_boundary(r.end))
        .cloned()
        .collect();
    sorted.sort_by_key(|r| (r.start, r.end));

    let mut spans = Vec::new();
    let mut cursor = 0usize;
    for r in sorted {
        let start = r.start.max(cursor);
        if start >= r.end {
            continue;
        }
        if cursor < start {
            spans.push(Span::styled(text[cursor..start].to_string(), normal));
        }
        spans.push(Span::styled(text[start..r.end].to_string(), matched));
        cursor = r.end;
    }
    if cursor < text.len() {
        spans.push(Span::styled(text[cursor..].to_string(), normal));
    }
    if spans.is_empty() {
        spans.push(Span::styled(text.to_string(), normal));
    }
    spans
}

/// Renders the one-line search prompt and ranked results list inside `area` (replacing the task list pane).
pub fn render(frame: &mut Frame<'_>, area: Rect, overlay: &SearchOverlay, theme: &Theme) {
    if area.width < 4 || area.height < 4 {
        return;
    }

    let scope_label = match overlay.scope() {
        SessionScope::CurrentView => "scope: view",
        SessionScope::All => "scope: all",
    };

    let block = Block::default()
        .title(format!(" Search [{scope_label}] "))
        .borders(Borders::ALL)
        .border_style(theme.border_focused);

    let inner = block.inner(area);
    frame.render_widget(block, area);

    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let prompt_line = Line::from(vec![
        Span::styled("/ ", theme.border_focused.add_modifier(Modifier::BOLD)),
        Span::styled(overlay.query().to_string(), theme.list_row),
        Span::styled("_", theme.muted),
        Span::styled(format!("  [{scope_label}]"), theme.muted),
    ]);

    let mut lines: Vec<Line<'static>> = vec![
        prompt_line,
        Line::from(Span::styled(
            "─".repeat(usize::from(inner.width)),
            theme.muted,
        )),
    ];

    if overlay.results().is_empty() {
        lines.push(Line::from(Span::styled("no matching tasks", theme.muted)));
    } else {
        let visible_rows = usize::from(inner.height.saturating_sub(2));
        for (idx, row) in overlay.results().iter().take(visible_rows).enumerate() {
            let is_sel = Some(idx) == overlay.highlighted_index();
            let base_style = if is_sel {
                theme.list_row_selected
            } else {
                theme.list_row
            };
            let hit_style = base_style.add_modifier(Modifier::BOLD | Modifier::UNDERLINED);

            let mut spans = vec![Span::styled(if is_sel { "▸ " } else { "  " }, base_style)];
            match row.field {
                MatchedField::Content => {
                    spans.extend(highlight_spans(
                        &row.content,
                        &row.ranges,
                        base_style,
                        hit_style,
                    ));
                }
                MatchedField::Description => {
                    spans.push(Span::styled(row.content.clone(), base_style));
                    spans.push(Span::styled(" [desc] ", theme.label));
                    spans.extend(highlight_spans(
                        &row.description,
                        &row.ranges,
                        base_style,
                        hit_style,
                    ));
                }
            }
            spans.push(Span::styled(
                format!("  ({})", row.project_name),
                theme.muted,
            ));
            lines.push(Line::from(spans));
        }
    }

    let para = Paragraph::new(lines);
    frame.render_widget(para, inner);
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::model::{Item, Priority, Project};
    use crate::db::{migrations, resources};
    use crate::ui::data::SqliteViewData;
    use crate::ui::view::{self, help};
    use ratatui::backend::TestBackend;
    use ratatui::Terminal;
    use rusqlite::Connection;

    fn render_full_frame(app: &App) -> String {
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        term.draw(|f| view::render(f, app, &theme)).unwrap();
        buffer_text(&term)
    }

    fn buffer_text(term: &Terminal<TestBackend>) -> String {
        let area = term.size().unwrap();
        let buf = term.backend().buffer();
        let mut out = String::new();
        for y in 0..area.height {
            for x in 0..area.width {
                out.push_str(buf[(x, y)].symbol());
            }
            out.push('\n');
        }
        out
    }

    fn seed_db() -> Connection {
        let mut conn = Connection::open_in_memory().unwrap();
        migrations::run(&mut conn).unwrap();
        conn.execute(
            "INSERT INTO user (id, full_name, email, raw_json) VALUES (?1, ?2, ?3, ?4)",
            rusqlite::params![
                "u1",
                "Test User",
                "user@example.invalid",
                serde_json::json!({
                    "id": "u1",
                    "full_name": "Test User",
                    "email": "user@example.invalid",
                    "inbox_project_id": "p1"
                })
                .to_string(),
            ],
        )
        .unwrap();
        for (id, name, order) in [("p1", "Inbox", 0), ("p2", "Work", 1)] {
            resources::project::upsert(
                &conn,
                &Project {
                    id: Id::new(id),
                    name: name.to_string(),
                    parent_id: None,
                    child_order: order,
                    color: "grey".to_string(),
                    is_favorite: false,
                    is_archived: false,
                    is_deleted: false,
                    view_style: "list".to_string(),
                },
            )
            .unwrap();
        }
        for (id, pid, content, desc, order) in [
            ("t1", "p1", "Buy coffee beans", "", 1),
            ("t2", "p1", "Water plants", "Check soil moisture", 2),
            (
                "t3",
                "p2",
                "Deploy search release",
                "Verify fuzzy needle",
                1,
            ),
        ] {
            resources::item::upsert(
                &conn,
                &Item {
                    id: Id::new(id),
                    content: content.to_string(),
                    description: desc.to_string(),
                    project_id: Id::new(pid),
                    section_id: None,
                    parent_id: None,
                    child_order: order,
                    priority: Priority::new(1).unwrap(),
                    labels: Vec::new(),
                    due: None,
                    deadline: None,
                    checked: false,
                    is_deleted: false,
                    added_at: "2026-11-10T00:00:00Z".to_string(),
                    completed_at: None,
                    user_id: Id::new("u1"),
                    responsible_uid: None,
                },
            )
            .unwrap();
        }
        conn
    }

    #[test]
    fn search_esc_restores_exact_previous_view_and_selection_buffer() {
        let conn = seed_db();
        let data = SqliteViewData::new(&conn, "2026-11-10");
        let mut app = App::new(&data).unwrap();
        app.apply(Action::Right, &data).unwrap(); // focus List in Inbox
        app.apply(Action::Down, &data).unwrap(); // select second task ("Water plants")
        assert_eq!(app.selected_task().unwrap().id, Id::new("t2"));

        let before = render_full_frame(&app);

        let (items, project_names) = load_search_items_from_db(&conn).unwrap();
        app.apply(Action::OpenSearch, &data).unwrap();
        let mut overlay = SearchOverlay::open(&app, items, project_names);
        let keymap = Keymap::default();

        // Scope toggle & query in search
        overlay.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &keymap);
        assert_eq!(overlay.scope(), SessionScope::All);
        for c in "fuzzy".chars() {
            overlay.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &keymap);
        }
        assert_eq!(overlay.results().len(), 1);
        assert_eq!(overlay.results()[0].id, "t3");
        assert_eq!(overlay.results()[0].field, MatchedField::Description);

        // Render search overlay and check prompt + description indicator + project name
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        let theme = Theme::default();
        term.draw(|f| render(f, f.area(), &overlay, &theme))
            .unwrap();
        let overlay_text = buffer_text(&term);
        assert!(overlay_text.contains("[scope: all]"));
        assert!(overlay_text.contains("[desc]"));
        assert!(overlay_text.contains("(Work)"));

        // Press Esc -> restores exact snapshot
        let ev = overlay.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &keymap);
        let SearchUiEvent::Cancelled(snap) = ev else {
            panic!("expected Cancelled");
        };
        apply_cancel(&mut app, &snap, &data).unwrap();
        let after = render_full_frame(&app);
        assert_eq!(before, after);
        assert_eq!(app.selected_task().unwrap().id, Id::new("t2"));
    }

    #[test]
    fn search_enter_jumps_to_task_project_and_selects_task() {
        let conn = seed_db();
        let data = SqliteViewData::new(&conn, "2026-11-10");
        let mut app = App::new(&data).unwrap();
        let (items, project_names) = load_search_items_from_db(&conn).unwrap();
        let mut overlay = SearchOverlay::open(&app, items, project_names);
        let keymap = Keymap::default();

        overlay.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE), &keymap);
        for c in "Deploy".chars() {
            overlay.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE), &keymap);
        }
        let ev = overlay.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE), &keymap);
        let SearchUiEvent::JumpToTask {
            project_id,
            task_id,
        } = ev
        else {
            panic!("expected JumpToTask");
        };

        apply_accept(&mut app, &project_id, &task_id, &data).unwrap();
        assert_eq!(app.active_view(), &UiViewId::Project(Id::new("p2")));
        assert_eq!(app.selected_task().unwrap().id, Id::new("t3"));
    }

    #[test]
    fn help_overlay_lists_open_search_and_scope_toggle_from_keymap() {
        let keymap = Keymap::default();
        let theme = Theme::default();
        let backend = TestBackend::new(100, 30);
        let mut term = Terminal::new(backend).unwrap();
        term.draw(|f| help::render(f, f.area(), &keymap, &theme))
            .unwrap();
        let text = buffer_text(&term);
        assert!(text.contains(Action::OpenSearch.description()));
        assert!(text.contains(Action::ToggleSearchScope.description()));
        assert!(text.contains("Tab"));
    }
}
