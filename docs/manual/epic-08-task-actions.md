# Epic 08: Task Mutations & Actions

This manual covers all task mutation workflows (`src/ui/actions/` and `src/ui/widgets/`), keybindings, optimistic cache behaviour, recurring task handling, and error recovery.

## 1. Keybindings (All Rebindable via `[keymap]` in `config.toml`)

| Default Key | Action Name (`config.toml`) | Context | Description |
|---|---|---|---|
| `a` | `add_task` | Global | Open Add Task form prefilled for current view |
| `A` | `add_subtask` | Global | Open Add Task form as a sub-task of selected task |
| `e` | `edit_task` | Task list | Open Edit Task form prefilled from selected task |
| `p` | `quick_priority` | Task list | Inline priority quick-edit (followed by `1`–`4`) |
| `d` | `quick_due` | Task list | Inline due-date quick-edit (raw string sent verbatim) |
| `x` | `toggle_complete` | Task list | Complete active task or uncomplete checked task |
| `D` | `delete_task` | Task list | Open Delete confirmation dialog (`y` / `n`) |
| `m` | `move_task` | Task list | Open filterable destination picker (project / section / parent) |
| `K` | `reorder_up` | Task list | Move task up within its sibling group |
| `J` | `reorder_down` | Task list | Move task down within its sibling group |
| `c` | `comments` | Task list | Open Comments modal (oldest-first) & add comment (`i`) |
| `E` | `view_errors` | Global | Open Sync Errors modal to inspect/dismiss failed commands |

## 2. Optimistic Write Model

Every task action passes through [`dispatch`](../../src/ui/actions/dispatch.rs) (`src/ui/actions/dispatch.rs`):
1. Verifies authentication (returns `ActionError::NotAuthenticated` without writing when no token exists).
2. Translates `TaskAction` into a Sync API `QueuedCommand` with a unique `uuid` v4 (and `temp_id` for `Add` / `AddComment`).
3. Atomically writes both the queued command (`commands` table) and the optimistic SQLite cache update in a single transaction.
4. Returns immediately with zero network wait. The background `SyncEngine` flushes queued commands in order, rewriting `temp_id_mapping` across dependent commands and cache rows.

While mutations are queued or unreachable, the bottom status bar indicates `[pending (N)]` or `[offline]`.

## 3. Recurring-Task Completion While Offline (Open Question 6)

Completing a recurring task locally (`x`) enqueues `item_complete` and sets `checked = true` in SQLite without inventing a local recurrence engine or guessing the next occurrence date. Once connectivity returns and the background sync flushes `item_complete`, the Todoist server computes the next recurrence date and pushes the active task with its new due date back into the local cache.

## 4. Failed Command Handling & Rollback

When the Sync API rejects an individual queued command in `sync_status`:
- **Status bar**: Displays `<N> failed (E)` and triggers a transient toast.
- **Errors view (`E`)**: Lists each failed command's type and server error text (never exposing API token material). Pressing `d` or `Enter` removes the entry from `commands`.
- **Keep vs Rollback**: Handled by `EPIC-05` (`docs/manual/epic-05-error-classes.md`). Permanent rejections (`NotFound`, `InvalidArgument`, `PermissionDenied`) roll back optimistic state; rate-limit / transient errors keep local state until resolved.

## 5. Manual Verification Checklist

Using a scratch test account (`todoist-tui auth`):
1. Press `a`, enter `"Smoke task"`, press `Enter` -> task appears immediately in list.
2. Press `K` / `J` on sibling tasks -> order shifts immediately and survives restart.
3. Press `x` -> task disappears from active view; press `D` -> confirm `y` -> deleted.
4. Disconnect network, edit a task with `e` -> status shows `[offline]` / `[pending (1)]`; reconnect and press `r` -> flushes cleanly with zero duplicate UUIDs.

