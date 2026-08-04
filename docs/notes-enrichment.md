# Enrichment notes (M2 — size, activity, stash)

Implementation notes for `src/scan/{size,status,stash,enrich}.rs`: the exact git
commands each signal uses, why the fallback chain looks the way it does, and the
severity distinction between *dirty* and *unpushed*.

---

## 1. Severity: dirty vs unpushed vs stashed

`git worktree remove` deletes **the working directory and the worktree's private
metadata**. It does *not* delete branches, commits, reflogs or stashes — those
live in the repository's shared object store / ref store (`<repo>/.git`), which
every linked worktree shares. This drives three different severities:

| Signal | Survives `git worktree remove`? | Severity | Marks active |
|---|---|---|---|
| **Dirty** (modified / staged / untracked) | **No — the work is gone** | destructive | yes |
| **Unpushed** (local-only commits) | Yes — commits stay in the object store, reachable from the branch | in-use signal | yes |
| **Stashed** | Yes — `refs/stash` is a shared (non-per-worktree) ref | informational | **never** |

- **Dirty is real data loss.** Nothing outside the working directory holds those
  bytes. This is the red indicator.
- **Unpushed is not data loss** — the branch and its commits survive removal and
  can be checked out again anywhere. What it *does* mean is "this worktree is
  clearly still in use, and its work exists in exactly one place (this machine)".
  The user explicitly asked to be warned about it, so it marks the worktree
  active, but at a lower severity (yellow) than dirty. The UI milestone renders
  the two differently; `ActiveReason::{Dirty, Unpushed, Both}` carries the
  distinction, and `ActiveReason::label()` spells it out in the confirmation
  modal.
- **Stashes never block a purge.** `refs/stash` is not in git's per-worktree ref
  set, so a stash created inside a linked worktree lands in the shared stack and
  is fully recoverable after removal (verified empirically before the project
  started — see SCOPE.md "Resolved questions"). `stash_count` is a hint only; it
  is deliberately *not* an input to `ActivityState`.

Corollary for the purge flow: `--force` is driven by `ActivityState::needs_force`
(anything not proven `Clean`), never by `stash_count`.

---

## 2. Commands used

All git invocations go through `crate::git`, which pins `LC_ALL=C`,
`GIT_TERMINAL_PROMPT=0`, `GIT_PAGER=cat`, `GIT_OPTIONAL_LOCKS=0`, `--no-pager`
and a null stdin, so no background worker can be blocked by a pager, an editor,
a credential prompt or `index.lock` contention.

### 2.1 Dirty — `src/scan/status.rs`

```
git status --porcelain --untracked-files=normal
```

Run with the **worktree directory as cwd**. Non-empty stdout (ignoring blank
lines) ⇒ dirty. Porcelain v1 already covers modified, staged *and* untracked in
one call.

`--untracked-files=normal` is passed explicitly rather than relying on the
default: a repository (or the user's global config) can set
`status.showUntrackedFiles=no`, which would silently hide untracked work and let
us call a worktree clean that is not. The flag on the command line wins over
config, so the probe cannot be defeated by configuration.

Individual directories of untracked files collapse to one entry (`dir/`) — we
only care whether the output is empty, so `normal` is enough and cheaper than
`all`.

### 2.2 Unpushed — `src/scan/status.rs`

Fallback chain, first applicable rule wins:

| # | Precondition | Command | Unpushed when |
|---|---|---|---|
| 1 | HEAD is on a branch **with** an upstream | `git rev-list --count @{upstream}..HEAD` | count > 0 |
| 2 | No upstream, but the repo has ≥ 1 remote | `git rev-list --count HEAD --not --remotes` | count > 0 |
| 3 | Repo has **no remotes at all** | `git rev-list --count HEAD --not refs/heads/<default>` | count > 0 |
| 4 | Detached HEAD | same as #2 (remotes present) or #3 (no remotes) | count > 0 |

Supporting probes:

```
git rev-parse --verify --quiet HEAD          # does HEAD resolve at all?
git symbolic-ref --quiet --short HEAD        # branch name, non-zero ⇒ detached HEAD
git rev-parse --verify --quiet @{upstream}   # does this branch track anything?
git remote                                   # any remotes configured?
```

The HEAD probe's exit code is read, not just its success: **1** means "HEAD does
not resolve" — an unborn branch in a freshly initialised repository, which has no
commits and therefore nothing unpushed (`Some(false)`). Anything else (**128**)
means this is not a working repository at all, which is *no answer* (`None`), not
"nothing unpushed". Verified: `git rev-parse --verify --quiet HEAD` exits 1 in an
empty repo and 128 outside one.

Default-branch resolution for rule #3, in order:

1. the **main checkout's** current branch —
   `git rev-parse --path-format=absolute --git-common-dir` then
   `git -C <common-dir> symbolic-ref --quiet --short HEAD`
   (the common dir's `HEAD` is the main worktree's HEAD, which is the closest
   thing a local-only repo has to a "default branch");
2. `refs/heads/main`, then `refs/heads/master`, whichever exists.

If none resolve, the probe reports *no answer* (see §3) rather than guessing.

**Why rules #3 and #4 exist.** Rule #2 asks "are there commits on no remote?".
In a repository with no remotes the answer is *always yes* for every commit ever
made, so every worktree in every local-only repo would light up as active —
which is not a signal, it is noise, and noise trains users to ignore the
warning. Rule #3 reinterprets the question the only way that carries information
locally: "does this worktree hold commits that are not yet on the default
branch?" — i.e. unmerged local work. A worktree sitting at (or merged into) the
default branch is then correctly reported clean.

**Why rule #1 outranks rule #2.** They can disagree, and rule #1 is the more
precise answer. If `feature` tracks `origin/feature` but the same commits were
also pushed to `origin/backup`, rule #2 sees the commits on *a* remote ref and
says "pushed", while rule #1 correctly reports that `feature`'s own upstream is
behind. A `git push` on that branch would still transfer objects, so the branch
*is* ahead of where it belongs. Pinned by a test.

Note that rules #1/#2 read **remote-tracking refs**, i.e. the last known state of
the remote. Enrichment never talks to the network (`git fetch` is out of scope
and would be far too slow for a background scan), so a stale remote-tracking ref
can over-report unpushed commits. Over-reporting is the safe direction: it asks
for one extra confirmation instead of silently green-lighting a purge.

### 2.3 Stashes — `src/scan/stash.rs`

```
git stash list --pretty=format:%gs
```

**One call per repository.** `refs/stash` is shared, so a single call from the
main checkout sees the stashes created inside every linked worktree; running it
per worktree would multiply subprocesses for identical output.

Which directory the call runs in is decided in `enrich.rs::stash_probe_dir`,
because `git stash list` **refuses to run outside a work tree**
(`fatal: this operation must be run in a work tree`, exit 128 — verified). So:

1. `<root>/.git` exists (a normal checkout) → run in the repo root;
2. otherwise (a **bare** repository, which can still own worktrees) → run in the
   first linked worktree that exists on disk; it shares the same `refs/stash`,
   so the answer is identical;
3. otherwise (orphaned group, nothing left on disk) → run in the root anyway,
   where the call fails and yields no counts.

This is still exactly one subprocess per repository; only the cwd changes.

`%gs` is the reflog subject. Two shapes exist:

```
WIP on <branch>: <sha> <subject>     # git stash push (no -m)
On <branch>: <message>               # git stash push -m <message>
WIP on (no branch): <sha> <subject>  # stashed while on a detached HEAD
```

Parsed with plain string ops (no regex dependency): strip the `WIP on ` / `On `
prefix, take everything up to the first `:`. That split is exact, not a
heuristic — git refnames may not contain `:` or a space
(`git check-ref-format`), so the first colon always terminates the branch name.
`(no branch)` entries are kept as a key; no branch can be named that, so they
simply never match a worktree.

The result is `HashMap<branch, count>`; the UI joins it to worktrees by branch
name.

### 2.4 Size — `src/scan/size.rs`

No git involved. Iterative (explicit-stack, not recursive) directory walk over
the worktree, summing `filesize::PathExt::size_on_disk_fast(&metadata)` for every
entry including directories themselves.

- **Allocated, not apparent.** `du`-style: this is the space the user actually
  gets back. `filesize` handles the platform split for us — `st_blocks * 512` on
  unix, `GetCompressedFileSize` (with a `len()` fallback) on Windows. Hand-rolled
  `st_blocks` would not compile on Windows, which is a tier-1 target for this
  project.
- **Symlinks are counted, never followed.** Every stat below the root is a
  `symlink_metadata` (`DirEntry::metadata()` already has that semantics), so a
  link contributes its own size and a directory symlink is not descended into —
  no double counting and no symlink cycle can hang the walker. One platform
  caveat: `filesize`'s Windows implementation `canonicalize`s the path, which
  resolves reparse points, so a symlink would be charged its *target's*
  allocation and the target counted a second time. Symlinks therefore
  contribute 0 on Windows and their own blocks on unix (normally 0 anyway —
  short link targets live inline in the inode).
- The **root** is the one path resolved *through* symlinks: the caller named it
  explicitly and means the directory it points at. Nothing below it is followed,
  so this cannot loop.
- **Explicit stack** instead of recursion: a pathological directory depth cannot
  blow the worker thread's stack.
- Only the working files are measured. Linked worktrees share the object store,
  so `.git` inside a worktree is a small pointer file — the number really is
  what removal reclaims.

---

## 3. Error policy: an error may never downgrade a warning

Every probe returns `Option<bool>` — `None` means *no answer*, which is
different from `Some(false)`.

- Both probes answered ⇒ `ActiveReason::combine(dirty, unpushed)`:
  `Clean` or `Active(..)`.
- A probe that answered **`true`** wins even when the other one failed: a
  worktree we have *proven* dirty is reported `Active(Dirty)`, not `Unknown`.
  This is deliberate and slightly stricter than "any failed probe ⇒ `Unknown`":
  `ActivityState::Unknown` reports `is_active() == false`, so downgrading a
  proven warning to `Unknown` would drop the worktree out of the active count,
  out of the active-first sort and out of the red row styling. Both still require
  force, but the user would see less than we know.
- Otherwise (no probe proved anything) ⇒ `ActivityState::Unknown`, which
  `needs_force()` treats exactly like an active worktree.

`Clean` is only ever returned when both probes succeeded and both said "no". No
error path can produce it.

`SizeState` follows the same rule: an unreadable **root** yields
`SizeState::Unknown`, never `Ready(0)`. Reporting zero bytes for a failed
measurement would understate what the user reclaims and, worse, make a huge
worktree look like a rounding error. Unreadable *entries inside* a readable root
are skipped instead — a single permission-denied file must not throw away the
whole measurement.

---

## 4. Worker pool — `src/scan/enrich.rs`

`spawn(groups, tx)` snapshots the paths it needs, starts one coordinator thread
and returns immediately; the TUI never blocks on enrichment.

**Phase order is cheapest-first, and the phases are barriers**, so the UI fills
in from the fastest signal to the slowest:

1. **Stashes** — one subprocess per *repository*.
2. **Activity** — a handful of subprocesses per worktree.
3. **Sizes** — the slow one: thousands of `stat` calls per worktree.

Within a phase the work is parallel (`rayon`,
`par_iter().for_each_with(tx.clone(), ..)` — `for_each_with` clones the sender
per worker, which is how a `Sender` that is `Send` but not `Sync` is fanned out).

**Cancellation** (rescan, quit) is a shared `AtomicBool`:

- checked before each phase, so a cancel that lands during the cheap phases skips
  the expensive one entirely;
- checked before each item;
- checked *inside* the size walk (per directory and every 128 entries), because a
  single multi-GB worktree would otherwise keep a core busy long after the user
  moved on;
- a failed `send` (the UI dropped the receiver on quit) sets the same flag, so a
  disconnected receiver stops the pass instead of panicking a worker.

`EnrichMsg::Done` is sent exactly once per `spawn`, on every exit path including
cancellation and an empty scan, so a consumer can always drain to a terminator.
`cancel()` is idempotent and safe after the pass has already finished.
`is_cancelled()` reports whether the pass was stopped (by `cancel` *or* by the
receiver hanging up). `wait()` blocks until the pass has fully stopped and
returns `false` if a worker panicked — rayon propagates a worker panic into the
coordinator thread, so this covers the whole pool. The TUI needs none of the
last two; tests and a deterministic shutdown do.

### Cost

Per worktree, worst case (a local-only repo, so the whole fallback chain runs):
`status` + `rev-parse HEAD` + `symbolic-ref` + `remote` + `git-common-dir` +
`symbolic-ref` on the shared dir + `rev-parse --verify <ref>` + `rev-list` ≈ 8
subprocesses. The common case (a branch with an upstream) is 4. They are cheap
individually and run in parallel across worktrees; if this ever shows up on a
huge scan, the fix is caching the per-*repository* answers (remotes, default
branch) once per group rather than once per worktree.

---

## 5. How these are kept honest

`tests/enrich.rs` builds real repositories and asserts exact enum variants,
exact counts and before/after deltas. Each assertion was checked against a
deliberately broken implementation — the test only earns its place if the
mutation fails it:

| Mutation | Test that caught it |
|---|---|
| drop the no-remotes fallback (rule 3) | `in_a_repo_with_no_remotes_only_unmerged_work_is_flagged` |
| let rule 2 outrank rule 1 | `an_upstream_that_is_behind_outranks_the_same_commits_on_another_remote_branch` |
| `Unknown` → `Clean` on a failed probe | `a_worktree_outside_any_repository_is_unknown_never_clean` |
| follow symlinks while walking | `size_counts_symlinks_without_following_them` |
| `cancel()` as a no-op | `cancel_stops_the_pass_before_everything_is_measured` |
| ignore a hung-up receiver | `a_receiver_that_hung_up_stops_the_pass_without_panicking` |
| send `Done` twice | every worker-pool test (`drain` requires disconnect after `Done`) |
| do the work inline in `spawn` | `spawn_returns_long_before_the_work_is_done` |
| merge the size phase into the activity phase | `enrichment_streams_every_signal_cheapest_first…` |
| split the stash subject on a space instead of `:` | the three stash tests + the parser unit tests |

Two notes on the timing test, learned the hard way:

- A wall-clock floor ("the pass must take more than 50 ms, otherwise the fixture
  is too small") is **flaky**: the real pass fans out across every core and
  finished in ~48 ms on this machine. The floor is now *measured* — the test
  times the same work done serially and compares against that, which scales with
  the machine.
- A timing assertion alone cannot catch a `spawn` that blocks but is internally
  parallel (it would look fast enough). The load-bearing assertion is therefore
  deterministic: right after `spawn` returns, the channel must not already
  contain `Done`. No implementation that finished the pass inline can satisfy
  that, at any speed.
