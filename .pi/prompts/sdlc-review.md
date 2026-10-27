---
description: Phase 3d — independent review of current task branch
argument-hint: "[base-branch, default develop]"
---
> Pi note: "delegate to <agent>" means: call the `subagent` tool with `agent: "<agent>"` (foreground, not async), pass the task text, and use its final report. Run shell commands with the `bash` tool.

1. Base = `$ARGUMENTS` or `develop` if empty.
2. Task id from branch name `task/T-xxx-*`. Get JSON with `python3 .sdlc/bin/graph.py show T-xxx`.
3. Delegate to **sdlc-reviewer** with task JSON and `git diff <base>...HEAD`.
4. Show the reviewer output verbatim.
5. `CHANGES_REQUESTED` → ask user: fix with sdlc-coder now? If yes, delegate fixes to **sdlc-coder** (BLOCKERS + MAJOR only), rerun tests, then re-run this review once.
6. Save output to `.sdlc/reviews/T-xxx.md` (gitignored) for PR description.
