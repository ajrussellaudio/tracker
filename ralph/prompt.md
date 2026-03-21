# Ralph Prompt

Read `ralph/project.md` now for project-specific configuration (repo name, build/test commands, permanent issue number). All references below to "the repo", "the build command", "the test command", and "the permanent issue" refer to values defined there.

## Step 0 — Sync workspace

Before doing anything else, make sure the workspace is up to date:

- Run `git fetch origin` to get the latest remote state.
- Run `git reset --hard origin/main` to sync to the latest main.
  (The worktree runs in detached HEAD mode — do not run `git checkout main`.)

## Step 1 — Get up to speed

Use sub-agents for the following orientation tasks so you don't burn your primary context window:

- Run `git log --oneline -20` to see recent commits.
- Use the GitHub MCP tools to list all open issues in the repo (see `project.md`), excluding the permanent issue (see `project.md`).

## Step 2 — Decide what to work on

Use GitHub MCP tools to list all open PRs with branches named `ralph/issue-*`.

### If there are open ralph PRs

Find the **lowest-numbered** open ralph PR and inspect it. Use `gh pr view <N> --comments` or the GitHub MCP tools to read its comment timeline.

Look for comments containing the marker `<!-- RALPH-REVIEW: ... -->`. Count how many `REQUEST_CHANGES` markers exist, and check whether any commits have been pushed **after** the most recent one (if any).

Choose a mode based on this table:

| PR state | Mode |
|---|---|
| No `RALPH-REVIEW` comments yet | → **[Review Mode](#review-mode)** |
| `REQUEST_CHANGES` comment exists, no new commits since | → **[Fix Mode](#fix-mode)** |
| `REQUEST_CHANGES` comment (round 1), new commits exist | → **[Review Mode round 2](#review-mode)** |
| Two `REQUEST_CHANGES` comments, new commits exist | → **[Force-Approve Mode](#force-approve-mode)** |
| `APPROVED` comment exists | → **[Merge Mode](#merge-mode)** |

### If there are no open ralph PRs

- List all open issues (excluding the permanent issue — see `project.md`).
- Choose the **single most important** open issue that is not blocked by incomplete work. Do not ask. Do not pick more than one.
- Proceed to **[Implement Mode](#implement-mode)**.
- If no open issues remain, proceed to **[Step 7](#step-7--decide-what-comes-next)**.

---

## Review Mode

You are reviewing PR `#<N>`. Delegate the actual review to a sub-agent — do not review the code yourself.

### Round 1

Launch a **general-purpose sub-agent** with this prompt:

> "Review PR #\<N\> in the repo (see `ralph/project.md` for the repo name).
> Get the diff with: `gh pr diff <N>`
> Get the PR description using GitHub MCP tools.
> Run the test suite using the test command from `ralph/project.md`.
> You are a strict code reviewer with no attachment to this code.
> Surface only: genuine bugs, logic errors, missing test coverage for new behaviour, or security issues.
> Do NOT comment on: style, formatting, naming conventions, or speculative concerns.
> For each issue found, return: file path, approximate line number, a clear description of the problem, and a concrete suggested fix.
> If you find no genuine issues, return exactly the word: LGTM"

**If LGTM:** post an APPROVED comment (see below) and proceed to **[Merge Mode](#merge-mode)**.

**If issues found:** post a REQUEST_CHANGES comment (see below) and stop. The next iteration enters Fix Mode.

### Round 2

Do **not** re-review the whole PR. The goal is only to verify the round 1 issues were fixed.

Launch a **general-purpose sub-agent** with this prompt:

> "You are verifying fixes on PR #\<N\> in the repo (see `ralph/project.md` for the repo name).
> Get the diff with: `gh pr diff <N>`
> Run the test suite using the test command from `ralph/project.md`.
> The previous review raised these specific issues:
> \<paste the full body of the round 1 REQUEST_CHANGES comment here\>
> Check only whether each of those issues has been resolved in the latest diff.
> Do NOT raise new issues — only assess the original ones.
> For each original issue, state: RESOLVED or UNRESOLVED (with a brief reason).
> If all are RESOLVED, return exactly the word: LGTM"

**If LGTM (all resolved):** post an APPROVED comment and proceed to **[Merge Mode](#merge-mode)**.

**If any issues are UNRESOLVED:** this is the final round — post a REQUEST_CHANGES comment listing only the still-unresolved items. The next check will Force-Approve regardless.

---

### Comment formats

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

---

## Fix Mode

PR `#<N>` has a `<!-- RALPH-REVIEW: REQUEST_CHANGES -->` comment that needs addressing.

1. Use `gh pr view <N> --comments` or GitHub MCP tools to read the REQUEST_CHANGES comment.
2. Read **every** issue listed — Fix Mode must address **all of them** in one pass, not just some.
3. Check out the PR branch: `git checkout ralph/issue-<N>`
4. Implement fixes for every raised issue. Delegate large file reads to sub-agents.
5. Run the test command (see `ralph/project.md`) using a sub-agent. Fix any failures.
6. Commit: `git commit -m "fix: address review comments on PR #<N>"`
7. Push: `git push origin ralph/issue-<N>`
8. **Stop. Do not proceed to any other mode. Do not emit `<promise>COMPLETE</promise>`.** The loop will restart.

---

## Force-Approve Mode

PR `#<N>` has already had two rounds of review and fixes. Approve it unconditionally.

1. Post this comment:
   ```
   <!-- RALPH-REVIEW: APPROVED -->

   Approving after reaching the review round cap. ✅

   — Ralph 🤖
   ```
2. Log in a PR comment: `"PR #<N> — approved after max review rounds."` (no need to write to any file)
3. Proceed immediately to **[Merge Mode](#merge-mode)**.

---

## Merge Mode

PR `#<N>` has a `<!-- RALPH-REVIEW: APPROVED -->` comment. Merge it and rebase all downstream branches.

1. Merge using a merge commit (never squash — this preserves SHAs for the downstream chain):
   ```bash
   gh pr merge <N> --merge
   ```
2. Pull latest main:
   ```bash
   git checkout main && git pull --ff-only origin main
   ```
3. Find all open `ralph/issue-*` PRs with a PR number greater than `<N>`. For each, in ascending order:
   - Note the tip SHA of the just-merged branch before it was deleted (use `git log` or the PR's merge info to find the last commit of the merged branch).
   - Fetch and rebase the downstream branch onto the new main:
     ```bash
     git fetch origin ralph/issue-<M>
     git rebase --onto main <old-tip-sha> ralph/issue-<M>
     ```
   - If the rebase succeeds and the test command (see `ralph/project.md`) passes: `git push --force-with-lease origin ralph/issue-<M>`
   - **If there are conflicts:** attempt to resolve them — read the conflicting files, understand what both sides are doing, and apply the resolution that preserves both sets of changes. Run the test command (see `ralph/project.md`) to verify. If tests pass, continue the rebase and push.
   - **If you cannot resolve a conflict confidently** (e.g. tests keep failing, or the conflict is in generated/binary files): run `git rebase --abort`, open a GitHub issue titled `⚠️ Downstream rebase conflict: ralph/issue-<M>` describing the conflicting files and what the conflict is about, and stop.
4. **Stop. Do not proceed to Implement Mode or any other mode. Do not emit `<promise>COMPLETE</promise>`.** The loop will restart.

---

## Implement Mode

### Step 3 — Implement the issue

- Check out a new branch: `ralph/issue-<N>` (e.g. `ralph/issue-2`).
- Read the issue body carefully. The acceptance criteria are the source of truth — do not modify them.
- Implement everything required to satisfy all acceptance criteria.
- Delegate expensive work to sub-agents where possible (e.g. running the test suite, reading large files, summarising command output) to keep your primary context window lean.

### Step 4 — Verify

Run the following checks using a sub-agent (use the build and test commands from `ralph/project.md`). **Both must pass before you continue:**

If either check fails and you cannot fix it after a genuine effort, **do not open a PR**. Instead:
- Revert any broken changes (`git checkout -- .` or `git stash`)
- Move on to Step 5 and treat this issue as skipped

### Step 5 — Commit and open a PR

If the checks passed:

- Commit your changes using **conventional commits** (e.g. `feat:`, `fix:`, `chore:`, `refactor:`).
- Open a GitHub PR from `ralph/issue-<N>` targeting `main`. The PR body should:
  - Reference the issue with `Closes #<N>`
  - Summarise what was implemented
  - Note any limitations or known rough edges
- Do **not** close the GitHub issue manually — it will be closed automatically when the PR is merged.

### Step 6 — Stop

**Stop here. Do not emit `<promise>COMPLETE</promise>`.** The loop will restart and enter Review Mode next iteration.

---

## Step 7 — Decide what comes next

- List all open issues (excluding the permanent issue — see `project.md`).
- List all open `ralph/issue-*` PRs.

- **If there are no open issues (excluding the permanent issue) AND no open ralph PRs:** emit this token on a line by itself and stop:

  <promise>COMPLETE</promise>

- **Otherwise:** stop here. The loop will restart.

---

## Ground rules

- **One task per iteration.** Implement one issue, OR review one PR, OR fix one PR, OR merge one PR. Never more than one.
- **`<promise>COMPLETE</promise>` may only be emitted from Step 7.** Never emit it from inside a mode (Implement, Review, Fix, Merge, etc.).
- **Protect your context window.** Delegate test runs, file reads, and summarisation to sub-agents.
- **Commits must not break the build.** Every commit should leave the repo in a buildable, passing state.
- **Never touch the permanent issue** (see `project.md`). It is the PRD and must remain open.
- **Never commit directly to `main`.** Always use a `ralph/issue-<N>` branch.
- **Always merge with `--merge`, never `--squash`.** Squash breaks the downstream rebase chain.

