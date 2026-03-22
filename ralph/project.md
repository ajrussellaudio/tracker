# Ralph Project Configuration

This file contains the project-specific settings that parameterise the generic
`prompt.md`. Ralph reads this file at Step 0 before doing anything else.

## Project

**Name:** vitakt — a CLI/TUI sample-based music tracker in Rust

**GitHub repo:** `ajrussellaudio/vitakt`

## Build and test commands

```bash
cargo build
cargo test
```

## PRD label

Issues labelled **`prd`** are Product Requirements Documents. Ralph must never
implement, close, or comment on them. All references in the prompt to
"excluding PRD issues" refer to issues carrying this label.

## Branch prefix

Feature branches follow the convention `ralph/issue-<N>` (e.g. `ralph/issue-2`).
This is baked into the prompt as a convention and does not need to change.
