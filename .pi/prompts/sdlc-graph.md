---
description: Phase 2 — turn one epic into a task graph
argument-hint: "EPIC-01"
---
> Pi note: "delegate to <agent>" means: call the `subagent` tool with `agent: "<agent>"` (foreground, not async), pass the task text, and use its final report. Run shell commands with the `bash` tool.

Epic id: $ARGUMENTS

1. Find `## $ARGUMENTS` in `docs/epics/*.epics.md`. Not found → stop.
2. `docs/tasks/$ARGUMENTS.graph.json` already exists → ask before overwriting.
3. Create branch `plan/$ARGUMENTS-graph`.
4. Delegate to **sdlc-planner** subagent, Mode B, for $ARGUMENTS.
5. Run `python3 .sdlc/bin/graph.py validate docs/tasks/$ARGUMENTS.graph.json`. Must pass.
6. Run `python3 .sdlc/bin/graph.py next docs/tasks/$ARGUMENTS.graph.json` and show first ready tasks.
7. Stop. Gate: lead reviews + merges.
