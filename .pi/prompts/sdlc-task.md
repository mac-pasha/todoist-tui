---
description: Phase 3 — implement one task end to end (code/design → unit tests → self-check)
argument-hint: "T-003"
---
> Pi note: "delegate to <agent>" means: call the `subagent` tool with `agent: "<agent>"` (foreground, not async), pass the task text, and use its final report. Run shell commands with the `bash` tool.

Task id: $ARGUMENTS

1. Find task: `python3 .sdlc/bin/graph.py show $ARGUMENTS`. Not found → stop.
2. Check deps done and status is `todo` or `in_progress`. Deps not done → stop, list them.
3. `git status` clean? Not clean → stop, ask user.
4. Create/checkout branch `task/$ARGUMENTS-<short-slug>`.
5. `python3 .sdlc/bin/graph.py set $ARGUMENTS in_progress`.
6. By task `type`:
   - `code` → delegate to **sdlc-coder** with full task JSON.
   - `design` → delegate to **sdlc-designer** with full task JSON.
   - `test-int` / `test-e2e` / `docs` → tell user to use `/sdlc-test` or `/sdlc-docs`, stop.
7. If agent stopped for out-of-scope file → show reason, ask user, stop.
8. Delegate to **sdlc-unit-tester** with task JSON + implementer report + `git diff`.
9. Unit tester found code bugs → send report back to implementer (step 6 agent). Max 3 loops. Still failing → `graph.py set $ARGUMENTS blocked --reason "<why>"`, stop.
10. Run project test + lint commands yourself. Must pass.
11. Commit: `feat($ARGUMENTS): <title>` (or `fix`/`refactor` if fits).
12. `python3 .sdlc/bin/graph.py set $ARGUMENTS review`.
13. Tell user: run `/sdlc-review`, then push + open PR.
