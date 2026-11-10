# Epic 11: Non-Interactive CLI Commands (`add`, `sync`, `today`)

This manual documents the non-interactive one-shot CLI surface in `src/cli/`. None of these commands ever enter the terminal alternate screen or raw mode, and ANSI styling is suppressed automatically when stdout is not a TTY or `--no-color` / `NO_COLOR` is set.

## 1. Exit Codes

Defined in [`src/error.rs`](../../src/error.rs):

| Code | Meaning | Trigger |
|---|---|---|
| `0` | Success | Command completed (including offline `add` queued durably) |
| `1` | Generic failure | Unclassified API/storage/config errors |
| `2` | Usage error | Invalid CLI flags or empty task text |
| `3` | Not authenticated | No stored token (`run todoist-tui auth`) |
| `4` | Offline / unreachable | Network or transport failure during `sync` |

## 2. Commands

### 2.1 `todoist-tui add <TEXT>`

Submits `<TEXT>` verbatim to the server-side quick-add endpoint (`POST /api/v1/tasks/quick`). Per **REQ-001 D1**, natural-language parsing of dates, priorities, or projects (`tomorrow 5pm #Work`) runs exclusively on the Todoist server — there is no local NL parser.

- **Online success (exit `0`)**: Upserts the returned task in SQLite and writes to stdout:
  ```text
  added: Buy oat milk (due: tomorrow)
  ```
- **Offline fallback (exit `0`)**: If the connection is unavailable, stores the unparsed text durably in SQLite (`commands.type = 'quick_add'`) and writes to stdout:
  ```text
  queued (offline): Buy oat milk tomorrow #Work
  ```
  Wait-free and safe against crashes. The queued text is flushed once via `/api/v1/tasks/quick` on the next `sync`.
- **Auth / 4xx error**: Fails with non-zero exit code without queueing.

### 2.2 `todoist-tui sync`

Flushes queued offline quick-adds and regular Sync API commands, then pulls incremental or full resource deltas.

- **Success (exit `0`)**:
  ```text
  synced: 4 pulled, 1 flushed, 0 pending
  ```
- **Offline (exit `4`)**: Leaves stdout completely empty and prints a single diagnostic to stderr:
  ```text
  sync failed: offline (1 pending)
  ```

### 2.3 `todoist-tui today [--json]`

Reads tasks due today or overdue directly from the local SQLite cache (**zero network calls**). Completed and deleted tasks are excluded. Ordering is deterministic: `priority DESC`, `due ASC`, `child_order ASC`, `id ASC`.

- **Plain-text mode (`todoist-tui today`)**:
  Emits one LF-terminated line per task (empty cache prints 0 bytes to stdout and exits `0`). Embedded `\t`, `\r`, `\n` inside fields are replaced with ASCII spaces.
  Columns (`\t`-separated): `id`, `priority` (`p1`..`p4`), `due` (`YYYY-MM-DD` or `-`), `project_name`, `labels` (comma-joined or `-`), `content`.
  Example:
  ```text
  6X4VfakeT001	p1	2026-04-15	Work	urgent,q2	Overdue urgent report
  ```
- **JSON mode (`todoist-tui today --json`)**:
  Emits a single-line JSON document with a stable `snake_case` DTO (`null` preserved for absent optional fields):
  ```json
  {"tasks":[{"id":"6X4VfakeT001","content":"Overdue urgent report","project_id":"6X4VfakePrj2","project_name":"Work","priority":4,"due":"2026-04-15","is_overdue":true,"labels":["urgent","q2"],"completed":false}]}
  ```

