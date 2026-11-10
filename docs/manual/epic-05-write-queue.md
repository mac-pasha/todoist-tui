# Epic 05: Durable Write Queue & Command Execution

This document specifies the contract, schema, state machine, and error handling for the durable SQLite-backed write queue (`src/queue/`).

## 1. Architecture Overview

All user mutations flow through the write queue. The queue persists writes to disk inside an atomic SQLite transaction alongside optimistic cache updates before network transmission.

```
+-------------------------------------------------------------+
| UI Actions / Domain Mutations (EPIC-08 / EPIC-09)           |
+-------------------------------------------------------------+
                             |
                   enqueue(&mut conn, cmd, local_apply)
                             v
+-------------------------------------------------------------+
| Atomic SQLite Transaction:                                  |
| 1. INSERT INTO commands (state = 'pending', attempts = 0)   |
| 2. local_apply(&mut tx) -> mutate cache tables              |
+-------------------------------------------------------------+
                             |
             build_flush_batch(&mut conn, limit)
                             v
+-------------------------------------------------------------+
| SyncEngine Flush Loop (EPIC-06):                            |
| 1. Mark batch 'in_flight', increment attempts               |
| 2. POST /api/v1/sync with batch commands                    |
| 3. resolve_temp_ids(tx, temp_id_mapping, cache_rewrite)     |
| 4. apply_sync_status(tx, batch, sync_status, rollback)      |
+-------------------------------------------------------------+
```

## 2. Table Schema & State Machine

### 2.1 Table Schema

Defined in `src/queue/schema.rs` and registered in SQLite migration v2:

```sql
CREATE TABLE IF NOT EXISTS commands (
    uuid TEXT PRIMARY KEY,
    type TEXT NOT NULL,
    args TEXT NOT NULL,
    temp_id TEXT NULL,
    state TEXT NOT NULL CHECK (state IN ('pending', 'in_flight', 'failed')),
    attempts INTEGER NOT NULL DEFAULT 0,
    created_at INTEGER NOT NULL,
    error TEXT NULL
);

CREATE INDEX IF NOT EXISTS idx_commands_state_created_at
    ON commands(state, created_at);
```

### 2.2 State Machine

```
              enqueue()
                 |
                 v
           +-----------+
    +----->|  pending  |<----------------+
    |      +-----------+                 |
    |            |                       |
    |            | build_flush_batch()   |
    |            v                       |
    |      +-----------+                 |
    |      | in_flight |                 |
    |      +-----------+                 |
    |         /    |    \                |
recover_on_start() |     \ sync_status absent (requeue)
    |              |      +--------------+
    |              v
    |        sync_status:
    |        +---+---+
    |        |       |
    |   "ok" v       v error
    |     [DELETE]  +--------+
    |               | failed |
    +---------------+--------+
```

1. **`pending`**: Newly enqueued command awaiting flush.
2. **`in_flight`**: Included in an active flush batch. Attempts counter incremented.
3. **`removed`**: On `"ok"` status, row is deleted from `commands`.
4. **`failed`**: On error status or when `attempts >= MAX_ATTEMPTS` (5), marked `failed` with error message recorded.
5. **Requeued / Recovered**: If a command is omitted from `sync_status`, or the process crashes mid-flight, `recover_on_start()` resets `in_flight` rows back to `pending` without changing their UUID.

## 3. Caller Contracts

### 3.1 Enqueue Contract (EPIC-08 / EPIC-09)
Callers use `crate::queue::enqueue::enqueue` or `enqueue_many`:
- **Single Transaction**: The command row and local cache mutation commit together. If `local_apply` fails, the transaction rolls back, leaving no trace in `commands` or cache.
- **No HTTP**: Callers must NEVER perform network I/O inside or before enqueue.
- **No Standalone Writes**: Cache tables should not be updated without a corresponding queued command (except when applying pulled sync responses).

### 3.2 Flush Contract (EPIC-06)
`SyncEngine` executes the flush sequence:
1. `build_flush_batch(&mut conn, limit)`: Selects oldest `pending` rows, marks them `in_flight`, increments `attempts`, and generates the `SyncRequest`.
2. `SyncClient::sync(&request)`: Sends commands to the Sync API.
3. `resolve_temp_ids(&tx, &temp_id_mapping, ...)`: Replaces temporary IDs across remaining commands and cache rows.
4. `apply_sync_status(&tx, &batch, &sync_status, rollback)`: Removes `"ok"` commands, executes rollback for `ErrorClass::should_rollback()` failures, and marks failed commands.

## 4. Idempotency & Crash Recovery

- **UUID Preservation**: Every command generates a UUID v4 exactly once upon creation. During retries or post-crash recovery, the identical UUID is transmitted. Todoist Sync API de-duplicates identical UUIDs within its idempotency window.
- **Ambiguous Failures**: Network dropouts, 5xx server errors, or process restarts leave commands eligible for retry with their original UUIDs.
- **Attempt Capping**: Commands failing more than 5 times (`MAX_ATTEMPTS`) are moved to `failed` to prevent infinite flush loops.

## 5. Error Classification & Rollbacks

Error classification inspects machine-readable `error_code` integers per `docs/manual/epic-05-error-classes.md`:
- **Rollback Errors**: E.g. `NotFound` (32, 404), `InvalidArgument` (15, 400), `PermissionDenied` (401, 403). Local optimistic cache row is reverted.
- **Keep Errors**: E.g. `RateLimited` (420), `ServerTransient` (500, 503). Local change is kept pending retry.
- **Unknown Error Codes**: Any unrecognized code defaults to `ErrorClass::Unknown` (rolled back for safety).

### Procedure for New Error Codes:
When a new Sync API error code is observed in production:
1. Add the error code to the classification table in `docs/manual/epic-05-error-classes.md`.
2. Update the match arms in `src/queue/status.rs:classify()`.
3. Add a unit test verifying classification and keep-vs-rollback behavior.

## 6. Open Questions & Handoffs

- **EPIC-06 Sync Engine**: Conflict resolution between incoming remote changes and pending local commands is handled by `src/sync/conflict.rs` (last-write-wins by updated timestamp).
- **Recurring Tasks**: Offline completion of recurring tasks requires server-side recurrence advancement; local cache marks completed until sync response updates due date.

