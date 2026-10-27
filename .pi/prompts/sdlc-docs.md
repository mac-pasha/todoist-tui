---
description: Phase 5 — update markdown docs for a task or epic
argument-hint: "EPIC-01 | T-003"
---
> Pi note: "delegate to <agent>" means: call the `subagent` tool with `agent: "<agent>"` (foreground, not async), pass the task text, and use its final report. Run shell commands with the `bash` tool.

Target: $ARGUMENTS

1. Collect the diff: task → current branch vs base; epic → `git log --grep "<task ids>"` diffs.
2. Delegate to **sdlc-doc-writer** with target + diff summary.
3. Show changed docs. Commit `docs($ARGUMENTS): update docs` on current branch.
