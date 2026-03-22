# Ralph — Merge Mode

PR #{{PR_NUMBER}} in `{{REPO}}` has been approved. Merge it.

Read `ralph/project.md` for the build and test commands.

## Step 0 — Sync workspace

Before doing anything else:

- Run `git fetch origin`
- Run `git reset --hard origin/main`
  (The worktree runs in detached HEAD mode — do not run `git checkout main`.)

## Step 1 — Verify CI

Check that all CI checks have passed:

```bash
gh pr checks {{PR_NUMBER}} --repo {{REPO}} < /dev/null
```

- If any check is **failed**: post a `<!-- RALPH-REVIEW: REQUEST_CHANGES -->` comment listing the failing check names, then emit `<promise>STOP</promise>` as your final output.
- If any check is **in progress**: emit `<promise>STOP</promise>` as your final output without posting a comment.
- Only proceed to Step 2 if all checks have passed.

## Step 2 — Merge

```bash
gh pr merge {{PR_NUMBER}} --repo {{REPO}} --merge < /dev/null
```

## Step 3 — Update workspace to new main

```bash
git fetch origin && git reset --hard origin/main
```

## Step 4 — Rebase downstream PRs

Find all open `ralph/issue-*` PRs with a PR number greater than {{PR_NUMBER}}. For each, in ascending order:

- Note the tip SHA of the just-merged branch (use the PR's merge info or `git log` to find the last commit of that branch).
- Fetch and rebase the downstream branch onto new main:
  ```bash
  git fetch origin ralph/issue-<M>
  git rebase --onto main <old-tip-sha> ralph/issue-<M>
  ```
- If the rebase succeeds and the test command (from `ralph/project.md`) passes: `git push --force-with-lease origin ralph/issue-<M>`
- **If there are conflicts:** attempt to resolve them — read the conflicting files, understand what both sides are doing, and apply the resolution that preserves both sets of changes. Run tests to verify. If tests pass, continue the rebase and push.
- **If you cannot resolve a conflict confidently** (e.g. tests keep failing, or the conflict is in generated/binary files): run `git rebase --abort`, open a GitHub issue titled `⚠️ Downstream rebase conflict: ralph/issue-<M>` describing the conflicting files, and stop.

## Step 5 — Unblock issues

Fetch all open issues that carry the `blocked` label:

```bash
gh issue list --repo {{REPO}} --label blocked --json number,body --limit 100 < /dev/null
```

The issue closed by this PR is the one referenced in the PR body as `Closes #<X>`. Look it up with:

```bash
gh pr view {{PR_NUMBER}} --repo {{REPO}} --json closingIssuesReferences --jq '.closingIssuesReferences[].number' < /dev/null
```

For each blocked issue, check whether its body contains `Blocked by #<X>` (case-insensitive). If it does:

- Inspect the body for **any other** `Blocked by #Y` references.
- For each such `#Y`, check whether issue `#Y` is still open:
  ```bash
  gh issue view <Y> --repo {{REPO}} --json state --jq .state < /dev/null
  ```
- If **all** blocking issues are now closed, remove the `blocked` label:
  ```bash
  gh issue edit <N> --repo {{REPO}} --remove-label "blocked" < /dev/null
  ```

## Step 6 — Stop

Emit this token as your **final output** and end your response immediately:

<promise>STOP</promise>
