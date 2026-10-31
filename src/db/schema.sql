-- Schema DDL for migration 1 (T-303, EPIC-04).
--
-- Every resource table is keyed by its Sync API id (`TEXT PRIMARY KEY`,
-- since Todoist ids are opaque strings, not necessarily integers we can
-- rely on for `ROWID` semantics) and models only the columns the read
-- queries need directly as typed columns; everything else the Sync API
-- returns rides along in `raw_json` so round-tripping a row back out to
-- the API (or future features) never loses data that wasn't modelled yet.
--
-- `meta` is a generic key/value table for small scalars that don't belong
-- to a resource (e.g. the Sync API's `sync_token`). `user` and
-- `day_orders` are each a single logical row describing the authenticated
-- account; see the per-table comments below for why they're still
-- modelled as tables rather than folded into `meta`.

CREATE TABLE IF NOT EXISTS projects (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    parent_id TEXT,
    child_order INTEGER NOT NULL DEFAULT 0,
    color TEXT,
    is_archived INTEGER NOT NULL DEFAULT 0,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    is_favorite INTEGER NOT NULL DEFAULT 0,
    raw_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS sections (
    id TEXT PRIMARY KEY,
    project_id TEXT NOT NULL,
    name TEXT NOT NULL,
    section_order INTEGER NOT NULL DEFAULT 0,
    is_archived INTEGER NOT NULL DEFAULT 0,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    raw_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS items (
    id TEXT PRIMARY KEY,
    project_id TEXT,
    section_id TEXT,
    parent_id TEXT,
    content TEXT NOT NULL,
    description TEXT,
    child_order INTEGER NOT NULL DEFAULT 0,
    checked INTEGER NOT NULL DEFAULT 0,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    priority INTEGER NOT NULL DEFAULT 1,
    due_date TEXT,
    labels TEXT NOT NULL DEFAULT '[]',
    added_at TEXT,
    raw_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS labels (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    color TEXT,
    item_order INTEGER NOT NULL DEFAULT 0,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    is_favorite INTEGER NOT NULL DEFAULT 0,
    raw_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS notes (
    id TEXT PRIMARY KEY,
    item_id TEXT NOT NULL,
    content TEXT NOT NULL,
    posted_at TEXT,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    raw_json TEXT NOT NULL
);

CREATE TABLE IF NOT EXISTS filters (
    id TEXT PRIMARY KEY,
    name TEXT NOT NULL,
    query TEXT NOT NULL,
    item_order INTEGER NOT NULL DEFAULT 0,
    is_deleted INTEGER NOT NULL DEFAULT 0,
    is_favorite INTEGER NOT NULL DEFAULT 0,
    raw_json TEXT NOT NULL
);

-- Single logical row (`id` fixed at `'me'`) describing the authenticated
-- account; kept as a table (rather than flattened into `meta`) because its
-- payload is a whole Sync API `user` object, not a scalar.
CREATE TABLE IF NOT EXISTS user (
    id TEXT PRIMARY KEY,
    full_name TEXT,
    email TEXT,
    raw_json TEXT NOT NULL
);

-- Single logical row (`id` fixed at `'me'`) holding the authenticated
-- user's day-order map (project/item id -> manual ordering position);
-- kept separate from `user` since the Sync API models it as its own
-- top-level resource with its own incremental updates.
CREATE TABLE IF NOT EXISTS day_orders (
    id TEXT PRIMARY KEY,
    raw_json TEXT NOT NULL
);

-- Generic scalar key/value store, e.g. the Sync API's `sync_token`.
CREATE TABLE IF NOT EXISTS meta (
    key TEXT PRIMARY KEY,
    value TEXT
);

-- Read-path indices: the TUI's primary views list items by project,
-- section or parent in manual order, plus due-date and checked-state
-- filters (today/overdue views, hiding completed tasks).
CREATE INDEX IF NOT EXISTS idx_items_project_order ON items (project_id, child_order);
CREATE INDEX IF NOT EXISTS idx_items_section_order ON items (section_id, child_order);
CREATE INDEX IF NOT EXISTS idx_items_parent_order ON items (parent_id, child_order);
CREATE INDEX IF NOT EXISTS idx_items_due_date ON items (due_date);
CREATE INDEX IF NOT EXISTS idx_items_checked ON items (checked);
