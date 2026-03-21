# Ralph Prompt

You are working autonomously on the **tracker** project — a CLI/TUI sample-based music tracker in Rust.

## Your task

1. **Get up to speed.** Read `ralph/progress.txt` and the recent `git log` to understand what has already been done.

2. **Check the issue queue.** Use the GitHub MCP tools to list all open issues in the `ajrussellaudio/tracker` repository (excluding issue #1, which is the PRD and should remain open).

3. **Pick one issue to work on.** Choose the most important open issue that is not blocked by unfinished work. Trust your own judgement — you do not need to ask.

4. **Do the work.**
   - Create a branch named `ralph/issue-<N>` (e.g. `ralph/issue-2`) and check it out.
   - Implement everything described in the issue body.
   - Make sure all acceptance criteria are met.
   - Run `cargo build` and `cargo test` before considering the work done.
   - Commit your changes with a clear, descriptive commit message.
   - Close the GitHub issue once the work is complete.

5. **Update the progress log.** Append a brief summary of what you did (issue number, title, what was implemented, any known limitations) to `ralph/progress.txt` and commit it alongside your work.

6. **Decide what comes next.** If all issues (except #1) are now closed, emit the following token on a line by itself and stop:

   <promise>COMPLETE</promise>

   Otherwise, stop here. The loop will restart and you will pick up the next issue in the next iteration.

## Ground rules

- **One issue per iteration.** Do not try to implement multiple issues in a single run.
- **Keep commits clean.** Each commit should leave the codebase in a working, buildable state.
- **Do not remove or modify acceptance criteria** in the issue bodies — they are the source of truth.
- **Do not close issue #1** (the PRD). It should remain open as a reference.
- **Branch per issue.** Always work on a dedicated `ralph/issue-<N>` branch, never directly on `main`.
