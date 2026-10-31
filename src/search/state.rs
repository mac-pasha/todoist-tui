//! Headless search session state machine (T-906, EPIC-10).
//!
//! [`SearchSession`] is the "/" search UX minus the terminal: it owns the
//! query text, the current result set, and the highlighted result, and
//! recomputes results via [`search`] (T-904) on every query mutation. It is
//! deliberately key-dispatch-free — EPIC-07's app loop (a later task) owns
//! translating key presses into calls on this type, so the state machine
//! itself is unit-testable without `crossterm`/`ratatui`.
//!
//! Opening a session ([`SearchSession::open`]) snapshots the caller's view
//! and selection as a [`ViewSnapshot`] so that cancelling
//! ([`SearchSession::cancel`]) can hand that exact snapshot back and the
//! caller can restore the view it was in before the user pressed `/`.
//! Accepting ([`SearchSession::accept`]) instead returns the currently
//! highlighted [`SearchHit`] (if any) for jump-to-task.
//!
//! [`ViewId`] is a thin wrapper around an opaque string rather than
//! EPIC-07's concrete view type, which has not landed yet; wiring this to
//! the real view enum later is expected to be a small `From`/`Into`
//! adapter, not a rewrite of this module.

// No consumers yet: EPIC-07's app loop calls into this module in a later
// task. `pub` alone does not silence `dead_code` in a binary crate, so
// allow it at module scope — same pattern as `src/search/query.rs` (T-904)
// and `src/search/mod.rs` (T-902).
#![allow(dead_code)]

use std::collections::HashSet;

use super::query::{search, SearchHit, SearchItem, SearchScope};

/// Opaque identifier for whichever view was active before a
/// [`SearchSession`] was opened.
///
/// Kept as a thin string wrapper (rather than depending on EPIC-07's view
/// type, which has not landed) so this module can be built and tested
/// ahead of that epic; the caller converts its real view type into a
/// `ViewId` at the call site.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct ViewId(String);

impl ViewId {
    /// Builds a `ViewId` from any string-like value.
    pub fn new(id: impl Into<String>) -> Self {
        ViewId(id.into())
    }

    /// Borrows the underlying identifier.
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl From<&str> for ViewId {
    fn from(id: &str) -> Self {
        ViewId::new(id)
    }
}

impl From<String> for ViewId {
    fn from(id: String) -> Self {
        ViewId::new(id)
    }
}

/// Which subset of items a [`SearchSession`] searches.
///
/// Mirrors [`SearchScope`], but holds no borrowed data of its own: the
/// session keeps the current view's ids internally and builds the borrowing
/// [`SearchScope`] from this tag plus that set on every recompute, which is
/// what lets [`SearchSession::set_scope`] toggle scope without losing the
/// typed query.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SessionScope {
    /// Only items whose id is in the view active when the session opened.
    CurrentView,
    /// Every item in the session's item set.
    All,
}

/// Snapshot of the caller's view and selection, captured at
/// [`SearchSession::open`] and handed back unchanged by
/// [`SearchSession::cancel`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ViewSnapshot {
    /// The view the caller was showing before search opened.
    pub previous_view: ViewId,
    /// The task selected in that view before search opened, if any.
    pub previous_selection: Option<String>,
}

/// Headless state machine backing the `/` search UX.
///
/// Holds a borrowed slice of [`SearchItem`]s (the full set of items the
/// session may search over) for its whole lifetime; `'a` ties the
/// session's results ([`SearchHit<'a>`]) to that slice, mirroring
/// [`search`]'s own borrowing.
pub struct SearchSession<'a> {
    items: &'a [SearchItem],
    current_view_ids: HashSet<String>,
    snapshot: ViewSnapshot,
    query: String,
    scope: SessionScope,
    results: Vec<SearchHit<'a>>,
    highlighted: usize,
}

impl<'a> SearchSession<'a> {
    /// Opens a new search session over `items`, scoped by default to
    /// `current_view_ids` (the ids currently rendered in `previous_view`).
    ///
    /// Snapshots `previous_view` and `previous_selection` so that
    /// [`cancel`](Self::cancel) can later hand them back unchanged, and runs
    /// an initial (empty-query) [`search`] so [`results`](Self::results) is
    /// populated from the moment the session opens.
    pub fn open(
        items: &'a [SearchItem],
        current_view_ids: HashSet<String>,
        previous_view: ViewId,
        previous_selection: Option<String>,
    ) -> Self {
        let mut session = SearchSession {
            items,
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
        session.recompute();
        session
    }

    /// Appends `c` to the query and recomputes results.
    pub fn push_char(&mut self, c: char) {
        self.query.push(c);
        self.recompute();
    }

    /// Removes the last character of the query (if any) and recomputes
    /// results.
    pub fn pop_char(&mut self) {
        self.query.pop();
        self.recompute();
    }

    /// Switches which items are considered and recomputes results without
    /// touching the typed query text.
    pub fn set_scope(&mut self, scope: SessionScope) {
        self.scope = scope;
        self.recompute();
    }

    /// Empties the query but keeps the session open with its original
    /// snapshot intact, recomputing results for the now-empty query.
    pub fn clear(&mut self) {
        self.query.clear();
        self.recompute();
    }

    /// Closes the session, returning the exact snapshot captured at
    /// [`open`](Self::open) so the caller can restore its previous view and
    /// selection. Works the same whether the query is empty or not.
    pub fn cancel(self) -> ViewSnapshot {
        self.snapshot
    }

    /// Closes the session, returning the currently highlighted
    /// [`SearchHit`] for jump-to-task, or `None` when there are no results.
    pub fn accept(self) -> Option<SearchHit<'a>> {
        let SearchSession {
            mut results,
            highlighted,
            ..
        } = self;
        if results.is_empty() {
            None
        } else {
            Some(results.swap_remove(highlighted))
        }
    }

    /// Moves the highlighted index to the next result, saturating at the
    /// last one. No-op when there are no results.
    pub fn select_next(&mut self) {
        if !self.results.is_empty() && self.highlighted + 1 < self.results.len() {
            self.highlighted += 1;
        }
    }

    /// Moves the highlighted index to the previous result, saturating at
    /// the first one. No-op when there are no results.
    pub fn select_previous(&mut self) {
        self.highlighted = self.highlighted.saturating_sub(1);
    }

    /// The query text typed so far.
    pub fn query(&self) -> &str {
        &self.query
    }

    /// The current scope.
    pub fn scope(&self) -> SessionScope {
        self.scope
    }

    /// The current result set, in the order returned by [`search`].
    pub fn results(&self) -> &[SearchHit<'a>] {
        &self.results
    }

    /// The index into [`results`](Self::results) that is currently
    /// highlighted, or `None` when there are no results.
    pub fn highlighted_index(&self) -> Option<usize> {
        if self.results.is_empty() {
            None
        } else {
            Some(self.highlighted)
        }
    }

    /// Re-runs [`search`] for the current query and scope, then clamps the
    /// highlighted index into the (possibly shrunk) result range.
    fn recompute(&mut self) {
        let scope = match self.scope {
            SessionScope::CurrentView => SearchScope::CurrentView {
                ids: &self.current_view_ids,
            },
            SessionScope::All => SearchScope::All,
        };
        self.results = search(self.items, &self.query, &scope);
        if self.results.is_empty() {
            self.highlighted = 0;
        } else if self.highlighted >= self.results.len() {
            self.highlighted = self.results.len() - 1;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn item(id: &str, content: &str) -> SearchItem {
        SearchItem {
            id: id.to_string(),
            content: content.to_string(),
            description: String::new(),
            project_id: "p1".to_string(),
            section_id: None,
        }
    }

    fn pies() -> Vec<SearchItem> {
        vec![
            item("t1", "apple pie"),
            item("t2", "apple tart"),
            item("t3", "apple cake"),
            item("t4", "banana split"),
        ]
    }

    fn ids(v: &[&str]) -> HashSet<String> {
        v.iter().map(ToString::to_string).collect()
    }

    fn open<'a>(items: &'a [SearchItem], view: &[&str]) -> SearchSession<'a> {
        SearchSession::open(items, ids(view), ViewId::new("inbox"), Some("t1".into()))
    }

    fn type_str(s: &mut SearchSession<'_>, text: &str) {
        for c in text.chars() {
            s.push_char(c);
        }
    }

    fn result_ids<'a>(s: &SearchSession<'a>) -> Vec<&'a str> {
        s.results().iter().map(|h| h.id).collect()
    }

    #[test]
    fn open_starts_with_empty_query_and_current_view_scope() {
        let items = pies();
        let s = open(&items, &["t1"]);
        assert_eq!(s.query(), "");
        assert_eq!(s.scope(), SessionScope::CurrentView);
        assert_eq!(s.highlighted_index().is_some(), !s.results().is_empty());
    }

    #[test]
    fn cancel_returns_exact_snapshot() {
        let items = pies();
        let s = open(&items, &["t1"]);
        assert_eq!(
            s.cancel(),
            ViewSnapshot {
                previous_view: ViewId::new("inbox"),
                previous_selection: Some("t1".to_string()),
            }
        );
    }

    #[test]
    fn cancel_snapshot_with_no_selection() {
        let items = pies();
        let s = SearchSession::open(&items, ids(&[]), ViewId::from("today"), None);
        let snap = s.cancel();
        assert_eq!(snap.previous_view.as_str(), "today");
        assert_eq!(snap.previous_selection, None);
    }

    #[test]
    fn cancel_after_mutations_returns_original_snapshot() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2"]);
        type_str(&mut s, "app");
        s.set_scope(SessionScope::All);
        s.select_next();
        s.pop_char();
        let snap = s.cancel();
        assert_eq!(snap.previous_view, ViewId::new("inbox"));
        assert_eq!(snap.previous_selection.as_deref(), Some("t1"));
    }

    #[test]
    fn clear_empties_query_and_keeps_snapshot() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3"]);
        type_str(&mut s, "apple");
        assert_eq!(s.query(), "apple");
        s.clear();
        assert_eq!(s.query(), "");
        // Session still usable after clear.
        s.push_char('a');
        assert_eq!(s.query(), "a");
        let snap = s.cancel();
        assert_eq!(snap.previous_view, ViewId::new("inbox"));
        assert_eq!(snap.previous_selection.as_deref(), Some("t1"));
    }

    #[test]
    fn clear_matches_fresh_session_results() {
        let items = pies();
        let fresh = open(&items, &["t1", "t2", "t3"]);
        let mut s = open(&items, &["t1", "t2", "t3"]);
        type_str(&mut s, "cake");
        s.clear();
        assert_eq!(result_ids(&s), result_ids(&fresh));
    }

    #[test]
    fn clear_preserves_scope() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        s.set_scope(SessionScope::All);
        type_str(&mut s, "x");
        s.clear();
        assert_eq!(s.scope(), SessionScope::All);
    }

    #[test]
    fn cancel_from_empty_query_still_closes() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        type_str(&mut s, "a");
        s.clear();
        assert_eq!(s.query(), "");
        let snap = s.cancel();
        assert_eq!(snap.previous_view, ViewId::new("inbox"));
        assert_eq!(snap.previous_selection.as_deref(), Some("t1"));
    }

    #[test]
    fn cancel_on_freshly_opened_session_closes() {
        let items = pies();
        let s = open(&items, &["t1"]);
        assert_eq!(s.query(), "");
        let _ = s.cancel();
    }

    #[test]
    fn accept_returns_highlighted_hit() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3"]);
        type_str(&mut s, "apple");
        assert_eq!(s.results().len(), 3);
        let expected = s.results()[0].id;
        assert_eq!(s.highlighted_index(), Some(0));
        assert_eq!(s.accept().map(|h| h.id), Some(expected));
    }

    #[test]
    fn accept_returns_hit_after_moving_highlight() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3"]);
        type_str(&mut s, "apple");
        s.select_next();
        s.select_next();
        let expected = s.results()[2].id;
        assert_eq!(s.highlighted_index(), Some(2));
        assert_eq!(s.accept().map(|h| h.id), Some(expected));
    }

    #[test]
    fn accept_returns_none_when_no_results() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3"]);
        type_str(&mut s, "zzzz");
        assert_eq!(s.results(), []);
        assert_eq!(s.highlighted_index(), None);
        assert!(s.accept().is_none());
    }

    #[test]
    fn accept_with_no_items_returns_none() {
        let items: Vec<SearchItem> = Vec::new();
        let s = open(&items, &[]);
        assert!(s.accept().is_none());
    }

    #[test]
    fn results_recomputed_on_each_push_and_pop() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3", "t4"]);
        s.push_char('b');
        assert_eq!(result_ids(&s), vec!["t4"]);
        s.push_char('z');
        assert_eq!(s.results(), []);
        s.pop_char();
        assert_eq!(result_ids(&s), vec!["t4"]);
        s.pop_char();
        assert_eq!(s.query(), "");
    }

    #[test]
    fn push_pop_sequence_tracks_query_text() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        type_str(&mut s, "abc");
        assert_eq!(s.query(), "abc");
        s.pop_char();
        assert_eq!(s.query(), "ab");
        s.push_char('x');
        assert_eq!(s.query(), "abx");
        s.pop_char();
        s.pop_char();
        s.pop_char();
        assert_eq!(s.query(), "");
    }

    #[test]
    fn pop_char_on_empty_query_is_noop() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        s.pop_char();
        s.pop_char();
        assert_eq!(s.query(), "");
        assert_eq!(s.scope(), SessionScope::CurrentView);
        // Still functional.
        s.push_char('a');
        assert_eq!(s.query(), "a");
    }

    #[test]
    fn pop_char_removes_whole_multibyte_char() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        s.push_char('a');
        s.push_char('é');
        s.push_char('日');
        s.pop_char();
        assert_eq!(s.query(), "aé");
        s.pop_char();
        assert_eq!(s.query(), "a");
    }

    #[test]
    fn highlight_clamped_when_result_set_shrinks() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3", "t4"]);
        type_str(&mut s, "apple");
        assert_eq!(s.results().len(), 3);
        s.select_next();
        s.select_next();
        assert_eq!(s.highlighted_index(), Some(2));

        // Narrow to exactly one match: "apple pie".
        type_str(&mut s, " pie");
        assert_eq!(result_ids(&s), vec!["t1"]);
        assert_eq!(s.highlighted_index(), Some(0));
        assert_eq!(s.accept().map(|h| h.id), Some("t1"));
    }

    #[test]
    fn highlight_clamped_to_last_on_partial_shrink() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3", "t4"]);
        type_str(&mut s, "apple");
        s.select_next();
        s.select_next();
        assert_eq!(s.highlighted_index(), Some(2));
        // "apple t" matches only "apple tart" (cake/pie lack a 't' after space).
        type_str(&mut s, " ta");
        let n = s.results().len();
        assert!((1..3).contains(&n), "expected shrink, got {n}");
        assert_eq!(s.highlighted_index(), Some(n - 1));
    }

    #[test]
    fn highlight_in_range_after_every_mutation() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3", "t4"]);
        type_str(&mut s, "apple");
        s.select_next();
        s.select_next();
        for c in "xyz".chars() {
            s.push_char(c);
            match s.highlighted_index() {
                Some(i) => assert!(i < s.results().len()),
                None => assert_eq!(s.results(), []),
            }
        }
        while !s.query().is_empty() {
            s.pop_char();
            match s.highlighted_index() {
                Some(i) => assert!(i < s.results().len()),
                None => assert_eq!(s.results(), []),
            }
        }
    }

    #[test]
    fn highlight_none_then_valid_after_results_return() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3"]);
        type_str(&mut s, "zz");
        assert_eq!(s.highlighted_index(), None);
        s.pop_char();
        s.pop_char();
        s.push_char('a');
        assert!(s.highlighted_index().is_some());
    }

    #[test]
    fn select_next_and_previous_saturate() {
        let items = pies();
        let mut s = open(&items, &["t1", "t2", "t3"]);
        type_str(&mut s, "apple");
        s.select_previous();
        assert_eq!(s.highlighted_index(), Some(0));
        for _ in 0..10 {
            s.select_next();
        }
        assert_eq!(s.highlighted_index(), Some(2));
        s.select_previous();
        assert_eq!(s.highlighted_index(), Some(1));
    }

    #[test]
    fn select_on_empty_results_is_noop() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        type_str(&mut s, "zzzz");
        s.select_next();
        s.select_previous();
        assert_eq!(s.highlighted_index(), None);
    }

    #[test]
    fn scope_toggle_preserves_query_and_changes_results() {
        let items = pies();
        // Current view contains only t1; t2/t3 are outside it.
        let mut s = open(&items, &["t1"]);
        type_str(&mut s, "apple");
        assert_eq!(result_ids(&s), vec!["t1"]);

        s.set_scope(SessionScope::All);
        assert_eq!(s.scope(), SessionScope::All);
        assert_eq!(s.query(), "apple");
        let mut all = result_ids(&s);
        all.sort_unstable();
        assert_eq!(all, vec!["t1", "t2", "t3"]);

        s.set_scope(SessionScope::CurrentView);
        assert_eq!(s.scope(), SessionScope::CurrentView);
        assert_eq!(s.query(), "apple");
        assert_eq!(result_ids(&s), vec!["t1"]);
    }

    #[test]
    fn scope_toggle_clamps_highlight() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        s.set_scope(SessionScope::All);
        type_str(&mut s, "apple");
        s.select_next();
        s.select_next();
        assert_eq!(s.highlighted_index(), Some(2));
        s.set_scope(SessionScope::CurrentView);
        assert_eq!(s.highlighted_index(), Some(0));
    }

    #[test]
    fn scope_set_to_same_value_is_idempotent() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        type_str(&mut s, "apple");
        let before = result_ids(&s);
        s.set_scope(SessionScope::CurrentView);
        assert_eq!(result_ids(&s), before);
        assert_eq!(s.query(), "apple");
    }

    #[test]
    fn scope_toggle_with_empty_query_keeps_query_empty() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        s.set_scope(SessionScope::All);
        assert_eq!(s.query(), "");
    }

    #[test]
    fn typing_after_scope_toggle_uses_new_scope() {
        let items = pies();
        let mut s = open(&items, &["t1"]);
        s.set_scope(SessionScope::All);
        type_str(&mut s, "apple t");
        assert!(result_ids(&s).contains(&"t2"));
    }

    #[test]
    fn current_view_with_empty_id_set_yields_no_results() {
        let items = pies();
        let mut s = open(&items, &[]);
        type_str(&mut s, "apple");
        assert_eq!(s.results(), []);
        assert!(s.accept().is_none());
    }

    #[test]
    fn view_id_conversions() {
        assert_eq!(ViewId::from("a"), ViewId::new("a"));
        assert_eq!(ViewId::from(String::from("a")).as_str(), "a");
    }
}
