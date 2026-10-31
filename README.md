# todoist-tui

A keyboard-driven terminal client for [Todoist](https://todoist.com), written in Rust.

It talks to the Todoist Sync API (`POST /api/v1/sync` only), keeps a local SQLite cache so
startup never waits on the network, and records local edits as durable Sync API commands
that flush when a connection is available — so the app stays usable offline and agrees with
the server once it is back.

- UI: `ratatui` + `crossterm`
- HTTP: `reqwest` with rustls (no OpenSSL in the build)
- Storage: `rusqlite` with the bundled SQLite (no system `libsqlite3`)
- Token: OS keychain via `keyring`, with a `0600` file fallback

## Status

`todoist-tui` is being built epic by epic (see [`docs/epics/REQ-001.epics.md`](docs/epics/REQ-001.epics.md),
[`docs/req/REQ-001.md`](docs/req/REQ-001.md)). What this means for the commands below:

| Area | State |
|---|---|
| Config, logging, CLI surface (`--help`, `--version`, `--config`) | implemented |
| `todoist-tui auth` / `todoist-tui logout` | implemented |
| Todoist Sync API client (`src/api`) | implemented |
| Local SQLite cache | not merged yet (EPIC-04) |
| Durable write queue | not merged yet (EPIC-05) |
| Background sync engine | not merged yet (EPIC-06) |
| Interactive TUI (running `todoist-tui` with no subcommand) | not merged yet (EPIC-07/08/09/10) |
| `todoist-tui add` / `sync` / `today` | not merged yet (EPIC-11) |

Sections that describe not-yet-merged behaviour say so inline. **The keybinding and
configuration-default tables in this file are the documentation contract**: T-1111 adds
`tests/docs_sync.rs`, which parses them out of this file and asserts they match the code's
default keymap and default config values. Keep the marker comments and the column headers
exactly as they are if you edit the rows.

## Install

### With `cargo install`

Requires a Rust toolchain (the repo pins one in `rust-toolchain.toml`, currently `1.99.1`).

```sh
cargo install todoist-tui
```

That builds the crate from source and installs the `todoist-tui` binary into `~/.cargo/bin`.
(Until the first crates.io release ships, use the tarball or the from-source route below.)

### Prebuilt release tarball

Tagged releases will publish prebuilt binaries for Linux (x86_64) and macOS (x86_64 and
aarch64) as `todoist-tui-<version>-<target>.tar.gz`, each with a `.sha256` checksum file
attached to the GitHub release (release workflow: T-1109, not merged yet — no tagged release
exists at the time of writing).

The repository slug is not fixed in `Cargo.toml` yet (it is added by T-1108); substitute it
for `$REPO` below.

```sh
REPO=<owner>/todoist-tui
VERSION=0.1.0
TARGET=x86_64-unknown-linux-gnu   # or aarch64-apple-darwin, x86_64-apple-darwin

curl -LO "https://github.com/$REPO/releases/download/v$VERSION/todoist-tui-$VERSION-$TARGET.tar.gz"
curl -LO "https://github.com/$REPO/releases/download/v$VERSION/todoist-tui-$VERSION-$TARGET.tar.gz.sha256"
shasum -a 256 -c "todoist-tui-$VERSION-$TARGET.tar.gz.sha256"   # on Linux: sha256sum -c
tar -xzf "todoist-tui-$VERSION-$TARGET.tar.gz"
install -m 0755 todoist-tui ~/.local/bin/todoist-tui
```

### Build from source

```sh
git clone https://github.com/$REPO.git
cd todoist-tui
cargo build --release
./target/release/todoist-tui --version
```

Or install the working tree directly:

```sh
cargo install --path .
```

`reqwest` is pinned to rustls and `rusqlite` to its bundled SQLite, so a C toolchain is only
needed for the SQLite amalgamation — no OpenSSL headers, no system `libsqlite3`. On Linux the
keychain backend speaks D-Bus in pure Rust; see [Troubleshooting](#troubleshooting) for the
runtime requirement.

## First run: authentication

`todoist-tui` authenticates with a personal Todoist API token (Bearer). Get one from
Todoist → Settings → Integrations → Developer, then:

```sh
todoist-tui auth
```

`auth` prompts on **stderr** with `Todoist API token: `, reads one line with terminal echo
disabled, validates the token against the API, and stores it. On success it prints
`Token saved.` on stdout. When stdin is not a TTY (piped or redirected) the line is read
plainly, so the command is scriptable:

```sh
printf 'tok_fake_exampletoken\n' | todoist-tui auth
```

The value above is a placeholder, so it will be rejected by server-side validation — the
example only shows the plumbing. Never put a real token in a script, a shell history or a CI
log.

The token is never logged, never formatted into an error message and never placed in a URL;
the log-writing side of the codebase only ever renders it through the `Secret` redaction
helper, which prints `***`.

### Where the token is stored

| Platform | Backend | Entry |
|---|---|---|
| macOS | Keychain (login keychain) | generic password, service `todoist-tui`, account `api-token` |
| Linux / other Unix-likes | Secret Service over D-Bus | item in the default collection, service `todoist-tui`, account `api-token` |
| Windows | Credential Manager | generic credential, service `todoist-tui`, account `api-token` |

When the platform keyring is unavailable, `auth` falls back to a file and prints a notice
naming the resolved path:

```
$XDG_CONFIG_HOME/todoist-tui/token
```

`XDG_CONFIG_HOME` is honoured on every platform; when unset or empty the platform default
applies (see [file locations](#file-locations)). On Unix the file is created with owner-only
mode `0600` from the first `open(2)`, so a more permissive file never exists even briefly.

### `logout`

```sh
todoist-tui logout
```

`logout` clears **both** token backends (keyring and file) whether or not the other one
fails, then runs the cache-wipe hook, and exits `0`. It is idempotent: running it with
nothing stored is a no-op, not an error. The cache-wipe hook is currently a no-op — the real
cache database arrives with EPIC-04, and until then `logout` prints
`Cleared cache (not yet implemented).`

## Keybindings

> **Placeholder.** The bindings below are the defaults specified by EPIC-07 (navigation,
> help, sync, quit) and are best-effort against the epic text; the TUI that implements them
> is not merged yet, and EPIC-08 (task actions), EPIC-09 (projects/sections/labels) and
> EPIC-10 (search) will add rows to this table. Treat the column set and the marker
> comments as final, and the rows as TBD until those epics land.

Vim-style navigation, with `?` opening an overlay generated from the active keymap (so a
customised keymap is always reflected there). Bindings can be rebound through the `keymap`
table in `config.toml`; see [Configuration](#configuration).

<!-- BEGIN KEYMAP -->
| Key | Context | Action |
|---|---|---|
| `j` | Sidebar, task list | Move selection down |
| `k` | Sidebar, task list | Move selection up |
| `h` | Global | Move focus left (detail → list → sidebar); collapse the selected project in the sidebar |
| `l` | Global | Move focus right (sidebar → list → detail); expand the selected project in the sidebar |
| `g` | Task list | Jump to the first task |
| `G` | Task list | Jump to the last task |
| `/` | Global | Open the search prompt |
| `r` | Global | Sync now |
| `?` | Global | Toggle the help overlay |
| `q` | Global | Quit |
<!-- END KEYMAP -->

The status bar reports sync state — synced, syncing, offline, N pending, or an error — plus
a relative last-sync time.

> **TBD (EPIC-08/09/10):** add/edit/complete/delete/move/reorder/comment on tasks, project /
> section / label management, and search scope toggling all get their own default bindings.
> They will be listed here before those epics are considered done.

## Configuration

The config file is TOML and lives at `<config dir>/config.toml`; see
[file locations](#file-locations). A missing file, or an empty one, is not an error — every
key has a default, so the app starts on defaults. Override the path per invocation with the
global flag:

```sh
todoist-tui --config ./scratch-config.toml auth
```

Unknown top-level keys are **rejected**, not ignored: a typo fails the load with a message
naming the key and its line, instead of silently having no effect. Keys inside the `[keymap]`
table are free-form and are interpreted by the TUI.

<!-- BEGIN CONFIG -->
| Key | Type | Default | Description |
|---|---|---|---|
| `sync_interval` | integer (seconds) | `60` | How often the background sync polls the server for remote changes. |
| `theme` | string | `"default"` | Name of the active colour theme. `"default"` is the built-in theme; 256-colour and truecolour terminals are detected automatically. |
| `keymap` | table of string → string | `{}` (empty) | Keybinding overrides: action name → key string. An entry replaces that action's default binding; an empty table means "use the built-in bindings". |
| `default_view` | string: `inbox`, `today`, or `upcoming` | `"inbox"` | View shown at startup. Any other value fails the load with a message naming the expected variants. |
<!-- END CONFIG -->

The loader does not validate `[keymap]` entries — the table is free-form by design, and the
TUI interprets it at startup, reporting an unknown action name or an unparsable key string
rather than silently dropping the entry.

Example `config.toml`:

```toml
sync_interval = 30
theme = "default"
default_view = "today"

[keymap]
quit = "x"
sync_now = "R"
```

> **TBD (EPIC-07):** the action-name spellings used as `[keymap]` keys in the example above
> follow the action enum names; the authoritative list is whatever `Keymap::from_config`
> accepts once T-603 lands, and this example will be aligned with it. The `config.toml` parse
> itself does not validate them — an unknown name is reported by the TUI at startup, with the
> offending entry named.

### Environment variables

| Variable | Effect |
|---|---|
| `XDG_CONFIG_HOME` | Base directory for the config file and the fallback token file. Honoured on every platform. |
| `XDG_STATE_HOME` | Base directory for the log file (and the cache DB, once EPIC-04 lands). Honoured on every platform. |
| `RUST_LOG` | `tracing` filter for the log file, default `info`. An unset **or malformed** value falls back to `info`. |
| `TODOIST_API_BASE_URL` | Overrides the API base URL (default `https://api.todoist.com`). Intended for pointing the client at a local mock server in tests; it does not relax the HTTPS requirement (see [Troubleshooting](#troubleshooting)). |

### File locations

| What | Linux | macOS | Windows |
|---|---|---|---|
| `config.toml` | `${XDG_CONFIG_HOME:-~/.config}/todoist-tui/config.toml` | `~/Library/Application Support/com.todoist.todoist-tui/config.toml` | `%APPDATA%\todoist\todoist-tui\config\config.toml` |
| fallback token | `${XDG_CONFIG_HOME:-~/.config}/todoist-tui/token` | `~/Library/Application Support/com.todoist.todoist-tui/token` | `%APPDATA%\todoist\todoist-tui\config\token` |
| log file | `${XDG_STATE_HOME:-~/.local/state}/todoist-tui/todoist-tui.log.YYYY-MM-DD` | `~/Library/Application Support/com.todoist.todoist-tui/todoist-tui.log.YYYY-MM-DD` | `%APPDATA%\todoist\todoist-tui\data\todoist-tui.log.YYYY-MM-DD` |
| cache DB (EPIC-04) | `${XDG_STATE_HOME:-~/.local/state}/todoist-tui/cache.db` | `~/Library/Application Support/com.todoist.todoist-tui/cache.db` | `%APPDATA%\todoist\todoist-tui\data\cache.db` |

macOS and Windows have no conventional state home, so the state directory falls back to the
platform data directory (never the purgeable cache directory) to keep the log and the cache
durable.

## Offline and sync behaviour

The Sync API contract, error taxonomy, retry/backoff parameters and the fixture-drift
procedure are documented in [`docs/manual/epic-03-sync-api.md`](docs/manual/epic-03-sync-api.md);
the design is in EPIC-05 (write queue) and EPIC-06 (sync engine) in
[`docs/epics/REQ-001.epics.md`](docs/epics/REQ-001.epics.md).

**First run and incremental sync.** The first sync sends `sync_token: "*"`, which the server
answers with a full snapshot. The response's `sync_token` is persisted with the snapshot, and
every later sync sends that token back, so the server returns only the deltas since the last
successful sync. Each request asks for the same fixed resource set —
`projects, items, sections, labels, notes, filters, user, day_orders` — because the Sync API
does not let a caller request a subset independently of full-vs-incremental. Deltas are
applied in one transaction: rows are upserted, and `is_deleted: true` removes them.

**Durable write queue.** Local edits do not go out as individual HTTP calls. Each one becomes
a Sync API command with a client-generated v4 `uuid` (and a `temp_id` for creates), enqueued
in the local SQLite database in the same transaction that optimistically applies the change
to the cache. Because both live in SQLite, **queued commands survive a restart**: kill the
process with N pending commands and the next start still has those N commands, verbatim.

**Flush is idempotent via `uuid`.** Pending commands are batched into one `/sync` request and
the response's per-command `sync_status` decides each outcome: `ok` drops the command from
the queue, an error marks it failed with the server's message (kept out of the retry loop) so
the UI can surface it. A command re-sent after an ambiguous failure **reuses the same
`uuid`**, so the server de-duplicates it and a write is never applied twice. `temp_id_mapping`
from the response rewrites temp IDs to real IDs in both the cached rows and any still-pending
command that referenced them (e.g. a task created inside a project created in the same
batch).

**Offline.** A transport failure moves the sync state to `Offline` and stops the retry loop
from hammering the API; edits keep working against the cache and the queue keeps growing.
When the network returns, the queue flushes with zero duplicates and zero losses.

**What 401 and 429 do.** These come from the client built in `src/api/` — see
[`docs/manual/epic-03-sync-api.md`](docs/manual/epic-03-sync-api.md) §3 for the full table:

- **401/403 → `Unauthorized`.** Reported as "the API token is unauthorized". It is **not
  retried** — the retry loop returns it immediately with zero waits, so an expired or
  revoked token produces one failure, not a retry storm. Re-run `todoist-tui auth`.
- **429 → `RateLimited { retry_after }`.** Retried. When the response carries a
  `Retry-After` header (delta-seconds) the wait before the next attempt is at least that
  long and overrides the computed backoff; without a usable header the normal exponential
  backoff applies. The default policy is 4 attempts, 500 ms base delay, 30 s cap and 20 %
  jitter, after which the call fails as `RetriesExhausted`.

**Conflict policy.** After a flush the server state wins and pending local commands are
replayed on top of it (last-write-wins), so an edit made elsewhere is not silently lost and a
local edit is not silently reverted.

> **Not merged yet.** The cache (EPIC-04), the write queue (EPIC-05), the background sync
> engine (EPIC-06) and the status-bar adapter are still task-graph entries. Today the sync
> client in `src/api/` is complete and tested against fixtures, but nothing calls it yet: with
> a stored token `todoist-tui sync` reports `not implemented: sync`. The behaviour described
> above is the contract those epics implement and what this section will describe once they
> land.

## Non-interactive CLI

Every subcommand runs a one-shot action and exits; none of them enter the alternate screen,
so their output is safe to pipe and script. `--config <PATH>` is a global flag and works
before or after the subcommand; `--help` and `--version` are clap's standard flags
(`todoist-tui --help` lists every subcommand).

```sh
todoist-tui add "buy milk tomorrow p1"
todoist-tui sync
todoist-tui today
todoist-tui today --json
```

| Command | Behaviour |
|---|---|
| `add "<text>"` | Add a task to the Inbox. Online, the text is parsed by the server-side quick-add endpoint (there is deliberately no local natural-language parser); offline, the raw text is queued locally ("added unparsed") and flushed by a later `sync`. Exits `0` in both cases. |
| `sync` | Run exactly one sync cycle (flush the queue, then pull deltas) and print a one-line summary with the last sync time and the pending count. Exits non-zero when offline or unauthenticated. |
| `today` | Print today's tasks as plain text, deterministic for a given cache, with no ANSI escapes when stdout is not a TTY. |
| `today --json` | Same data as JSON on stdout, against a documented stable schema. |

Exit codes: `0` success; `2` usage error (clap); non-zero for unauthenticated, offline and
transport failures.

> **Not merged yet.** `add`, `sync` and `today` exist on the CLI surface but are stubs. They
> first require a stored token: without one they fail with the message
> ``not authenticated: no API token is stored; run `todoist-tui auth` to add one`` and exit
> `1`. With a token they report `not implemented: <command>`. All three are implemented by
> EPIC-11. Running `todoist-tui` with no subcommand — the TUI — is EPIC-07 and reports
> `not implemented: tui` today.

## Troubleshooting

### The keyring is unavailable and the token went to a file

On Linux the keychain backend needs a Secret Service implementation (`gnome-keyring`,
KWallet, …) reachable on the session D-Bus. Headless machines, containers, `sudo` sessions
and some minimal desktops have none of that.

You do not need to fix it — `auth` detects an unusable keyring and falls back to
`<config dir>/token` (mode `0600`), printing a notice with the resolved path. The token file
is consulted only when the keyring has nothing stored, so re-running `todoist-tui auth` once
a Secret Service daemon is running stores the token in the keyring again; the old file entry
stays on disk until `todoist-tui logout`, which clears both backends.

`logout` clears both backends deliberately: a locked keyring must not prevent the file
fallback from being cleared, and vice versa.

When building from source, `libsqlite3-sys` compiles the bundled SQLite amalgamation, so a C
compiler is needed (no system `libsqlite3`, no OpenSSL). A working secret store is a
*runtime* requirement only, and no test in this repo touches the real keychain.

### Finding the logs and turning up the volume

Logs go to a daily-rotated file in the state directory — `todoist-tui.log.YYYY-MM-DD`,
matching `todoist-tui.log*` (see [file locations](#file-locations)). Nothing is written to
stdout/stderr while the TUI runs, so terminal drawing is never corrupted.

`RUST_LOG` drives the filter, defaulting to `info`:

```sh
RUST_LOG=trace todoist-tui sync
RUST_LOG=todoist_tui::api=debug todoist-tui auth
```

An unset **or malformed** `RUST_LOG` falls back to `info` rather than failing. ANSI styling is
disabled in log output so the files stay greppable. Secrets render as `***` — a log line
never contains your token.

### Resetting the local cache

The token and the cache are separate; `logout` clears both (the cache-wipe hook is a no-op
until EPIC-04 lands the database), so the simple reset is:

```sh
todoist-tui logout
```

Once the cache DB exists you can also delete it by hand and the next start recreates an empty
database with a fresh full sync — replace the path with the one from
[file locations](#file-locations):

```sh
rm -f "${XDG_STATE_HOME:-$HOME/.local/state}/todoist-tui/cache.db"
```

`logout` never deletes remote data: nothing leaves the machine except the Sync API requests
your own edits generate.

### "refusing to call the Todoist API over a non-HTTPS URL"

Both the auth token check (`src/auth/validate.rs`) and the sync client (`src/api/client.rs`)
reject any base URL that is not `https://<host>` before any network activity, and both build
their `reqwest` client with `https_only` in release builds. So this error means the base URL
in use is not HTTPS — almost always `TODOIST_API_BASE_URL` left set, or a hand-built URL with
a typo (`https:/`, a leading space, a missing scheme).

```sh
echo "$TODOIST_API_BASE_URL"   # should be empty, or an https:// URL
```

Loopback `http://` URLs (`http://127.0.0.1`, `http://localhost`, `http://[::1]`) are accepted
as a test seam **only** in debug builds, so a mock server works in `cargo test` while a
release binary stays HTTPS-only. Setting `TODOIST_API_BASE_URL` alone never weakens that
check.

### "not authenticated: no API token is stored"

No token reached the store. Run `todoist-tui auth`. If it keeps happening after a successful
`auth`, check which backend was used (the fallback notice, or the absence of one) and whether
`XDG_CONFIG_HOME` differs between the two invocations — that changes the fallback file path.

### "the API token is unauthorized"

The API rejected the token (401/403). It is not retried. Generate a fresh token in Todoist →
Settings → Integrations → Developer, then `todoist-tui auth` again. `TODOIST_API_BASE_URL`
pointing at a mock server that does not know your token produces the same message.

### Rate limited (HTTP 429)

The client honours `Retry-After` and backs off automatically (4 attempts, 500 ms base, 30 s
cap, 20 % jitter), so a transient limit resolves itself. If it persists, raise
`sync_interval` in `config.toml`.

## Development

Requires the pinned toolchain from `rust-toolchain.toml` (`1.99.1`, with `rustfmt` and
`clippy`); `rustup` installs it automatically in this repository.

```sh
cargo fmt --all -- --check
cargo clippy --all-targets -- -D warnings
cargo test --all
cargo build --release
```

The first three commands are exactly what CI runs (`.github/workflows/ci.yml`, jobs `fmt`,
`clippy`, `test`), on `ubuntu-latest` and `macos-latest`. Run them before opening a PR; a
clippy warning fails the workflow.

The workflow also has two build jobs: `build` (`cargo build --release`, both platforms) and
`linux-bare-build`, which builds on a bare `ubuntu-latest` with only `libdbus-1-dev`,
`libsecret-1-dev` and `pkg-config` installed, to prove there is no hidden host dependency in
the dependency tree.

Conventions worth knowing before changing code:

- Module layout: `src/main.rs`, `src/cli/`, `src/config/`, `src/auth/`, `src/api/`,
  `src/error.rs`, `src/log.rs`.
- Every fallible signature returns the crate-wide `Result<T> = Result<T, AppError>` with
  `thiserror`-typed error enums in `src/error.rs`; `anyhow` is only allowed at the top of
  `main`.
- Secrets are never formatted into logs — use the `Secret<T>` redaction helper in `src/log.rs`
  (`Display`/`Debug` render `***`; raw access only through the grep-able `Secret::expose`).
- Tests use fixtures and fakes only. No real tokens, emails or account data anywhere in the
  repository, tests included — use `tok_fake_...` placeholders.
- `cargo fmt` uses `rustfmt.toml` (edition 2021, 100 columns); clippy thresholds live in
  `clippy.toml`.
- Agentic SDLC workflow and per-phase rules: [`CLAUDE.md`](CLAUDE.md),
  [`docs/sdlc/README.md`](docs/sdlc/README.md).

### More documentation

- [`docs/manual/epic-02-auth-checks.md`](docs/manual/epic-02-auth-checks.md) — token storage
  backends, the echo-off manual check, `logout` verification.
- [`docs/manual/epic-03-sync-api.md`](docs/manual/epic-03-sync-api.md) — Sync API request and
  response contract, error taxonomy, retry/backoff, fixture-drift procedure.
- [`docs/req/REQ-001.md`](docs/req/REQ-001.md) — the requirement this project implements.
- [`docs/epics/REQ-001.epics.md`](docs/epics/REQ-001.epics.md) — the epic breakdown.

## License

MIT.
