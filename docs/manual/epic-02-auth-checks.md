# EPIC-02 manual checks: token storage, fallback and the echo check

One EPIC-02 acceptance item cannot be proven by a test: terminal echo is off while
the Todoist API token is typed. This page is that artefact, plus the storage facts
needed to interpret it. Run it by hand once per EPIC-02 change touching `auth`,
`logout` or the token backends. Installation and keybindings are EPIC-12's job.

## 1. Where the token is stored

`todoist-tui auth` writes one entry to the platform credential store via the
`keyring` crate (its default platform-native backends), or to a file when that
store is unusable. The service/account pair is always `todoist-tui` / `api-token`:

| Platform | Backend | Entry |
|---|---|---|
| macOS | Keychain (login keychain) | generic password, service `todoist-tui`, account `api-token` |
| Linux / other Unix-likes | Secret Service over D-Bus (zbus) | item in the default (login) collection, attributes `service=todoist-tui`, `username=api-token`, label `keyring:api-token@todoist-tui` |
| Windows | Credential Manager | generic credential stored by the `keyring` crate's Windows-native backend, service `todoist-tui`, user/account `api-token` (exact target-name formatting is the backend's business) |

**Fallback file.** When the keyring is unavailable the token goes to
`$XDG_CONFIG_HOME/todoist-tui/token`. `XDG_CONFIG_HOME` is honoured on every
platform; when unset or empty the platform default applies — `~/.config/todoist-tui/token`
on Linux, `~/Library/Application Support/com.todoist.todoist-tui/token` on macOS,
`%APPDATA%\todoist\todoist-tui\config\token` on Windows. `auth` prints the resolved
path in its fallback notice, so you never have to derive it by hand (nothing is
printed when the keyring succeeds).

On Unix the file is created with owner-only mode `0600` from the first `open(2)`
call (written to a sibling temp file, then renamed into place), so a more permissive
file never exists even momentarily. On Windows it gets the platform default
permissions; tightening ACLs is out of scope for EPIC-02.

## 2. When the fallback is chosen

`TokenStore::resolve()` probes the keyring before prompting and picks the file
backend as primary when the probe reports the keyring unusable: the store cannot be
opened at all (e.g. `NoDefaultStore` on an unsupported platform), or reading it fails
with a platform-failure / no-storage-access error — the usual case on **headless
Linux with no Secret Service daemon** (no D-Bus session, no `gnome-keyring`/
`KWallet`). A *missing* entry is not "unavailable": a fresh install with an empty
keyring still uses the keyring. The fallback prints, after a successful save:

```
OS keyring unavailable; falling back to token file at /home/dev/.config/todoist-tui/token (mode 0600)
```

Reads still consult the other backend when the primary holds nothing, so a token
stored before a backend change is still found. `logout` clears both backends
unconditionally — a failure clearing one backend does not prevent clearing the
other.

## 3. Manual checklist

Use a fake token such as `tok_fake_abc123` for the visual steps. Checks 3.3 and 3.4
need a token the Todoist API accepts — type your own at the prompt, never as a
shell argument, a log entry or an edit to this file.

### 3.1 Terminal echo is off while typing

1. In a real terminal (not piped), run `todoist-tui auth`.
2. Confirm the prompt text `Todoist API token:` plus one trailing space appears on
   stderr with no trailing newline (`2>` moves it, `1>` leaves it in place).
3. Type `tok_fake_abc123`, with one typo plus a backspace in the middle, then Enter.

Expected: nothing appears between the prompt and the newline after Enter — no
characters, no asterisks, no backspace echo. The fake token is then rejected by the
API (`invalid token: the Todoist API rejected it`, non-zero exit, nothing stored;
offline you get the transport error instead) — fine, echo was observable while
typing.

Two deliberate caveats: if raw mode cannot be entered at all, `auth` warns that it
`could not disable terminal echo` (the token will be visible) and falls back to a
plain line read; Ctrl-C arrives as a key event cancelling the prompt rather than a
SIGINT, and an external `kill -INT` skips terminal restoration.

### 3.2 The token never appears in `ps` or shell history

`auth` takes no token argument — the value comes from the TTY, or stdin when piped.

1. Start `todoist-tui auth` and leave it at the prompt.
2. In another terminal: `ps aux | grep '[t]odoist-tui'`.
3. Submit the fake token, then run `history | grep tok_`.

Expected: step 2 shows only a process whose arguments are `todoist-tui auth`, no
token-shaped text; step 3 prints nothing. Scripted use is
`printf '%s\n' 'tok_fake_abc123' | todoist-tui auth` — the value is stdin.

### 3.3 `logout` leaves neither a keyring entry nor the fallback file

1. Store a token with `todoist-tui auth`; note the backend the notice names.
2. Run `todoist-tui logout`.
3. Spot-check the backend used (these variants print metadata only — never add a
   flag that prints the secret):
   - macOS: `security find-generic-password -s todoist-tui -a api-token` without
     `-w`, or search Keychain Access for `todoist-tui`.
   - Linux: `secret-tool search service todoist-tui username api-token >/dev/null`,
     then `echo $?`.
   - Windows: `cmdkey /list`, look for the `Generic`-type credential whose user is
     `api-token` (the exact target name is the backend's formatting).
4. Check the file at the path from the notice (`$XDG_CONFIG_HOME/todoist-tui/token`).
5. Run a guarded command, e.g. `todoist-tui today`.

Expected: step 2 prints `Cleared stored API token.` and
`Cleared cache (not yet implemented).` and exits 0. Step 3 finds no entry
(`could not be found in the keychain` on macOS; non-zero `$?` on Linux). Step 4 fails
with `No such file or directory` — also on a keyring-primary machine, where the file
never existed. Step 5 exits non-zero with
``auth: not authenticated: no API token is stored; run `todoist-tui auth` to add one``.
A second `logout` also exits 0.

### 3.4 The fallback file is mode `0600`

With the fallback backend in use and a token stored:

1. Take the path from the notice `auth` printed.
2. Print the mode: `stat -c %a <path>` on Linux, `stat -f %Lp <path>` on macOS
   (`-c` is a GNU extension the BSD `stat` on macOS does not support).
3. `chmod 666 <path>`, run `auth` again with an accepted token, re-check.

Expected: `600` every time, including after step 3 — `store` always creates a fresh
`0600` temp file and renames it over the target instead of writing in place.

## 4. What `logout` does today

`logout` wipes **the stored token only**. Cache wiping is a no-op hook:
`NoopCacheWipe` is wired in and its `wipe()` deletes nothing, because the cache
database does not exist yet. EPIC-04 lands the SQLite cache and replaces the hook
with the real `delete_all()`; only then does `logout` clear cached tasks. The
`Cleared cache (not yet implemented).` line is the user-visible reminder of that gap.

## 5. Fakes only

Every token here is a fake (`tok_fake_abc123`), matching the test fixtures. Never
commit a real token, account id or personal data to this file, a fixture or a log.

Behaviour source of truth: [`src/auth/store.rs`](../../src/auth/store.rs),
[`src/auth/keyring_source.rs`](../../src/auth/keyring_source.rs),
[`src/auth/file_source.rs`](../../src/auth/file_source.rs),
[`src/cli/auth.rs`](../../src/cli/auth.rs), [`src/cli/logout.rs`](../../src/cli/logout.rs),
[`src/auth/cache_wipe.rs`](../../src/auth/cache_wipe.rs).
