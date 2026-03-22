# Ralph — Review Mode (Round 2)

You are verifying that round-1 review issues have been fixed on PR #{{PR_NUMBER}} in `{{REPO}}`.

Read `ralph/project.md` for the build and test commands.

## Step 0 — Sync workspace

Before doing anything else:

- Run `git fetch origin`
- Run `git reset --hard origin/main`
  (The worktree runs in detached HEAD mode — do not run `git checkout main`.)

## Step 1 — Find the original issues

Use `gh pr view {{PR_NUMBER}} --repo {{REPO}} --comments` or GitHub MCP tools to read the PR comment timeline. Find the `<!-- RALPH-REVIEW: REQUEST_CHANGES -->` comment and note every issue it listed.

## Step 2 — Verify fixes

Delegate verification to a sub-agent. Do **not** re-review the whole PR.

Launch a **general-purpose sub-agent** with this prompt:

> "You are verifying fixes on PR #{{PR_NUMBER}} in `{{REPO}}`.
> Get the diff with: `gh pr diff {{PR_NUMBER}} --repo {{REPO}}`
> Run the test suite using the test command in `ralph/project.md`.
> The previous review raised these specific issues:
> <paste the full body of the round 1 REQUEST_CHANGES comment here>
> Check only whether each of those issues has been resolved in the latest diff.
> Do NOT raise new issues — only assess the original ones.
> For each original issue, state: RESOLVED or UNRESOLVED (with a brief reason).
> If all are RESOLVED, return exactly the word: LGTM"

**If LGTM (all resolved):** post an APPROVED comment (see below), then emit the following token as your **final output** and end your response immediately:

<promise>STOP</promise>

**If any issues are UNRESOLVED:** this is the final round — post a REQUEST_CHANGES comment listing only the still-unresolved items. Then emit the following token as your **final output** and end your response immediately:

<promise>STOP</promise>

## Comment formats

Post all review comments by writing the body to a temp file and using `--body-file` with stdin closed:

```bash
cat > /tmp/ralph-review.md << 'EOF'
<comment body here>
EOF
gh pr comment {{PR_NUMBER}} --repo {{REPO}} --body-file /tmp/ralph-review.md < /dev/null
rm /tmp/ralph-review.md
```

**APPROVED:**
```
<!-- RALPH-REVIEW: APPROVED -->

LGTM — no blocking issues found. ✅

— Ralph 🤖
```

**REQUEST_CHANGES:**
```
<!-- RALPH-REVIEW: REQUEST_CHANGES -->

The following issues need addressing before this can merge:

1. **`path/to/file.rs` ~line N** — Description of the problem.
   Suggested fix: ...

— Ralph 🤖
```
