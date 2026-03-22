#!/bin/bash
# Ralph — Long-running Copilot CLI agent loop with bash-side mode routing
#
# Usage:
#   ./ralph.sh <max_iterations>
#
# Example:
#   ./ralph.sh 20
#
# Each iteration:
#   1. Checks GitHub for open ralph PRs or open issues (in bash)
#   2. Determines which mode to run (implement, review, review-round2, fix,
#      force-approve, or merge)
#   3. Loads the mode-specific prompt from ralph/modes/<mode>.md
#   4. Substitutes {{REPO}}, {{PR_NUMBER}}, {{ISSUE_NUMBER}} placeholders
#   5. Runs Copilot with that focused, self-contained prompt
#
# The loop stops early if Copilot emits <promise>COMPLETE</promise> or if
# bash routing finds no open PRs and no open issues.

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
GIT_ROOT="$(git -C "$SCRIPT_DIR" rev-parse --show-toplevel)"
MODES_DIR="$SCRIPT_DIR/modes"
WORKTREE_DIR="${GIT_ROOT%/*}/$(basename "$GIT_ROOT")-ralph-workspace"

# Extract repo slug from project.md (line: "**GitHub repo:** `owner/name`")
REPO=$(grep -m1 'GitHub repo' "$SCRIPT_DIR/project.md" | grep -oE '`[^`]+`' | tr -d '`')

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

if [[ ! -d "$MODES_DIR" ]]; then
  echo "Error: Modes directory not found at $MODES_DIR"
  exit 1
fi

if [[ -z "$REPO" ]]; then
  echo "Error: Could not extract repo slug from $SCRIPT_DIR/project.md"
  exit 1
fi

# ── Worktree setup ─────────────────────────────────────────────────────────────

# Remove the worktree on exit (clean finish, error, or Ctrl-C).
cleanup() {
  if git -C "$GIT_ROOT" worktree list | grep -q "$WORKTREE_DIR"; then
    echo ""
    echo "  Removing worktree at ${WORKTREE_DIR} …"
    git -C "$GIT_ROOT" worktree remove --force "$WORKTREE_DIR" 2>/dev/null || true
  fi
}
trap cleanup EXIT

if [[ -d "$WORKTREE_DIR" ]]; then
  echo ""
  echo "⚠️  A Ralph workspace already exists at:"
  echo "   $WORKTREE_DIR"
  echo ""
  echo "This usually means a previous run did not exit cleanly."
  echo "Remove it with:  git worktree remove --force $WORKTREE_DIR"
  exit 1
fi

echo ""
echo "  Creating worktree at ${WORKTREE_DIR} …"
git -C "$GIT_ROOT" worktree add --detach "$WORKTREE_DIR" origin/main

# ── Routing ────────────────────────────────────────────────────────────────────

# Populates MODE, PR_NUMBER, ISSUE_NUMBER based on current GitHub state.
# MODE is one of: implement | review | review-round2 | fix | force-approve | merge | complete
determine_mode() {
  PR_NUMBER=""
  ISSUE_NUMBER=""

  echo "  🔄 Syncing workspace…"
  (cd "$WORKTREE_DIR" && git fetch origin && git reset --hard origin/main) > /dev/null 2>&1

  echo "  🔍 Checking for open ralph PRs in ${REPO}…"
  OPEN_RALPH_PRS=$(gh pr list --repo "$REPO" --state open \
    --json number,headRefName \
    --jq '[.[] | select(.headRefName | startswith("ralph/issue-"))] | sort_by(.number)' \
    < /dev/null 2>/dev/null || echo "[]")

  PR_COUNT=$(echo "$OPEN_RALPH_PRS" | jq length)

  if [[ "$PR_COUNT" -gt 0 ]]; then
    PR_NUMBER=$(echo "$OPEN_RALPH_PRS" | jq -r '.[0].number')

    COMMENT_BODIES=$(gh pr view "$PR_NUMBER" --repo "$REPO" \
      --json comments --jq '[.comments[].body] | join("\n---\n")' \
      < /dev/null 2>/dev/null || echo "")

    APPROVED_COUNT=$(echo "$COMMENT_BODIES" | grep -c "RALPH-REVIEW: APPROVED" 2>/dev/null || true)
    CHANGES_REQUESTED=$(echo "$COMMENT_BODIES" | grep -c "RALPH-REVIEW: REQUEST_CHANGES" 2>/dev/null || true)

    # Route based on the *last* RALPH-REVIEW comment, not just whether any approval exists.
    # This prevents an infinite loop where merge mode posts REQUEST_CHANGES (CI failure)
    # and routing blindly routes back to merge because an older APPROVED comment exists.
    LAST_REVIEW_TYPE=$(gh pr view "$PR_NUMBER" --repo "$REPO" \
      --json comments \
      --jq '[.comments[] | select(.body | test("RALPH-REVIEW:"))] | last | .body |
        if test("RALPH-REVIEW: APPROVED") then "APPROVED"
        elif test("RALPH-REVIEW: REQUEST_CHANGES") then "REQUEST_CHANGES"
        else "" end' \
      < /dev/null 2>/dev/null || echo "")

    if [[ "$LAST_REVIEW_TYPE" == "APPROVED" ]]; then
      MODE="merge"
    elif [[ "$LAST_REVIEW_TYPE" == "REQUEST_CHANGES" ]]; then
      # Check whether commits were pushed after the last REQUEST_CHANGES comment
      LAST_RC_TIME=$(gh pr view "$PR_NUMBER" --repo "$REPO" \
        --json comments \
        --jq '[.comments[] | select(.body | contains("RALPH-REVIEW: REQUEST_CHANGES"))] | last | .createdAt // ""' \
        < /dev/null 2>/dev/null || echo "")
      LATEST_COMMIT_TIME=$(gh pr view "$PR_NUMBER" --repo "$REPO" \
        --json commits \
        --jq '.commits | last | .committedDate // ""' \
        < /dev/null 2>/dev/null || echo "")

      if [[ -n "$LATEST_COMMIT_TIME" && -n "$LAST_RC_TIME" && "$LATEST_COMMIT_TIME" > "$LAST_RC_TIME" ]]; then
        # New commits were pushed after the last RC.
        if [[ "${APPROVED_COUNT:-0}" -gt 0 ]]; then
          # PR already cleared a review pass — go straight to merge (CI will be re-checked there)
          MODE="merge"
        elif [[ "${CHANGES_REQUESTED:-0}" -ge 2 ]]; then
          MODE="force-approve"
        else
          MODE="review-round2"
        fi
      else
        MODE="fix"
      fi
    else
      MODE="review"
    fi

    echo "  ▶  Mode: $MODE  (PR #$PR_NUMBER)"
  else
    echo "  🔍 No open ralph PRs — checking issues…"

    # Pick highest-priority open issue: high-priority label first, then lowest number
    ISSUE_NUMBER=$(gh issue list --repo "$REPO" --state open \
      --json number,labels --limit 100 \
      --jq '
        [.[] | select(.labels | map(.name) | (contains(["prd"]) or contains(["blocked"])) | not)]
        | (
            (map(select(.labels | map(.name) | contains(["high priority"]))) | sort_by(.number) | first)
            // (sort_by(.number) | first)
          )
        | .number // empty
      ' \
      < /dev/null 2>/dev/null || echo "")

    if [[ -n "$ISSUE_NUMBER" ]]; then
      MODE="implement"
      echo "  ▶  Mode: $MODE  (Issue #$ISSUE_NUMBER)"
    else
      MODE="complete"
      echo "  ▶  Mode: $MODE  (no open issues or PRs)"
    fi
  fi
}

# Loads the mode file and substitutes {{REPO}}, {{PR_NUMBER}}, {{ISSUE_NUMBER}}.
build_prompt() {
  local mode_file="$MODES_DIR/$MODE.md"
  if [[ ! -f "$mode_file" ]]; then
    echo "  ❌  Mode file not found: $mode_file"
    exit 1
  fi

  PROMPT=$(cat "$mode_file")
  PROMPT="${PROMPT//\{\{REPO\}\}/$REPO}"
  PROMPT="${PROMPT//\{\{PR_NUMBER\}\}/$PR_NUMBER}"
  PROMPT="${PROMPT//\{\{ISSUE_NUMBER\}\}/$ISSUE_NUMBER}"
}

# ── Main loop ──────────────────────────────────────────────────────────────────

echo ""
echo "╔══════════════════════════════════════════════════════════════╗"
echo "║  Ralph — Copilot agentic loop                                ║"
echo "║  Max iterations: $MAX_ITERATIONS$(printf '%*s' $((46 - ${#MAX_ITERATIONS})) '')║"
echo "╚══════════════════════════════════════════════════════════════╝"

for i in $(seq 1 "$MAX_ITERATIONS"); do
  echo ""
  printf "\033[1;36m"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  echo "  🤖 Ralph — iteration $i / $MAX_ITERATIONS"
  echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
  printf "\033[0m"

  # Update terminal tab/window title and tmux window name.
  printf "\033]0;🤖 Ralph — iteration %s / %s\007" "$i" "$MAX_ITERATIONS"
  printf "\033k🤖 Ralph %s/%s\033\\" "$i" "$MAX_ITERATIONS"

  MODE=""
  determine_mode

  # All done — no copilot needed
  if [[ "$MODE" == "complete" ]]; then
    echo ""
    printf "\033[1;32m"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  ✅  Ralph completed all tasks at iteration $i / $MAX_ITERATIONS"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    printf "\033[0m"
    exit 0
  fi

  build_prompt

  OUTPUT=$(
    cd "$WORKTREE_DIR" && copilot \
      --prompt "$PROMPT" \
      --allow-all \
      --autopilot \
      2>&1 | tee /dev/stderr
  ) || true

  # Belt-and-suspenders COMPLETE check (bash routing handles this now, but
  # keep the signal detection in case a mode emits it unexpectedly)
  if echo "$OUTPUT" | grep -q "<promise>COMPLETE</promise>"; then
    echo ""
    printf "\033[1;32m"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    echo "  ✅  Ralph completed all tasks at iteration $i / $MAX_ITERATIONS"
    echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
    printf "\033[0m"
    exit 0
  fi

  if echo "$OUTPUT" | grep -q "<promise>STOP</promise>"; then
    echo ""
    echo "  ✔  Iteration $i / $MAX_ITERATIONS done — restarting"
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
echo "    • Tune ralph/modes/ if Copilot is going in the wrong direction"
echo "━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━━"
exit 1
