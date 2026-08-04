# worktree-purge — Project Scope & Progress Tracker

A Rust TUI app (ratatui) that scans a folder for git worktrees, groups them by
parent repository, and lets the user reclaim disk space by purging worktrees
individually or in batch — with a clear visual warning for "active" worktrees
that still hold unpushed/uncommitted work.

**Status legend:** `[ ]` not started · `[~]` in progress · `[x]` done

This file tracks scope and progress. Shipped work moves to `CHANGELOG.md`
(same convention as [markdown-reader](https://github.com/leboiko/markdown-reader)).

---

## 1. Product definition

### What it does

```
worktree-purge <folder>
```

1. Recursively scans `<folder>` for git repositories and their linked worktrees.
2. Shows a tree-style TUI: one collapsible group per repository, worktrees as
   children.
3. Each worktree row shows: **name**, **size on disk**, **active indicator**.
4. User selects worktrees (individually, per-repo "select all", or global
   "select all") and purges them after a confirmation step.

### What it deliberately does NOT do (v1)

- Never touches the **main** checkout of a repo — only *linked* worktrees are
  listed and purgeable.
- No branch deletion, no `git gc`, no remote operations.
- No config file / persistence — it's a run-and-done utility.
- No mouse support in v1 (keyboard-first; mouse is a nice-to-have).

### Definitions

| Term | Meaning |
|------|---------|
| **Repository** | A directory with a `.git` *directory* (the main checkout) or a bare repo that owns worktrees. |
| **Worktree** | A *linked* worktree: directory whose `.git` is a **file** pointing at `<repo>/.git/worktrees/<name>`. |
| **Active** | The worktree holds work that is not safely elsewhere. Two signals, **different severities** (see below): uncommitted changes (staged, unstaged or untracked) **or** commits that exist on no remote. |
| **Orphaned** | A worktree directory whose back-pointer targets a repo/metadata that no longer exists (still purgeable — it's pure disk waste). |
| **Size** | Recursive disk usage (allocated blocks, `du`-style) — this is the space the user actually gets back. |

#### The two "active" signals are not equally dangerous

`git worktree remove` deletes the working directory and the
`.git/worktrees/<name>` metadata. It does **not** delete branches or commits —
those live in the repository's shared object store and survive removal. So:

- **Dirty (uncommitted changes)** → the work is *destroyed* by purging. git
  itself refuses to remove a dirty worktree without `--force`, and so do we.
- **Unpushed commits** → the work *survives* the purge (the branch ref and its
  commits remain in the owning repo); what the flag really tells you is "this
  worktree is still in use and its work isn't backed up to a remote."

Both mark a worktree active — showing unpushed work was an explicit product
requirement — but the UI renders them at different severities (dirty red,
unpushed yellow) and the confirmation modal states which case applies rather
than implying everything is about to be lost. Overstating the danger trains
users to click through the warning, which is exactly what the second
confirmation gate exists to prevent.

---

## 2. Key design decisions

- **Git interaction: shell out to the `git` CLI**, not `git2`/`gix`.
  Rationale: if worktrees exist, git is installed; `git worktree remove` +
  `git worktree prune` do the correct metadata cleanup that a raw
  `rm -rf` would leave dangling; status checks (`git status --porcelain=v2
  --branch`, `git rev-list --count @{u}..HEAD`) are one subprocess each and
  trivially correct. Revisit `gix` only if subprocess overhead shows up on
  huge scans.
- **Scan strategy: parallel directory walk, prune on hit.** Walk with
  `ignore::WalkParallel` (or `jwalk`); when a `.git` entry is found, classify
  (dir → repo, file → worktree), record it, and **do not descend further**
  into that tree. This keeps the scan O(non-repo dirs) instead of touching
  every file. Worktrees are then grouped by resolving their `gitdir:`
  back-pointer; additionally `git worktree list --porcelain` runs per
  discovered repo to catch worktrees living *outside* the scanned folder
  (listed but marked "outside scan root").
- **Progressive loading.** The TUI opens immediately after discovery; sizes
  and active-status are computed by a background worker pool (rayon or
  std threads + channel) and stream into the UI (spinner → value). No frozen
  UI on multi-GB trees.
- **Purge = `git worktree remove <path>`** from the owning repo, with
  `--force` only when the user explicitly confirms an active/dirty worktree;
  followed by `git worktree prune`. Orphans (no owning repo) fall back to a
  plain recursive delete after the same confirmation. Every purge reports
  per-item success/failure in the UI; the list refreshes after a batch.
- **Deleting is destructive → double confirmation for active worktrees.**
  Batch confirmation modal lists every doomed worktree with its size and a
  summed "space reclaimed"; active ones are highlighted and require a
  separate explicit "yes, I understand" step (e.g. typed `force`or a second
  distinct keypress) so muscle-memory can't nuke unpushed work.

### Resolved questions (decided 2026-08-04)

- [x] **crates.io: publish at v0.1.0.** Name `worktree-purge` confirmed
      available (sparse-index 404 as of 2026-08-04; note `worktree-gc` is
      taken — prior art in the space). Registry verification is part of the
      M5 release checklist.
- [x] **Stashes: shown as info, do NOT mark active.** Verified empirically:
      a stash created inside a worktree lands in the shared `refs/stash`
      stack and remains fully recoverable after `git worktree remove` — no
      data loss, so it must not block purge. Instead, rows show a stash-count
      hint (e.g. `2 stashes`) mapped by branch name from one
      `git stash list` per repo (cheap: one subprocess per repo, not per
      worktree).
- [x] **Windows: full tier — binaries shipped AND CI-tested.**
      `windows-latest` joins the CI test matrix; the release workflow ships
      the `.zip` like markdown-reader. Resolved in implementation: disk usage
      goes through the `filesize` crate, which provides the allocated-size
      call on every platform, so no `st_blocks`/`GetCompressedFileSize` split
      was needed. The first CI run caught two genuine Windows issues —
      fixtures pointing git config at `/dev/null`, and guard tests hard-coding
      Unix paths (`/Users` is not absolute on Windows) — both now fixed.

---

## 3. Architecture sketch

```
src/
  main.rs          — clap CLI, terminal setup/teardown, event loop
  scan/
    walker.rs      — parallel FS walk, .git classification, prune-on-hit
    repo.rs        — repo/worktree model, gitdir back-pointer resolution
    status.rs      — active detection (dirty / unpushed) via git subprocess
    size.rs        — recursive disk-usage worker
  app/
    state.rs       — AppState: tree of RepoGroup { worktrees, expanded },
                     selection set, sort order, in-flight jobs
    events.rs      — key handling → actions
    purge.rs       — purge planner + executor, result reporting
  ui/
    tree.rs        — repo/worktree tree rendering, indicators, checkboxes
    modal.rs       — confirmation + force-confirmation + results modals
    statusbar.rs   — totals (repos, worktrees, total size, selected size)
    help.rs        — `?` overlay
```

### Data model (core types)

```rust
struct RepoGroup { root: PathBuf, name: String, worktrees: Vec<Worktree>, expanded: bool }
struct Worktree {
    path: PathBuf,
    name: String,               // directory name / worktree name
    branch: Option<String>,
    size: SizeState,            // Pending | Computing | Ready(u64)
    activity: ActivityState,    // Pending | Clean | Active(ActiveReason) — reasons: Dirty, Unpushed, Both
    orphaned: bool,
    outside_scan_root: bool,
    selected: bool,
}
```

### Keybindings (v1)

| Key | Action |
|-----|--------|
| `↑/↓` `j/k` | Move cursor (skips collapsed children) |
| `←/→` `h/l` / `Enter` | Collapse / expand repo group |
| `Space` | Toggle selection on worktree (on a repo header: toggle all its worktrees) |
| `a` | Select/deselect all in current repo |
| `A` | Select/deselect all everywhere |
| `d` / `x` | Purge selected (opens confirmation modal) |
| `s` | Cycle sort: name / size desc / active-first |
| `r` | Rescan |
| `?` | Help overlay |
| `q` / `Esc` | Quit / close modal |

### Visual indicators

- `●` yellow — active (unpushed commits), `●` red — active (dirty), tooltip
  reason in the row detail; clean worktrees get a dim `○`.
- Orphaned worktrees tagged `[orphan]`.
- Stash hint (info only, never blocks purge): dim `⚑ 2 stashes` on rows whose
  branch has entries in the repo's stash stack.
- Selected rows: checkbox `[x]` + highlighted style.
- Repo header shows aggregate: `repo-name  (4 worktrees · 1.2 GB · 2 active)`.

---

## 4. Milestones

### M0 — Scaffolding & repo hygiene
- [x] `cargo init` (edition 2024), binary name `worktree-purge`
- [x] Dependencies (latest at implementation time — verify with `cargo add`):
      `ratatui`, `crossterm`, `clap` (derive), `anyhow`, `ignore`, `rayon`;
      dev: `insta`, `tempfile`
- [x] `git init` + first commit; create GitHub repo `leboiko/worktree-purge`
      (public, MIT) mirroring markdown-reader's setup
- [x] Community files: `LICENSE` (MIT), `README.md` (stub), `CHANGELOG.md`,
      `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`, `SECURITY.md`,
      `.github/ISSUE_TEMPLATE/*`, PR template, `dependabot.yml`
- [x] CI workflow (port from markdown-reader): rustfmt, clippy `-D warnings`
      (ubuntu + macos + windows), tests (stable + MSRV floor, all three
      OSes), `cargo deny check` (`deny.toml`)

### M1 — Scanner
- [x] Parallel walk with prune-on-hit; classify `.git` dir vs file
- [x] Resolve worktree → owning repo via `gitdir:` back-pointer
- [x] Per-repo `git worktree list --porcelain` merge (catches out-of-root
      worktrees; authoritative branch names)
- [x] Handle: bare repos, orphaned worktrees, nested repos/submodules
      (submodule checkouts are NOT listed as worktrees), scan root itself
      being a repo, permission-denied dirs (skip + count, don't crash)
- [x] Integration tests against fixture repos built in `tempfile` dirs
      (real `git` invocations): multi-repo, orphan, out-of-root, bare,
      submodule-containing — not just the happy path

### M2 — Status & size enrichment
- [x] Background worker pool; results streamed to UI via channel
- [x] Active detection: dirty (`git status --porcelain=v2` incl. untracked),
      unpushed (`rev-list @{u}..HEAD`; no-upstream → check unique commits vs
      all remote refs)
- [x] Disk-usage computation (allocated blocks on unix via `st_blocks`;
      Windows path via allocated size with file-len fallback; linked
      worktrees share the object store, so size = working-tree files +
      private metadata only)
- [x] Stash-count hint: one `git stash list` per repo, entries mapped to
      worktrees by branch name (info only — never blocks purge)
- [x] Tests: each active-reason permutation, size correctness vs `du -sk`
      tolerance on a fixture tree

### M3 — TUI
- [x] Tree view with expand/collapse per repo; cursor navigation
- [x] Selection: per-item, per-repo, global; selection survives
      expand/collapse and rescan-by-path where possible
- [x] Indicators (active/orphan) + stash-count hint on rows, progressive
      spinners for pending size/status
- [x] Status bar totals + selected-size live sum; sort cycling
- [x] Help overlay
- [x] Snapshot tests with `insta` + ratatui `TestBackend` — assert exact
      row content/positions (per testing rules: no assertion a no-op
      implementation could satisfy)

### M4 — Purge
- [x] Purge planner: selected worktrees → per-repo `git worktree remove`
      batches + orphan deletions
- [x] Confirmation modal (list, sizes, total reclaimed); separate hard
      confirmation for active worktrees (`--force` path)
- [x] Execution off the UI thread with per-item progress + result modal
      (successes, failures with stderr excerpt)
- [x] `git worktree prune` per touched repo; list refresh; freed-space toast
- [x] Tests: purge clean, purge active w/ force, purge orphan, purge failure
      (locked worktree / permission denied) — assert the worktree is
      *actually gone* from both disk and `git worktree list`

### M5 — Polish & release
- [x] README: features, install (cargo install + prebuilt binaries), usage,
      keybindings table
- [ ] Screenshots/GIF (VHS or asciinema) — README currently has no imagery
- [x] Release workflow (port from markdown-reader): tag-driven, per-platform
      archives (linux gnu/musl x86_64+aarch64, macOS x86_64+aarch64,
      windows), SHA256SUMS, with hard-fail guards on a zero/partial artifact set
- [x] Public GitHub repo created and pushed; CI green across Linux/macOS/Windows
- [ ] `v0.1.0` tag → **verify artifacts actually landed** on the Releases
      page (not just "workflow green")
- [ ] crates.io publish (name confirmed available 2026-08-04) + verify the
      listed version actually appears on the registry.
      **Note:** `release.yml` has no `publish-crate` job yet — either add one
      (see ROADMAP) or publish manually.
- [x] ROADMAP.md seeded with v2 ideas (below)

---

## 5. v2 ideas (out of scope for v1 — do not build yet)

- Mouse support; `--dry-run`; JSON output mode for scripting
- Stale-branch hints ("branch merged into default → safe to purge")
- Age column (last commit / last mtime) + sort by age
- Config file for default scan roots; watch mode
- `git gc`/prune-objects size reclaim in main repos

---

## 6. Reference

- Conventions source: the author's [markdown-reader](https://github.com/leboiko/markdown-reader)
  (CI matrix, release pipeline, community files, ROADMAP/CHANGELOG discipline)
- git worktree docs: https://git-scm.com/docs/git-worktree
  (`list --porcelain`, `remove`, `prune`, lock semantics)
