# Contributing

Thanks for contributing to `worktree-purge`.

## Development setup

```sh
git clone https://github.com/leboiko/worktree-purge.git
cd worktree-purge
cargo build
```

## Before opening a PR

Run the usual checks:

```sh
cargo fmt --all
cargo clippy --all-targets -- -D warnings
cargo test
```

If your change touches packaging, releases, or dependency policy, also run:

```sh
cargo deny check
```

## What makes a good PR

- Keep the scope tight
- Add or update tests when behavior changes
- Update the README when user-facing behavior changes
- Prefer repo patterns over introducing a new style or abstraction

## Areas where help is especially useful

- Scanner correctness (nested repos, submodules, permission-denied dirs)
- Git status edge cases (dirty/unpushed detection, detached HEAD, no upstream)
- TUI rendering and snapshot tests
- Cross-platform disk-usage accuracy (Linux/macOS `st_blocks` vs Windows)
- Purge safety (confirmation flows, failure handling, `git worktree prune`)

## Reporting bugs

Use the issue templates when possible. For security-sensitive issues, use the
process in [SECURITY.md](SECURITY.md).
