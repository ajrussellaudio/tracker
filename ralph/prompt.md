# Ralph Prompt

You are working autonomously on the **tracker** project — a CLI/TUI sample-based music tracker in Rust.

## Step 1 — Get up to speed

Use sub-agents for the following orientation tasks so you don't burn your primary context window:

- Read `ralph/progress.txt` to see what previous iterations have done.
- Run `git log --oneline -20` to see recent commits.
- Use the GitHub MCP tools to list all open issues in `ajrussellaudio/tracker`, excluding issue #1 (the PRD, which stays open permanently).

## Step 2 — Pick one issue

- List all open issues (excluding #1) using the GitHub MCP tools.
- List all open PRs to find which issues already have a `ralph/issue-<N>` branch with an open PR.
- Choose the **single most important** open issue that:
  - is not blocked by incomplete work, and
  - does **not** already have an open PR.
- Use your own judgement. Do not ask. Do not pick more than one.
- If every open issue (excluding #1) already has an open PR, skip to Step 7 immediately.

## Step 3 — Implement it

- Check out a new branch: `ralph/issue-<N>` (e.g. `ralph/issue-2`).
- Read the issue body carefully. The acceptance criteria are the source of truth — do not modify them.
- Implement everything required to satisfy all acceptance criteria.
- Delegate expensive work to sub-agents where possible (e.g. running the test suite, reading large files, summarising command output) to keep your primary context window lean.

## Step 4 — Verify

Run the following checks using a sub-agent. **Both must pass before you continue:**

```bash
cargo build
cargo test
```

If either check fails and you cannot fix it after a genuine effort, **do not open a PR**. Instead:
- Revert any broken changes (`git checkout -- .` or `git stash`)
- Note what you attempted and why it failed in `ralph/progress.txt`
- Move on to Step 5 and treat this issue as skipped

## Step 5 — Commit and open a PR

If the checks passed:

- Commit your changes using **conventional commits** (e.g. `feat:`, `fix:`, `chore:`, `refactor:`).
- Open a GitHub PR from `ralph/issue-<N>` targeting `main`. The PR body should:
  - Reference the issue with `Closes #<N>`
  - Summarise what was implemented
  - Note any limitations or known rough edges
- Do **not** close the GitHub issue manually — it will be closed automatically when the PR is merged.

## Step 6 — Update the progress log

Append a brief entry to `ralph/progress.txt` and commit it:

```
## Issue #<N> — <title>
Status: done / skipped
Branch: ralph/issue-<N>
PR: #<PR number> (or N/A if skipped)
Summary: <one or two sentences>
```

## Step 7 — Decide what comes next

- List all open issues (excluding #1) using the GitHub MCP tools.
- List all open PRs to see which issues already have a `ralph/issue-<N>` PR in flight.

- **If every open issue (excluding #1) either is already closed or has an open PR:** emit this token on a line by itself and stop:

  <promise>COMPLETE</promise>

- **Otherwise:** stop here. The loop will restart and pick up the next issue.

---

## Ground rules

- **One issue per iteration.** Never implement more than one.
- **Protect your context window.** Delegate test runs, file reads, and summarisation to sub-agents.
- **Commits must not break the build.** Every commit should leave the repo in a buildable, passing state.
- **Never touch issue #1.** It is the PRD and must remain open.
- **Never commit directly to `main`.** Always use a `ralph/issue-<N>` branch.
