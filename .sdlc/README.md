# How the SDLC harness works

This document explains, step by step, what happens when a team uses this harness: which files exist, what each slash command does, how a model is chosen, and (in depth) how the Python task-graph tool `graph.py` works. For day-to-day usage see [`docs/sdlc/README.md`](../docs/sdlc/README.md); this file is the "under the hood" companion.

## Contents
1. [The idea in one page](#1-the-idea-in-one-page)
2. [What is in the harness](#2-what-is-in-the-harness)
3. [How a model gets chosen](#3-how-a-model-gets-chosen)
4. [The pipeline, phase by phase](#4-the-pipeline-phase-by-phase)
5. [The eight agents](#5-the-eight-agents)
6. [The task graph file](#6-the-task-graph-file)
7. [graph.py in depth](#7-graphpy-in-depth)
8. [What is enforced by code and what only by instruction](#8-what-is-enforced-by-code-and-what-only-by-instruction)
9. [When things go wrong](#9-when-things-go-wrong)
10. [FAQ](#10-faq)

---

## 1. The idea in one page

AI coding agents are fast, but left alone they wander off scope, grade their own homework, and burn money on the strongest model for every trivial step. The harness answers with five rules:

1. **Small tasks with an explicit scope.** Work is cut into tasks that each list the exact files they may touch and the acceptance criteria that prove they are done.
2. **Everything is a file in git.** Requirement, epics and task graph are Markdown/JSON in the repo, reviewed like code. There is no hidden state.
3. **Separate roles.** A planner plans, a coder codes, a different model writes the tests, a third reviews. No agent checks its own work.
4. **Human gates.** A person approves the epics, approves the task graph, merges every task, and is the only one who can mark a task `done`.
5. **Central model routing.** Agents ask for a *combo name* (`sdlc-code`, `sdlc-review`, ...). The OmniRoute gateway decides which real model answers, so models and costs change in one place for every project.

```
   YOU                          CLAUDE CODE (+ this harness)                 OMNIROUTE GATEWAY
 ┌─────────────────┐  /sdlc-*   ┌──────────────────────────────┐  model: sdlc-code  ┌─────────────────┐
 │ docs/req/REQ-1  │ ─────────▶ │ command  ─▶  subagent        │ ─────────────────▶ │ combo sdlc-code │ ─▶ Kimi / GLM ...
 │ review + merge  │ ◀───────── │ (.claude/commands, /agents)  │ ◀───────────────── │ (ordered models)│
 └─────────────────┘  artifacts └──────────────┬───────────────┘      response     └─────────────────┘
                                               │ reads / updates task state
                                               ▼
                              docs/tasks/EPIC-01.graph.json   (via python3 .sdlc/bin/graph.py)
```

## 2. What is in the harness

| Path | What it is | Who changes it |
|---|---|---|
| `CLAUDE.md` | Top: the **Project** section (stack, test/lint commands, conventions). Below: the pipeline table and the 10 **hard rules**. Every agent reads it. | You (Project section only) |
| `.claude/settings.json` | Gateway URL, default model names, tool permissions (allow/deny lists) | Kit admin |
| `.claude/settings.local.json` | **Your** gateway key. Git-ignored. | You |
| `.claude/agents/sdlc-*.md` | The 8 role agents: instructions, allowed tools, and `model: <combo>` | Kit admin |
| `.claude/commands/sdlc-*.md` | The 9 slash commands. Each is a short written procedure Claude follows. | Kit admin |
| `.sdlc/bin/graph.py` | The task-graph tool: validate, list ready tasks, change status | Kit admin |
| `.sdlc/bin/phase.sh` | Fallback that runs one agent headless with an explicit combo | Kit admin |
| `.sdlc/schema/taskgraph.schema.json` | Written description of the graph file format | Kit admin |
| `.sdlc/templates/` | `REQ.md` requirement template, `example.graph.json` | Kit admin |
| `.sdlc/reviews/` | Review reports saved by `/sdlc-review` (git-ignored) | Generated |
| `docs/req/` `docs/epics/` `docs/tasks/` `docs/uat/` | The artifacts: requirements, epics, task graphs, UAT checklists | Humans + planner |

Rule 10 in `CLAUDE.md` forbids agents from editing `.claude/`, `.sdlc/` or `CLAUDE.md` unless you ask them to.

## 3. How a model gets chosen

1. **Where requests go.** `.claude/settings.json` sets `ANTHROPIC_BASE_URL` to the OmniRoute gateway, so Claude Code sends every request there instead of to Anthropic. Your key (`ANTHROPIC_AUTH_TOKEN`) comes from `.claude/settings.local.json`.
2. **What name is sent.** The main session uses `"model": "sdlc-code"`. The "opus", "sonnet" and "haiku" slots are mapped to `sdlc-plan`, `sdlc-code` and `sdlc-docs`. Each agent file has its own `model:` line (for example `sdlc-reviewer` uses `sdlc-review`). That string is sent unchanged as the model name.
3. **What the gateway does.** A *combo* is an ordered list of real models plus a strategy (usually `priority`: try the first, fall back to the next on failure or exhausted quota). Your key may only call the `sdlc-*` combos. The response header `X-OmniRoute-Decision` records which real model served the request.
4. **Why it is built this way.** To change which model does code review you edit one file in the `omniroute-ops` repo. No project changes. The current chain per combo is in the table in [`docs/sdlc/README.md`](../docs/sdlc/README.md), section 6.

Claude Code prints `Model "sdlc-code" isn't recognized`. That is expected: it does not know the combo names and assumes a 200k context window. If a subagent ever seems to use the wrong model, check the gateway request log, or run an agent headless with `.sdlc/bin/phase.sh <agent> "<task>"`, which passes `--model <combo>` explicitly.

## 4. The pipeline, phase by phase

Every command below is a Markdown recipe in `.claude/commands/`. Claude follows its numbered steps, calling a subagent for the heavy part. The shape is always: **command → subagent → artifact in git → human gate.**

### Phase 0: Setup
- `new-project.sh` (or `make new-project`) copies the harness into a folder, sets the gateway URL and project name, and updates `.gitignore`.
- You add your key to `.claude/settings.local.json` and fill in the Project section of `CLAUDE.md`.
- `/sdlc-doctor` checks six things: gateway URL and token set; `GET /v1/models` returns 200; all 8 combos exist on the gateway; Python 3.9+ and a git repo; no `<project>` placeholders left in `CLAUDE.md`; every agent's `model:` matches its expected combo.

### Phase 1: Requirement → epics (`/sdlc-epics REQ-001`)
1. Check `docs/req/REQ-001.md` exists. If not, stop and point to the template `.sdlc/templates/REQ.md`.
2. Create branch `plan/REQ-001-epics`.
3. Delegate to **sdlc-planner** in *Mode A*: read the requirement, the Project section of `CLAUDE.md`, and skim the repo (so it knows what already exists). Write `docs/epics/REQ-001.epics.md`.
4. Show you the epic list and any open questions.
5. Stop.

*Each epic* has: `## EPIC-NN: title`, Goal, In scope, Out of scope, Dependencies, Acceptance (testable bullets), Risks. An epic is a shippable slice of about 3 to 15 tasks. Anything unclear goes into an "Open questions" list at the top. The planner must not invent business rules.

**Your gate:** read the file, fix it, commit, open a PR for the lead, merge.

### Phase 2: Epic → task graph (`/sdlc-graph EPIC-01`)
1. Find the `## EPIC-01` heading in `docs/epics/*.epics.md`. Not found: stop.
2. If `docs/tasks/EPIC-01.graph.json` exists, **ask before overwriting**.
3. Create branch `plan/EPIC-01-graph`.
4. Delegate to **sdlc-planner** in *Mode B*: read the code the epic touches, then write the graph (format in section 6). The planner must run `graph.py validate` on its own output and fix errors until it passes.
5. Run `graph.py validate` again itself. It must pass.
6. Run `graph.py next` and show the first ready tasks.
7. Stop. The planner finishes with a summary: task count, critical path, parallel groups, open questions.

**Your gate:** check that tasks are small (one PR, reviewable in under 30 minutes, about 400 changed lines or fewer), that each `files` list is concrete, that `acceptance` items are testable, and that `deps` show only real ordering (schema before API before UI). Merge via PR.

### Phase 3: The task loop

**3a. Pick a task: `/sdlc-next`** runs `graph.py next` and shows a table. It also suggests which tasks different people could do in parallel (those that share no files). `/sdlc-status` runs `graph.py status` and lists blocked tasks first.

**3b. Implement: `/sdlc-task T-001`** (13 steps):

| # | Step | Notes |
|---|---|---|
| 1 | `graph.py show T-001` | Not found: stop |
| 2 | Check status is `todo` or `in_progress` and all deps are `done` | Otherwise stop and list the unmet deps |
| 3 | `git status` must be clean | Otherwise stop and ask you |
| 4 | Create or check out `task/T-001-<short-slug>` | One task = one branch = one PR |
| 5 | `graph.py set T-001 in_progress` | State change #1 |
| 6 | Delegate by task `type`: `code` → **sdlc-coder**, `design` → **sdlc-designer** with the full task JSON | `test-int`, `test-e2e`, `docs` tasks are refused here; use `/sdlc-test` or `/sdlc-docs` |
| 7 | If the agent stopped because it needs a file outside `files` | Show the reason, ask you, stop |
| 8 | Delegate to **sdlc-unit-tester** with task JSON + the implementer's report + `git diff` | A different model writes the tests |
| 9 | If the tester found real code bugs, send the report back to the implementer | **At most 3 loops.** Still failing: `graph.py set T-001 blocked --reason "..."`, stop |
| 10 | Run the project's test and lint commands (from the Project section) | Must pass |
| 11 | Commit `feat(T-001): <title>` | |
| 12 | `graph.py set T-001 review` | State change #2 |
| 13 | Tell you to run `/sdlc-review`, then push and open a PR | |

Step 12 happens after the commit, so check `git status` afterwards: if the graph file is modified, commit it (`chore(T-001): mark review`). In practice the model sometimes includes it in the main commit.

**3c. Review: `/sdlc-review [base]`**
1. Base branch is the argument, default `develop`.
2. Task id comes from the branch name `task/T-xxx-*`; the task JSON from `graph.py show`.
3. Delegate to **sdlc-reviewer** with the task JSON and `git diff <base>...HEAD`.
4. Show the report verbatim.
5. If the verdict is `CHANGES_REQUESTED`, ask whether to have the coder fix the BLOCKERS and MAJOR items; if yes, fix, re-run tests, and review **once more**.
6. Save the report to `.sdlc/reviews/T-001.md` (git-ignored) for your PR description.

**3d. Your gate: merge, then close the task.** Agents can take a task only as far as `review`. After you merge:
```bash
python3 .sdlc/bin/graph.py set T-001 done --human
git commit -am "chore(T-001): done"
```
Only now do tasks that depend on T-001 become ready (see `next` in section 7).

### Phase 4: Integration and E2E tests (`/sdlc-test int|e2e EPIC-01`)
1. First argument must be `int` or `e2e`.
2. Load the epic's graph; warn if code tasks are not yet in `review` or `done`.
3. Create branch `test/EPIC-01-int` (or `-e2e`).
4. Delegate to **sdlc-integration-tester** or **sdlc-e2e-tester** with the epic section and the graph.
5. Report results. Bugs found are listed with a suggested owner task id; **production code is not fixed here**.

The integration tester uses a real test database and mocks external services at the HTTP boundary; it covers the happy path, auth failures, validation failures, an external service being down, and idempotent retries. The E2E tester also writes a manual checklist `docs/uat/EPIC-01.uat.md` for QA, and must ask which E2E tool to use if none is set up.

### Phase 5: Docs (`/sdlc-docs EPIC-01` or a task id)
1. Collect the diff (a task: current branch vs base; an epic: `git log --grep` for its task ids).
2. Delegate to **sdlc-doc-writer**: only `*.md` files, CHANGELOG entries under "Unreleased" with task ids.
3. Show the changed docs and commit `docs(<target>): update docs`.

## 5. The eight agents

Each agent is a Markdown file: frontmatter (`name`, `description`, `tools`, `model`) plus the instructions. Claude Code enforces the `tools` list, so an agent cannot use a tool that is not listed.

| Agent | Combo | Tools | Input | Produces | Must not |
|---|---|---|---|---|---|
| `sdlc-planner` | `sdlc-plan` | Read, Grep, Glob, Write, Bash (only `graph.py`) | REQ, or one epic | Epics file / task graph | Write code |
| `sdlc-coder` | `sdlc-code` | Read, Grep, Glob, Edit, Write, Bash | Task JSON | Code + a report (files, acceptance mapping, commands run, notes for the tester) | Touch files outside `files`; write unit tests |
| `sdlc-designer` | `sdlc-design` | same as coder | Task JSON (type `design`) | UI code; states covered | Hard-code values that exist as design tokens |
| `sdlc-unit-tester` | `sdlc-unittest` | same as coder | Task JSON + coder report + diff | Tests, acceptance-to-test map, bugs found | Edit production code; use real personal data |
| `sdlc-reviewer` | `sdlc-review` | Read, Grep, Glob, Bash (only `git diff/log/show`) | Task JSON + diff | `VERDICT` + BLOCKERS / MAJOR / MINOR / ACCEPTANCE | Edit anything |
| `sdlc-integration-tester` | `sdlc-test-int` | same as coder | Epic + graph | Integration tests, bug reports | Modify production code |
| `sdlc-e2e-tester` | `sdlc-e2e` | same as coder | Epic + UI tasks | E2E tests, `docs/uat/*.uat.md` | Modify production code (except test ids, which it must report) |
| `sdlc-doc-writer` | `sdlc-docs` | Read, Grep, Glob, Edit, Write, Bash (only `git diff/log`) | Epic/task + diff | Markdown + CHANGELOG | Edit code |

Subagents start with a fresh context: they only know what the command passes them plus `CLAUDE.md`. That is deliberate. It keeps each step independent and keeps the token bill small.

## 6. The task graph file

One JSON file per epic: `docs/tasks/EPIC-NN.graph.json`. Task ids (`T-001`, ...) are global across all epics.

```json
{
  "epic": "EPIC-01",
  "title": "Agent onboarding - NID submission",
  "source": "docs/epics/REQ-001.epics.md",
  "tasks": [
    {
      "id": "T-002",
      "title": "POST /api/agent-onboardings submit NID",
      "type": "code",
      "stack": "laravel",
      "deps": ["T-001"],
      "files": ["app/Http/Controllers/AgentOnboardingController.php", "routes/api.php"],
      "acceptance": ["422 when nid length is not 10 or 17 digits", "201 with onboarding id on success"],
      "context": "Follow the pattern in app/Models/Merchant.php",
      "estimate_h": 1.5,
      "status": "todo"
    }
  ]
}
```

| Field | Required | Meaning |
|---|---|---|
| `epic`, `title`, `tasks` | yes (top level) | The graph header and the task list. `source` (path to the epics file) is optional. |
| `id` | yes | `T-` plus 3 or more digits (a convention, see section 7.8) |
| `title` | yes | Short description |
| `type` | yes | `code`, `design`, `test-int`, `test-e2e` or `docs`. Decides which agent/command handles it. |
| `deps` | yes | Ids that must be `done` first. May point at tasks in other epics. |
| `files` | yes, non-empty | The only files the agent may create or change (test files are exempt) |
| `acceptance` | yes, non-empty | Testable statements; each must be proven by a test or command output |
| `status` | yes | `todo`, `in_progress`, `review`, `done`, `blocked` |
| `stack`, `context`, `estimate_h`, `assignee` | no | Hints for the agent and for planning |
| `blocked_reason` | set by the tool | Why the task is blocked; removed automatically on any other status |

Good `acceptance`: "POST /api/agents returns 422 when nid length is not 10 or 17". Bad: "works well".

## 7. graph.py in depth

### 7.1 What it is
`.sdlc/bin/graph.py` is about 160 lines of Python using only the standard library. It is the **single source of truth operator** for task state: agents and humans change a task's status only through it ("never hand-edit `status`"). Run it from the **project root**: it looks for graphs in `docs/tasks/` relative to the current directory.

```
python3 .sdlc/bin/graph.py validate <file>       check one graph file
python3 .sdlc/bin/graph.py next [EPIC-xx|file]   list tasks that can start now
python3 .sdlc/bin/graph.py show T-xxx            print one task as JSON
python3 .sdlc/bin/graph.py set T-xxx <status> [--reason "..."] [--human]
python3 .sdlc/bin/graph.py status                progress summary of all graphs
```

### 7.2 The data it works on
- Graph files are `docs/tasks/*.graph.json`, read in sorted order.
- Many commands first build a **global map** `task id → status` across *all* graph files (`global_status`). That is what allows a task in EPIC-02 to depend on a task in EPIC-01.
- Nothing is cached or locked. Every command reads the files, acts, and (for `set`) writes the file back.

### 7.3 The code, piece by piece

| Piece | What it does |
|---|---|
| `TASK_DIR = "docs/tasks"` | Where graphs live (relative to the current directory) |
| `STATUSES`, `TYPES` | The allowed status and type values |
| `REQ` | Fields every task must have: `id, title, type, deps, files, acceptance, status` |
| `AGENT_MAX` | Defined but **never used**. It documents the intent that agents stop at `review`; the real check is the `--human` flag in `set_status`. |
| `die(msg)` | Print `✗ msg` to stderr and exit with code 1 |
| `load` / `save` | Read JSON / write JSON with 2-space indent, UTF-8 kept, trailing newline |
| `all_graphs()` | Sorted list of `docs/tasks/*.graph.json` |
| `resolve(arg)` | `None` → all graphs; an existing file path → that file; otherwise `docs/tasks/<arg>.graph.json`; else die |
| `global_status()` | `{task id: status}` over every graph |
| `find(tid)` | Search all graphs for the id; return `(path, graph, task)` or die. The **first match wins**. |

### 7.4 Commands in detail

#### `validate <file>`: is this graph well-formed?
Collects *all* problems, prints each as `✗ ...`, and exits 1 if there is any; otherwise prints `✓ <file>: N tasks valid`. In order:

1. Top-level keys `epic`, `title`, `tasks` exist.
2. **Duplicate ids** inside this file.
3. For every task:
   a. each field in `REQ` exists (`missing 'acceptance'`);
   b. `type` is one of the five types;
   c. `status` is one of the five statuses;
   d. `files` is non-empty; `acceptance` is non-empty;
   e. every `deps` entry is a known id (known = ids in this file plus ids in all other graphs); a task cannot depend on itself.
4. **Cycle detection** (depth-first search), described below.

How the cycle check works: build `adj[task] = [deps that are in this file]`. Walk each unvisited task depth-first, keeping a `state` per task: *1 = on the current path*, *2 = finished*. If the walk reaches a task that is in state 1, the path has looped back on itself, so it records `cycle: A -> B -> C -> A`. Tasks in state 2 are skipped, so a shared dependency ("diamond") is **not** a false alarm.

```
T-101 depends on T-103, T-102 on T-101, T-103 on T-102:
dfs(T-101)  state[T-101]=1   → dep T-103
  dfs(T-103) state[T-103]=1  → dep T-102
    dfs(T-102) state[T-102]=1 → dep T-101 … state is 1 → cycle: T-101 -> T-103 -> T-102 -> T-101
```

Real output on a healthy graph, then on deliberately broken ones:

```text
$ python3 .sdlc/bin/graph.py validate docs/tasks/EPIC-01.graph.json
✓ docs/tasks/EPIC-01.graph.json: 3 tasks valid
[exit 0]
```
```text
$ python3 .sdlc/bin/graph.py validate docs/tasks/EPIC-09.graph.json
✗ duplicate ids: ['T-001']
✗ T-002: missing 'acceptance'
✗ T-002: empty acceptance
✗ T-003: unknown dep T-777
✗ T-004: depends on itself
✗ T-005: bad type backend
✗ T-006: bad status finished
✗ T-007: empty files
✗ cycle: T-004 -> T-004
[exit 1]
```
```text
$ python3 .sdlc/bin/graph.py validate docs/tasks/EPIC-08.graph.json
✗ cycle: T-101 -> T-103 -> T-102 -> T-101
[exit 1]
```
Note that a missing `acceptance` key is reported twice (missing, then empty), and a self-dependency is reported twice (`depends on itself`, then as a one-task cycle). Both are harmless duplicates.

#### `next [EPIC-xx | file]`: what can start now?
A task is **ready** when:
- its `status` is exactly `todo`, **and**
- every id in its `deps` has status `done` in the global map (an empty `deps` list counts as satisfied).

It prints one tab-separated line per ready task: `id`, `type`, `epic`, `title`; or `no ready tasks`. With no argument it looks at all graphs; with an epic id or a file path, just that graph. Dependencies are always looked up globally.

Two consequences worth knowing: a dependency in `review` is **not** done, so downstream tasks wait for your merge and `done --human`; and a dependency id that does not exist anywhere makes the task silently never ready (which is why you validate first).

```text
$ python3 .sdlc/bin/graph.py next
T-001	code	EPIC-01	Migration: agent_onboardings table
[exit 0]
$ python3 .sdlc/bin/graph.py set T-001 in_progress
✓ T-001: todo → in_progress
[exit 0]
$ python3 .sdlc/bin/graph.py next
no ready tasks
[exit 0]
$ python3 .sdlc/bin/graph.py set T-001 review
✓ T-001: in_progress → review
[exit 0]
$ python3 .sdlc/bin/graph.py next
no ready tasks
[exit 0]
$ python3 .sdlc/bin/graph.py set T-001 done --human
✓ T-001: review → done
[exit 0]
$ python3 .sdlc/bin/graph.py next
T-002	code	EPIC-01	POST /api/agent-onboardings submit NID
[exit 0]
```

#### `show T-xxx`: print one task
Finds the task, then prints its JSON with two extra keys at the top: `epic` and `graph` (the file path). Commands use this to hand the full task to an agent.

```text
$ python3 .sdlc/bin/graph.py show T-002
{
  "epic": "EPIC-01",
  "graph": "docs/tasks/EPIC-01.graph.json",
  "id": "T-002",
  "title": "POST /api/agent-onboardings submit NID",
  "type": "code",
  "stack": "laravel",
  "deps": [
    "T-001"
  ],
  "files": [
    "app/Http/Controllers/AgentOnboardingController.php",
    "app/Http/Requests/SubmitNidRequest.php",
    "routes/api.php"
  ],
  "acceptance": [
    "422 when nid length is not 10 or 17 digits",
    "201 with onboarding id on success",
    "NID never written to logs"
  ],
  "status": "in_progress"
}
[exit 0]
```

#### `set T-xxx <status> [--reason "..."] [--human]`: change a status
The rules, in the order the code applies them:

| Condition | Result |
|---|---|
| Status is not one of the five | `✗ bad status ...`, exit 1 |
| Status is `done` and `--human` is absent | `✗ only humans set 'done' ...`, exit 1 |
| Task id not found in any graph | `✗ task ... not found`, exit 1 |
| Status is `blocked` and `--reason` is absent | `✗ blocked needs --reason "..."`, exit 1 |
| Status is `blocked` | store the text after `--reason` as `blocked_reason` |
| Any other status | **remove** `blocked_reason` if present |
| Otherwise | change `status`, rewrite the whole graph file, print `✓ T-xxx: old → new` |

```text
$ python3 .sdlc/bin/graph.py set T-001 done
✗ only humans set 'done' (add --human after merge)
[exit 1]
$ python3 .sdlc/bin/graph.py set T-002 blocked
✗ blocked needs --reason "..."
[exit 1]
$ python3 .sdlc/bin/graph.py set T-002 blocked --reason "needs the Merchant model refactor first"
✓ T-002: todo → blocked
[exit 0]
$ python3 .sdlc/bin/graph.py status
EPIC-01 Agent onboarding - NID submission: done=1 review=0 in_progress=0 todo=1 blocked=1
   ⛔ T-002 POST /api/agent-onboardings submit NID — needs the Merchant model refactor first
[exit 0]
$ python3 .sdlc/bin/graph.py set T-002 in_progress
✓ T-002: blocked → in_progress
[exit 0]
$ python3 .sdlc/bin/graph.py set T-002 finished
✗ bad status finished
[exit 1]
$ python3 .sdlc/bin/graph.py show T-999
✗ task T-999 not found
[exit 1]
```
(The `--reason` text must be passed as one quoted argument: `--reason "needs the Merchant model refactor first"`.)

#### `status`: progress summary
For each graph it prints one line with the count per status (`done`, `review`, `in_progress`, `todo`, `blocked`), followed by one `⛔` line for every blocked task with its reason. With no graphs: `no graphs in docs/tasks`.

### 7.5 Exit codes and usage

| Situation | Exit code |
|---|---|
| Success (including `no ready tasks`, `no graphs in docs/tasks`) | 0 |
| `graph.py` with **no arguments**: prints the usage text | 0 |
| Any `die()` problem: bad status, task not found, `done` without `--human`, graph file not found | 1 |
| `validate` found problems | 1 |
| Unknown command or wrong number of arguments | 2 (usage text printed) |
| Unexpected crash (malformed JSON, see 7.8) | 1 (Python traceback) |

### 7.6 The status machine: intended vs enforced

```
            set in_progress        set review          set done --human
   todo ───────────────▶ in_progress ─────────▶ review ───────────────▶ done
     ▲                       │   ▲                 │                       (human, after merge)
     └───────────────────────┴───┴─── set blocked --reason "..." ◀── from any status
```
That is the **intended** flow (also written in `CLAUDE.md`). What `graph.py` actually **enforces** is only:
- the status name is valid;
- `done` needs the `--human` flag;
- `blocked` needs a reason.

It does **not** check that a move is a legal step. Any status can be set from any other (shown in 7.8).

### 7.7 Which command calls what

| Slash command | `graph.py` calls |
|---|---|
| `/sdlc-graph` | `validate <new graph>` (the planner and the command both), then `next <graph>` |
| `/sdlc-next` | `next [epic]` |
| `/sdlc-status` | `status` |
| `/sdlc-task` | `show T-xxx`; `set T-xxx in_progress`; on repeated failure `set T-xxx blocked --reason "..."`; at the end `set T-xxx review` |
| `/sdlc-review` | `show T-xxx` |
| You, after merge | `set T-xxx done --human` |

### 7.8 What graph.py does not check (each verified by running it)

| Behaviour | Why it matters | Workaround |
|---|---|---|
| **The JSON Schema file is never loaded.** `validate` is "schema-lite". Ids like `FOO`, and unknown extra keys, pass. | The schema says ids look like `T-nnn` and forbids extra keys; the tool does not. | Keep to the naming convention; review the graph in the PR. |
| **Duplicate ids across two graph files are not detected.** | `show` and `set` act on the first match (sorted file order); the second graph's task is unreachable. | Use unique ids; check `grep -h '"id"' docs/tasks/*.json`. |
| **Any status can be set from any status**, with no dependency check (e.g. `todo → in_progress` while deps are not done; `done → todo` without `--human`). | The "agents stop at `review`" rule is a promise, not a lock. | Rule 4 in `CLAUDE.md`; review the git history of the graph file. |
| **`--human` is just a flag.** Anyone who passes it can set `done`. | The permission list allows `graph.py` with any arguments. | Treat it as a guard against mistakes, not against a hostile agent. |
| **A `blocked` task with no reason is accepted** when the file is edited by hand. | `status` then prints a dangling `—`. | Always use `set ... blocked --reason`. |
| **`--reason` as the very last argument crashes** with `IndexError`. | A Python traceback instead of a message. | Always give the reason text. |
| **`set` rewrites the whole file** with 2-space indent (inline arrays become multi-line). | The first status change on a hand-written file produces a large diff (the 46-line example became 60 lines); later changes are one-line diffs. | Expect the one-time reformat. |
| **Relative to the current directory.** Run elsewhere and it says `no graphs in docs/tasks` with exit 0. | Looks like "nothing to do". | Run from the project root. |
| **A malformed JSON graph file crashes** `validate`, `next` and `status` (they read every graph). `show` and `set` crash only if their search reaches the bad file before finding the task. | One bad file breaks most commands. | Run `validate` after every graph edit. |
| **No locking.** Two simultaneous `set` calls on one file could overwrite each other. | Rare in normal use. | One person changes statuses at a time per epic. |

Real output for the behaviours above:

```text
$ python3 .sdlc/bin/graph.py validate docs/tasks/EPIC-01.graph.json
✓ docs/tasks/EPIC-01.graph.json: 3 tasks valid
[exit 0]
$ python3 .sdlc/bin/graph.py validate docs/tasks/EPIC-02.graph.json
✓ docs/tasks/EPIC-02.graph.json: 1 tasks valid
[exit 0]
$ python3 .sdlc/bin/graph.py set T-003 review
✓ T-003: todo → review
[exit 0]
$ python3 .sdlc/bin/graph.py status
EPIC-01 Limits demo: done=0 review=1 in_progress=0 todo=1 blocked=1
   ⛔ T-002 Blocked without reason — 
EPIC-02 Reuses an id: done=0 review=0 in_progress=0 todo=1 blocked=0
[exit 0]
$ python3 .sdlc/bin/graph.py set T-003 in_progress
✓ T-003: review → in_progress
[exit 0]
$ python3 .sdlc/bin/graph.py set FOO done --human
✓ FOO: todo → done
[exit 0]
$ python3 .sdlc/bin/graph.py set FOO todo
✓ FOO: done → todo
[exit 0]
$ python3 .sdlc/bin/graph.py set T-002 blocked --reason
Traceback (most recent call last):
  File ".sdlc/bin/graph.py", line 156, in <module>
    elif cmd == "set" and len(a) >= 3: set_status(a[1], a[2], a[3:])
                                       ~~~~~~~~~~^^^^^^^^^^^^^^^^^^^
  File ".sdlc/bin/graph.py", line 129, in set_status
    t["blocked_reason"] = rest[rest.index("--reason") + 1]
                          ~~~~^^^^^^^^^^^^^^^^^^^^^^^^^^^^
IndexError: list index out of range
[exit 1]
$ cd /tmp && python3 /path/to/project/.sdlc/bin/graph.py status
no graphs in docs/tasks
[exit 0]
```

## 8. What is enforced by code and what only by instruction

| Rule | Enforced by | Strength |
|---|---|---|
| An agent can only use its listed tools | Claude Code (`tools:` in the agent file) | Hard |
| No `git push --force`, no reading `.env` or `secrets/` | Claude Code permissions in `.claude/settings.json` | Hard |
| Only `graph.py`, `git status/diff/log`, `git checkout -b`, `git add`, `git commit` run without a prompt | Claude Code allow list | Hard |
| Graph file is well-formed, no cycles, no unknown deps | `graph.py validate` (if someone runs it) | Hard, but only on demand |
| `done` needs `--human`; `blocked` needs a reason | `graph.py set` | Medium (flag can be passed by anyone) |
| Coder and unit-tester use different model families | Gateway: the two combos draw from separate model pools | Hard while the combos stay separate |
| Touch only files in the task's `files` | Instructions + the reviewer's check #1 | Soft |
| Every acceptance item is proven | Instructions + reviewer's ACCEPTANCE section | Soft |
| One task = one branch = one PR | Instructions + `/sdlc-task` creating the branch | Soft |
| Reviewer never edits | Its tool list has no Edit/Write; Bash is limited to `git diff/log/show` | Hard |
| Planner never writes code | Instructions (it does have Write, for the epics and graph files) | Soft |
| Tests fail 3 times: stop and block | `/sdlc-task` step 9 (instruction) | Soft |
| No secrets or real personal data in code, tests or prompts | Instructions + the `.env`/`secrets/` read deny | Soft |
| Only a human merges and closes tasks | Your own review process | Process |

## 9. When things go wrong

| Situation | What the harness does | What you do |
|---|---|---|
| Agent needs a file outside `files` | Stops and reports the file and why (`/sdlc-task` step 7) | Decide: update the graph in a small PR, or reject the scope change |
| Tests keep failing (3 loops) | Task is set `blocked` with a reason | Read `blocked_reason` (`/sdlc-status`), fix the cause or take over, then `set T-xxx in_progress` |
| `git status` not clean at `/sdlc-task` | Stops | Commit or stash; often it is the graph file from the previous task |
| Reviewer says `CHANGES_REQUESTED` | Offers one fix round with the coder | Approve the fix round, or fix by hand |
| Dependency still in `review` | Dependent task is not "ready" | Merge the dependency's PR and run `set ... done --human` |
| Wrong model answers | `X-OmniRoute-Decision` shows the real model | Check the combo in the gateway repo, or use `phase.sh` with an explicit combo |
| Graph file edited and now broken | Other commands may crash | `python3 .sdlc/bin/graph.py validate docs/tasks/EPIC-NN.graph.json` |

## 10. FAQ

**Can two developers work in parallel?** Yes, on tasks that share no files (`/sdlc-next` suggests which). They each change their own task's status in their own branch; because `graph.py` rewrites the file, the first edit of a hand-written graph produces a large formatting diff, so merge the graph's reformatting early.

**Where does cost control happen?** At the gateway: combos choose cheaper models for tests and docs and stronger ones for planning and review; subagents start with small, fresh contexts; and prompt caching works per provider, so keeping one primary model per combo keeps the cache warm.

**Why not let the coder write its own tests?** A model tends to write tests that pass for the code it just wrote, including its mistakes. A different model, told to be adversarial, finds more.

**How do I add a new role or phase?** Add an agent file in `.claude/agents/` and a command in `.claude/commands/`, add its combo in the gateway repo, and mention it in the pipeline table in `CLAUDE.md`. Keep rule 10 in mind: only maintainers should edit these files.

**Is there a Python-free way to read state?** The graph files are plain JSON. `jq '.tasks[] | {id,status}' docs/tasks/*.graph.json` shows every task's status. Change status only through `graph.py`.
