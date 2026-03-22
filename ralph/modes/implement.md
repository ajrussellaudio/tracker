# Ralph — Implement Mode

You are implementing GitHub issue #{{ISSUE_NUMBER}} in the `{{REPO}}` repository.

Read `ralph/project.md` for the build and test commands.

## Step 0 — Sync workspace

Before doing anything else:

- Run `git fetch origin`
- Run `git reset --hard origin/main`
  (The worktree runs in detached HEAD mode — do not run `git checkout main`.)

## Step 1 — Get up to speed

- Run `git log --oneline -10` to see recent commits.
- Read issue #{{ISSUE_NUMBER}} using GitHub MCP tools. The acceptance criteria are the source of truth.

## Step 2 — Implement

- Check out a new branch: `git checkout -b ralph/issue-{{ISSUE_NUMBER}}`
- Implement everything required to satisfy all acceptance criteria.
- Delegate expensive work to sub-agents where possible (running the test suite, reading large files, summarising command output) to keep your primary context window lean.

## Step 3 — Verify

Run the build and test commands from `ralph/project.md` using a sub-agent. **Both must pass before you continue.**

If either check fails and you cannot fix it after a genuine effort, **do not open a PR**. Instead:

- Revert any broken changes (`git checkout -- .` or `git stash`)
- Emit the following token as your final output and stop:

  <promise>STOP</promise>

## Step 4 — Update CHANGELOG and commit

If the checks passed:

**Update `CHANGELOG.md`** in the repo root before committing:

- If `CHANGELOG.md` does not exist, create it with this header:
  ```
  # Changelog

  All notable changes to this project will be documented in this file.

  The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/).

  ## [Unreleased]
  ```
- Add an entry under `## [Unreleased]` using the appropriate subsection (`### Added`, `### Changed`, `### Fixed`, `### Removed`). One concise bullet per logical change. Include the issue number in parentheses, e.g.:
  ```
  ### Added
  - Quit confirmation modal when there are unsaved changes (#53)
  ```
- If `## [Unreleased]` already exists, append to the correct subsection (or create it if needed). Do not create a new `## [Unreleased]` block.
- Do not add version headers or dates.

**Commit** all changes (code + CHANGELOG) together using conventional commits (`feat:`, `fix:`, `chore:`, `refactor:`).

**Open a GitHub PR** from `ralph/issue-{{ISSUE_NUMBER}}` targeting `main`. The PR body should:
- Reference the issue with `Closes #{{ISSUE_NUMBER}}`
- Summarise what was implemented
- Note any limitations or known rough edges

Do **not** close the GitHub issue manually — it closes automatically when the PR is merged.

## Step 5 — Stop

Your work this iteration is done.

Emit this token as your **final output** and stop:

<promise>STOP</promise>

Any output after this token violates the rules.
