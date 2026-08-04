---
name: Bug report
about: Report something that isn't working correctly
title: "[Bug] "
labels: bug
assignees: ""
---

## Description

A clear and concise description of what the bug is.

## Steps to Reproduce

1. Run `worktree-purge <folder>` with ...
2. Select/expand ...
3. Observe ...

If the bug is in scan results (missing/misclassified repos or worktrees) or
active-status detection, include the output of `git worktree list --porcelain`
and `git status --porcelain=v2 --branch` run inside the affected worktree.

## Expected Behavior

What you expected to happen.

## Actual Behavior

What actually happened. Include any error messages, stack traces, or
screenshots. For TUI display issues, screenshots help a lot.

## Environment

| Field | Value |
|---|---|
| OS | e.g. macOS 15.2 / Ubuntu 24.04 / Windows 11 |
| Terminal | e.g. iTerm2, Ghostty, kitty, tmux, Windows Terminal |
| Rust version | `rustc --version` |
| worktree-purge version | `worktree-purge --version` |
| Installation method | `cargo install` / binary release / built from source |

## Additional Context

Any other context about the problem, e.g. repository size, number of
worktrees, whether the affected worktree is orphaned or outside the scan
root.
