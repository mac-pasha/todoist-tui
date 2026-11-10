# EPIC-04 manual: SQLite cache schema, delta application, read queries and cache wipe

This page documents the local SQLite cache introduced in EPIC-04: where the
database file lives on disk, how file permissions and connection pragmas are
enforced, the schema and migration runner, transactional Sync API delta
application, read queries exposed to the TUI and CLI, completed-item retention,
and `logout` cache wipe semantics. It is a factual reference for anyone
modifying `src/db/**` or the `tests/cache_*.rs` integration suites. User-facing
README/install documentation belongs to EPIC-12, not here.

## 1. Database location, permissions and connection pragmas

### Path resolution

`src/config/paths.rs::cache_db_file()` places `cache.db` inside the application
state directory resolved by `paths::state_dir()`:

| Platform / override | Resolved path |
|---|---|
| `XDG_STATE_HOME` set and non-empty (any OS) | `$XDG_STATE_HOME/todoist-tui/cache.db` |
| Linux (default) | `~/.local/state/todoist-tui/cache.db` |
| macOS (default, falls back to `ProjectDirs::data_dir`) | `~/Library/Application Support/com.todoist.todoist-tui/cache.db` |
| Windows (default, best-effort via `ProjectDirs::data_dir`) | `%APPDATA%\todoist\todoist-tui\data\cache.db` |

An empty `XDG_STATE_HOME=""` is treated as unset per the XDG Base Directory
specification. On macOS and Windows — where `directories::ProjectDirs::state_dir`
returns `None` — `state_dir()` falls back to the durable `data_dir()`, never to
an OS-purgeable cache directory.

### File and directory permission guarantees (`0600` / `0700`)

- `src/db/open.rs::open_default()` calls `paths::ensure_state_dir()` before
  opening `<state_dir>/cache.db`. On Unix, `ensure_state_dir()` creates any
  missing directory components and tightens the state directory to mode `0700`
  (owner read/write/traverse only), re-applying `0700` even if the directory
  already existed with a wider mode.
- Both `open::open_default()` and `open::open_at(path)` tighten the database
  file itself to Unix mode `0600` (`OWNER_READ_WRITE`) immediately after
  `rusqlite::Connection::open`. Opening a pre-existing database file with a
  wider mode (such as `0644`) re-tightens it back to `0600`.
- Open failures return `DbError::Open { path, source }`; permission-tightening
  failures return `DbError::Permissions { path }`. Neither error variant ever
  interpolates cached task content or token material.

### Per-connection pragmas

Every connection returned by `open::open_default()` or `open::open_at()` has
three pragmas applied before the caller receives it:

| Pragma | Value | Purpose |
|---|---|---|
| `PRAGMA foreign_keys` | `ON` (`1`) | Enforces foreign-key constraints on every connection. |
| `PRAGMA journal_mode` | `WAL` | Enables write-ahead logging so concurrent readers do not block on a writer. |
| `PRAGMA busy_timeout` | `5000` ms | Waits up to 5 seconds on lock contention before returning `SQLITE_BUSY`. |

## 2. Schema, indices and migrations

### Tables (`src/db/schema.sql`)

Every resource table uses the Sync API string identifier as `TEXT PRIMARY KEY`,
projects the fields required by indexed read queries into typed columns, and
stores the complete serde-serialised resource in `raw_json` so unmodelled or
nested fields survive round-trips without loss:

| Table | Primary key | One-line purpose |
|---|---|---|
| `projects` | `id TEXT PRIMARY KEY` | Cached Todoist projects with hierarchy (`parent_id`), `child_order`, flags (`is_archived`, `is_deleted`, `is_favorite`) and `raw_json`. |
| `sections` | `id TEXT PRIMARY KEY` | Cached project sections keyed by `id` with `project_id`, `section_order`, archive/delete flags and `raw_json`. |
| `items` | `id TEXT PRIMARY KEY` | Cached tasks with parent links (`project_id`, `section_id`, `parent_id`), `child_order`, `checked`, `is_deleted`, `priority`, flattened `due_date`, JSON `labels` array and `raw_json`. |
| `labels` | `id TEXT PRIMARY KEY` | Cached personal/shared labels with `name`, `color`, `item_order`, `is_deleted`, `is_favorite` and `raw_json`. |
| `notes` | `id TEXT PRIMARY KEY` | Cached task/project comments with `item_id` (empty-string sentinel `""` for project-level notes), `content`, `posted_at`, `is_deleted` and `raw_json`. |
| `filters` | `id TEXT PRIMARY KEY` | Cached saved filters with `name`, `query`, `item_order`, `is_deleted`, `is_favorite` and `raw_json`. |
| `user` | `id TEXT PRIMARY KEY` | Single-row table holding the authenticated account's profile (`full_name`, `email`, and `raw_json` including `inbox_project_id` / `tz_info`). |
| `day_orders` | `id TEXT PRIMARY KEY` | Single-row table (`id = 'me'`) storing the Sync API `day_orders` JSON map in `raw_json`. |
| `meta` | `key TEXT PRIMARY KEY` | Generic key/value store for sync bookkeeping (`sync_token`, `last_sync_at`, `full_sync_date_utc`). |

### Indices (`src/db/schema.sql`)

| Index | Columns | One-line purpose |
|---|---|---|
| `idx_items_project_order` | `items (project_id, child_order)` | Covers per-project task lists (`items_by_project`, `inbox`) in manual `child_order`. |
| `idx_items_section_order` | `items (section_id, child_order)` | Covers per-section task lists (`items_by_section`) in manual `child_order`. |
| `idx_items_parent_order` | `items (parent_id, child_order)` | Covers direct subtask lookups (`subtasks_of`) in manual `child_order`. |
| `idx_items_due_date` | `items (due_date)` | Covers lexicographic due-date range scans for `today` (including overdue) and `upcoming`. |
| `idx_items_checked` | `items (checked)` | Covers filtering active (`checked = 0`) vs. completed (`checked = 1`) tasks in `items_by_label` and `all_active_items`. |

### Schema version and adding migration `N+1`

`src/db/migrations.rs` tracks the schema version using SQLite's built-in
`PRAGMA user_version` (no separate migrations table):

- **Current version**: `pub const SCHEMA_VERSION: i32 = MIGRATIONS.len() as i32`
  (currently **`2`**, corresponding to `SCHEMA_V1 = include_str!("schema.sql")`
  for the EPIC-04 cache tables and `SCHEMA_V2 = crate::queue::schema::COMMANDS_DDL`
  for the EPIC-05 offline `commands` queue table).
- **Idempotence**: `migrations::run(&mut Connection) -> Result<i32>` reads
  `PRAGMA user_version` and applies only steps where `target > current`. Running
  `migrations::run` repeatedly on an already-migrated database is a no-op that
  returns `Ok(SCHEMA_VERSION)`.
- **Forward-version guard**: If a database file on disk has
  `user_version > SCHEMA_VERSION` (written by a newer binary), `migrations::run`
  refuses to touch it and returns
  `DbError::UnsupportedSchemaVersion { found, supported: SCHEMA_VERSION }`.

To add migration `N+1` (for example, EPIC-05's offline `commands` queue table as
migration `2`):

1. Define the new DDL string in `src/db/migrations.rs` (either inline or via a
   new `include_str!(...)` file), using `CREATE TABLE IF NOT EXISTS` /
   `CREATE INDEX IF NOT EXISTS` so partial pre-existing state is tolerated.
2. Append the new SQL slice to `const MIGRATIONS: &[&str] = &[SCHEMA_V1, SCHEMA_V2];`.
   `SCHEMA_VERSION` automatically increments to `2` because it is derived from
   `MIGRATIONS.len()`.
3. `migrations::run` will execute migration `2` inside its own transaction and
   bump `PRAGMA user_version` to `2` before committing; existing v1 databases
   upgrade in place from `1 -> 2` while fresh databases run `1` then `2`.
4. Add unit tests in `src/db/migrations.rs` asserting both a fresh `0 -> N+1`
   run and an incremental `N -> N+1` upgrade.

## 3. Transactional delta application (`apply_sync`)

`src/db/apply.rs::apply_sync(&mut Connection, &SyncResponse) -> Result<ApplyStats>`
applies an entire Sync API response inside a **single** `rusqlite::Transaction`:

1. **Full-sync detection and table clear**: A response is treated as a full sync
   when `resp.full_sync || resp.full_sync_date_utc.is_some()`. On a full sync,
   `apply_sync` executes `CLEAR_ALL_RESOURCES_SQL` at the start of the
   transaction, deleting all rows from `notes`, `items`, `sections`, `projects`,
   `labels`, `filters`, `user` and `day_orders` (children before parents) so
   stale rows deleted remotely while the client was offline do not survive.
2. **Upsert vs. `is_deleted: true`**: For each list resource (`projects`,
   `sections`, `items`, `notes`, `labels`, `filters`), items with
   `is_deleted: true` are removed via their module's `delete` helper (deleting
   an absent ID is a no-op `Ok(())`); all other items are written via
   `INSERT ... ON CONFLICT(id) DO UPDATE` (`upsert`), overwriting every mapped
   column and `raw_json`.
3. **Single-row resources (`user`, `day_orders`)**:
   - When `resp.user` is `Some(user)`, `resources::user::upsert` deletes any row
     with `id <> user.id` and upserts the current user row.
   - When `is_full_sync || !resp.day_orders.is_empty()`,
     `resources::day_order::replace_all` overwrites the `'me'` row in
     `day_orders`. On an incremental sync with an empty `day_orders` map, the
     existing `day_orders` row is left untouched.
4. **Incremental preservation**: On an incremental sync (`!is_full_sync`), any
   row not mentioned in the delta's resource arrays is left untouched.
5. **Metadata cursor update**: Before committing, `apply_sync` upserts
   `sync_token` (`resp.sync_token`), `last_sync_at` (current UTC timestamp in
   `YYYY-MM-DDTHH:MM:SSZ` format), and — when `resp.full_sync_date_utc` is
   `Some` — `full_sync_date_utc` into the `meta` table inside the same
   transaction.
6. **Atomic rollback on failure**: If any SQL statement or JSON encoding fails
   partway through `apply_sync`, the uncommitted `rusqlite::Transaction` is
   dropped and SQLite rolls back every change in the batch. Both `meta`
   (`sync_token`, `last_sync_at`, `full_sync_date_utc`) and every resource table
   remain byte-for-row identical to their pre-call state.
7. **Redaction-safe stats**: `ApplyStats` records only per-resource
   `ResourceCounts { upserted, deleted }`, booleans and counts, so formatting
   `ApplyStats` with `{:?}` never leaks task content, personal data or sync
   tokens.

## 4. Read queries, ordering guarantee and checked-item retention

### Available read queries (`src/db/queries.rs`)

Every item and resource query reconstructs full domain models (`Item`,
`Project`, `Section`, `Label`, `Filter`, `Note`) from the stored `raw_json`
column so fields not projected into dedicated SQLite columns (`due.string`,
`due.is_recurring`, `due.timezone`, `deadline`, `completed_at`,
`responsible_uid`, `file_attachment`, ...) survive round-trips without loss:

| Function | Signature | Filter & index coverage |
|---|---|---|
| `items_by_project` | `(conn, project_id: &Id) -> Result<Vec<Item>>` | `project_id = ?1 AND checked = 0 AND is_deleted = 0` (`idx_items_project_order`) |
| `items_by_section` | `(conn, section_id: &Id) -> Result<Vec<Item>>` | `section_id = ?1 AND checked = 0 AND is_deleted = 0` (`idx_items_section_order`) |
| `subtasks_of` | `(conn, parent_id: &Id) -> Result<Vec<Item>>` | `parent_id = ?1 AND checked = 0 AND is_deleted = 0` (`idx_items_parent_order`) |
| `today` | `(conn, today_date: &str) -> Result<Vec<Item>>` | `due_date IS NOT NULL AND due_date <= "<YYYY-MM-DD>~" AND checked = 0 AND is_deleted = 0` (`idx_items_due_date`); includes overdue items and items due today with a `THH:MM:SS` suffix |
| `upcoming` | `(conn, today_date: &str, days: u32) -> Result<Vec<Item>>` | `due_date > "<today>~" AND due_date <= "<today+days>~" AND checked = 0 AND is_deleted = 0` (`idx_items_due_date`); returns `Ok(vec![])` when `days == 0` |
| `items_by_label` | `(conn, label_name: &str) -> Result<Vec<Item>>` | `checked = 0 AND is_deleted = 0` and `json_each(items.labels)` contains `label_name` (`idx_items_checked`) |
| `inbox` | `(conn) -> Result<Vec<Item>>` | Resolves the user's `inbox_project_id` from `user.raw_json` (falling back to a project flagged `inbox_project`/`is_inbox_project` or named `"Inbox"`), then delegates to `items_by_project`; returns `Ok(vec![])` when no `user` row is cached |
| `all_active_items` | `(conn) -> Result<Vec<Item>>` | All items with `checked = 0 AND is_deleted = 0` (`idx_items_checked`) |
| `item_by_id` | `(conn, id: &Id) -> Result<Option<Item>>` | Single item by primary key where `is_deleted = 0` (**includes** `checked = 1` items) |
| `all_projects` | `(conn) -> Result<Vec<Project>>` | All projects with `is_deleted = 0`, ordered by `child_order ASC, id ASC` |
| `sections_by_project` / `all_sections` | `... -> Result<Vec<Section>>` | Non-deleted sections ordered by `section_order ASC, id ASC` |
| `all_labels` / `all_filters` | `... -> Result<Vec<Label>>` / `Result<Vec<Filter>>` | Non-deleted labels/filters ordered by `item_order ASC, id ASC` |
| `notes_for_item` | `(conn, item_id: &Id) -> Result<Vec<Note>>` | Non-deleted notes for `item_id` (guards against empty sentinel `""`), ordered by `posted_at ASC, id ASC` |
| `day_orders` | `(conn) -> Result<HashMap<Id, i32>>` | Decodes integer positions from the cached `day_orders` JSON map |

### Deterministic ordering guarantee

Every item view query (`items_by_project`, `items_by_section`, `subtasks_of`,
`today`, `upcoming`, `items_by_label`, `inbox`, `all_active_items`) orders its
result set by:

```sql
ORDER BY child_order ASC, id ASC
```

When two items share the same `child_order` (for example after concurrent edits
or bulk imports), the secondary sort on `id ASC` guarantees a deterministic,
reproducible order across runs and platforms. Against a 5,000-item / 25-project
cache (`tests/cache_perf.rs`), both `today` and `items_by_project` complete in
well under the 50 ms median budget.

### Checked-item retention rule (`REQ-001` D3 / Open Question 3)

Per `REQ-001` D3, v1 retains completed (`checked: true`) items in the `items`
table rather than deleting them when they are completed:

- When a delta marks an item `checked: true`, `item::upsert` writes the row with
  `checked = 1` (and preserves `completed_at` and all other fields in
  `raw_json`).
- All active-task view queries filter on `checked = 0 AND is_deleted = 0`, so
  completed tasks immediately disappear from `today`, `upcoming`, `inbox`, and
  project/section/label views.
- `queries::item_by_id` filters only on `is_deleted = 0` (not `checked = 0`), so
  `uncomplete` (`FR-27`, `EPIC-08`) can still look up a recently completed item
  by ID and reverse the completion without calling the network first.
- **Retention lifetime**: A `checked: true` item remains in the local cache
  until either (a) a subsequent incremental delta marks it `is_deleted: true`,
  or (b) a subsequent full sync (`full_sync: true` or `full_sync_date_utc`
  present) replaces all resource tables and no longer lists that completed item
  in its snapshot (`tests/cache_checked_retention.rs`).

## 5. `logout` cache wipe (`SqliteCacheWipe`)

`src/db/wipe.rs::SqliteCacheWipe` implements `crate::auth::cache_wipe::CacheWipe`
with `describe() == "cache"` and is wired into `todoist-tui logout` in
`src/cli/logout.rs`:

- `SqliteCacheWipe::wipe()` resolves `<state_dir>/cache.db` via
  `paths::cache_db_file()` and removes, in order:
  1. `<state_dir>/cache.db-wal` (WAL sidecar)
  2. `<state_dir>/cache.db-shm` (shared-memory sidecar)
  3. `<state_dir>/cache.db` (main database file)
- Sidecars are removed before the main database file so an interrupted or
  partially-failed unlink never leaves orphaned `-wal`/`-shm` files next to a
  missing or recreated database.
- `io::ErrorKind::NotFound` on any of the three files is treated as `Ok(())`,
  making `wipe()` idempotent when called repeatedly or when no cache database
  was ever created.
- Reopening the cache after a wipe (`open::open_default()` +
  `migrations::run()`) creates a fresh `0600` database file at
  `SCHEMA_VERSION`, with zero rows in all tables and `meta::sync_token() == None`.

### What a user loses on `todoist-tui logout`

Running `todoist-tui logout` deletes both the stored API credential (from the OS
keyring and/or `<config_dir>/token`) and the entire `cache.db` SQLite database:

- All locally cached projects, sections, tasks, subtasks, labels, notes, saved
  filters, `user` profile and `day_orders` are erased from disk (offline viewing
  is unavailable until the user authenticates and syncs again).
- The stored `sync_token`, `last_sync_at` and `full_sync_date_utc` cursors are
  removed, so the next login must perform a full sync (`sync_token: "*"`).
- Any retained `checked: true` items kept locally for undo (`uncomplete`) are
  removed.
- Once EPIC-05's offline `commands` queue table lands in `cache.db`, wiping the
  database on `logout` also discards any pending offline mutations that had not
  yet been synced to Todoist.

## 6. Open question: `child_order` vs. `day_orders` ordering semantics

- **Unverified against live data**: Every item view query in EPIC-04 orders by
  `child_order ASC, id ASC`, and `queries::day_orders` exposes the top-level
  `day_orders` map (`HashMap<Id, i32>`) separately. The exact relationship
  between `items.child_order` and the top-level `day_orders` map in Todoist's
  live Sync API v1 — specifically whether `day_orders` overrides `child_order`
  inside the Today view, whether `day_orders` keys are exclusively item IDs or
  can also include project IDs (as in `tests/fixtures/sync_full.json`), and
  whether `child_order` is 0-indexed or 1-indexed across all official clients —
  has **not** yet been verified against a live Todoist account. This remains an
  open question to reconcile when wiring view rendering and manual reordering in
  EPIC-07 / EPIC-08.

## 7. Fakes only

Every token, identifier and email in this document, in
`tests/fixtures/sync_full.json`, in `tests/fixtures/sync_incremental.json` and
in `tests/cache_*.rs` is synthetic (`tok_fake_abc123`, IDs prefixed `6X4Vfake`,
sync tokens prefixed `fake`, email `user@example.invalid`). Never commit a real
token, user ID or personal task content to this manual, a fixture, a test or a
log.

Behaviour source of truth: [`src/db/mod.rs`](../../src/db/mod.rs),
[`src/db/open.rs`](../../src/db/open.rs),
[`src/db/schema.sql`](../../src/db/schema.sql),
[`src/db/migrations.rs`](../../src/db/migrations.rs),
[`src/db/meta.rs`](../../src/db/meta.rs),
[`src/db/resources/mod.rs`](../../src/db/resources/mod.rs),
[`src/db/apply.rs`](../../src/db/apply.rs),
[`src/db/queries.rs`](../../src/db/queries.rs),
[`src/db/wipe.rs`](../../src/db/wipe.rs),
[`tests/cache_apply.rs`](../../tests/cache_apply.rs),
[`tests/cache_perf.rs`](../../tests/cache_perf.rs),
[`tests/cache_checked_retention.rs`](../../tests/cache_checked_retention.rs),
[`tests/fixtures/sync_incremental.json`](../../tests/fixtures/sync_incremental.json).
