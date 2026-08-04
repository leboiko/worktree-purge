# Purge engine — design notes

Implementation notes for `src/app/purge.rs` (milestone M4). This is the only
part of `worktree-purge` that destroys user data, so the notes below record
*why* each decision was made and every failure mode found empirically against
real git.

All behaviour below was verified against **git 2.47.0** on macOS (APFS) by
building throwaway repositories and worktrees and observing exit codes,
`stderr`, filesystem state and `.git/worktrees/<id>` metadata.

---

## 1. Removal strategy

### Why `git worktree remove` and not a directory delete

A linked worktree is two things, not one:

1. the working directory (`<wt>/`, whose `.git` is a *file* pointing at the
   repository), and
2. an administrative directory inside the owning repository at
   `<repo>/.git/worktrees/<id>/` holding `HEAD`, `gitdir`, `index`,
   `commondir`, and optionally `locked`.

`rm -rf <wt>` removes only (1). The repository keeps listing the worktree
forever: `git worktree list` still reports it (annotated `prunable gitdir file
points to non-existent location`), `git worktree add` refuses to reuse the
name, and the checked-out branch stays "in use by another worktree" so it
cannot be checked out or deleted. The user is left with invisible breakage in
exchange for the disk space.

`git worktree remove` does both halves: it deletes the working directory *and*
`<repo>/.git/worktrees/<id>`, and removes the whole `worktrees/` parent when it
empties. Verified: after `git worktree remove` on a clean worktree,
`.git/worktrees` no longer exists and `git worktree list --porcelain` reports
only the main checkout. This is why the removal always runs from the owning
repository (`item.repo_root`) rather than touching the directory directly.

Two more properties that matter:

- **The branch survives.** `git worktree remove` (even with `--force --force`)
  never deletes the branch that was checked out. Verified: a worktree on branch
  `feature` is removed and `git branch --list` still shows `feature`. Branch
  deletion is explicitly out of scope for v1, and the removal path must not
  acquire it by accident.
- **The main checkout is protected by git itself.** `git worktree remove
  <main>` fails with `fatal: '<path>' is a main working tree` and *stays*
  failing under `--force --force`. That is a welcome second line of defence,
  but it is not our first: the model only ever represents linked worktrees, so
  a `PurgeItem` should never point at a main checkout in the first place.

### Command shape

Repo-owned removal, with the process working directory set to `item.repo_root`:

```
git --no-pager worktree remove [--force --force] <absolute worktree path>
```

(`--no-pager` and the `LC_ALL=C` / `GIT_TERMINAL_PROMPT=0` / `GIT_PAGER=cat` /
`GIT_OPTIONAL_LOCKS=0` environment come from `git::run`, so output is
deterministic and nothing can block a background worker on a pager or a
credential prompt.)

Per-repo cleanup, once per touched repository:

```
git --no-pager worktree prune
```

The path argument is **always absolute**. `git::run` sets the child process's
working directory to the repository, so a relative path would resolve against
the *repository*, not against the user's shell — verified: from a shell sitting
in the parent directory, `git -C <repo> worktree remove wtg` resolved `wtg`
relative to the repo and still worked only because the layout happened to line
up. Passing absolute paths removes the ambiguity entirely. Paths are passed as
`OsString`, never through `to_string_lossy()`, so a non-UTF-8 path is not
silently mangled into a *different* path before being handed to a delete.

### `--force` is passed **twice**

This is the single most important discovery in this milestone.

| worktree state | `remove` | `remove --force` | `remove --force --force` |
|---|---|---|---|
| clean | ok | ok | ok |
| dirty (modified / staged / untracked) | **refuses** (exit 128) | ok | ok |
| contains submodules | **refuses** (exit 128) | ok | ok |
| **locked** | **refuses** (exit 128) | **refuses** (exit 128) | ok |

`git worktree lock` marks a worktree as not-to-be-removed, and a *single*
`--force` does not override it — git says so in the error itself:

```
fatal: cannot remove a locked working tree;
use 'remove -f -f' to override or unlock first
```

`--force` is a counter in git, not a boolean. So the engine emits `--force
--force` whenever the caller passes `force = true`. Verified harmless on a
clean worktree: exit 0, main checkout untouched, branch retained. Had we passed
a single `--force`, the "purge with force" path would have failed on exactly
the worktrees a user most wants to clear out (the ones they deliberately
locked), and the UI would have shown a confusing "use -f -f" message after the
user had already confirmed twice.

### git does **not** protect unpushed commits

Verified: a worktree with a clean working tree but commits that exist on no
remote is removed by a plain `git worktree remove`, exit 0, no warning. git's
only refusal condition is working-tree *dirtiness* (plus submodules and locks).

Consequence: the `requires_force` / double-confirmation gate around unpushed
work is **entirely an application-level policy**. There is no git backstop for
it. Losing that gate in the UI silently loses user commits, and no amount of
care inside `purge.rs` can compensate — by the time `purge_one` runs, the
decision has been made.

---

## 2. The orphan fallback

An *orphan* is a worktree directory whose owning repository no longer exists
(`item.repo_root == None`). There is no `.git/worktrees/<id>` left to maintain
and no repository to run git from, so the metadata argument above does not
apply: the only correct action is a recursive directory delete.

That makes it the one code path in the program that deletes a directory tree
without git mediating, which makes it the one path that can destroy something
the user did not select. It is therefore guarded before a single byte is
touched. `guard_orphan_path` is `pub` specifically so the guard can be tested
directly against dangerous inputs without asking a test to gamble on the delete
being refused.

**The guard rejects:**

| rejected input | reason |
|---|---|
| a relative path | resolution depends on the process CWD; the deleted tree would not be knowable from the item alone |
| `/`, `C:\` | filesystem root |
| `/Users`, `/home`, `/tmp`, `/var`, `/opt`, any single-component path | fewer than two path components below the root — no plausible worktree lives there, and a truncated/corrupt path lands here |
| the user's home directory (`$HOME` / `%USERPROFILE%`) | exact match |
| any **ancestor of** the home directory | catches `/Users` on macOS and `/home` on Linux even when the depth rule would not |
| a path that does not exist | nothing to free; treated as a clean no-op, not a delete |
| a path that is a **file** | a worktree is a directory; a file here means the item is stale or wrong |
| a path that is a **symlink** (including a dangling one) | deleting through a link would destroy the target, which is outside the selection |

The depth rule counts components that are neither the root nor a Windows drive
prefix, so `/data/wt` (2) passes while `/wt` (1) does not. A worktree sitting
directly at a filesystem root is not a real configuration; refusing it is
cheaper than being wrong.

### Order of operations: validate before measuring

The structural half of the guard runs before *anything else* in `purge_one`,
including the size measurement, and it runs on the repo-owned path too — not
just on orphans.

This was a bug before it was a rule. The first version measured the freed bytes
first (reasonable: the directory has to be counted before it is deleted) and
guarded second. The guard correctly refused to delete `/` — but only after
`measure_tree` had already started walking the entire filesystem from the root.
The test suite did not fail; it hung, and the purge worker thread would have
hung with it in production. **A guard that stops the deletion but not the
traversal is only half a guard.**

So the ordering is now: structural checks → measure → delete → verify. The
repo-owned path gets the structural check as well, because the size
measurement is a recursive walk regardless of who ends up making the deletion
decision. `tests/purge.rs` pins this with a wall-clock bound: refusing `/` must
return in well under a second, which fails loudly instead of hanging if the
order is ever swapped back.

The symlink rule is worth spelling out. `Path::exists()` follows symlinks, so a
**dangling** symlink reports `false` from `exists()` while the directory entry
is still very much there — verified: after replacing a worktree directory with
`ln -s /nowhere wt`, `[ -e wt ]` is false but `[ -L wt ]` is true. The guard
therefore uses `symlink_metadata` and requires `file_type().is_dir()`, which
rejects both directory symlinks (whose target is outside the selection) and
dangling ones. `std::fs::remove_dir_all` also refuses to traverse a symlink,
but the guard does not delegate the decision: an explicit, tested refusal beats
relying on a standard-library implementation detail.

---

## 3. Verifying that the removal actually happened

A purge tool that reports freed space it did not free is worse than one that
fails loudly, so success is never inferred from git's exit code alone. After
any removal reported as successful, the engine re-checks the filesystem and
only reports `Ok(freed)` when **no directory remains at the worktree path**.

The check is "no *directory* remains", not "nothing remains", for a reason
found empirically:

> **git exits 0 while the given path survives.** Point `git worktree remove` at
> a *symlink* to a worktree instead of the worktree itself and git resolves it,
> deletes the real directory, cleans the metadata and exits 0 — but the symlink
> entry at the path we were told to remove is still there, now dangling.

Treating that as a failure would be a lie in the other direction: the space
really was reclaimed. Checking for a surviving *directory* reports it as the
success it is, while still catching the case that actually matters — the
directory is still sitting on disk consuming the space we just told the user we
had freed.

---

## 4. Failure modes found

Every one of these was reproduced against real git, not reasoned about.

### 4.1 git exits **non-zero**, directory survives, **metadata is already gone**

The worst one, and it is not rare — it is what any permission problem looks
like.

`git worktree remove` deletes the administrative directory *unconditionally*,
even when deleting the working directory failed. From git's own source the
comment is explicit: "continue on even if ret is non-zero, there's no going
back from here". Reproduced two ways:

- worktree's **parent** directory read-only →
  `error: failed to delete '<path>': Permission denied`, exit **255**
- an **immutable file** inside the worktree (macOS `chflags uchg`) →
  `error: failed to delete '<path>': Operation not permitted`, exit **255**
- a **read-only subdirectory** inside the worktree → same, exit **255**

In all three: the directory is still on disk, and `.git/worktrees/` has already
been removed. **A failed removal converts a tracked worktree into an orphan.**

Consequences that the rest of the app has to live with:

- The engine reports `Err`, which is correct — no space was freed.
- Retrying the *same* `PurgeItem` will now fail differently, with `fatal:
  '<path>' is not a working tree`, because the metadata git needed is gone.
  Retry-in-place is therefore not a sound recovery strategy; the correct
  recovery is a rescan, after which the directory is classified as an orphan
  and can be removed through the orphan path (which is a plain delete and will
  hit the same permission wall, but at least reports it honestly).
- The UI must not assume a failed item is unchanged. It is not.

### 4.2 git exits **0**, given path survives (symlink)

Covered in §3. Removal via a symlink to the worktree: real directory gone,
metadata gone, exit 0, dangling symlink left at the given path.

### 4.3 Working directory already gone, metadata present

`git worktree remove` exits **0** and cleans the metadata. This is the useful
case, and it is why a repo-owned item whose directory has vanished is still
handed to git rather than short-circuited: there is real cleanup left to do.

### 4.4 Path unknown to the repository

`fatal: '<path>' is not a working tree`, exit **128**, nothing touched. Two
ways to get here: the worktree was already removed (see 4.1), or `repo_root`
does not own the path — verified by asking repo B to remove repo A's worktree,
which fails cleanly and leaves the directory intact. A wrong `repo_root` can
never cause a wrong deletion; git validates ownership.

### 4.5 Worktree path replaced by a file, or by a dangling symlink

`fatal: validation failed, cannot remove working tree: '<path>/.git' does not
exist`, exit **128**, and the metadata is **left in place** (unlike 4.1). The
file/symlink survives. Reported as a failure; `git worktree prune` will clear
the stale metadata on the next batch.

### 4.6 Main checkout

`fatal: '<path>' is a main working tree`, exit **128**, under any force level.

### 4.7 `git worktree prune` cannot prune locked metadata

`prune` exits 0 but deliberately leaves stale entries whose worktree is locked.
So a locked worktree whose directory was deleted out from under git stays in
`git worktree list` until it is unlocked. Nothing to do about it here; noted so
that a future "why is this still listed" bug report is not a mystery.

### 4.8 `git worktree prune` outside a repository

`fatal: not a git repository`, exit **128**. `prune_repo` is best-effort and
ignores this, so a repository that disappeared mid-batch cannot fail a batch.

### 4.9 git cannot be executed at all

`git::run` returns `Err(io::Error)` when the binary is missing or the working
directory does not exist (a repository deleted mid-batch). Reported as a
per-item failure with the io error text; never a panic. No `unwrap`/`expect`
appears on any subprocess result in this module.

---

## 5. Freed-bytes accounting

The size has to be measured **before** the removal, because afterwards there is
nothing left to measure. The walk uses allocated size (`filesize`, already a
dependency) rather than apparent file length, so the number matches what the
filesystem actually gives back, and it does not follow symlinks — a symlink
inside a worktree points at bytes that removing the worktree will not free.

Deliberate choices:

- **An unmeasurable size never fails a removal.** If the walk errors (a
  permission-denied subdirectory, a directory that vanished mid-walk) the
  engine falls back to `item.size` — the value the scanner computed earlier —
  and to `0` if that is also unknown, then proceeds with the removal. Reporting
  `Ok(0)` under-promises reclaimed space, which is the safe direction to be
  wrong in. Refusing to delete because we could not count would be absurd.
- **Partial results are kept.** Subdirectories that fail to read contribute
  nothing rather than aborting the whole measurement, so one unreadable
  subdirectory costs accuracy, not the entire figure.
- **The walk is iterative, not recursive.** An explicit stack, so a
  pathologically deep tree cannot overflow the stack of a worker thread.
- The measurement is *fresh*, not `item.size`, because the scanner's value can
  be minutes old — long enough for a build to have run.
- **Nothing on disk means zero freed**, even when `item.size` says otherwise. A
  repo-owned item whose directory already vanished still goes to git (there is
  metadata to clean, §4.3), but it reports `Ok(0)`: the space was reclaimed by
  whoever deleted the directory, not by us, and the run's "freed" total must
  not double-count it.

**Known duplication:** `scan::size::dir_size` (another agent's milestone) will
compute the same thing. It is `todo!()` at the time of writing, so calling it
would panic the purge worker; `purge.rs` carries its own private walk instead.
Once `scan::size` lands, `measure_freed` should delegate to it and keep only
the fallback chain. Flagged as a follow-up rather than silently living with two
implementations.

---

## 6. Batch execution

`purge_all` processes items **in the order given** and invokes `progress` once
per item immediately after that item completes, so the UI streams results as
they land instead of waiting for the batch.

- **One failure never aborts the batch.** Every item is attempted; failures are
  reported per item. There is no early return anywhere in the loop.
- **No parallelism within a repository.** Concurrent `git worktree remove`
  invocations against one repository contend on the same index/ref locks and
  produce spurious "unable to create '.git/index.lock'" failures. Removals are
  cheap (a directory delete) and the wall-clock cost is dominated by the
  filesystem, not by git, so serial execution costs nothing worth having.
- **`prune_repo` runs once per touched repository, after the removals**, not
  once per item. `git worktree prune` rescans the whole `worktrees/` directory,
  so running it per item is O(n²) in subprocess work for zero benefit.
  Repositories are de-duplicated by path, preserving first-touch order so the
  sequence of subprocesses is deterministic (a `HashSet` would not be) and
  reproducible in tests.
- Pruning covers repositories whose items **failed** too, which is precisely
  the 4.5 case: the removal failed and left stale metadata behind that `prune`
  can clear.
- Orphan items have no repository and so contribute nothing to the prune set.

## 7. How the tests were verified

Every assertion in `tests/purge.rs` and the `purge.rs` unit tests was checked
against a deliberately broken implementation: the engine was mutated one defect
at a time and the suite had to notice. A mutation that still passes means the
corresponding test proves nothing.

Thirteen mutations, all caught:

| mutation | caught by |
|---|---|
| `--force` passed once instead of twice | `a_locked_worktree_needs_force_and_a_single_force_flag_is_not_enough` |
| `purge_all` returns on the first failure | `one_failure_does_not_abort_the_batch` |
| `prune_repo` does nothing | `prune_repo_clears_stale_metadata` |
| `purge_all` never calls `prune_repo` | `a_batch_prunes_stale_metadata_once_per_touched_repository` |
| freed bytes are a constant | `freed_bytes_track_the_actual_content_of_each_worktree` |
| freed bytes echo the scanner's `item.size` | same |
| the measurement follows symlinks | `freed_bytes_do_not_follow_symlinks_out_of_the_worktree` |
| raw `remove_dir_all` instead of `git worktree remove` | `purging_a_clean_worktree_...` (metadata leak) and `purging_a_dirty_worktree_...` (protection lost) |
| post-removal verification skipped | `verification_fails_while_a_directory_remains` |
| verification made over-strict | `removal_through_a_symlink_is_a_success_...` |
| git's exit status ignored | `purging_a_dirty_worktree_without_force_...` |
| git's reason replaced by a generic message | dirty + locked message assertions |
| main-checkout refusal removed | `the_main_checkout_is_refused_...` |

Three of these initially slipped through, and each gap was a real weakness:

- **Verification skipped** went unnoticed because no fixture makes git lie
  about a surviving *directory*. Closed with direct unit tests on
  `verify_removed`; the end-to-end case is not reproducible with real git, and
  untested safety code is not safety code.
- **git's exit status ignored** passed because the post-removal verification
  caught the failure anyway — defence in depth working, but the user lost the
  *actionable* message ("use --force to delete it") in exchange for a generic
  "still on disk". The tests now assert git's own reason survives into the
  error.
- **Main-checkout refusal removed** passed because git refuses it too. The
  test now pins our refusal specifically, so the guarantee does not rest
  entirely on a subprocess behaving as expected.

The guard was deliberately **not** mutated to "accept everything": that
mutation would have attempted to delete `/`.

## 8. What this module deliberately does not do

- No branch deletion, no `git gc`, no remote operations (out of scope for v1).
- No retry. Per 4.1, retrying a failed removal in place is not sound — the
  metadata git needs may already be gone. Recovery is a rescan.
- No confirmation logic. `force` arrives as a decided fact; the gate lives in
  the UI. `purge_one` will do exactly what it is told.
