# Agentic SDLC — Developer Guide

How to build features in this repo with the agentic SDLC harness: you write a requirement, AI agents plan, code, test, review and document in small steps, and **a human approves at every gate**.

> Worked example: `examples/todo-app` in the kit repo has a real app with this harness installed, plus a sample requirement, epics and task graph.
>
> Want to know how it works inside (every phase step by step, the agents, model routing, and the `graph.py` task tool in depth)? See [`.sdlc/README.md`](../../.sdlc/README.md).

## Contents
1. [How it works](#1-how-it-works)
2. [One-time setup](#2-one-time-setup)
3. [Your first feature, step by step](#3-your-first-feature-step-by-step)
4. [Command reference](#4-command-reference)
5. [Task graph](#5-task-graph)
6. [Models and routing](#6-models-and-routing)
7. [Rules](#7-rules)
8. [Setting up a project (new or existing)](#8-setting-up-a-project-new-or-existing)
9. [Troubleshooting](#9-troubleshooting)
10. [Known limitations](#10-known-limitations)
11. [Using pi instead of Claude Code](#11-using-pi-instead-of-claude-code)

---

## 1. How it works

```
docs/req/REQ-001.md            you write it (template: .sdlc/templates/REQ.md)
   │  /sdlc-epics REQ-001      planner  → epics               ── lead reviews PR
   ▼
docs/epics/REQ-001.epics.md
   │  /sdlc-graph EPIC-01      planner  → task graph          ── lead reviews PR
   ▼
docs/tasks/EPIC-01.graph.json  tasks with deps, files, acceptance criteria
   │  /sdlc-next               which tasks can start now
   │  /sdlc-task T-001         coder (or designer) → unit tester → tests+lint → commit
   │  /sdlc-review             independent reviewer, read-only
   │  you: merge, then mark the task done                     ── human gate
   ▼  (all tasks of the epic done)
   │  /sdlc-test int EPIC-01   integration tests
   │  /sdlc-test e2e EPIC-01   E2E tests + UAT checklist
   │  /sdlc-docs EPIC-01       markdown / CHANGELOG updates
```

Each phase is **one slash command → one specialised subagent → one artifact in git → one human gate**.

| Subagent | Does | Never does |
|---|---|---|
| `sdlc-planner` | Requirement → epics, epic → task graph | Write code |
| `sdlc-coder` | Implements one `code` task | Write its own unit tests |
| `sdlc-designer` | Implements one `design` (UI) task | |
| `sdlc-unit-tester` | Writes unit tests for code it did not write | Edit production code |
| `sdlc-reviewer` | Reviews the branch diff against the task | Edit anything |
| `sdlc-integration-tester` | Integration tests for an epic | Edit production code |
| `sdlc-e2e-tester` | E2E tests + `docs/uat/EPIC-NN.uat.md` | Edit production code (except test ids) |
| `sdlc-doc-writer` | Updates `*.md` docs and CHANGELOG | Edit code |

You always talk to Claude Code; it delegates to the right subagent.

---

## 2. One-time setup

You need: Claude Code, `git`, `python3` ≥ 3.9, and a personal OmniRoute key from your lead.

**1. Add your key** (the file is gitignored; never commit it):
```bash
cp .claude/settings.local.json.example .claude/settings.local.json
# edit it and paste your key as ANTHROPIC_AUTH_TOKEN
chmod 600 .claude/settings.local.json
```

**2. Optional: fewer permission prompts.** Add your project's test and lint commands to `.claude/settings.local.json`:
```json
{
  "env": { "ANTHROPIC_AUTH_TOKEN": "<your key>" },
  "permissions": { "allow": ["Bash(npm test:*)", "Bash(go test:*)", "Bash(gofmt:*)"] }
}
```

**3. Base branch.** `/sdlc-review` compares against `develop` by default. Use that branch name or pass your own (`/sdlc-review main`).

**4. Check everything:** open Claude Code in the repo root and run `/sdlc-doctor`. Every line must be ✓ before you start. It checks the gateway URL and key, that all 8 `sdlc-*` combos exist, Python and git, that the project section of `CLAUDE.md` is filled in, and that each agent maps to the right combo.

Claude Code will print `Model "sdlc-code" isn't recognized · context assumed to be 200k tokens`. That is expected and harmless: `sdlc-code` is a gateway combo name, not an Anthropic model.

---

## 3. Your first feature, step by step

### Step 1: Write the requirement
```bash
cp .sdlc/templates/REQ.md docs/req/REQ-001.md
```
Fill in problem, goal, functional requirements, non-functional requirements, out of scope and open questions. **This is the most important input.** Vague requirements give vague epics. Anything you leave out of "Out of scope" may get planned.

### Step 2: Requirement → epics
```
/sdlc-epics REQ-001
```
Creates branch `plan/REQ-001-epics`, and the planner writes `docs/epics/REQ-001.epics.md`: for each epic a goal, in/out of scope, dependencies, testable acceptance bullets and risks. Unclear points are listed under "Open questions" instead of being invented.

**Your gate:** read it, fix anything wrong, commit, and open a PR for the lead. Merge before continuing.

### Step 3: Epic → task graph
```
/sdlc-graph EPIC-01
```
Creates branch `plan/EPIC-01-graph`, and the planner writes `docs/tasks/EPIC-01.graph.json`, validates it with `graph.py`, and shows the first ready tasks. If the graph already exists, it asks before overwriting.

**Your gate:** check that tasks are small (one PR, about 400 changed lines or fewer), that `files` lists real paths, that `acceptance` items are testable, and that `deps` reflect real ordering. Merge via PR.

### Step 4: Pick and implement a task
```
/sdlc-next
/sdlc-task T-001
```
`/sdlc-task`:
1. Checks the task exists, its dependencies are `done`, and `git status` is clean.
2. Creates `task/T-001-<slug>` and sets the task `in_progress`.
3. Delegates by task `type`: `code` → coder, `design` → designer. (`test-int`, `test-e2e` and `docs` tasks are refused here; use `/sdlc-test` and `/sdlc-docs`.)
4. Delegates to the unit tester, which writes tests for the new code.
5. If tests expose bugs, loops back to the coder, **at most 3 times**. Still failing → task set `blocked` with a reason.
6. Runs the project's test and lint commands itself. They must pass.
7. Commits `feat(T-001): <title>` and sets the task to `review`.

Afterwards run `git status`. If `docs/tasks/<epic>.graph.json` shows as modified, commit it (`chore(T-001): mark review`).

### Step 5: Independent review
```
/sdlc-review            # base branch defaults to develop
/sdlc-review main       # or name your base
```
A different model reviews the diff against the task and answers in a fixed format: `VERDICT`, `BLOCKERS`, `MAJOR`, `MINOR`, and an `ACCEPTANCE` check per item. It also flags files changed outside the task's `files`. If the verdict is `CHANGES_REQUESTED`, it offers to have the coder fix BLOCKERS and MAJOR items, then reviews once more. The report is saved to `.sdlc/reviews/T-001.md` (gitignored), handy for your PR description.

### Step 6: Merge and close the task (you)
Agents can only take a task to `review`. You close it after merge:
```bash
git checkout develop
git merge --no-ff task/T-001-<slug>          # or merge the PR in your Git host
python3 .sdlc/bin/graph.py set T-001 done --human
git commit -am "chore(T-001): done"
```
Only now do tasks that depend on `T-001` become ready.

### Step 7: Repeat, then finish the epic
Loop `/sdlc-next` → `/sdlc-task` → `/sdlc-review` → merge. Tasks without shared files can be done in parallel by different developers.

When all code tasks are done:
```
/sdlc-test int EPIC-01      # integration tests, branch test/EPIC-01-int
/sdlc-test e2e EPIC-01      # E2E tests + docs/uat/EPIC-01.uat.md, branch test/EPIC-01-e2e
/sdlc-docs EPIC-01          # README / API docs / CHANGELOG
```
These do not change task status. Mark the `test-int`, `test-e2e` and `docs` tasks yourself with `graph.py set … done --human` after merging. Bugs found by the test agents are reported with a suggested owner task. They are not fixed automatically.

---

## 4. Command reference

| Command | What it does | Needs | Output |
|---|---|---|---|
| `/sdlc-doctor` | Checks gateway, key, combos, tooling, `CLAUDE.md`, agent mapping | – | ✓/✗ list |
| `/sdlc-epics REQ-001` | Requirement → epics | `docs/req/REQ-001.md` | `docs/epics/REQ-001.epics.md`, branch `plan/REQ-001-epics` |
| `/sdlc-graph EPIC-01` | Epic → task graph | the `## EPIC-01` heading in an epics file | `docs/tasks/EPIC-01.graph.json`, branch `plan/EPIC-01-graph` |
| `/sdlc-next [EPIC-01]` | Lists tasks that are `todo` with all deps `done` | a graph | table |
| `/sdlc-status` | Progress per epic, blocked tasks first | a graph | summary |
| `/sdlc-task T-001` | Implements one `code`/`design` task end to end | clean git, deps done | branch `task/T-001-<slug>`, 1 commit |
| `/sdlc-review [base]` | Independent read-only review | a `task/T-xxx-*` branch | report, `.sdlc/reviews/T-xxx.md` |
| `/sdlc-test int\|e2e EPIC-01` | Integration or E2E tests for an epic | epic graph | test files, branch `test/EPIC-01-int` or `-e2e` |
| `/sdlc-docs EPIC-01\|T-003` | Updates markdown docs | work to document | docs commit `docs(<id>): update docs` |

### `graph.py` (`python3 .sdlc/bin/graph.py …`)
| Command | Purpose |
|---|---|
| `validate <file>` | Checks required fields, duplicate ids, unknown deps, cycles |
| `next [EPIC-01\|file]` | Ready tasks |
| `show T-001` | Task JSON |
| `set T-001 <status>` | Change status. `blocked` needs `--reason "…"`. `done` needs `--human`. |
| `status` | Summary across all graphs |

**Never hand-edit `status`.** Always use `graph.py`.

---

## 5. Task graph

One file per epic: `docs/tasks/EPIC-NN.graph.json`, validated by `.sdlc/schema/taskgraph.schema.json`.

```json
{
  "id": "T-002",
  "title": "API: DELETE /api/todos/completed",
  "type": "code",
  "stack": "go",
  "deps": ["T-001"],
  "files": ["backend/internal/api/handler.go"],
  "acceptance": ["Returns 200 with {\"deleted\": <n>}", "Literal route is not captured by /api/todos/{id}"],
  "context": "Follow the style of the existing handlers.",
  "estimate_h": 1.5,
  "status": "todo"
}
```

| Field | Rule |
|---|---|
| `type` | `code`, `design`, `test-int`, `test-e2e` or `docs` |
| `files` | Exact paths the agent may create or edit. Anything else needs a human decision. Unit-test files are exempt. |
| `acceptance` | Concrete and testable. Bad: "works well". Good: "returns 422 when nid length ≠ 10 or 17". |
| `deps` | Real ordering only (schema → API → UI). More parallel tasks = faster. |
| `status` | `todo → in_progress → review → done` (human). Any → `blocked` with `blocked_reason`. |

**Good tasks:** one PR, reviewable in under 30 minutes, about 400 changed lines or fewer. If a task needs a file that is not in `files`, the agent stops. Add the file to the graph in a small PR; do not widen scope silently.

**Unblocking:** fix the cause, then `graph.py set T-004 in_progress`, then re-run `/sdlc-task T-004`.

---

## 6. Models and routing

Each agent has a `model:` that is an **OmniRoute combo name**, not a model. The gateway turns a combo into a prioritised chain of real models with automatic fallback.

| Combo | Used for | Currently served by (first choice → fallback) |
|---|---|---|
| `sdlc-plan` | Epics, task graphs | Claude Opus 5 → Kimi k3 |
| `sdlc-code` | Implementation | Kimi coding → GLM 5.1 |
| `sdlc-unittest` | Unit tests | Qwen 3.8 27B → GPT-OSS 120B (Cloudflare) → Claude Sonnet 5.5 |
| `sdlc-design` | UI | Kimi k3 (vision) → Claude Opus 4.7 |
| `sdlc-review` | Code review | Claude Opus 4.7 → GPT-OSS 120B (Cloudflare) → Kimi k3 |
| `sdlc-test-int` | Integration tests | GPT-OSS 120B → Qwen 3.8 27B (Cloudflare) → Claude Sonnet 5.5 |
| `sdlc-e2e` | E2E / UAT | Qwen 3.8 27B (Cloudflare) → GLM 5.1 → Kimi |
| `sdlc-docs` | Markdown | GPT-OSS 20B → Mistral Small 3.1 (Cloudflare) → GLM 5.3 flash |

The coder and the unit tester draw from separate model pools (coder: Kimi, GLM; unit tester: Qwen, GPT-OSS, Claude), so rule 5 (different model family) holds by construction. Keep the pools disjoint when you edit combos.

The table reflects the gateway config today. **Models are changed centrally** in the `omniroute-ops` repo (`omniroute/combos.json`), never in this repo.

**Check which model really served a request:** open the OmniRoute dashboard request log. The model column shows the combo name, and the provider/model that answered is shown next to it. Or ask the gateway admin.

If a subagent seems to ignore its combo, use the headless fallback:
```bash
.sdlc/bin/phase.sh sdlc-reviewer "Review task T-003 against develop"
```

---

## 7. Rules

Every agent follows the "Hard rules" in `CLAUDE.md`. In short:

- Touch only files in the task's `files`. One task = one branch = one PR.
- Every acceptance item must be proven by a test or command output.
- Agents never mark a task `done` and never use `--human`. You do, after merging.
- The coder never writes its own unit tests. The reviewer never edits code. The planner never writes code.
- Three failed attempts on the same issue → the task is `blocked` and handed to you.
- No secrets, real personal data, account numbers or production data in code, tests, fixtures or prompts. Fakes only.
- Do not change `.claude/`, `.sdlc/` or `CLAUDE.md` unless you intend to (these files shape every agent).

**What stays with you:** approving epics and graphs, reviewing and merging PRs, and closing tasks. The harness makes the AI work visible and reviewable. It does not replace your review.

---

## 8. Setting up a project (new or existing)

### 8.1 Quick path: the setup script
From a checkout of the kit repo:
```bash
./new-project.sh ~/Projects/my-app --url http://192.168.12.4:20128 --name my-app --key-file ~/path/to/my.key
```
| Option | Meaning |
|---|---|
| `<target-dir>` | Created if missing. Can be an empty folder, a folder with code, or an existing git repo. |
| `--url` | Gateway URL **with** `http://` or `https://`. Or set `OMNIROUTE_URL` once in your shell. |
| `--name` | Project name written into `CLAUDE.md` (default: folder name) |
| `--key-file` | Optional. Writes your key to `.claude/settings.local.json` (mode 600, git-ignored). It is never printed. |

Same thing through `make` (from the kit folder):
```bash
make new-project TARGET=~/Projects/my-app URL=http://192.168.12.4:20128 NAME=my-app KEY_FILE=~/path/to/my.key
```
`make` alone shows help. Use an absolute path or `~/…` for `TARGET`, because relative paths resolve from the kit folder. Only values given on the `make` command line are used, so stray environment variables cannot change the project name.

It copies the harness, sets the gateway URL and project name, adds the harness rules to `.gitignore` without touching your other lines, and runs `git init` (branch `develop`) when the folder is not in a repo yet. It does **not** commit, and it refuses to run if `CLAUDE.md`, `.claude/`, `.sdlc/` or `docs/sdlc/` already exist. It leaves the other `CLAUDE.md` placeholders for you, so `/sdlc-doctor` fails until you fill them in. That is intended.

### 8.2 Brand-new project from a blank folder
The pipeline needs something to work with: the planner reads the repo, and `/sdlc-task` runs your test and lint commands. So bootstrap before the first requirement.

**1. Decide the stack** (language, framework, test runner). Agents follow what exists; they do not design your architecture well from nothing.

**2. Create the folder and scaffold a minimal skeleton yourself** (30–60 minutes, outside the harness):

| Needed | Why | Example |
|---|---|---|
| Folder layout | Tasks list exact file paths in `files` | `backend/`, `frontend/`, `docs/` |
| One command that runs the tests | `/sdlc-task` runs it and it must pass | `go test ./...`, `npm test`, `php artisan test` |
| One command that lints or typechecks and exits 0 when clean | Same | `gofmt -l . && go vet ./...`, `npm run typecheck` |
| One passing test | Proves the runner works and shows your test style | any trivial test |
| One real piece of reference code | The coder copies the patterns of neighbouring files | a health endpoint with its test |
| A way to handle config and secrets | Agents must never put secrets in code | `.env.example` + environment variables |
| A `README.md` with run instructions | The docs agent keeps it up to date | run, test, lint commands |
| Optional: CI running the same commands | The pipeline's "CI green" gate is yours to wire up | any CI service |

**3. Install the harness** (this also runs `git init` if needed):
```bash
/path/to/omniroute-sdlc-kit/new-project.sh . --url http://192.168.12.4:20128 --key-file ~/path/to/my.key
```
Run it from inside the scaffolded folder, or give the folder path.

**4. Fill the Project section at the top of `CLAUDE.md`.** This is the agents' only description of your project, so be specific:
```markdown
## Project
- Name: shop-api
- Stack: Laravel 11 API + MySQL 8
- Run tests: `php artisan test`
- Lint: `./vendor/bin/pint --test`
- Conventions: controllers thin, logic in app/Services; errors as {error:{code,message}}; no PII in logs
- Never touch: database/migrations that are already deployed
```
(`examples/todo-app/CLAUDE.md` is a full worked example.)

**5. Verify and commit:**
```bash
claude            # then run /sdlc-doctor — all ✓
git add -A && git commit -m "chore: skeleton + sdlc harness"
```

**6. Write a small first requirement.** Copy `.sdlc/templates/REQ.md` to `docs/req/REQ-001.md`. Pick one thin feature (for example "list and create items") so your first epic works as a pilot of the whole pipeline.

**7. Run the pipeline** as in [section 3](#3-your-first-feature-step-by-step). On a blank project, check the first task graph with extra care: `files` must match your skeleton's layout, and foundational tasks (data model, shared helpers) must come before the features that use them.

### 8.3 Existing project
Run the same script (`./new-project.sh /path/to/app --url …`), or copy by hand:
```bash
cp -r sdlc-harness/. /path/to/app-repo/
cd /path/to/app-repo
cat .gitignore.sdlc >> .gitignore && rm .gitignore.sdlc
```
then edit `ANTHROPIC_BASE_URL` in `.claude/settings.json`, fill in `CLAUDE.md`, make sure a `develop` base branch exists, and run `/sdlc-doctor`. Every developer then follows [One-time setup](#2-one-time-setup).

### 8.4 Habits that pay off
- When a review catches a convention problem, add a line to the `CLAUDE.md` project section so the next task does not repeat it.
- Keep the base branch name consistent (`develop`), because `/sdlc-review` uses it by default.
- Start with one small requirement and one epic. Widen only after one full cycle (plan → code → review → merge → docs) went through cleanly.

---

## 9. Troubleshooting

| Symptom | Cause / fix |
|---|---|
| `401` from the gateway | Wrong or missing key in `.claude/settings.local.json`, or the URL scheme is wrong (`http` vs `https`) |
| `403 Model … is not allowed for this API key` | Your key only allows the `sdlc-*` combos. Something asked for a raw model id. |
| `429 usage_limit_exceeded … daily usage quota` | Per-key spending limit on the gateway. Ask the admin to check your key's limits. |
| `429 … exhausted their quota` | An upstream provider account is out of quota. The combo falls back automatically; if every model in it is out, wait for the reset or ask the admin to add capacity. |
| `502 … quality validation … shorter than 50 chars` | A combo with a response-length check (e.g. `sdlc-docs`) got a very short reply. Retry with a real prompt. |
| `/sdlc-task` refuses to start, "git not clean" | Commit or stash your changes. Often it is the graph file from the previous task. |
| Task shows `blocked` | Read `blocked_reason` (`/sdlc-status`). Fix the cause or take the task over manually, then set it back to `in_progress`. |
| Agent says it needs a file outside `files` | Update the graph (small PR) or ask for a scope change. Do not let the agent widen scope. |
| `graph.py set … done` fails with "only humans" | Working as designed. Add `--human` after merging. |
| Wrong model in the gateway log | See the `phase.sh` fallback in [Models and routing](#6-models-and-routing). |
| `Model "sdlc-code" isn't recognized` warning | Expected. See [One-time setup](#2-one-time-setup). |

---

## 10. Known limitations

- **Open models on Cloudflare handle tests and docs first.** They passed a coding and tool-call check, but they are less proven than Claude for test quality. Review the tests from the first few tasks. The Cloudflare plan has its own limits; when they are hit, requests fall through to the next model in the chain.
- **Test, E2E and docs tasks have no status automation.** `/sdlc-test` and `/sdlc-docs` do not update task status; close those tasks with `graph.py` yourself.
- **No spending cap on keys right now.** On the current gateway version, per-key USD limits appeared to block requests even with about $0 recorded spend, so they are turned off. Watch usage in the dashboard.
- **Quota is shared.** Several combos draw on the same subscriptions. Heavy parallel use by a team can exhaust a window; fallbacks help but do not remove the limit.

---

## 11. Using pi instead of Claude Code

The pipeline is the same; only the agent you type into changes. Each developer can use either, or both in one repo, because they share `CLAUDE.md`, `.sdlc/` and `docs/`.

**Set up a project for pi**
```bash
./new-project.sh ~/Projects/my-app --agent pi            # pi only (no gateway URL needed)
./new-project.sh ~/Projects/my-app --agent both --url http://<gateway>:<port>
make new-project TARGET=~/Projects/my-app AGENT=pi       # same thing through make
```
**Project that already has the Claude harness** (`CLAUDE.md` + `.sdlc/` present): `--agent pi` installs only the `.pi/` files and leaves `CLAUDE.md`, `.claude/`, `.sdlc/`, `docs/` and `.gitignore` untouched. **Project without the harness:** it installs the shared parts (`CLAUDE.md`, `.sdlc/`, `docs/`) plus `.pi/`, and no `.claude/`. Either way it never overwrites a file, and it refuses to replace a pi file with the same name.

**Once per machine** (the gateway and your key live in `~/.pi/agent`, not in the project): open pi and run `/omni setup` (gateway URL and your key), then `/omni sync`. `/model` should then list `omni/sdlc-plan`, `omni/sdlc-code`, and so on.

**Once per project:** run `pi` in the folder, answer `/trust` (pi asks before loading project agents and prompts), then `/sdlc-doctor`. Everything in section 3 works the same.

| Claude Code | pi |
|---|---|
| `.claude/agents/sdlc-*.md` | `.pi/agents/sdlc-*.md`, run by the `pi-subagents` extension; `model: omni/<combo>` |
| `.claude/commands/*.md` | `.pi/prompts/*.md` (same `$ARGUMENTS`) |
| `.claude/settings.json` + local key | gateway and key in `~/.pi/agent`; project `.pi/settings.json` lists the packages and default model |
| `CLAUDE.md` | the same file (pi reads it too; our agents set `inheritProjectContext: true`) |

**What is different, and the cost of it**
- **Pi has no permission system.** Claude Code's deny rules (no `git push --force`, no reading `.env` or `secrets/`) and command patterns (reviewer may only run `git diff/log/show`) have no equivalent. The generated agents and `.pi/APPEND_SYSTEM.md` state these limits in text, and tool lists still remove `edit`/`write` from read-only agents, but `bash` can do anything. For sensitive projects, run pi in a container or VM and keep real `.env` files out of the folder.
- **`/sdlc-doctor` differs:** it checks the pi files, and asks you to confirm the gateway with `/omni` and `/model`.
- **The `.pi/` files are generated.** To change an agent or command, edit the Claude version in the kit and regenerate with `python3 pi-flavour/to-pi.py sdlc-harness <project>`. The converter never overwrites, so delete the old `.pi/agents/sdlc-*` and `.pi/prompts/sdlc-*` first. Do not hand-edit them in many projects.

On a first run, do one tiny `/sdlc-task` and check the OmniRoute request log (or `make usage`) to confirm each subagent reaches its own combo.
