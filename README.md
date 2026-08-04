# worktree-purge

A terminal UI for finding and reclaiming disk space from stale git worktrees.

Point it at a folder. It finds every git repository underneath, groups their
linked worktrees by repo, shows you what each one costs in disk space and
whether it still holds work, and lets you delete the ones you no longer need —
individually or in batches.

```
worktree-purge ~/dev
```

## Why

Linked worktrees are cheap to create and easy to forget. A year of
`git worktree add` leaves a development folder carrying tens of gigabytes of
half-finished branches, and there is no built-in way to see which ones are
still live. `git worktree list` tells you they exist; it does not tell you what
they cost or whether deleting one would throw away work.

## Features

- **Fast, pruning scan.** A parallel filesystem walk that stops descending the
  moment it identifies a repository, so the cost is proportional to the
  directories you *don't* care about, not to the size of your repos.
- **Grouped by repository**, expandable and collapsible, so a folder with
  twenty repos stays readable.
- **Size per worktree**, measured as allocated blocks — the space you actually
  get back, not apparent file length.
- **Activity indicators** that distinguish work that would be *lost* from work
  that is merely *local* (see [Safety](#safety) below).
- **Batch selection** — per worktree, per repository, or everything at once.
- **Progressive loading.** The interface appears immediately after discovery;
  sizes and git status stream in from background workers, so a multi-gigabyte
  tree never blocks the UI.
- **Worktrees outside the scanned folder are still found**, via each
  repository's own worktree metadata, and flagged as such.
- **Orphan detection** for worktree directories whose repository is gone —
  pure disk waste, and invisible to git itself.

## Safety

Deleting things is the whole point of this tool, so it is deliberate about it.

**The main checkout of a repository is never listed and never removable.** Only
linked worktrees are candidates.

**Removal goes through `git worktree remove`**, not a recursive delete, so the
repository's `.git/worktrees/<name>` metadata is cleaned up too. A plain
`rm -rf` leaves the repo referencing a worktree that no longer exists. Orphaned
directories, which by definition have no repository left to maintain, are the
only case that falls back to a direct delete.

**Two kinds of "active" worktree, shown at different severities.** This
distinction matters, and most tools get it wrong:

| Indicator | Meaning | What happens if you purge it |
|---|---|---|
| 🔴 uncommitted changes | Modified, staged, or untracked files | **The work is destroyed.** git itself refuses without `--force`, and so does this tool until you confirm twice. |
| 🟡 unpushed commits | Commits that exist on no remote | The work **survives** — `git worktree remove` deletes the working directory, not branches or commits, which live in the shared object store. The flag means "still in use and not backed up". |

Overstating the danger is how you train people to click through warnings, so
the confirmation dialog tells you which case applies rather than implying
everything is about to be lost.

**Anything not proven clean requires the extra confirmation.** A worktree whose
status check has not finished, or failed, is treated as unsafe. An error can
never downgrade a warning into a green light.

**Stashes never block a purge.** Stashes created inside a worktree live in the
repository's shared stash stack and remain fully recoverable after the worktree
is removed. They are shown as an informational hint (`⚑ 2`) so you know related
work exists, and nothing more.

## Installation

### From crates.io

```sh
cargo install worktree-purge
```

### Prebuilt binaries

Download from the [releases page](https://github.com/leboiko/worktree-purge/releases).
Archives are published for Linux (gnu and musl, x86_64 and aarch64), macOS
(Intel and Apple Silicon) and Windows, with `SHA256SUMS` alongside.

macOS binaries are not code-signed, so Gatekeeper will object on first run —
right-click → Open once to get past it, or build from source.

### From source

```sh
git clone https://github.com/leboiko/worktree-purge
cd worktree-purge
cargo install --path .
```

Requires Rust 1.88 or newer and a `git` executable on `PATH`.

## Usage

```sh
worktree-purge [FOLDER]
```

`FOLDER` defaults to the current directory.

### Keybindings

| Key | Action |
|---|---|
| `↑` `↓` / `j` `k` | Move cursor |
| `←` `→` / `h` `l` | Collapse / expand repository |
| `Enter` | Toggle expansion |
| `Space` | Toggle selection (on a repo header: all its worktrees) |
| `a` | Select or deselect all worktrees in the current repository |
| `A` | Select or deselect everything |
| `d` / `x` | Purge selected (or the worktree under the cursor) |
| `s` | Cycle sort: name → size → active first |
| `r` | Rescan |
| `g` / `G` | Jump to top / bottom |
| `?` | Help overlay |
| `q` / `Esc` | Quit, or close the current dialog |

## How it works

Discovery uses two sources, because neither is sufficient alone. The
filesystem walk finds worktrees whose repository has been deleted — invisible
to git. Each repository's `git worktree list` finds worktrees that live outside
the folder you scanned — invisible to the walk. Results are merged and
deduplicated by canonical path.

Submodule checkouts also have a `.git` file, and are explicitly excluded: a
worktree's private directory contains `gitdir` and `commondir` marker files,
and a submodule's module directory does not. Path shape is not a reliable
discriminator, because git writes submodule pointers as relative paths and
worktree pointers as absolute ones.

`docs/notes-scanner.md` records the empirically verified git behaviour behind
these rules.

## Contributing

Issues and pull requests are welcome — see [CONTRIBUTING.md](CONTRIBUTING.md).
Planned work lives in [ROADMAP.md](ROADMAP.md).

## License

MIT — see [LICENSE](LICENSE).
