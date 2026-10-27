---
description: Phase 4 — integration or E2E tests for an epic
argument-hint: "int|e2e EPIC-01"
---
> Pi note: "delegate to <agent>" means: call the `subagent` tool with `agent: "<agent>"` (foreground, not async), pass the task text, and use its final report. Run shell commands with the `bash` tool.

Args: $ARGUMENTS  (first = kind, second = epic id)

1. Kind must be `int` or `e2e`. Else stop.
2. Load `docs/tasks/<EPIC>.graph.json`. Warn if code tasks not in `review`/`done`.
3. Branch `test/<EPIC>-<kind>`.
4. `int` → delegate to **sdlc-integration-tester**. `e2e` → delegate to **sdlc-e2e-tester**. Pass epic section + graph.
5. Report results. Bugs found → list with suggested owner task id. Don't fix production code here.
