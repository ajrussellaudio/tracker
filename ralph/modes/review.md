# Ralph — Review Mode (Round 1)

You are reviewing PR #{{PR_NUMBER}} in the `{{REPO}}` repository.

Read `ralph/project.md` for the build and test commands.

## Step 0 — Sync workspace

Before doing anything else:

- Run `git fetch origin`
- Run `git reset --hard origin/main`
  (The worktree runs in detached HEAD mode — do not run `git checkout main`.)

## Step 1 — Review

Delegate the review to a sub-agent. Do not review the code yourself.

Launch a **general-purpose sub-agent** with this prompt:

> "Review PR #{{PR_NUMBER}} in `{{REPO}}`.
> Get the diff with: `gh pr diff {{PR_NUMBER}} --repo {{REPO}}`
> Get the PR description using GitHub MCP tools.
> Run the test suite using the test command in `ralph/project.md`.
> You are a strict code reviewer with no attachment to this code.
> Surface only: genuine bugs, logic errors, missing test coverage for new behaviour, or security issues.
> Do NOT comment on: style, formatting, naming conventions, or speculative concerns.
> For each issue found, return: file path, approximate line number, a clear description of the problem, and a concrete suggested fix.
> If you find no genuine issues, return exactly the word: LGTM"

**If LGTM:** post an APPROVED comment (see below), then emit the following token as your **final output** and end your response immediately:

<promise>STOP</promise>

**If issues found:** post a REQUEST_CHANGES comment (see below), then emit the following token as your **final output** and end your response immediately:

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
