#!/usr/bin/env bash
# Fallback: run a phase headless with an explicit combo, if subagent `model:` routing
# doesn't reach OmniRoute as expected in your Claude Code version.
# Usage: .sdlc/bin/phase.sh <agent> "<instruction>"
#   .sdlc/bin/phase.sh sdlc-reviewer "Review task T-003 against develop"
set -euo pipefail
agent="${1:?agent name}"; shift
file=".claude/agents/$agent.md"
[[ -f "$file" ]] || { echo "no agent $file"; exit 1; }
model=$(awk -F': *' '/^model:/{print $2; exit}' "$file")
body=$(awk 'BEGIN{c=0} /^---$/{c++; next} c>=2' "$file")
exec claude --model "$model" -p "$body

---
TASK: $*"
