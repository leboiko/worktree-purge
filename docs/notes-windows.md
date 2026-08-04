# Windows notes

Windows is a fully supported, CI-tested tier. Everything below was found by
running the suite on `windows-latest`, not reasoned about in advance — the
first three CI runs after the initial push were all Windows failures, and one
of them was a genuine product bug rather than a test artefact.

## The product bug: verbatim paths break git

`Path::canonicalize` on Windows returns a **verbatim** path:

```
\\?\C:\Users\you\dev\wt-feature
```

Git on Windows is MSYS-based and **cannot parse that form**. Handing it to
`git worktree add` or `git worktree remove` fails with exit 128.

Every path this tool gives to git flows through `scan::repo::normalize`, which
originally returned `canonicalize`'s output unchanged. The consequence was not
cosmetic: **worktree removal would have failed for every Windows user**, which
is the entire point of the program.

`normalize` now strips the prefix via `strip_verbatim`. Two deliberate limits:

- Only **drive** paths (`\\?\C:\...`) are simplified. `\\?\UNC\server\share`
  keeps its prefix, because removing it there names a different path.
- Stripping gives up the verbatim form's exemption from the legacy
  260-character `MAX_PATH` limit. That is the right trade: a path git cannot
  parse is useless to us at any length, and modern Windows supports long paths
  natively.

`normalize_never_emits_a_verbatim_prefix` asserts this against a real
canonicalised temp directory rather than a string literal, so it fails if
`normalize` ever forwards `canonicalize` unchanged again.

**Fixing `normalize` alone was not enough.** Three other call sites resolved
paths independently — `resolve_gitdir_pointer`, the CLI root in `main.rs`, and
the fixture's `git_common_dir` — and each reintroduced the prefix. The lesson
is that a single raw `Path::canonicalize` anywhere is enough to break Windows,
so there is now one wrapper, `scan::repo::canonicalize`, and **nothing in
`src/` should call `Path::canonicalize` directly.** If you add a call, use the
wrapper.

## Test-only issues, and why each was more than a typo

**`/dev/null` is not a path on Windows.** The fixtures neutralised the
developer's global git config with `GIT_CONFIG_GLOBAL=/dev/null`. Replaced
with a path that simply does not exist (git treats an unreadable config as
empty) plus `GIT_CONFIG_NOSYSTEM=1`, which needs no path at all.

**Bare local paths as git remotes are parsed as scp URLs.** `git remote add
origin C:\path\to\origin.git` makes git read `C:` as a *hostname*, and it
fails with `hostname contains invalid characters`. Local remotes in fixtures
are now `file:///C:/path/to/origin.git` URLs.

**`/Users` is not absolute on Windows.** The purge engine's dangerous-path
guard tests hard-coded Unix paths. The guard itself was already correct — it
refused them via the relative-path rule, which is the fail-safe direction —
but that meant the *depth* and *home-directory* rules were never exercised on
Windows at all. Sample paths are now per-platform, and a test pins the
fail-safe behaviour explicitly.

**A conditional test can pass while testing nothing.** The guard's home
directory assertion read `HOME`, which is frequently unset on Windows, inside
an `if let`. It therefore passed silently without asserting anything. It now
reads the same variable the guard itself uses (`USERPROFILE` on Windows).

**`/usr` does not exist on Windows.** The "the filesystem survived" assertion
now picks the platform's system directory.

**Separators leak into string assertions.** A test checking that a warning
"names the path" searched for the literal `no/such/directory`, but the message
renders with the platform separator, so it could never match on Windows. Build
the expected fragment with `Path::join` instead of writing it as a literal.
Note that `Path`/`PathBuf` *equality* is not affected — Windows treats `/` and
`\` as equivalent when comparing components — so only string-level assertions
need this care.

**Symlink creation needs a privilege.** `std::os::windows::fs::symlink_dir`
requires `SeCreateSymbolicLinkPrivilege` (admin, or Developer Mode). Where it
is unavailable the scenario cannot be staged at all, so that test skips with a
message instead of reporting a guard failure for an environmental reason. The
Unix path stays strict, so the rule is never entirely unverified.

## CI lesson: `--no-fail-fast`

`cargo test` stops at the first failing test **binary**. With the lib tests
failing, `tests/purge.rs`, `tests/scan.rs` and `tests/enrich.rs` never ran on
Windows — their Unix assumptions were invisible rather than passing. That
turned one platform port into one fix per CI round-trip.

`ci.yml` now runs `cargo test --no-fail-fast` so a single failure cannot hide
the rest. Worth keeping in mind when adding a platform: the first red run
shows you only the first *binary* that broke.

## Things that were fine

- `filesize::PathExt::size_on_disk` works on Windows, so no `st_blocks` /
  `GetCompressedFileSize` split was needed — the original scope doc expected
  to need one.
- The TUI snapshot tests pass unchanged; ratatui rendering is
  platform-independent.
- All 154 unit tests and the UI suite passed on Windows before any of the
  fixes above, so the core model and rendering were never in question.
