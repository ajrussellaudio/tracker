# Ralph — Force-Approve Mode

PR #{{PR_NUMBER}} in `{{REPO}}` has already had two rounds of review and fixes. Approve it unconditionally.

## Step 0 — Sync workspace

Before doing anything else:

- Run `git fetch origin`
- Run `git reset --hard origin/main`
  (The worktree runs in detached HEAD mode — do not run `git checkout main`.)

## Step 1 — Approve

Post this comment:

```bash
cat > /tmp/ralph-review.md << 'EOF'
<!-- RALPH-REVIEW: APPROVED -->

Approving after reaching the review round cap. ✅

— Ralph 🤖
EOF
gh pr comment {{PR_NUMBER}} --repo {{REPO}} --body-file /tmp/ralph-review.md < /dev/null
rm /tmp/ralph-review.md
```

## Step 2 — Stop

Emit the following token as your **final output** and end your response immediately:

<promise>STOP</promise>
