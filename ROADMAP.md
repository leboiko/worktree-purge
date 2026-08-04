# Roadmap

Tracked list of planned features for `worktree-purge`. Items here are
**considered** or **in progress**, out of scope for v1; shipped work moves
to [`CHANGELOG.md`](CHANGELOG.md).

When picking what to work on next: skim this file first. When agreeing on a
new feature, add it here. When shipping, remove the entry (and let the
CHANGELOG be the historical record).

Seeded from `SCOPE.md` §5 "v2 ideas" (out of scope for v1 — do not build yet).

---

## Next up

### Quick wins (under a session each)

- **`--dry-run`** — print what would be purged (paths + sizes + reasons)
  without touching disk or git metadata. Useful for scripting and for
  cautious first-time use.

### Medium projects (1-3 sessions)

- **Mouse support** — click to select/expand/collapse, matching the
  keyboard-first v1 interactions. Deliberately deferred out of v1 to keep
  the initial TUI scope tight.
- **JSON output mode for scripting** — `--json` (or similar) prints the
  scan result (repos, worktrees, sizes, active status) as structured data
  instead of launching the TUI, for use in scripts/CI.
- **Age column + sort by age** — last-commit or last-mtime column on each
  worktree row, plus a new sort mode. Complements the existing name /
  size-desc / active-first cycle.

## Bigger ideas (multi-session)

- **Stale-branch hints** — detect when a worktree's branch has been merged
  into the repo's default branch ("branch merged into default → safe to
  purge") and surface it as an additional hint alongside the existing
  active/orphan indicators. Needs a merge-base check per worktree branch
  against the resolved default branch, cheap per-repo but needs care around
  repos with no clear default branch.
- **`git gc` / prune-objects size reclaim in main repos** — after purging
  worktrees, the *shared* object store in the main repo can still hold
  objects that were only reachable from the now-deleted worktree branches.
  Offer an opt-in follow-up step that runs `git gc --prune=now` (or similar)
  in the owning repo and reports the additional space reclaimed. Needs to
  be clearly separated from the worktree-purge confirmation flow since it
  operates on the main checkout, which v1 never touches.
- **Config file for default scan roots; watch mode** — a config file
  (e.g. `~/.config/worktree-purge/config.toml`) that remembers frequently
  scanned root folders, plus a `--watch` mode that re-scans on filesystem
  changes instead of requiring manual `r` rescans. Two related but
  separable features; config-file support is the smaller first step.

---

## Deferred / parked

- **crates.io registry publish automation in `release.yml`** — `SCOPE.md`
  M5 calls for a v0.1.0 crates.io publish with registry verification.
  `release.yml` currently only builds and publishes GitHub Release binary
  archives; a `publish-crate` job (mirroring markdown-reader's
  `cargo publish --token ${{ secrets.CARGO_REGISTRY_TOKEN }}` pattern) can
  be added once the M5 release checklist is actually being executed.
