---
description: Phase 1 — break a requirement into epics
argument-hint: "REQ-001"
---
> Pi note: "delegate to <agent>" means: call the `subagent` tool with `agent: "<agent>"` (foreground, not async), pass the task text, and use its final report. Run shell commands with the `bash` tool.

Requirement id: $ARGUMENTS

1. Check `docs/req/$ARGUMENTS.md` exists. Missing → tell user to create it from `.sdlc/templates/REQ.md`, stop.
2. Create branch `plan/$ARGUMENTS-epics`.
3. Delegate to the **sdlc-planner** subagent, Mode A, input `docs/req/$ARGUMENTS.md`, output `docs/epics/$ARGUMENTS.epics.md`.
4. Show the user: epic list (id + title + 1 line), open questions.
5. Stop. Gate: lead reviews + merges the PR. Next: `/sdlc-graph EPIC-NN` per epic.
