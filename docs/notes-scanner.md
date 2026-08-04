# Scanner notes (M1)

Design notes for `src/scan/walker.rs` and `src/scan/repo.rs`. Everything below
was verified empirically against **git 2.47.0** with real fixture repositories,
not taken from documentation. Where git's actual behaviour differs from the
"obvious" assumption it is called out as **[surprise]**.

---

## 1. The two-phase scan

```
scan(root)
  phase 1  parallel filesystem walk  -> Vec<Hit>, Vec<skipped>
  phase 2  git interrogation + grouping -> ScanResult
```

Phase 1 touches the filesystem only (cheap, wide, parallel). Phase 2 shells out
to git a bounded number of times (once or twice per *repository*, never per
worktree) and is where all authoritative metadata comes from.

### Phase 1: prune on hit

`ignore::WalkBuilder` in parallel mode with **every filter explicitly off**:

| setting | value | why |
|---|---|---|
| `standard_filters` | `false` | turns off the whole default bundle in one call |
| `hidden` | `false` | **critical** — the default hides dotfiles, so `.git` would be invisible |
| `ignore` / `git_ignore` / `git_global` / `git_exclude` | `false` | worktrees very often live in gitignored directories (`.worktrees/`, `tmp/`) |
| `parents` | `false` | a `.gitignore` *above* the scan root must not hide worktrees inside it |
| `require_git` | `false` | belt and braces: no VCS-conditional ignore logic at all |
| `follow_links` | `false` | symlink loops would hang the scan |

For **every directory entry** the visitor does one `stat` of `<dir>/.git`:

- `Ok` -> classify, record if relevant, and **always** return `WalkState::Skip`.
- `NotFound` -> fall through to the bare-repo probe.
- any other error (typically `EACCES`) -> record in `ScanResult::skipped`,
  return `Skip`.

Pruning happens on *any* `.git` entry — including submodules and unrecognised
pointers — because the performance property ("the scan is O(non-repo
directories)") must not depend on classification succeeding. The cost is one
extra `stat` per directory, which is far cheaper than the `readdir` the walker
would otherwise perform on repository internals.

`WalkState::Skip` only means "do not descend" when the entry is a directory
(confirmed in the `ignore` 0.4.33 source); that is exactly what we want, and it
is why the probe has to happen on the *parent directory* entry rather than on
the `.git` entry itself — by the time the walker yields `.git`, its parent has
already been descended into.

The bare-repo probe is gated on `<dir>/HEAD` being a file so that the extra
`stat`s and the two git subprocesses only ever run for real candidates.

### Phase 2: identity is the git common dir

`git rev-parse --path-format=absolute --git-common-dir` is the repository's
stable identity: the main checkout and every linked worktree of a repo all
report the same value (`<repo>/.git`, or the bare directory itself). Groups are
keyed by that path, canonicalised.

`git worktree list --porcelain` is run **once per common dir**, from the repo
root when the walk found it, otherwise from any discovered worktree of that repo
(the output is identical whichever member you run it from — verified). It is
authoritative for branch, HEAD, lock and prunable state, and it is what catches
worktrees living outside the scan root.

---

## 2. Classification rules

### `.git` **directory** -> main checkout

Recorded as a repository. Its common dir is `<dir>/.git`.

### Directory that is itself bare -> repository

All of the following must hold, checked in this order (cheapest first):

1. `HEAD` is a file, `objects/` is a directory, `refs/` is a directory.
2. `git rev-parse --path-format=absolute --git-common-dir` from the directory
   resolves to **the directory itself**. Without this check a plain
   subdirectory of a repo could be mistaken for a repo, and a decoy directory
   containing a stray `HEAD` file would be picked up.
3. `git rev-parse --is-bare-repository` is `true`.

### `.git` **file** -> linked worktree, submodule, or orphan

The pointer is `gitdir: <path>`, resolved relative to the `.git` file's parent
when it is not absolute.

**[surprise]** The critical discriminator is *not* reliably the path shape. A
submodule's pointer is written **relative** (`gitdir: ../../.git/modules/vendor/sublib`)
while a worktree's is written **absolute** (`gitdir: /abs/repo/.git/worktrees/wt`),
but neither form is guaranteed. The robust discriminator is the **marker files
inside the target directory**:

| target contains | meaning |
|---|---|
| `gitdir` **and** `commondir` | linked worktree |
| neither | submodule module dir (or an exotic gitdir) |

Verified: `<repo>/.git/worktrees/<name>/` contains `commondir` (holding `../..`)
and `gitdir` (holding the absolute path *back to the `.git` file*), whereas
`<super>/.git/modules/<path>/` contains neither. A submodule module directory
otherwise looks exactly like a `.git` directory (`config`, `HEAD`, `objects`,
`refs`, ...), so a "does the target look like a repo?" test cannot separate them.

Submodules are pruned and **never recorded**.

### Orphans

A `.git` file whose target does **not** exist. To avoid ever proposing the
deletion of something that is not a worktree, an orphan is only recorded when
the dangling target has the exact shape `<common>/worktrees/<name>` (i.e. its
parent directory is named `worktrees`). A dangling `modules/` pointer — an
orphaned submodule — is deliberately ignored.

The intended repo is derived from the dangling path: common dir =
`<common>`, repo root = `<common>` with a trailing `.git` component stripped.

**[surprise]** Once the owning repo is gone, git is *completely* useless in the
orphan directory: `git rev-parse` and `git worktree list` both exit 128 with
`fatal: not a git repository: <dangling gitdir>`. Everything about an orphan
therefore has to come from the filesystem.

Orphans join an existing live group when their derived common dir matches one
(the repo survived but its `worktrees/<name>` metadata was removed); otherwise
they form a `RepoGroup { orphaned: true }` whose `root` is a path that does not
exist on disk. In both cases `Worktree::orphaned` is set.

---

## 3. `git worktree list --porcelain`, precisely

```
worktree /abs/path/to/repo
HEAD de0def2d4d28dc65e721dcf32a986c3ae8e610d1
branch refs/heads/main
<blank>
worktree /abs/path/to/wt-detached
HEAD de0def2d4d28dc65e721dcf32a986c3ae8e610d1
detached
<blank>
worktree /abs/path/to/wt-locked
HEAD de0def2d...
branch refs/heads/locked
locked in use by CI
<blank>
worktree /abs/path/to/wt-gone
HEAD de0def2d...
branch refs/heads/gone
prunable gitdir file points to non-existent location
```

Confirmed behaviours:

- **Ordering**: the main worktree (or the bare repo) is always the first
  record; the rest are sorted by path. The scanner does not rely on ordering
  alone — it *also* excludes the entry whose path equals the repo root derived
  from the common dir.
- **Bare repos**: the first record is `worktree <bare dir>` followed by `bare`,
  with **no** `HEAD` and **no** `branch` line.
- **`locked` with no reason** is the bare word `locked`. Modelled as
  `Some(String::new())` so "locked without a reason" stays distinguishable from
  "not locked".
- **[surprise] Lock reasons are C-quoted when they contain control characters**:
  a two-line reason comes out as `locked "first line\nsecond line"` — quoted,
  with the newline escaped, on a single line. `-z` mode emits it raw instead.
  The parser therefore runs a C-style unquote (`\n \t \r \a \b \f \v \" \\` and
  octal `\NNN`) on any reason wrapped in double quotes.
- **[surprise] Worktree *paths* are NOT quoted**, even for tabs and non-ASCII,
  and this is unaffected by `core.quotePath`. Verified with `wt<TAB>tab` and
  `wt-café`. So the path is everything after `worktree ` verbatim, to end of
  line — do not trim it and do not unquote it.
- `prunable` may or may not carry a reason; the model only needs the boolean.
- A prunable record still carries `HEAD` and `branch`, and the record is kept:
  a metadata-only worktree is exactly the kind of junk this tool exists to
  clean up.

**[surprise] Never run `git worktree list` inside a submodule.** From
`super/vendor/sublib` it reports

```
worktree /abs/super/.git/modules/vendor/sublib
```

— it names the **module directory** as the worktree path, not the checkout.
Treating a submodule as a repository would therefore surface a path inside
`.git/` as a purge target. Two defences: submodules are never recorded as
repositories, and any candidate worktree path containing a literal `.git`
component is dropped.

---

## 4. Deduplication, paths and identity

- Every path that is compared or stored goes through `repo::normalize`:
  `canonicalize`, falling back to "canonicalise the parent and re-attach the
  file name" for paths that do not exist (prunable worktrees), falling back to
  a purely lexical `.`/`..` fold. Needed on macOS where the temp dir is
  `/var/folders/...` -> `/private/var/folders/...`; without it a worktree found
  by both the walk and `worktree list` would be stored twice.
- A worktree found by the walk *and* reported by `worktree list` is merged into
  the single entry built from the porcelain record (which carries more
  information). Walk-only discoveries are still kept — a copied worktree
  directory points at the original's gitdir, so git never mentions the copy,
  yet the copy is real disk waste.
- `outside_scan_root` is `!path.starts_with(scan_root)`, i.e. a genuine
  statement about location — *not* "the walk missed it". A worktree nested
  inside a pruned repository (`repo/.worktrees/feature`) is inside the scan
  root even though only `worktree list` found it.
- `head` is stored as the 7-character short sha, matching the field's
  documented meaning.

---

## 5. Deliberate output choices

- **The main checkout is never in `RepoGroup::worktrees`.** It is the group's
  `root`. Excluded twice over: by position (first porcelain record) and by
  value (path equal to the derived repo root).
- **Groups with no worktrees are dropped.** A repository with nothing purgeable
  is noise; a developer folder can easily hold 200 repos of which 5 have
  worktrees. This is also why plain directories, submodules and bare origins
  created by fixtures produce no groups at all.
- **Nothing under a `.git` component is ever reported** as a worktree.
- Groups and worktrees are sorted by name, with the path as tie-breaker, so
  output is deterministic across runs and across walk thread interleavings.
- Unreadable directories go to `ScanResult::skipped` (sorted, deduplicated —
  a permission failure is usually reported twice, once by the walker's
  `readdir` and once by our own `.git` probe).
- `size` and `activity` are left `Pending`; M2 fills them in.

---

## 6. Concurrency

The walk is CPU/IO-bound work on plain OS threads (`ignore`'s own pool), not on
an async runtime, so a `std::sync::Mutex` around the result accumulator is the
right primitive — there are no await points to hold a guard across. Lock
contention is negligible because the mutex is only taken on a *hit* or an
error, not once per directory. Poisoned locks are recovered with
`PoisonError::into_inner` rather than `unwrap()`: a panicking sibling thread
must not turn a partial scan into a crash.

---

## 7. Requirements and limits

- Needs `git >= 2.31` for `rev-parse --path-format=absolute` (2021; well below
  any plausible floor).
- If the `git` binary is missing entirely the scan degrades instead of failing:
  worktrees are still discovered from `.git` files and grouped via each
  `commondir` file, but branch/lock/prunable state and out-of-root worktrees
  are unavailable. A warning is emitted.
- The lexical `..` fold in `resolve_gitdir_pointer` is not symlink-correct;
  the parent directory is canonicalised first to keep that window as small as
  possible. A `gitdir:` pointer that traverses a symlink via `..` is not
  something git itself writes.
