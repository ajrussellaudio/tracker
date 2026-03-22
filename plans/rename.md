# Plan: Project Rename (`tracker` → `vitakt`)

## Architectural decisions

- **Binary name:** `vitakt` — the command users type in the terminal
- **Core crate:** `vitakt-core` — the internal library crate (private, not published)
- **Config directory:** `~/.config/vitakt/theme.toml` — replaces `~/.config/tracker/`
- **File extension:** `.trk` — **unchanged**; this is the project file format, not the app name
- **GitHub repo:** `ajrussellaudio/vitakt` — rename via GitHub repo settings; GitHub will redirect the old URL automatically

> ⚠️ The config directory rename is a **breaking change** for existing users: their `~/.config/tracker/theme.toml` will no longer be read. Consider adding a one-time migration (copy old path to new path on startup) or documenting the manual step.

---

## Phase 1: Cargo workspace and crate names

### What to build

Rename all crate and package identifiers so the project compiles cleanly under the new name.

- `tracker/Cargo.toml` — `name = "tracker"` → `name = "vitakt"`; `[[bin]] name = "tracker"` → `name = "vitakt"`; `repository` URL; local dep `tracker-core` → `vitakt-core`
- `tracker-core/Cargo.toml` — `name = "tracker-core"` → `name = "vitakt-core"`
- Root `Cargo.toml` — workspace `members = ["tracker-core", "tracker"]` → `["vitakt-core", "vitakt"]`
- Rename the `tracker/` directory to `vitakt/` and `tracker-core/` to `vitakt-core/`
- `dist-workspace.toml` — update any name references

Verify: `cargo build --workspace` passes.

### Acceptance criteria

- [ ] `cargo build --workspace` succeeds with no errors
- [ ] `cargo test --workspace` passes
- [ ] The compiled binary is named `vitakt` (check `target/release/vitakt`)

---

## Phase 2: Source code strings

### What to build

Update every hardcoded `"tracker"` string in Rust source that is user-visible or used as a filesystem path.

- `vitakt/src/theme.rs` line ~241 — config dir: `.join("tracker")` → `.join("vitakt")`
- `vitakt/src/main.rs` — usage/error messages that reference the binary name:
  - `"Launch with \`tracker path/to/..."` → `\`vitakt path/to/..."`
  - `"Usage: tracker [path.trk]"` → `"Usage: vitakt [path.trk]"`
- `vitakt/src/main.rs` — test harness `argv[0]` strings: `"tracker".to_string()` → `"vitakt.to_string()`
- `vitakt/src/main.rs` — test fixture temp-file prefixes (`tracker_phrase_roundtrip.trk` etc.) → `vitakt_phrase_roundtrip.trk` etc.

> Note: the config directory rename means existing users' `~/.config/tracker/theme.toml` will be silently ignored. See the migration note in Architectural Decisions above.

### Acceptance criteria

- [ ] `cargo test --workspace` passes
- [ ] Running the binary and passing `--unknown-flag` shows the correct new binary name in the usage message
- [ ] The theme file is read from `~/.config/vitakt/theme.toml` (verify by placing a theme file there)

---

## Phase 3: Docs and tooling

### What to build

Update all human-readable files and tooling config so they reflect the new name consistently.

- `README.md` — heading, install `curl` URL, binary invocation examples, `git clone` URL, `cd` directory, `target/release/` path
- `CHANGELOG.md` — config path reference (`~/.config/tracker/theme.toml` entry), GitHub issue URLs
- `ralph/project.md` — project name, GitHub repo slug
- `.github/workflows/release.yml` — re-run `dist generate` after the Cargo rename to regenerate installer filename and artifact names
- `plans/rename.md` (this file) — replace all `vitakt` placeholders with the real name once chosen

### Acceptance criteria

- [ ] README install one-liner references `vitakt-installer.sh` and the correct repo URL
- [ ] `ralph/project.md` points to `ajrussellaudio/vitakt`
- [ ] No remaining occurrences of `"tracker"` as a project name in any doc or config file (`grep -r "tracker" --include="*.md" --include="*.toml" --include="*.yml"` returns only legitimate uses of the word "tracker" as a genre/concept, not as this project's name)

---

## Phase 4: GitHub repo rename

### What to build

Rename the GitHub repository itself. This is a one-step action in GitHub repo Settings → General → Repository name.

GitHub will automatically create a redirect from `ajrussellaudio/tracker` to `ajrussellaudio/vitakt`, so existing links, clones, and Ralph's remote will continue to work without immediate updates.

After renaming:
- Run `git remote set-url origin git@github.com:ajrussellaudio/vitakt.git` in any local clones
- The redirect covers most cases, but updating the remote avoids relying on the redirect long-term

### Acceptance criteria

- [ ] `github.com/ajrussellaudio/vitakt` loads the repository
- [ ] `github.com/ajrussellaudio/tracker` redirects correctly
- [ ] `git push origin main` succeeds from a local clone after updating the remote URL
- [ ] GitHub Actions workflows run successfully on a push after the rename
