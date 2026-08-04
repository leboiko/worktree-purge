# CI / release notes

Working notes on what was ported from
[`markdown-reader`](https://github.com/leboiko/markdown-reader), what was
adapted, and what didn't translate. Written before the workflows themselves
so the reasoning is on record, not reconstructed after the fact.

## Facts read from this repo before writing anything

- `Cargo.toml`: `rust-version = "1.88"` → this is the MSRV floor for the test
  matrix. `edition = "2024"`. Single binary crate (no workspace), binary name
  `worktree-purge`, `license = "MIT"`, `repository =
  "https://github.com/leboiko/worktree-purge"`.
- `[profile.release]` already sets `strip = true` (plus `lto = true`,
  `codegen-units = 1`). markdown-reader's `Cargo.toml` has **no**
  `[profile.release]` section at all, which is why its `release.yml` has a
  manual "Strip binary" step per platform. worktree-purge doesn't need
  that step — cargo strips at build time. Carrying the manual strip steps
  over would have been redundant, and for `aarch64-unknown-linux-musl`
  specifically there is no standard `apt`-installable
  `aarch64-linux-musl-strip` on `ubuntu-latest`, so a manual strip step would
  have been a real headache for that target. Dropped, not ported.
- `tests/common/mod.rs` (the fixture builder used by integration tests)
  already runs every git invocation with `GIT_CONFIG_GLOBAL=/dev/null`,
  `GIT_CONFIG_SYSTEM=/dev/null`, explicit `GIT_AUTHOR_NAME` /
  `GIT_AUTHOR_EMAIL` / `GIT_COMMITTER_NAME` / `GIT_COMMITTER_EMAIL` env vars,
  and `-c init.defaultBranch=main` on `git init`. So the fixtures are already
  hermetic and don't strictly depend on the runner's global git config.
  Even so, CI now also sets a global `user.name` / `user.email` /
  `init.defaultBranch` before the test job runs, as a second line of
  defense — any test added later (by another agent, in parallel) that
  shells out to `git init`/`git commit` without going through the shared
  fixture helper would otherwise hit `fatal: unable to auto-detect email
  address` on a fresh, freshly-provisioned runner with no git identity
  configured. Cheap insurance, matches the task's explicit ask.
- `src/git.rs` (the app's own git wrapper, as opposed to the test fixture
  helper) never commits anything — it only reads (`status`, `rev-list`,
  `worktree list`, `stash list`) or removes (`worktree remove`, `worktree
  prune`). No identity needed there.

## CI workflow (`ci.yml`)

Ported: the job shape (fmt / clippy / test / deny), `concurrency` block,
`env` block (`CARGO_TERM_COLOR`, `CARGO_INCREMENTAL=0`, `RUST_BACKTRACE=1`),
action versions (`actions/checkout@v7`, `dtolnay/rust-toolchain@stable`,
`Swatinem/rust-cache@v2`, `EmbarkStudios/cargo-deny-action@v2`), and the
`fail-fast: false` matrix convention.

Adapted:
- **Windows added to both the `clippy` and `test` matrices.** The reference
  only runs clippy/tests on ubuntu + macos; Windows is a deliberate
  first-class tier for worktree-purge (see `SCOPE.md` §"Resolved
  questions"), so `windows-latest` joins both matrices.
- **MSRV bumped to `1.88`** (read from this repo's `Cargo.toml`, not
  assumed/copied from markdown-reader's `1.92`).
- **Git identity + default branch configured before the `test` job runs**
  (see above) — the reference project doesn't need this because its tests
  don't create git fixtures that commit.
- **`cargo test`** rather than `cargo test --workspace` — worktree-purge is a
  single-package crate, not a Cargo workspace (markdown-reader is, with
  `mermaid-text` as a second member), so `--workspace` would be a no-op
  affordance carried over from a context that doesn't apply here.

Deliberately **not** ported:
- **`doc` job** (docs build with `continue-on-error: true`). The task's job
  list for `ci.yml` is exactly rustfmt / clippy / tests / `cargo deny
  check`; adding an unrequested job is scope creep for a from-scratch repo
  with no existing doc-warning debt to manage. Easy to add later if wanted.
- **`audit` job** (separate `cargo-audit` run, `continue-on-error: true`).
  The `deny` job's `command: check bans licenses sources advisories`
  already runs the `advisories` check via `cargo-deny`, which covers the
  same RUSTSEC-advisory signal `cargo-audit` would provide. Running both
  tools would be duplicate signal for a crate with a much smaller
  dependency tree than markdown-reader's (no syntect/resvg/mermaid stack).
  Follow-up: if `cargo-deny`'s advisory data ever proves less current than
  `cargo-audit`'s, add the job back.

## Release workflow (`release.yml`)

Ported: the trigger shape (`v*.*.*` tag push + `workflow_dispatch` for
back-fills), the `RESOLVED_TAG` env-level tag resolution (this is the exact
mechanism that fixes the "green run, no artifacts" bug documented in the
reference's header — `github.ref_name` is the tag under a tag-push trigger
but the *branch name* under `workflow_dispatch`, so every job/step that
needs the tag reads `env.RESOLVED_TAG` instead of `github.ref_name`
directly), the per-platform build job shape (`build-linux` via `cross`,
`build-macos` native, `build-windows` native + `Compress-Archive`), the
`SHA256SUMS` + unversioned-alias generation, the changelog-section
extraction via `awk`, and `softprops/action-gh-release@v3` for publishing.
Action versions matched: `actions/checkout@v7`, `actions/upload-artifact@v7`,
`actions/download-artifact@v8`, `dtolnay/rust-toolchain@stable`,
`Swatinem/rust-cache@v2`, `softprops/action-gh-release@v3`.

Adapted:
- **`aarch64-unknown-linux-musl` added** to the Linux build matrix. The task
  spec calls for gnu + musl on both x86_64 and aarch64 (the reference only
  builds musl for x86_64). `cross` has an image for this target, so it's a
  matrix-entry addition, no new mechanism.
- **No manual "Strip binary" step** — see the `[profile.release] strip =
  true` note above. Cargo already strips at build time for every target,
  including the added `aarch64-unknown-linux-musl` one that would otherwise
  have needed a strip tool nobody has installed by default.
- **No `--package <crate>` flag** on `cargo build`/`cross build` — the
  reference passes `--package markdown-tui-explorer` because its repo is a
  workspace with two publishable members. worktree-purge is a single crate,
  so a plain `cargo build --release --target <target>` builds the right
  (only) binary.
- **Loud-failure guard against the reference's documented near-miss.** The
  reference's header comment describes a `Create Release` step that once
  silently produced a green run with zero artifacts because of the
  `github.ref_name` vs `workflow_dispatch` tag mismatch. That specific bug
  is structurally prevented by `RESOLVED_TAG` (ported as-is), but this
  workflow adds two extra hard-fail checks the reference doesn't have, so a
  *different* zero-artifact failure mode (a build job silently producing 0
  or a partial set of archives, e.g. from a `cross` image outage on one
  target) can't slip through as a green run either:
  1. **`Verify artifacts downloaded`** in the `release` job, right after
     `actions/download-artifact`, counts `*.tar.gz` + `*.zip` files and
     fails the job with `::error::` if the count is below the expected 7
     (4 Linux + 2 macOS + 1 Windows archive).
  2. **`Verify GitHub Release has assets`**, right after
     `softprops/action-gh-release`, calls `gh release view --json assets`
     on the tag just published and fails if the asset count is zero. This
     catches the exact "release created, but nothing attached" shape of
     the original bug even if some future refactor reintroduces it via a
     different code path.

Deliberately **not** ported:
- **`publish-crate`, `publish-homebrew`, `publish-aur` jobs.** None of these
  are in the task's requirements for `release.yml`, and none of the
  supporting infrastructure they depend on exists in this repo yet
  (`packaging/homebrew/*.tmpl`, `packaging/aur/*.tmpl`,
  `scripts/render-homebrew-formula.sh`, `scripts/render-aur-pkgbuild.sh` are
  all reference-project-only files, and I own none of them here). `SCOPE.md`
  M5 does list a crates.io publish as a v0.1.0 goal — **follow-up:** once
  the registry-verification step in M5 is actually being executed by
  whoever runs the release, a `publish-crate` job can be added the same way
  the reference does it (`cargo publish --token
  ${{ secrets.CARGO_REGISTRY_TOKEN }}`, gated on the tag push). Not adding
  it now rather than half-wiring a job with no target repo/tap to publish
  to.
- **Apple notarization TODO block mechanics** — the explanatory text is
  kept (see below) but there's nothing else workflow-specific to adapt;
  it's already inert until secrets are added.

## `deny.toml`

Ported as-is: `[graph]` target list (with `x86_64-pc-windows-msvc` added —
worktree-purge ships Windows binaries, the reference's `deny.toml` predates
adding Windows or simply never needed it there), `[advisories]` structure
(`yanked = "deny"`, `unmaintained = "all"`), the full `[licenses] allow`
list, `confidence-threshold`, `[bans]` structure (`multiple-versions =
"warn"`, `wildcards = "deny"`, the `openssl`/`native-tls` bans), and the
`[sources]` registry lock-down.

Adapted:
- **`ignore = []` under `[advisories]`** instead of the reference's five
  entries — those are all specific to syntect/resvg/mermaid-rs-renderer/
  ureq, none of which worktree-purge depends on
  (`ratatui`, `crossterm`, `clap`, `anyhow`, `ignore`, `rayon`, `filesize`,
  `insta`, `tempfile` pull a much shallower, more current dependency tree
  with no known outstanding RUSTSEC advisories at the time of writing).
  Left the list present-but-empty (rather than removing the section) so
  the next advisory that needs a targeted ignore has an obvious place to
  land, with the same "must cite the upstream blocker" convention noted in
  a comment.
- **Bans `openssl`/`native-tls` kept** even though nothing in
  worktree-purge's dependency graph currently pulls TLS at all — it's a
  no-cost guard against a future dependency (e.g. an HTTP client, if a
  crates.io version-check feature like markdown-reader's is ever added)
  accidentally dragging in OpenSSL instead of rustls.
- **No `skip`/`skip-tree` entries added** — `cargo deny check` was not run
  against a real lockfile as part of this task (the crate's dependency set
  was read from `Cargo.toml`, not resolved), so there's no known
  multiple-version conflict to pre-emptively skip. If CI's first real run
  surfaces one, add it there with a comment explaining why.

## Community files (`LICENSE`, `CONTRIBUTING.md`, `CODE_OF_CONDUCT.md`,
`SECURITY.md`, `.github/*`)

Ported near-verbatim with `markdown-reader` → `worktree-purge` (and
`markdown-tui-explorer` → `worktree-purge`, since there's no separate
library crate here) substitutions:

- `LICENSE` — MIT, same copyright holder (`leboiko`), year bumped to 2026 to
  match the repo's actual creation date (already 2026 in the reference too,
  no change needed there).
- `CODE_OF_CONDUCT.md` — Contributor Covenant 2.1, ported verbatim (no
  project-specific content in the reference's copy to adapt).
- `SECURITY.md` — same private-disclosure-via-GitHub-Security-Advisories
  policy, URL repointed to `leboiko/worktree-purge`.
- `CONTRIBUTING.md` — dropped the Nix-shell mention (`nix develop`) since
  there's no `flake.nix` in this repo, and the mermaid-specific reporting
  guidance in "Areas where help is especially useful". Replaced with
  worktree-purge-relevant areas (scanner correctness, git-status edge
  cases, TUI rendering/snapshot tests, cross-platform disk-usage, purge
  safety). Kept the same `cargo fmt` / `clippy -D warnings` / `cargo test`
  / `cargo deny check` check sequence.
- `.github/ISSUE_TEMPLATE/bug_report.md` — reshaped the "Environment" table
  for a single-binary CLI/TUI tool (dropped the `mermaid-text`
  version/terminal-image-protocol rows that don't apply; kept OS, terminal,
  Rust version, worktree-purge version, install method) and reworded
  "Steps to Reproduce" around scanning/purging worktrees instead of
  opening markdown files.
- `.github/ISSUE_TEMPLATE/feature_request.md` — dropped the two-component
  "Scope" checklist (markdown-reader/mermaid-text) since there's only one
  component here.
- `.github/ISSUE_TEMPLATE/config.yml` — ported verbatim
  (`blank_issues_enabled: false`).
- `.github/pull_request_template.md` — dropped the Mermaid-rendering
  checklist line and the "root vs crate-local CHANGELOG" distinction (only
  one CHANGELOG here); kept the same shape otherwise (Summary / Changes /
  Test Plan / Checklist / Linked Issue).
- `.github/dependabot.yml` — ported verbatim (weekly Monday cadence, cargo +
  github-actions ecosystems, patch/minor grouping, 10 open-PR cap).

## `CHANGELOG.md` / `ROADMAP.md`

- `CHANGELOG.md` — Keep a Changelog format + SemVer note, ported header
  verbatim, `## [Unreleased]` section left with empty `### Added` /
  `### Changed` / `### Fixed` headers (no shipped entries invented — the
  actual feature code is still being written by other agents in parallel).
- `ROADMAP.md` — seeded directly from `SCOPE.md` §5 "v2 ideas" (mouse
  support, `--dry-run`, JSON output, stale-branch hints, age column +
  sort, config file/watch mode, `git gc`/prune-objects reclaim), following
  the reference's roadmap-holds-in-progress / changelog-holds-history
  convention verbatim.

## Things that plain didn't translate

- Nothing Mermaid/markdown-specific (`mermaid-text` workspace member,
  Nix flake, AUR/Homebrew packaging scripts, `crop_logo.swift` /
  `generate_social_preview.swift`, blog-asset docs) — none of it is
  relevant to a worktree-cleanup TUI and none of it is in this task's file
  ownership list.
- The reference's `nix.yml` and `release-mermaid-text.yml` workflows are
  both specific to markdown-reader's Nix packaging and its second
  publishable crate; worktree-purge has neither, so nothing to port.
