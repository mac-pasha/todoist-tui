# Sync Engine Architecture & Behaviour

The background sync engine (`src/sync/`) coordinates all network synchronization against the Todoist Sync API (`POST /api/v1/sync`). It keeps the local SQLite cache current, drains the durable write queue, detects connectivity interruptions, and guarantees consistent conflict resolution.

## 1. Sync Cycle Sequence

Each cycle executes atomically via [`SyncCycle::run_once`](../src/sync/cycle.rs):

1. **Drain & Flush Batch**: Selects pending commands from the SQLite `commands` queue (up to 100 per batch in enqueue order), marking them `in_flight`.
2. **Token Selection**: Reads the stored incremental `sync_token` from cache metadata. Uses `"*"` if absent (full sync), otherwise uses the stored incremental token.
3. **API Request**: Submits the combined `SyncRequest` (`sync_token`, `resource_types`, and `commands`) to `POST /api/v1/sync`.
   - On failure: In-flight commands revert to `pending` with their original UUIDs intact; `sync_token` is untouched.
4. **Temp ID Resolution**: Applies `temp_id_mapping` from the response across both queued commands and cached SQLite resource rows.
5. **Status Application**: Dispatches per-command `sync_status` entries: `"ok"` rows are deleted; errors are classified and rolled back or kept per `docs/manual/epic-05-error-classes.md`.
6. **Cache Delta / Replacement**: Applies resources (`items`, `projects`, `sections`, `labels`, `notes`, `filters`) transactionally to SQLite. Stores the new `sync_token`.
7. **Conflict Replay (FR-18)**: Replays any still-pending local edits on top of the updated cache.
8. **Full-Sync Lag Check**: If `full_sync: true` and `full_sync_date_utc` is older than 5 minutes (`DEFAULT_LAG_THRESHOLD_SECS = 300`), runs one immediate follow-up incremental cycle to catch up.

## 2. `SyncState` Lifecycle Events

Subscribers (such as the TUI status bar) receive updates through `SyncHandle::subscribe()`:

| Variant | Meaning |
|---|---|
| `SyncState::Syncing` | A sync cycle is actively contacting the Todoist API. |
| `SyncState::Synced { at }` | Cycle completed cleanly; local cache and remote state are synchronized. |
| `SyncState::Pending { count }` | Indicates `count` queued mutations awaiting flush. |
| `SyncState::Offline` | Network or transport failure encountered; engine is backing off. Local mutations remain fully functional. |
| `SyncState::Error { msg }` | Non-transport error (e.g., 401 Unauthorized or storage failure); safe display text without secret material. |

## 3. Conflict Resolution Policy (FR-18 LWW)

Todoist Sync API is server-authoritative for shared state, while offline edits must preserve user intent. `src/sync/conflict.rs` enforces **Server Wins + Pending Local Replay (Last-Write-Wins)**:

- Incoming server deltas are applied to SQLite first.
- Any commands remaining `pending` in the write queue (e.g., created while offline or rate-limited) are re-applied field-by-field onto matching cached items.
- **Worked Example (Cross-Field Merge / REQ-001 D2)**:
  Suppose item `X` has remote due date updated to `"2026-12-25"` in the pulled response, while a pending local `item_update` modified `X`'s `content` to `"Buy milk"`:
  1. Cache receives the server response (`X` gets remote `due = "2026-12-25"`).
  2. `replay_pending_conflicts` reads the pending local command and applies only `content = "Buy milk"` onto `X`.
  3. Result: `X` preserves **both** `content: "Buy milk"` and `due: "2026-12-25"`.

## 4. Offline Resilience & Backoff

Managed by [`OfflineTracker`](../src/sync/offline.rs):
- Transport failures (`SyncError::is_offline()` or exhausted connection retries) switch state to `SyncState::Offline`.
- Retry intervals grow exponentially starting from `sync_interval` up to a hard cap of **5 minutes (`DEFAULT_MAX_OFFLINE_BACKOFF = 300s`)**.
- Over a 30-minute outage (with default 60s interval), retry rate is strictly bounded (<= 10 attempts, preventing retry storms).
- Local reads and writes are never blocked while offline.
- Manual user sync triggers (`SyncHandle::trigger()`) bypass the wait delay immediately (coalesced to at most 1 pending run while active).
- Upon the first successful cycle after an outage, the backoff resets to `sync_interval`.

## 5. Configuration & Tuning

Configured via `Config` in [`src/config/model.rs`](../src/config/model.rs):
- `sync_interval`: Polling interval between automatic background sync cycles (default: `60` seconds).
- Full-sync lag threshold: Hardcoded safety threshold of `300` seconds (`5` minutes). When a full sync snapshot exceeds this age, an incremental sync runs immediately once per cycle (non-recursive).

