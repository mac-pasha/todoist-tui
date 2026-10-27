# CLAUDE.md

<!-- ===== PROJECT SECTION: fill per repo ===== -->
## Project
- Name: web-scraper
- Stack: <e.g. Laravel 11 API + Flutter (BLoC) app>
- Run tests: `<e.g. php artisan test>` / `<flutter test>`
- Lint: `<e.g. ./vendor/bin/pint --test>` / `<flutter analyze>`
- Conventions: <folder layout, naming, error format, logging rules>
- Never touch: <e.g. migrations already in prod, payment signing code>
<!-- ===== END PROJECT SECTION ===== -->

## Agentic SDLC (mandatory workflow)

All work goes through this pipeline. Each phase = one slash command, one subagent, one artifact in git, one human gate.
Models are chosen by OmniRoute combos, not by you. Each subagent's `model:` is a combo name.

| # | Phase | Command | Subagent | Combo | Artifact | Human gate |
|---|---|---|---|---|---|---|
| 1 | Req → Epics | `/sdlc-epics REQ-001` | sdlc-planner | sdlc-plan | `docs/epics/REQ-001.epics.md` | Lead approves PR |
| 2 | Epic → Task graph | `/sdlc-graph EPIC-01` | sdlc-planner | sdlc-plan | `docs/tasks/EPIC-01.graph.json` | Lead approves PR |
| 3a | Code | `/sdlc-task T-003` | sdlc-coder | sdlc-code | code diff | tests green |
| 3b | Unit tests | (inside `/sdlc-task`) | sdlc-unit-tester | sdlc-unittest | test files | tests green |
| 3c | Design | (task type `design`) | sdlc-designer | sdlc-design | UI code | dev visual check |
| 3d | Review | `/sdlc-review` | sdlc-reviewer | sdlc-review | review report | human merges |
| 4 | Integration / E2E | `/sdlc-test int\|e2e EPIC-01` | sdlc-integration-tester / sdlc-e2e-tester | sdlc-test-int / sdlc-e2e | test suites | CI green |
| 5 | Docs | `/sdlc-docs EPIC-01` | sdlc-doc-writer | sdlc-docs | `*.md` diff | in same PR |

Other: `/sdlc-next` (ready tasks), `/sdlc-status` (progress), `/sdlc-doctor` (check setup).

### Files
- `docs/req/REQ-xxx.md` — requirement, written by human (template `.sdlc/templates/REQ.md`)
- `docs/epics/REQ-xxx.epics.md` — epic list
- `docs/tasks/EPIC-xx.graph.json` — task graph, schema `.sdlc/schema/taskgraph.schema.json`
- `.sdlc/bin/graph.py` — validate graph, list ready tasks, set status. Always use it; never hand-edit `status`.

### Hard rules (all agents)
1. Scope = the task JSON. Touch only files in task `files`. Need another file → stop, explain, ask.
2. Every `acceptance` item must be met and proven (test or command output).
3. One task = one branch `task/T-xxx-<slug>` = one PR. No mixing tasks.
4. Never mark a task `done` and never use `--human`. Max status by agent = `review`. Human runs `graph.py set T-xxx done --human` after merge.
5. Coder never writes its own unit tests. Unit tests come from sdlc-unit-tester (different model family).
6. Reviewer never edits code. Report only.
7. No secrets, real NID/phone/account numbers, or prod data in code, tests, fixtures or prompts. Use fakes.
8. Planner never writes code.
9. Tests fail 3 times on same issue → stop, set task `blocked` with reason, hand to human.
10. Don't change `.claude/`, `.pi/`, `.sdlc/`, or this file unless user asks.

### Task status flow
`todo → in_progress → review → done` (human) · any → `blocked` (with `blocked_reason`)
