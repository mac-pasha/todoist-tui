# SDLC project rules for pi

This project uses the agentic SDLC harness. Follow the "Agentic SDLC" section of CLAUDE.md.
Delegate phases to the sdlc-* agents with the `subagent` tool.

Pi has no permission system, so these limits depend on you:
- Never read, print or copy `.env*`, `secrets/**`, or any key or token file.
- Never run `git push`, `git push --force`/`-f`, or `git reset --hard`.
- Only humans mark tasks `done` (`graph.py ... done --human`).
