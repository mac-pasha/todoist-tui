# Epic 07: Interactive TUI Shell & Navigation

This manual describes the interactive terminal UI shell (`src/ui/`), layout constraints, keymap configuration, color themes, and event loop wiring.

## 1. Frame Layout & 80x24 Terminal Guard

The shell renders a 3-pane workspace above a 1-row bottom status bar (`src/ui/view/layout.rs`):

```
+----------------------+---------------------------+-------------------------+
| Navigation (Sidebar) | Task List                 | Detail & Comments       |
| (fixed ~24 cols)     | (~55% remaining width)    | (~45% remaining width)  |
|                      |                           |                         |
| Views:               | [ ] P1 Task title [due]   | Task Title              |
|   Inbox              |   [ ] P3 Sub-task         | Priority: P1            |
|   Today              | -- Section Name --        | Due:      Today         |
|   Upcoming           | [ ] P2 Other task @label  | Labels:   @errand       |
| Projects:            |                           |                         |
|   ▾ Work             |                           | Description & Comments  |
+----------------------+---------------------------+-------------------------+
| [synced] | last sync: just now | ?: help | q: quit                         |
+----------------------------------------------------------------------------+
```

- **Minimum Terminal Size**: `80x24` (`MIN_WIDTH = 80`, `MIN_HEIGHT = 24`).
- **Guard Behaviour**: If the terminal shrinks below `80x24`, the workspace is cleared and replaced with a centered message: `terminal too small: need 80x24, have <w>x<h>`. Resizing back to `>= 80x24` immediately restores the 3-pane layout.

## 2. Default Keybindings & `[keymap]` Configuration

Every binding in `Keymap::default()` (`src/ui/keymap.rs`) powers both input dispatch and the `?` help overlay (`src/ui/view/help.rs`), guaranteeing zero drift:

| Key | Action Name (`[keymap]`) | Context | Description |
|---|---|---|---|
| `j` | `down` | Sidebar, task list | Move selection down |
| `k` | `up` | Sidebar, task list | Move selection up |
| `h` | `left` | Global | Move focus left / collapse |
| `l` | `right` | Global | Move focus right / expand |
| `g` | `first` | Task list | Jump to first item |
| `G` | `last` | Task list | Jump to last item |
| `/` | `open_search` | Global | Open search prompt |
| `r` | `sync_now` | Global | Trigger immediate sync |
| `?` | `toggle_help` | Global | Toggle help overlay |
| `q` | `quit` | Global | Quit application |

### Overriding Keybindings in `config.toml`

Add a `[keymap]` table in `config.toml`:

```toml
[keymap]
down = "n"
up = "p"
quit = "Ctrl+q"
```

Rebinding `down = "n"` unbinds the default `j` for `down` and updates both input handling and the `?` help overlay.

## 3. Themes & Color-Depth Detection

Theme resolution (`src/ui/theme.rs`) reads `theme` from `config.toml` (default: `"default"`):

```toml
theme = "default"
```

Terminal color capability (`ColorDepth::detect`) follows standard environment rules in priority order:
1. If `COLORTERM` contains `"truecolor"` or `"24bit"` (case-insensitive) -> **TrueColor** (24-bit `Color::Rgb`).
2. Otherwise, if `TERM` contains `"256color"` -> **Ansi256** (downgraded 8-bit palette `Color::Indexed`).
3. Otherwise -> **Ansi16** (basic 16-color ANSI palette).

## 4. Architecture & Data Wiring

- **Read Layer (`ViewData`)**: When authenticated, `todoist-tui` / `todoist-tui tui` (`src/cli/tui.rs`) opens `SqliteViewData` (WAL mode read connection) backed by the local SQLite cache and starts the background `SyncEngine`. When unauthenticated, it falls back cleanly to `FixtureData::sample()`.
- **Status Bar (`StatusModel`)**: Subscribes to `SyncHandle::subscribe()`, transitioning cleanly across `synced`, `syncing...`, `offline`, `pending (N)`, and `error: <msg>` with relative last-sync age (`never`, `just now`, `3m ago`).
- **Mutation & Search Extensions**: Interactive task create/edit/complete/comment flows live in `EPIC-08` (`src/ui/actions/`), structure management in `EPIC-09` (`src/ui/structure/`), and full search/saved-filter evaluation in `EPIC-10` (`src/ui/search.rs`, `src/ui/filters_view.rs`).

