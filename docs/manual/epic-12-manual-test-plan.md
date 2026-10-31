# EPIC-12 manual test plan: real-account checklist and terminal matrix

Three EPIC-12 acceptance items cannot be proven by a test: that the client behaves
correctly against a live Todoist account, that it looks right across terminal
sizes and colour depths on both supported platforms, and that `logout` followed by
a fresh start behaves like a first run for a human. This page is that artefact.

Execute the whole plan once per release candidate (and once per change that touches
sync, the write queue, the TUI renderer or the keyring backends). Every row in
§6 must end up `PASS`, `FAIL` or `N/A`; a `FAIL` blocks the release until it is
fixed or re-classified in writing here.

A second person who has never read the source must be able to run this page top to
bottom and produce the same conclusions. If a step needs code knowledge, the step
is wrong — fix the page.

Auth storage internals (where the token lives, when the file fallback is chosen)
and the sync client contract are documented elsewhere and are not repeated here:
see [§7](#7-references).

**Current status of this document: Run 1 is a partial execution.** At commit
`71da816` only `auth` and `logout` exist, so only the checks that do not need the
TUI, the sync cycle or the write queue could be run. Run 1 (§6.1) records what was
actually observed — real pass/fail values, not placeholders — and marks every
remaining check `NOT RUN` with the reason. The EPIC-12 acceptance item "executed
once against the dedicated test project, with results recorded" is **not yet
satisfied** by Run 1; it needs a full run and a human tester, on real terminals,
for the matrix in §3. See [§6.1](#61-run-1-partial) for the fill-in procedure.

## 1. Setup

### 1.1 Data-safety rules (mandatory)

- Use a **dedicated Todoist account that holds no real personal data**, or a
  dedicated project inside an account you own. Recommended: a separate account
  with a throwaway address, containing exactly one project named
  `tui-manual-test` and nothing else.
- **Never record any of the following in this file, in the results table, in a PR,
  in a ticket or in a chat log:** the API token (not even partially, not even a
  hash), the account email address, real task/project/comment text, or the
  contents of the cache database or log file. Test tasks use invented text only
  (`manual-check task 1`, `manual-check task 2`, ...).
- Paste the token only into the `auth` prompt (§2.1) — never as a shell argument,
  never into a file, never into `history`. `auth` takes no token argument for
  exactly this reason.
- Take the token from Todoist → Settings → Integrations → Developer → API token of
  the **test** account.

### 1.2 Isolated config and state

Run every check with the config and state directories redirected into a scratch
directory, so a test run cannot touch an everyday installation and so the cache can
be inspected and deleted without hunting for platform paths:

```sh
mkdir -p ~/todoist-tui-manual && cd ~/todoist-tui-manual
export XDG_CONFIG_HOME="$PWD/config"
export XDG_STATE_HOME="$PWD/state"
mkdir -p "$XDG_CONFIG_HOME" "$XDG_STATE_HOME"
```

Both variables are honoured on Linux and macOS, and an empty value counts as
unset. Leave them exported for the whole session. To start over, delete the
directory and recreate the two subdirectories — that is the clean-room reset used
by §2.2 and §2.8.

### 1.3 Build under test

```sh
git log -1 --format='%h %ad' --date=short   # record this in §6.1
cargo build --release
```

Use `target/release/todoist-tui` (below: `todoist-tui`) for every check, unless a
step says otherwise. Record the commit and the crate version from
`todoist-tui --version` in §6.1.

### 1.4 Environment record

Record in §6.1, for each platform run: OS and version, terminal emulator and
version, `$TERM`, `$COLORTERM` (or "unset"), and the window size used for the
"large" column of the matrix (§3). Note whether a keyring daemon is running on
Linux (`gnome-keyring`/`KWallet` present, or headless with none) — the expectation
for §2.1 depends on it.

## 2. Functional checklist

Each check has a tick box and one unambiguous expected result. "Todoist web" means
the test account's list view in a browser, refreshed manually. Timings are measured
with a second terminal running `date +%T` or a stopwatch; "≤ 5 s" means the change
is visible in Todoist web within 5 seconds of the key press without a manual
refresh of the TUI.

### 2.1 First-run auth

1. Ensure no token is stored: `todoist-tui logout` (exits 0 even when nothing is
   stored), then `todoist-tui today`.
2. Run `todoist-tui auth` in a real terminal.
3. Type the test account's token, press Enter.

- [ ] **2.1.1 Guard before auth**

  Expected: `todoist-tui today` exits with status **1**; stderr carries the single
  line `auth: not authenticated: no API token is stored;` followed by the hint
  `run `todoist-tui auth` to add one`; stdout is empty.

- [ ] **2.1.2 Prompt and echo**

  Expected: stderr shows `Todoist API token: ` (one trailing space, no newline) and
  nothing appears as the token is typed — no characters, no asterisks.

- [ ] **2.1.3 Success**

  Expected: stdout contains `Token saved.`, exit status **0**. If the OS keyring is
  unavailable (headless Linux), one extra line naming the fallback token file path
  appears on stderr; that is a pass, the token is still stored. Detailed storage
  checks: [§7](#7-references).

- [ ] **2.1.4 Wrong token rejected**

  Repeat `auth` and type a deliberately wrong value (e.g. `tok_fake_wrong`).

  Expected: stderr contains `invalid token: the Todoist API rejected it`, exit
  status **1**, and the previously stored valid token still works (`todoist-tui
  today` still exits 0). A rejected token must not replace a working one.

### 2.2 First launch and full sync

1. Clean-room reset (§1.2), then `todoist-tui auth` (§2.1) and start
   `todoist-tui` with no subcommand.
2. Immediately record the time to first usable screen.
3. In Todoist web of the test account, create `manual-check remote 1` in the
   `tui-manual-test` project while the TUI runs.
4. Wait up to 60 s without touching the TUI (background sync interval default is
   60 s), then press `r` (manual sync).

- [ ] **2.2.1 Cold start is fast**

  Expected: the UI is usable (sidebar, list and status bar drawn, keys respond) in
  **under 500 ms**, and the task data appears before any network round-trip
  completes (a cold cache with no network still draws the shell).

- [ ] **2.2.2 Full sync populates the cache**

  Expected: within 5 s of the first start the sidebar lists the `tui-manual-test`
  project and the task list shows the project's tasks; the status bar reads
  `synced` and shows a last-sync time. Debug build only, if you need a second
  signal: the log file (§4) contains a sync entry; release builds must not be
  observed this way.

- [ ] **2.2.3 Background sync picks up a remote change**

  Expected: `manual-check remote 1` appears in the TUI within **60 s** of being
  created in Todoist web, without restarting the TUI or pressing a key.

- [ ] **2.2.4 Manual sync works and is visible**

  Expected: pressing `r` triggers a sync (status bar briefly `syncing`, back to
  `synced`) and the last-sync time advances.

### 2.3 Create

- [ ] **2.3.1 Create a task in the TUI**

  Use the add binding (see the keybindings table in `README.md`), text
  `manual-check task 1`, target project `tui-manual-test`.

  Expected: the task appears in the TUI list **immediately**, before the network
  round-trip completes, and in Todoist web within **≤ 5 s** with the same text and
  project. Exactly one task appears — no duplicate after the next sync.

- [ ] **2.3.2 Create a task in a just-created project**

  Create a new project `manual-check project B`, then a task
  `manual-check task 2` in it, within a few seconds.

  Expected: the task stays attached to `manual-check project B` in Todoist web
  after the next sync (the temporary id must resolve to the real project id); no
  task appears in Inbox and none is orphaned.

- [ ] **2.3.3 Add a comment**

  Add a comment `manual-check comment 1` to `manual-check task 1`.

  Expected: the comment is readable in the TUI detail pane immediately and present
  in Todoist web within ≤ 5 s, once.

### 2.4 Edit

- [ ] **2.4.1 Edit content**

  Change `manual-check task 1` to `manual-check task 1 edited`.

  Expected: the TUI shows the new text immediately; Todoist web shows it within
  ≤ 5 s; the task keeps its project, priority, labels and due date.

- [ ] **2.4.2 Edit several fields at once**

  In one edit, change priority to 4, set a due date of tomorrow and add a label.

  Expected: all three land in Todoist web, and a subsequent sync does not revert
  any of them (no flicker back to the old values).

- [ ] **2.4.3 Cross-field conflict keeps both sides**

  With the TUI closed or offline, change `manual-check task 1 edited`'s content in
  Todoist web to `manual-check task 1 remote`, and set its due date in the TUI to a
  different day (or vice versa, one field each side). Reconnect and sync.

  Expected: the later write of each individual field wins — the task ends up with
  the remote content and the TUI due date (both survive). No field is silently
  reset to its pre-change value.

### 2.5 Complete and uncomplete

- [ ] **2.5.1 Complete**

  Complete `manual-check task 2` in the TUI.

  Expected: it leaves the active list immediately, shows as completed in Todoist
  web within ≤ 5 s, and does **not** reappear in the active list after the next
  sync.

- [ ] **2.5.2 Uncomplete a recent completion**

  Uncomplete `manual-check task 2` from the TUI (v1 supports recent completions).

  Expected: it is active again in both places within ≤ 5 s, with its project and
  labels intact.

- [ ] **2.5.3 Complete a recurring task offline (documented behaviour)**

  Disconnect the network, complete a recurring task (`manual-check recurring`, set
  to repeat daily in Todoist web beforehand), then reconnect and sync.

  Expected: the completion is shown optimistically while offline; after the sync
  the task reappears with the due date **the server** picks for the next
  occurrence (server-authoritative; the TUI must not invent a date). Any local
  predicted date must be corrected, not kept.

### 2.6 Delete

- [ ] **2.6.1 Delete asks for confirmation**

  Delete `manual-check task 1 edited`; when the confirmation prompt appears, cancel
  it.

  Expected: the task is unchanged in the TUI **and** still present in Todoist web
  after a sync. No delete command was sent.

- [ ] **2.6.2 Delete after confirming**

  Delete it again and confirm.

  Expected: it disappears from the TUI immediately and from Todoist web within
  ≤ 5 s; it does not reappear after the next sync or after a restart.

### 2.7 Offline mutations, then reconnect

- [ ] **2.7.1 Mutations are accepted offline**

  Disconnect the network (unplug/disable Wi-Fi — a blocked port is not enough,
  since DNS failure and refusal are different paths). Create
  `manual-check offline 1`, edit `manual-check task 2`, complete
  `manual-check remote 1`.

  Expected: each action is reflected in the TUI immediately, the status bar shows
  `offline` with a pending count that increases by one per action (`3 pending`
  after the three actions), and nothing blocks on a network timeout.

- [ ] **2.7.2 Queue survives a restart**

  Quit the TUI, restart it while still offline.

  Expected: the pending count is still `3`, and the local view still shows the
  created task, the edited text and the completed state. Nothing is lost.

- [ ] **2.7.3 Reconnect flushes exactly once**

  Reconnect, press `r` (or wait for the background sync), then force a second sync.

  Expected: after the first sync the pending count is `0` and the status bar reads
  `synced`; Todoist web shows exactly one `manual-check offline 1`, `manual-check
  task 2` with the edited text, and `manual-check remote 1` completed. After the
  second sync the counts in Todoist web are unchanged — no duplicate task, no
  double completion.

- [ ] **2.7.4 Lost writes**

  Compare what the TUI intended (§2.7.1) with what Todoist web shows.

  Expected: zero lost writes and zero duplicates — all three intents present
  exactly once.

### 2.8 `logout` then fresh start

1. Ensure a token and a populated cache exist (§2.2–§2.7).
2. Run `todoist-tui logout`.
3. Run `todoist-tui today` and start `todoist-tui`.
4. Re-authenticate and start `todoist-tui` again.

- [ ] **2.8.1 Logout clears everything**

  Expected: stdout contains `Cleared stored API token.` plus one `Cleared ...` line
  naming what the cache wipe did, exit status **0**; afterwards the token
  file/keyring entry is gone (see [§7](#7-references)) and no cached tasks remain —
  `state/` holds no populated cache (or none at all). Running `logout` a second
  time also exits **0** (idempotent). At commit `71da816` the second line reads
  `Cleared cache (not yet implemented).` because the SQLite cache hook is a no-op
  until EPIC-04; that wording is expected there.

- [ ] **2.8.2 No token after logout**

  Expected: `todoist-tui today` exits **1** with the same `auth: not authenticated`
  message as §2.1.1, and neither `todoist-tui` (no subcommand) nor any command
  prints task text from the old cache — no task or project name that existed before
  the logout may appear on stdout or stderr.

- [ ] **2.8.3 Fresh start behaves like a first run**

  After re-authenticating and starting to a *non-empty* Todoist account (add
  `manual-check remote 2` in Todoist web first):

  Expected: the TUI performs a **full** sync (not an incremental one): every
  project, label and active task of the test account appears, including ones the
  pre-logout cache never saw — `manual-check remote 2` is visible. Status bar ends
  on `synced` with a fresh last-sync time.

- [ ] **2.8.4 Second start after re-auth is incremental**

  Quit and restart.

  Expected: the start is served from cache (fast, §2.2.1) and the task data is
  identical; no task is duplicated by the re-sync.

### 2.9 Non-interactive CLI commands

Run each command from a shell (`todoist-tui` = `target/release/todoist-tui`). All
of them must work without entering the alternate screen: the terminal must show
plain output only, with the prompt on the next line afterwards.

- [ ] **2.9.1 `add` online**

  `todoist-tui add "manual-check cli 1 tomorrow 5pm #tui-manual-test"`

  Expected: exit **0**; stdout contains a line starting with `added:`; the task
  exists in the `tui-manual-test` project of Todoist web within ≤ 5 s with the due
  date the server parsed (tomorrow 17:00 local). The text is passed through
  unparsed — no local natural-language parsing exists.

- [ ] **2.9.2 `add` offline**

  Disconnect the network, then run
  `todoist-tui add "manual-check cli 2"`.

  Expected: exit **0**; stdout contains `queued (offline)` and the text; no
  alternate-screen escape sequences. In Todoist web the task does not exist yet.

- [ ] **2.9.3 `add` offline is flushed exactly once**

  Reconnect and run `todoist-tui sync`, then `todoist-tui sync` again.

  Expected: after the first `sync`, `manual-check cli 2` exists in Todoist web
  exactly once (Inbox, unparsed text). The second `sync` adds nothing.

- [ ] **2.9.4 `sync` summary**

  `todoist-tui sync`

  Expected: exit **0** and exactly one summary line on stdout containing the last
  sync time and the pending count (`0` at this point), naming both values so a
  script can read them. Offline, the same command exits **non-zero** and the error
  goes to stderr.

- [ ] **2.9.5 `sync` unauthenticated**

  `todoist-tui logout && todoist-tui sync; echo $?`

  Expected: prints `1`; stderr carries the `auth: not authenticated` message;
  stdout has no summary line. **Re-authenticate** (`todoist-tui auth`, §2.1.3)
  before continuing — the remaining checks need a stored token.

- [ ] **2.9.6 `today` plain text**

  `todoist-tui today` and `todoist-tui today | od -c | grep -c 033`

  Expected: exit **0**; stdout lists the tasks due today, one per line, sorted
  deterministically; the second command prints `0` (no ANSI escapes when stdout is
  not a TTY). Re-running the command prints byte-identical output.

- [ ] **2.9.7 `today --json`**

  `todoist-tui today --json | python3 -m json.tool > /dev/null && echo ok`

  Expected: prints `ok` (valid JSON), exit **0**, exactly one JSON document on
  stdout, no other text.

- [ ] **2.9.8 `--help` and `--version`**

  `todoist-tui --help`, `todoist-tui --version`, `todoist-tui sync --version`.

  Expected: exit **0** for all three; `--help` lists all five subcommands
  (`auth`, `logout`, `sync`, `add`, `today`) with a description each and the global
  `--config` option; both version invocations print the **same version number**
  (`todoist-tui sync --version` prefixes it with `todoist-tui-sync`, which is
  expected). A mistyped subcommand (`todoist-tui nope`) exits **2** with a clap
  usage error on stderr.

- [ ] **2.9.9 `--config` override**

  `todoist-tui --config /tmp/tt-config.toml today` with a file containing
  `sync_interval = 5`, while a valid token is stored.

  Expected: exit **0** — the command reads that file (it is accepted) and succeeds
  as any authenticated `today` would. A file with an unknown key (`sync_intervl =
  5`) makes the same command exit **1** with a config parse error naming the key
  and line — unknown keys are rejected, never silently ignored.

  At a commit where `today` is still a stub, the first command instead fails at the
  guard/`not implemented` stage with exit 1 **after** the file was accepted; that
  distinguishes config handling from the command itself and is a provisional pass
  for the config half of this check only.

### 2.10 Log file hygiene

- [ ] **2.10.1 No token in the log**

  After all of the above, scan the log for the real token material (§4 for the log
  location). Type the token at a silent prompt so it is neither echoed nor written
  to `history`; do not put it in the command line:

  ```sh
  read -rs TT_TOKEN            # paste the test token, Enter; nothing is echoed
  grep -rF -- "$TT_TOKEN" "$XDG_STATE_HOME/todoist-tui"; echo "matches: $?"
  grep -rEi 'api[_-]?token|bearer' "$XDG_STATE_HOME/todoist-tui" | grep -v '\*\*\*'
  grep -rF 'tok_fake' "$XDG_STATE_HOME/todoist-tui"; echo "matches: $?"
  unset TT_TOKEN
  ```

  Expected: each `matches:` line reports `1` (no match) and prints nothing, and the
  `bearer`/`api-token` scan prints nothing (redacted values render as `***`). Any
  hit is a `FAIL` — record which command matched, never the matched line.

## 3. Terminal matrix

Run every cell of the matrix on both platforms. Use the same build (§1.3) and the
same populated test account. For each cell, perform the following five actions and
judge them together (that is the cell's pass/fail):

1. Start the TUI; the first screen renders completely — sidebar, task list, detail
   pane and status bar all visible, no clipped or overlapping text, no `?` boxes
   where arrows/icons should be.
2. Move the selection through 10 items with `j`, then back with `k`; the highlight
   follows the selection and the detail pane updates.
3. Open the help overlay (`?`): it lists every default binding and fits the width
   without wrapping mid-row.
4. Complete one task and undo the completion (§2.5): the state change is visible
   without a redraw artefact (no half-erased line, no colour bleed into the detail
   pane).
5. Quit with `q`: the terminal is restored — cursor visible, no leftover alternate
   screen, prompt on a clean line.

Colour rules per depth: in a 16-colour terminal the client must not emit
truecolor SGR sequences (`38;2;` / `48;2;`), and every element must stay
distinguishable using only the 16 base colours (priority, due date, labels and the
status-bar states must not collapse into one colour). In a truecolor terminal the
theme's exact colours appear and no element is invisible against the background.

Optional capture check for the colour rule (Linux `script` syntax; on macOS drop
`-c` and pass the command after the file):

```sh
script -q /tmp/tt-cap.txt -c "todoist-tui"   # quit with q, then:
grep -c "38;2;" /tmp/tt-cap.txt              # must print 0 in a 16-colour cell
```

Larger-window cells use a **200x60** window (resize the emulator, or
`tmux new-session -x 200 -y 60`). "80x24" means a window of exactly 80 columns by
24 rows (emulator resized, or `tmux new-session -x 80 -y 24`) — the documented
minimum the UI must fit in.

| ID | Platform | Colour depth (`$TERM` / `$COLORTERM`) | Window | Expected | Pass/Fail | Notes |
|---|---|---|---|---|---|---|
| M1 | Linux | 16-colour (`xterm`, `COLORTERM` unset) | 80x24 | Passes all five actions; fits exactly, no wrap, no truecolor SGR | | |
| M2 | Linux | 16-colour (`xterm`, `COLORTERM` unset) | 200x60 | Passes all five actions; content stays aligned, no stretching artefacts, no truecolor SGR | | |
| M3 | Linux | truecolor (`xterm-256color`, `COLORTERM=truecolor`) | 80x24 | Passes all five actions; theme colours exact, nothing invisible, fits exactly | | |
| M4 | Linux | truecolor (`xterm-256color`, `COLORTERM=truecolor`) | 200x60 | Passes all five actions; theme colours exact; detail pane and status bar use the extra width | | |
| M5 | macOS | 16-colour (`xterm`, `COLORTERM` unset) | 80x24 | Same expectation as M1 | | |
| M6 | macOS | 16-colour (`xterm`, `COLORTERM` unset) | 200x60 | Same expectation as M2 | | |
| M7 | macOS | truecolor (`xterm-256color`, `COLORTERM=truecolor`) | 80x24 | Same expectation as M3 | | |
| M8 | macOS | truecolor (`xterm-256color`, `COLORTERM=truecolor`) | 200x60 | Same expectation as M4 | | |

### 3.1 Below the minimum size (both platforms)

- [ ] **3.1.1 Too-small guard**

  Resize the window to **79x23** (or `tmux new-session -x 79 -y 23`) with the TUI
  running.

  Expected: a single clear "terminal too small" message, no partial layout, no
  panic, no garbage. Enlarging back to 80x24 restores the normal view without a
  restart, and `q` still quits cleanly.

- [ ] **3.1.2 Live resize**

  Drag-resize the window from 200x60 down to 80x24 and back while a long task list
  is shown.

  Expected: the layout reflows on every step; no stale characters, no scroll or
  offset drift, no lost selection.

## 4. Where the files are

Needed to verify §2.8 and §2.10 and to report a failure usefully. All paths follow
`XDG_CONFIG_HOME` / `XDG_STATE_HOME` when set and non-empty (§1.2), otherwise the
platform default.

| Item | Path |
|---|---|
| Config file | `$XDG_CONFIG_HOME/todoist-tui/config.toml` |
| Fallback token file (only when the keyring is unavailable) | `$XDG_CONFIG_HOME/todoist-tui/token`, mode `0600` |
| Cache database | inside `$XDG_STATE_HOME/todoist-tui` |
| Log file | `$XDG_STATE_HOME/todoist-tui/todoist-tui.log.YYYY-MM-DD`, one file per day; `RUST_LOG` sets the filter (default `info`) |

On macOS without the XDG variables, config lives under
`~/Library/Application Support/`, and state (log + cache) sits next to it — keeping
§1.2's exports set for the whole session avoids having to reason about this.

## 5. Reporting a failure

For each `FAIL` record, in the notes column or below the results table: the check
id, the platform/terminal from §1.4, the exact key sequence, what appeared, what
was expected, and the relevant log timestamp — **not** the log line, which may
contain account data. Never attach the cache database or a raw log file to a PR;
reproduce instead.

## 6. Results

### 6.1 Run 1 (partial)

| Field | Value |
|---|---|
| Date | 2026-10-30 |
| Version / commit | `0.1.0` / `71da816` |
| Tester | T-1112 doc run (author), non-interactive shell only — **not** the human sign-off run |
| Platform run | macOS (Darwin x86_64), `$TERM=xterm-256color`, `$COLORTERM` unset, no keyring prompt exercised; shell only, no terminal-emulator matrix |
| Build | `cargo build` (debug), binary `target/debug/todoist-tui` |
| Isolation | `XDG_CONFIG_HOME=/tmp/ttrun/config`, `XDG_STATE_HOME=/tmp/ttrun/state`, both recreated empty at the start |
| Outcome | **PARTIAL — 13 checks run, 13 as expected; 26 checks NOT RUN** |

**Scope of what was executed.** At `71da816` only `auth` (real HTTP validation) and
`logout` are implemented: `todoist-tui` with no subcommand exits
`not implemented: tui`, and `sync`, `add` and `today` exit
`auth: not authenticated` before reaching their `not implemented` stub. The TUI,
sync cycle and write queue land in EPIC-05…EPIC-11 and EPIC-07. So Run 1 covers the
guard, the auth prompt/validation path with a fake token, `logout` idempotency, the
CLI surface, the config loader and the log-hygiene scan — and nothing that requires
a rendering terminal or a real account.

**How to complete Run 1.** After EPIC-11 lands, run §2 and §3 on each platform, add
a `Run 2` section with the same three tables filled in for the interactive checks,
and record the terminal matrix cells from §3 there. Only then is the EPIC-12
"executed once with results recorded" acceptance item met. Flagged in the T-1112 PR
description.

**Tokens used.** Only the fake value `tok_fake_abc123` (rejected by the live API as
expected); no real token, account, email or task text was used or recorded.

### 6.2 Functional outcome table (Run 1)

`PASS` = expected result observed verbatim. `NOT RUN` = the feature under test does
not exist at this commit (reason given) or needs a human at a real terminal.

| Check | Expected (one line) | Outcome | Notes |
|---|---|---|---|
| 2.1.1 | Guard exits 1 with `auth: not authenticated` | **PASS** | stdout empty; stderr exactly the documented message; exit 1 |
| 2.1.2 | Prompt on stderr, no echo while typing | **PASS** (piped part only) | Prompt `Todoist API token: ` on stderr with no newline, value read from stdin. TTY echo suppression needs a real terminal — re-check in Run 2 |
| 2.1.3 | `Token saved.`, exit 0 | **NOT RUN** | Needs a real token; no test account available to this run |
| 2.1.4 | Wrong token rejected, exit 1, old token kept | **PARTIAL PASS** | `tok_fake_abc123` → stderr `invalid token: the Todoist API rejected it` + `auth: the API token is invalid`, exit 1, nothing stored. "Old token kept" half not run (needs a valid token first) |
| 2.2.1 | Usable UI in under 500 ms | **NOT RUN** | TUI not implemented (`not implemented: tui`) |
| 2.2.2 | Full sync populates cache, status `synced` | **NOT RUN** | Sync cycle (EPIC-06) not implemented |
| 2.2.3 | Remote change visible within 60 s | **NOT RUN** | as 2.2.2 |
| 2.2.4 | `r` syncs, last-sync time advances | **NOT RUN** | as 2.2.2 |
| 2.3.1 | Task in TUI instantly, in Todoist within 5 s, once | **NOT RUN** | TUI + write queue (EPIC-05/08) not implemented |
| 2.3.2 | Task in new project resolves to real project id | **NOT RUN** | as 2.3.1 |
| 2.3.3 | Comment visible both sides, once | **NOT RUN** | as 2.3.1 |
| 2.4.1 | Edited text lands, other fields unchanged | **NOT RUN** | as 2.3.1 |
| 2.4.2 | Priority + due + label all land | **NOT RUN** | as 2.3.1 |
| 2.4.3 | Cross-field conflict: both changes survive | **NOT RUN** | as 2.3.1 |
| 2.5.1 | Completion lands, no resurrection | **NOT RUN** | as 2.3.1 |
| 2.5.2 | Uncomplete restores task with fields | **NOT RUN** | as 2.3.1 |
| 2.5.3 | Recurring offline completion corrected by server | **NOT RUN** | as 2.3.1 |
| 2.6.1 | Cancelled delete changes nothing | **NOT RUN** | as 2.3.1 |
| 2.6.2 | Confirmed delete lands, stays deleted | **NOT RUN** | as 2.3.1 |
| 2.7.1 | Three offline actions queued, count 3 | **NOT RUN** | write queue (EPIC-05) not implemented |
| 2.7.2 | Queue survives restart, count still 3 | **NOT RUN** | as 2.7.1 |
| 2.7.3 | Flush once, second sync sends nothing | **NOT RUN** | as 2.7.1 |
| 2.7.4 | Zero lost writes, zero duplicates | **NOT RUN** | as 2.7.1 |
| 2.8.1 | Logout clears token and cache, exit 0, idempotent | **PASS** (token part) | stdout `Cleared stored API token.` + `Cleared cache (not yet implemented).`, exit 0; a second `logout` also exit 0 with identical output. Cache line is the documented EPIC-04 no-op stub — the cache-wipe assertion itself cannot pass until EPIC-04 |
| 2.8.2 | No cached data, guard active again | **PASS** (part) | `today` after `logout` → exit 1 with the `auth: not authenticated` message; `todoist-tui` (no subcommand) → `not implemented: tui`, no cached data printed (there is no cache yet) |
| 2.8.3 | Re-auth does a full sync, new remote task appears | **NOT RUN** | needs a real account + sync cycle |
| 2.8.4 | Next start is cached and incremental | **NOT RUN** | as 2.8.3 |
| 2.9.1 | `add` online: exit 0, `added:`, server-parsed date | **NOT RUN** | EPIC-11 not implemented |
| 2.9.2 | `add` offline: exit 0, `queued (offline)` | **NOT RUN** | EPIC-11 not implemented |
| 2.9.3 | Queued add flushed exactly once | **NOT RUN** | EPIC-11 not implemented |
| 2.9.4 | `sync` summary line, exit 0 / non-zero offline | **NOT RUN** | only the unauthenticated path exists: `sync` with no token → exit 1, no summary |
| 2.9.5 | `sync` unauthenticated: exit 1, no summary | **PASS** | exit 1, stdout empty, stderr `auth: not authenticated: …` |
| 2.9.6 | `today` deterministic text, no ANSI when piped | **NOT RUN** | EPIC-11 not implemented; the guard fires first |
| 2.9.7 | `today --json` valid JSON | **NOT RUN** | EPIC-11 not implemented |
| 2.9.8 | Help/version exit 0; bad subcommand exits 2 | **PASS** | `--help` exit 0 listing `auth`, `logout`, `sync`, `add`, `today`, `help` and `--config`; `--version` `todoist-tui 0.1.0`; `sync --version` `todoist-tui-sync 0.1.0`; `nope` → exit 2, `error: unrecognized subcommand 'nope'` |
| 2.9.9 | `--config` honoured; unknown key exits 1 | **PASS** | valid file accepted (then failed at the auth guard, exit 1, as expected without a token); `sync_intervl = 5` → exit 1 with `config: failed to parse config file … unknown field `sync_intervl`, expected one of `sync_interval`, `theme`, `keymap`, `default_view`` |
| 2.10.1 | No token material in the log | **PASS** | after the whole run the day's log held 3 lines (`auth … outcome="rejected"`, 2× `logout … outcome="cleared"`) and 0 bytes of token material: both the `api_token`/`bearer` scan and the `tok_fake` scan matched nothing |
| 3.1.1 | 79x23 shows the too-small guard, recovers | **NOT RUN** | TUI not implemented |
| 3.1.2 | Live resize reflows without artefacts | **NOT RUN** | TUI not implemented |

### 6.3 Matrix outcome table (Run 1)

Not executed: no TUI exists to render. All eight cells stay `NOT RUN` until Run 2.

| Cell | Outcome | Notes |
|---|---|---|
| M1 | NOT RUN | no TUI at `71da816` |
| M2 | NOT RUN | no TUI at `71da816` |
| M3 | NOT RUN | no TUI at `71da816` |
| M4 | NOT RUN | no TUI at `71da816` |
| M5 | NOT RUN | no TUI at `71da816` |
| M6 | NOT RUN | no TUI at `71da816` |
| M7 | NOT RUN | no TUI at `71da816` |
| M8 | NOT RUN | no TUI at `71da816` |

## 7. References

- `README.md` — install, first-run auth and `logout`, the keybindings table used
  in §2.3.1, the configuration reference used in §2.9.9, offline/sync behaviour,
  and **troubleshooting** (keyring unavailable and the file fallback, log location
  and `RUST_LOG`, cache reset, HTTPS-only errors):
  [`../../README.md#troubleshooting`](../../README.md#troubleshooting). The README
  is written by T-1110; if the link does not resolve yet, that task has not landed.
- [`epic-02-auth-checks.md`](epic-02-auth-checks.md) — where the token is stored per
  platform, when the file fallback is chosen, the echo check, `ps`/`history`
  checks, `0600` permissions and what `logout` clears. §2.1 and §2.8 here only
  assert the user-visible behaviour; the storage facts live there.
- [`epic-03-sync-api.md`](epic-03-sync-api.md) — the sync client contract, error
  taxonomy and retry behaviour behind §2.2 and §2.7.
- `docs/epics/REQ-001.epics.md` — the acceptance criteria this plan serves
  (EPIC-07 for the terminal guard and sizes, EPIC-11 for the non-interactive
  commands, EPIC-12 for this plan).
