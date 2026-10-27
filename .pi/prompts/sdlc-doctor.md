---
description: Check harness setup for pi (gateway, combos, agents, project files)
---
Check and report ✓/✗ for each. Never print keys or tokens.

1. Gateway: run `/omni` (the OmniRoute extension dashboard) and confirm the server is healthy. If the `omni` provider is missing, tell the user to run `/omni setup` and then `/omni sync`.
2. Combos: `/model` must list all 8 as `omni/<combo>`: sdlc-plan, sdlc-code, sdlc-unittest, sdlc-design, sdlc-review, sdlc-test-int, sdlc-e2e, sdlc-docs.
3. Provider config: `jq -r '.providers.omni.baseUrl // empty' ~/.pi/agent/models.json` prints the gateway URL. Print the URL only, never the apiKey.
4. `python3 --version` is 3.9 or newer; this folder is a git repo (base branch `develop`).
5. CLAUDE.md project section is filled in (no `<project>` placeholders).
6. Each `.pi/agents/sdlc-*.md` has `model: omni/<expected combo>` and `inheritProjectContext: true`:
   planner→sdlc-plan, coder→sdlc-code, unit-tester→sdlc-unittest, designer→sdlc-design, reviewer→sdlc-review, integration-tester→sdlc-test-int, e2e-tester→sdlc-e2e, doc-writer→sdlc-docs.
7. `.pi/prompts/` has the 9 `sdlc-*.md` files and `.pi/settings.json` lists `npm:pi-subagents` and `npm:omniroute-pi-ext-integration`.
8. The `subagent` tool is available: list the agents with it and confirm the 8 `sdlc-*` agents appear.
Then tell the user: to confirm each subagent really hits its combo, run one tiny task and check the OmniRoute request log (or `make usage` in the kit). The model column should show the combo name.
