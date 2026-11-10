# EPIC-10 — Instant Fuzzy Search & Saved Filters

## Overview

`todoist-tui` provides an offline-first incremental `/` search overlay and read-only saved Filter views evaluated strictly against the local SQLite cache.

## Incremental Search (`/`)

- **Keybindings**:
  - `/` (`Action::OpenSearch`) opens the one-line search prompt replacing the task list pane.
  - `Tab` (`Action::ToggleSearchScope`) toggles between `CurrentView` (active sidebar view) and `All` (every active cached task) without losing the typed query.
  - `Up` / `Down` move selection among ranked hits; `Enter` jumps directly to the task inside its parent project.
  - `Esc` cancels search and restores the exact pre-search view and task selection snapshot.
- **Ranking & Highlighting**:
  - `src/search/fuzzy.rs` scores contiguous and word-boundary matches across task `content` and `description` using preallocated scratch buffers (`<16ms` p95 at 5k items in release builds).
  - Hits matching only in `description` show a `[desc]` badge alongside the parent project name, with matched byte ranges highlighted.

## Saved Filters (Read-Only)

Saved Todoist filters appear under the **Filters** group in the sidebar ordered by `item_order` (with deleted filters excluded).

- **Supported grammar subset (`src/filters/parse.rs`, `src/filters/eval.rs`)**:
  - Leaves: `today`, `overdue`, `no date` / `no due date`, `no labels` / `no label`, `p1`..`p4`, `#project`, `@label`.
  - Boolean operators: `|` (or), `&` (and), `!` (not), parentheses `( ... )`.
- **Unsupported queries**:
  - Queries outside the local subset (e.g. `assigned to: me`) fail fast wholesale via `filters::parse`, displaying `server-defined filter - cannot be evaluated locally` with the reason and zero task rows rather than returning a misleading partial result.
- **Read-only guard**:
  - Task and structure mutating actions are blocked while a saved Filter view is focused (`ui::filters_view::allow_action_in_view`).

## References

- `src/search/` (`fuzzy.rs`, `query.rs`, `state.rs`)
- `src/filters/` (`parse.rs`, `eval.rs`)
- `src/ui/search.rs`, `src/ui/filters_view.rs`
- `docs/manual/epic-10-filter-support.md`

