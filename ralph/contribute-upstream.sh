#!/bin/bash
# ralph/contribute-upstream.sh
#
# Propagate changes to ralph.sh and/or modes/ back to ajrussellaudio/ralph.
# project.md is intentionally excluded — it is tracker-specific.
#
# Usage:
#   ./ralph/contribute-upstream.sh
#   ./ralph/contribute-upstream.sh "brief description of the change"

set -euo pipefail

SCRIPT_DIR="$(cd "$(dirname "${BASH_SOURCE[0]}")" && pwd)"
UPSTREAM_REPO="ajrussellaudio/ralph"
GENERIC_FILES=("ralph.sh")
WORK_DIR=$(mktemp -d)
BRANCH="contrib/tracker-$(date +%Y%m%d-%H%M%S)"
PR_TITLE="${1:-"chore: sync improvements from tracker"}"

# ── Cleanup ────────────────────────────────────────────────────────────────────

cleanup() {
  rm -rf "$WORK_DIR"
}
trap cleanup EXIT

# ── Preflight ──────────────────────────────────────────────────────────────────

if ! command -v gh &>/dev/null; then
  echo "Error: 'gh' not found. Install the GitHub CLI first."
  exit 1
fi

# ── Clone upstream ─────────────────────────────────────────────────────────────

echo ""
echo "  Cloning $UPSTREAM_REPO …"
gh repo clone "$UPSTREAM_REPO" "$WORK_DIR" -- --quiet --depth=1

# ── Diff generic files ─────────────────────────────────────────────────────────

CHANGED=()
for f in "${GENERIC_FILES[@]}"; do
  if ! diff -q "$SCRIPT_DIR/$f" "$WORK_DIR/$f" > /dev/null 2>&1; then
    CHANGED+=("$f")
  fi
done

# Check modes/ directory (add any new or changed mode files)
MODES_CHANGED=()
if [[ -d "$SCRIPT_DIR/modes" ]]; then
  mkdir -p "$WORK_DIR/modes"
  while IFS= read -r -d '' mode_file; do
    rel="${mode_file#$SCRIPT_DIR/}"
    if ! diff -q "$mode_file" "$WORK_DIR/$rel" > /dev/null 2>&1; then
      MODES_CHANGED+=("$rel")
    fi
  done < <(find "$SCRIPT_DIR/modes" -name "*.md" -print0)
fi

ALL_CHANGED=("${CHANGED[@]}" "${MODES_CHANGED[@]}")

if [[ ${#ALL_CHANGED[@]} -eq 0 ]]; then
  echo "  No differences found — $UPSTREAM_REPO is already up to date."
  exit 0
fi

echo "  Changed: ${ALL_CHANGED[*]}"

# ── Build PR body ──────────────────────────────────────────────────────────────

PR_BODY_FILE=$(mktemp)
{
  echo "Propagated from [ajrussellaudio/tracker](https://github.com/ajrussellaudio/tracker)."
  echo ""
  echo "## Changes"
  echo ""
  for f in "${CHANGED[@]}"; do
    echo "### \`$f\`"
    echo '```diff'
    diff "$WORK_DIR/$f" "$SCRIPT_DIR/$f" || true
    echo '```'
    echo ""
  done
  for f in "${MODES_CHANGED[@]}"; do
    echo "### \`$f\`"
    echo '```diff'
    diff "$WORK_DIR/$f" "$SCRIPT_DIR/$f" 2>/dev/null || cat "$SCRIPT_DIR/$f"
    echo '```'
    echo ""
  done
  echo "_\`project.md\` is not included — it is tracker-specific config._"
} > "$PR_BODY_FILE"

# ── Commit and push ────────────────────────────────────────────────────────────

cd "$WORK_DIR"
git checkout -b "$BRANCH"

for f in "${CHANGED[@]}"; do
  cp "$SCRIPT_DIR/$f" "$f"
done
for f in "${MODES_CHANGED[@]}"; do
  cp "$SCRIPT_DIR/$f" "$f"
done

git add "${ALL_CHANGED[@]}"
git commit -m "$PR_TITLE"
git push origin "$BRANCH"

# ── Open PR ────────────────────────────────────────────────────────────────────

echo ""
PR_URL=$(gh pr create \
  --repo "$UPSTREAM_REPO" \
  --title "$PR_TITLE" \
  --body-file "$PR_BODY_FILE" \
  --base main \
  --head "$BRANCH")

echo "  ✅  PR opened: $PR_URL"
