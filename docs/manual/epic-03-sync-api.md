# EPIC-03 manual: the Sync API client contract and fixture-drift procedure

This page documents the request/response contract `SyncClient` implements, its
error taxonomy and retry behaviour, and how to refresh the fixture that pins the
response shape against a live account. It is a factual reference for anyone
changing `src/api/{client,sync,retry}.rs` or `tests/fixtures/sync_full.json`.
User-facing README/install content belongs to EPIC-12, not here.

## 1. Request contract

`SyncClient::sync` sends one `POST {base}/api/v1/sync` request per call, with
`Authorization: Bearer <token>` (the token is read only through `Token::expose`
when building that header — it is never formatted into the URL, a log line or
an error) and a JSON body built from `SyncRequest`:

| Field | Full sync (`SyncRequest::full()`) | Incremental (`SyncRequest::incremental(token)`) |
|---|---|---|
| `sync_token` | `"*"` | the caller-supplied token, typically the previous response's `sync_token` |
| `resource_types` | all eight types below, in order | the same eight types, in order |
| `commands` | omitted from the wire body (`skip_serializing_if`) when empty | same |

The eight resource types are fixed by `src/api/sync.rs::RESOURCE_TYPES` and are
requested together, every time — the Sync API does not let a caller ask for a
subset independently of the full/incremental distinction described above:

```
projects, items, sections, labels, notes, filters, user, day_orders
```

`RESOURCE_TYPES` is the single source of truth both `SyncRequest::full()`/
`::incremental()` and the T-209 wiremock tests read from, so the request body
and the test assertion cannot drift apart from each other (they can still drift
from the real API — see §3).

`SyncResponse` deserialises `sync_token`, `full_sync`, `temp_id_mapping`,
`sync_status`, `full_sync_date_utc`, plus one array per resource type above
(each defaulting to empty when the server omits it; `user` is `Option`). Every
resource struct (`Project`, `Item`, `Section`, `Label`, `Note`, `Filter`,
`User`) and the envelope itself tolerate unknown JSON fields — a key the model
doesn't recognise, at any level, does not fail deserialisation.

## 2. Transport

`SyncClient::new`/`SyncClient::with_clock` build **one** reused `reqwest::Client`
with `https_only(true)`, a connect timeout and a request timeout (named
constants in `src/api/client.rs`, not bare literals). A non-`https://` base URL
is rejected with `ApiError::InsecureBaseUrl` before the client is even built —
no network activity happens. The same debug-only loopback exception as
`src/auth/validate.rs` applies: in a `debug_assertions` build only,
`http://127.0.0.1`, `http://localhost` and `http://[::1]` are also accepted, so
test harnesses (including the T-209 wiremock suite) can point the client at a
plaintext local mock server; this path cannot be reached in a release binary.

## 3. Error taxonomy

`sync()`'s `Result` is a `crate::error::AppError` wrapping one of these
`ApiError` variants (exact names from `src/error.rs`):

| Variant | When | Retried? |
|---|---|---|
| `Unauthorized` | HTTP 401 or 403 | No |
| `RateLimited { retry_after }` | HTTP 429; `retry_after` is the `Retry-After` header parsed as delta-seconds, `None` if the header is absent or not a plain non-negative integer | Yes |
| `Server { status }` | HTTP 5xx (and, as a documented catch-all, any other unmapped non-2xx status) | Yes |
| `Transport { source }` | the request/connection failed before a status code was available (DNS, TLS, timeout, connection refused, ...) | Yes |
| `Decode { source }` | the response body could not be parsed into `SyncResponse` | No |
| `InsecureBaseUrl` | non-HTTPS `base_url` (checked at construction, not per-request) | n/a (never reaches `sync()`) |
| `RetriesExhausted { attempts }` | the retry budget ran out while retrying one of the retryable variants above | n/a (terminal) |

`ApiError::is_retryable()` is the single source of truth for the "Retried?"
column: `Server`, `RateLimited` and `Transport` are retryable; everything else
is not. `sync()` runs the full attempt — HTTP send **and** status-code mapping
— through the T-206 `retry()` helper, so a 429 or 5xx is retried exactly like a
transport failure, not just a `send()` failure.

### Backoff parameters

`RetryPolicy::new()` (also its `Default`) is: `max_attempts: 4`, `base_delay:
500ms`, `max_delay: 30s`, `jitter_fraction: 0.2` (±20%). The delay for attempt
`n` is `base_delay * 2^(n-1)`, capped at `max_delay`, then jittered by up to
±20% of the capped value. A `RateLimited` response's `Retry-After` hint, when
present, raises (never lowers) the computed delay for that attempt — the
client waits at least as long as the server asked. Both the policy and the
clock (`Sleeper` trait) are injectable via `SyncClient::with_clock`/
`with_retry_policy`, which is how the test suite asserts backoff behaviour
without real sleeping (`RecordingSleeper` in `src/api/retry.rs`).

## 4. Refreshing the fixture

`tests/fixtures/sync_full.json` is a hand-written, hand-verified stand-in for a
real full-sync response, reconciled field-by-field against `SyncResponse`/the
domain structs as of the date in its `_fixture.shape_captured` header. It is
**not** a captured live response committed as-is — refreshing it means:

1. Make one real `POST /api/v1/sync` full-sync call against your own test
   account (`sync_token: "*"`, the eight `RESOURCE_TYPES`) and save the raw
   response body somewhere outside the repo.
2. Diff its shape (field names, nesting, which fields are present/absent)
   against the current `tests/fixtures/sync_full.json` and against
   `src/api/sync.rs`/`src/api/model/*.rs`. Add any new fields to the domain
   structs first (as their own task/PR) if the live shape has grown.
3. Rebuild the fixture by hand from that diff — do **not** copy the raw
   response into the repository. Every id must be replaced with an obviously
   fake one following the existing convention (`6X4Vfake...`), the only email
   must become `user@example.invalid`, and the sync token must start with
   `fake`. No real account id, email, phone number, credential or other prod
   data may appear anywhere in the committed file.
4. Keep at least the shapes the EPIC-03 acceptance criteria name: two
   projects, four items (recurring `due`, a `deadline`, one `checked`, one
   `is_deleted`), one section, two labels, one note, one filter, the `user`
   object, `day_orders`, `temp_id_mapping`, both `sync_status` forms (the
   `"ok"` string and the per-command error object), `full_sync: true` and
   `full_sync_date_utc`. Keep the deliberately-unknown keys at top level,
   inside an item and inside a `due` object (see the fixture's own `_fixture`
   note for the current list) — they exist to prove unknown-field tolerance.
5. Update `_fixture.shape_captured` to the date you did this, and update
   `tests/sync_fixture.rs`'s assertions if any field value you rely on
   changed.
6. Run `cargo test --all` — `tests/sync_fixture.rs` and the T-209 wiremock
   suite in `tests/sync_client.rs` both exercise this fixture and must stay
   green.

## 5. Out of scope for this epic

- Persistence, queueing, scheduling and conflict handling (EPIC-04 and later
  own the local cache and sync orchestration).
- Any endpoint other than `/api/v1/sync` — in particular the quick-add and
  completed-tasks endpoints are gated on open questions 1 and 3 in
  `docs/epics/REQ-001.epics.md` and are not implemented here.
- `src/auth/validate.rs` remains in the tree as a minimal, single-request
  "is this token accepted" check (no retries, no response parsing); it does
  not call `SyncClient` and is **superseded** by this client for anything
  beyond that yes/no question, per its own module documentation.

## 6. Fakes only

Every token and id in this document, in `tests/fixtures/sync_full.json` and in
`tests/sync_client.rs` is fake (`tok_fake_abc123`, ids prefixed `6X4Vfake`, the
email `user@example.invalid`). Never commit a real token, account id or
personal data to this file, a fixture, a test or a log.

Behaviour source of truth: [`src/api/client.rs`](../../src/api/client.rs),
[`src/api/sync.rs`](../../src/api/sync.rs), [`src/api/retry.rs`](../../src/api/retry.rs),
[`src/error.rs`](../../src/error.rs), [`src/auth/validate.rs`](../../src/auth/validate.rs),
[`tests/sync_client.rs`](../../tests/sync_client.rs),
[`tests/sync_fixture.rs`](../../tests/sync_fixture.rs),
[`tests/fixtures/sync_full.json`](../../tests/fixtures/sync_full.json).
