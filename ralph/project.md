# Ralph Project Configuration

This file contains the project-specific settings that parameterise the generic
`prompt.md`. Ralph reads this file at Step 0 before doing anything else.

## Project

**Name:** tracker — a CLI/TUI sample-based music tracker in Rust

**GitHub repo:** `ajrussellaudio/tracker`

## Build and test commands

```bash
cargo build
cargo test
```

## Permanent issue

Issue **#1** is the PRD. It must never be closed or touched. All references in
the prompt to "excluding issue #1" or "never touch issue #1" refer to this number.

## Branch prefix

Feature branches follow the convention `ralph/issue-<N>` (e.g. `ralph/issue-2`).
This is baked into the prompt as a convention and does not need to change.
