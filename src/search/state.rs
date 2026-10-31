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
