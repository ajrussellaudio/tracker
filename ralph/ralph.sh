#!/bin/bash
# Ralph — Long-running Copilot CLI agent loop
#
# Usage:
#   ./ralph.sh <max_iterations>
#
# Example:
#   ./ralph.sh 20
#
# On each iteration, Copilot reads ralph/prompt.md and works autonomously
# using --autopilot until it decides the current task is done. The loop
# stops early if Copilot emits <promise>COMPLETE</promise> in its output,
# signalling that all GitHub issues have been resolved.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
PROMPT_FILE="$SCRIPT_DIR/prompt.md"
PROGRESS_FILE="$SCRIPT_DIR/progress.txt"

# ── Argument validation ────────────────────────────────────────────────────────

if [[ $# -ne 1 ]] || ! [[ "$1" =~ ^[1-9][0-9]*$ ]]; then
  echo "Usage: $(basename "$0") <max_iterations>"
  echo ""
  echo "  max_iterations  A positive integer — how many Copilot iterations to"
  echo "                  allow before giving up. There is no default; you must"
  echo "                  decide how many loops is reasonable for your task."
  echo ""
  echo "Example:"
  echo "  $(basename "$0") 20"
  exit 1
fi

MAX_ITERATIONS="$1"

# ── Preflight checks ───────────────────────────────────────────────────────────

if ! command -v copilot &>/dev/null; then
  echo "Error: 'copilot' not found in PATH. Install the GitHub Copilot CLI first."
  exit 1
fi

if [[ ! -f "$PROMPT_FILE" ]]; then
  echo "Error: Prompt file not found at $PROMPT_FILE"
  exit 1
fi

# ── Initialise progress log ────────────────────────────────────────────────────

if [[ ! -f "$PROGRESS_FILE" ]]; then
  {
    echo "# Ralph Progress Log"
    echo "Started: $(date)"
    echo "---"
  } > "$PROGRESS_FILE"
fi

# ── Main loop ──────────────────────────────────────────────────────────────────

echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  Ralph — Copilot agentic loop                                ║"
echo "║  Max iterations: $MAX_ITERATIONS$(printf '%*s' $((46 - ${#MAX_ITERATIONS})) '')║"
echo "╚══════════════════════════════════════════════════════════════╝"

for i in $(seq 1 "$MAX_ITERATIONS"); do
  echo ""
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  Iteration $i / $MAX_ITERATIONS"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"

  {
    echo ""
    echo "## Iteration $i — $(date)"
  } >> "$PROGRESS_FILE"

  # Run Copilot in non-interactive autopilot mode.
  # Output is streamed live to the terminal (tee /dev/stderr) and also
  # captured so we can scan for the completion signal.
  PROMPT="$(cat "$PROMPT_FILE")"
  OUTPUT=$(
    copilot \
      --prompt "$PROMPT" \
      --allow-all \
      --autopilot \
      2>&1 | tee /dev/stderr
  ) || true

  # Check for completion signal
  if echo "$OUTPUT" | grep -q "<promise>COMPLETE</promise>"; then
    echo ""
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  ✅  Ralph completed all tasks at iteration $i / $MAX_ITERATIONS"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    {
      echo ""
      echo "## COMPLETE — $(date)"
      echo "All tasks finished at iteration $i."
    } >> "$PROGRESS_FILE"
    exit 0
  fi

  echo ""
  echo "  Iteration $i done. $(( MAX_ITERATIONS - i )) iteration(s) remaining."
  sleep 2
done

# ── Max iterations reached ─────────────────────────────────────────────────────

echo ""
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
echo "  ⚠️   Ralph reached the max of $MAX_ITERATIONS iteration(s) without"
echo "       receiving a completion signal."
echo ""
echo "  Options:"
echo "    • Run again with more iterations to continue"
echo "    • Check ralph/progress.txt for a summary of what was done"
echo "    • Tune ralph/prompt.md if Copilot is going in the wrong direction"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
{
  echo ""
  echo "## STOPPED — $(date)"
  echo "Reached max iterations ($MAX_ITERATIONS) without completion signal."
} >> "$PROGRESS_FILE"
exit 1
