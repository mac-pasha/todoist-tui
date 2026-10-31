# EPIC-05 spike: Sync API command error classes and the keep-vs-rollback matrix

This page is the deliverable of spike T-401 (EPIC-05, "Durable write queue & command
execution"). It fixes four things for the whole epic:

1. the classes of per-command Sync API errors the queue must handle;
2. for each class, whether the optimistic local change is KEPT or ROLLED BACK (FR-13);
3. the fallback behaviour for unrecognised `error_code` values;
4. the maximum flush batch size.

It is the single source of truth consumed by:

- T-408 (`src/queue/status.rs`): `classify(&CommandError) -> ErrorClass` and the
  keep/rollback decision implement exactly the matrix in §3 and cite this file.
  Class names are part of that contract — they must not be renamed or renumbered
  without editing this file and the code in the same change.
- T-406 (`src/queue/flush.rs`): the default `max_batch` value (§6).
- T-409 (`src/queue/recovery.rs`): the attempts cap referenced by its acceptance (§7).
- T-412 (`docs/manual/epic-05-write-queue.md`): links here for the matrix.

## 1. Scope and inputs

FR-13 (requirement 13, `docs/req/REQ-001.md`) reads:

> The system shall read per-command `sync_status`; on error, mark command failed,
> surface error in UI, keep/rollback local state per error type.

The input to classification is the per-command error object already modelled by
`CommandError` in [`src/api/sync.rs`](../../src/api/sync.rs):

| Field | Type | Notes |
|---|---|---|
| `error_code` | `Option<i64>` | absent (`None`) is itself an input case — see §4 |
| `error` | `Option<String>` | human-readable message; stored in the `commands.error` column |
| `extra` | flattened map | `error_tag`, `error_extra`, ... kept verbatim; not used for classification |

A successful command is the literal string `"ok"` (`CommandStatus::Ok`); it is not
an error class and only means "remove from the queue".

Transport-level failures of the whole `POST /sync` request (HTTP 401/403/429/5xx,
connect/timeout, decode) are a different layer, owned by `ApiError` and the T-206
retry helper — see §3 of `docs/manual/epic-03-sync-api.md`. This matrix classifies
only errors the server reports **per command** inside a 2xx response, keyed by
`command.uuid`.

## 2. Error classes

Six classes. These names are stable; T-408's `ErrorClass` enum mirrors them 1:1.

| Class | Meaning |
|---|---|
| `NotFound` | the command targets an object that does not exist server-side |
| `InvalidArgument` | the command itself is malformed, or built on stale/invalid state |
| `PermissionDenied` | the server refuses the action for this account or project |
| `RateLimited` | the command was throttled; nothing is wrong with the change itself |
| `ServerTransient` | a server-side error expected to clear on its own |
| `Unknown` | fallback for anything not mappable to the five classes above (§4) |

## 3. The matrix

Classification keys on `error_code` **only** — never on the `error` message text.
The example messages below pin the documented wording style but are not match
targets; the server may reword them at any time.

| Example `error_code` | Example `error` text | Class | Retryable? | Optimistic change | Rationale (FR-13) |
|---|---|---|---|---|---|
| `32` | `item does not exist` | `NotFound` | No | ROLLED BACK | target object is gone server-side; keeping the row would diverge permanently |
| `404` | `Project not found` | `NotFound` | No | ROLLED BACK | same object-missing situation as 32; a retry of a deterministic failure cannot converge |
| `15` | `invalid argument` (`error_tag: ARGUMENT_INVALID`) | `InvalidArgument` | No | ROLLED BACK | the command as constructed fails identically on every retry; keeping it freezes a phantom row |
| `403` | `User does not have permission to do this action` | `PermissionDenied` | No | ROLLED BACK | the server refuses the actor, not the timing; retry/keep can never land |
| `420` | `Too many requests. Try again later.` | `RateLimited` | Yes | KEPT | throttling says nothing about the change's validity; retry later with backoff |
| `500` | `Internal server error` | `ServerTransient` | Yes | KEPT | server-side blip; keep the optimistic state and retry on a later flush |
| `503` | `Service temporarily unavailable` | `ServerTransient` | Yes | KEPT | transient outage; same reasoning as 500 |
| any unlisted code | *(whatever the server sends — never used for classification)* | `Unknown` | No | ROLLED BACK | an unrecognised failure cannot prove the change landed; roll back and surface it |

Provenance of the codes: `15`, `32` and `404` appear in the EPIC-03 fixtures and
unit tests (`src/api/sync.rs`); `403`, `420`, `500` and `503` are the Sync API
error codes that correspond to these failure kinds. Codes seen in older Sync API
revisions but **not** confirmed against the current endpoint are mapped
provisionally; the §4 fallback guarantees defined behaviour regardless:

| Example `error_code` | Provisional class | Note |
|---|---|---|
| `400` | `InvalidArgument` | request could not be processed — treated as malformed command |
| `401` | `PermissionDenied` | per-command auth refusal; HTTP-level 401 is already `ApiError::Unauthorized` |
| `409` | `InvalidArgument` | command built on stale server state; conflict policy itself is EPIC-06 (FR-18) |
| `418` | `NotFound` | temp id cannot be resolved — the referenced parent command failed, so the child cannot stand alone |

Every non-retryable class above ROLLS BACK; both retryable classes KEEP. There is
no class where retryable and rollback mix.

## 4. Fallback rule: no undefined behaviour

Single fallback class: **`Unknown`**. It applies whenever an error object is
present but no code in §3 matches, specifically:

- `error_code` is absent (`CommandError::error_code` is `None`);
- `error_code` is present but not listed in §3;
- the error object carries only free-form fields.

`Unknown` is **not retryable** and the optimistic change is **ROLLED BACK**.
Rationale: an unrecognised failure is unproven; silently keeping an optimistic
change that may never land is the one outcome FR-13 cannot tolerate, while a
rollback plus UI surfacing (EPIC-07) lets the user re-apply through the normal
flow. One consequence: a poisoned command never auto-retries and never stalls
the queue (T-408's mixed-response rule).

Note on typing: `error_code` deserialises as `Option<i64>`. A server that sends a
non-integer `error_code` makes the whole `sync_status` entry fail to decode as
`CommandStatus`, which surfaces upstream as `ApiError::Decode` at the EPIC-03
layer — such a response never reaches `classify` at all. This is tracked in §8.

## 5. KEPT vs ROLLED BACK, mechanically

- **KEPT** means: the optimistic cache mutation applied at enqueue time (T-405)
  stays; the command row leaves `in_flight` and returns to `pending` with its
  original `uuid`, so a later flush re-sends it and the server deduplicates by
  `uuid` (idempotency precondition).
- **ROLLED BACK** means: the caller-supplied rollback hook in T-408 reverses the
  enqueue-time mutation in the same transaction; the command row is marked
  `failed` with the `error` text stored for UI surfacing (EPIC-07). It is out of
  the retry loop and never auto-requeued.

Related rule, tested by T-408 but not an error class: a command that was
`in_flight` and **absent from `sync_status`** (ambiguous outcome, e.g. the
request body was accepted but the status is missing) is treated as KEPT and
returns to `pending` with its original `uuid` — a safe re-send under uuid
idempotency. This also covers the whole-request failure case: if `POST /sync`
fails at the transport layer, no `sync_status` exists and the same rule applies.

## 6. Maximum flush batch size

**`max_batch = 100` commands per flush request.** This is the default T-406's
`build_flush_batch` uses and the number its tests pin (100 pending commands
produce exactly one request). Rationale:

- 100 keeps request bodies comfortably small even with the largest expected
  command payloads, well below any payload limits on the request path;
- it bounds the blast radius of a bad request: at most 100 commands share a
  flush cycle, and per-command `sync_status` still isolates individual failures
  so one poisoned command never stalls the other 99;
- it stays far enough below any undocumented server-side cap on commands per
  request that our flushes never trip one (the cap itself is unconfirmed — §8);
- it matches the Sync API's batching model of "many commands, one request"
  while keeping every flush a single round trip.

A flush is a pure command submission: its `SyncRequest` carries **empty
`resource_types`**. Resource pulls happen in the regular full/incremental sync
(EPIC-03/06) and are never piggybacked onto a flush.

## 7. Attempts cap for retryable classes

**Cap: 5 flush attempts per command.** Only `RateLimited`, `ServerTransient` and
the ambiguous re-sends of §5 consume attempts (each flush cycle of a command
increments `attempts` once, per T-406). When a command's `attempts` exceeds the
cap it is marked `failed` with the error text `max attempts exceeded` (T-409),
surfaced via the UI instead of churning forever. Rationale: at the 60s background
sync interval (FR-16) five attempts span roughly five minutes of flush cycles,
enough to ride out a brief outage or throttle window, while keeping a
persistently failing command from retrying indefinitely. HTTP-level retries
(T-206 backoff) do not consume queue attempts: they happen inside one flush and
never produce a `sync_status` to apply.

## 8. Open questions

Things this spike could not confirm from the Sync API documentation, handed to
EPIC-06 (and T-412's manual):

1. **Recurring-task completion offline** (also REQ-001 open question 6): whether
   an offline-queued complete of a recurring task reports the advanced due date
   or needs special handling is unconfirmed; EPIC-05/08 currently assume
   server-authoritative behaviour (optimistic completion, corrected on next sync).
2. **Per-command `RateLimited`/`ServerTransient`**: HTTP-level 429/5xx are
   confirmed and handled by EPIC-03; whether the Sync API ever reports these
   conditions per command inside `sync_status` is unconfirmed. The classes exist
   so the queue handles them if seen; T-408's table-driven test covers both.
3. **Provisional codes 400/401/409/418**: seen in older Sync API revisions, not
   confirmed against the current endpoint. Each is safely covered by its mapped
   class or the `Unknown` fallback either way.
4. **Server-side cap on commands per request**: not documented as a hard number;
   100 is our conservative default (§6). Revisit only if flushes start failing.
5. **`error_code` typing**: `CommandError.error_code` is `Option<i64>` in code,
   but the sync.rs doc-comment example shows a string-style code
   (`"PROJECT_NOT_FOUND"`). If the server ever sends a string code, the untagged
   `CommandStatus` decode fails and the whole response surfaces as
   `ApiError::Decode` — that would need a code change in EPIC-03/05, out of
   scope for this docs task.
6. **Attempts cap tuning**: the cap of 5 (§7) is a local default, not derived
   from server guidance; tune after real-world use.

## 9. Example `sync_status` payload

All identifiers below are fakes; nothing here is copied from a real account.

```json
{
  "sync_status": {
    "cmd-fake-0001": "ok",
    "cmd-fake-0002": {
      "error_code": 32,
      "error": "item does not exist"
    },
    "cmd-fake-0003": {
      "error_code": 15,
      "error": "invalid argument",
      "error_tag": "ARGUMENT_INVALID",
      "error_extra": { "argument": "due_string" }
    },
    "cmd-fake-0004": {
      "error": "unexpected failure shape"
    }
  }
}
```

Expected classification: `cmd-fake-0001` removed from the queue;
`cmd-fake-0002` → `NotFound` (rolled back); `cmd-fake-0003` → `InvalidArgument`
(rolled back); `cmd-fake-0004` has no `error_code` → `Unknown` fallback (rolled
back).
