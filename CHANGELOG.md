# Changelog

All notable changes to `worktree-purge` are documented here.

The format is based on [Keep a Changelog](https://keepachangelog.com/en/1.1.0/),
and this project adheres to [Semantic Versioning](https://semver.org/spec/v2.0.0.html).

## [Unreleased]

### Added

- **Worktree discovery.** Parallel filesystem walk that prunes as soon as a
  directory is identified as a repository, merged with each repository's own
  `git worktree list` so worktrees living outside the scanned folder are found
  too. Orphaned worktrees — directories whose owning repository is gone, and
  which git itself can no longer see — are detected from the filesystem alone.
  Submodule checkouts are explicitly excluded, and the main checkout of a
  repository is never listed or removable.
- **Background enrichment.** Disk size (allocated blocks), activity status and
  stash counts stream in from a cancellable worker pool after the interface is
  already on screen, so a multi-gigabyte tree never blocks the UI.
- **Activity detection** distinguishing uncommitted changes from unpushed
  commits, with a fallback chain covering tracked branches, branches with no
  upstream, repositories with no remotes at all, and detached HEAD.
- **Tree interface** with collapsible repository groups, per-worktree,
  per-repository and global selection, three sort modes, a help overlay, and a
  status bar showing totals and the selected reclaim.
- **Two-stage confirmation** for removal. Any batch containing a worktree that
  is dirty, locked, or whose status could not be determined requires a second,
  deliberately different keypress (`F`) before anything is deleted.
- **Purge engine** built on `git worktree remove` plus `git worktree prune`,
  with a recursive-delete fallback for orphans guarded against dangerous paths.
  Removals are verified against the filesystem rather than trusted from an exit
  code, and one failure never aborts the rest of a batch.
- Cross-platform CI (Linux, macOS, Windows) on stable and the 1.88 MSRV, plus a
  tag-driven release pipeline that fails loudly rather than publishing a
  release with missing artifacts, and a crates.io publish job that verifies the
  version is actually live on the registry afterwards.

### Fixed

- **Worktree removal on Windows.** Paths were passed to git in
  `canonicalize`'s verbatim form (`\\?\C:\...`), which git on Windows cannot
  parse — every removal would have failed with exit 128. See
  `docs/notes-windows.md`.

[Unreleased]: https://github.com/leboiko/worktree-purge/commits/master
