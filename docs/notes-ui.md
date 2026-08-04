# TUI, view-model and key handling — implementation notes

Covers `src/app/state.rs`, `src/app/events.rs` and `src/ui/**`. Written for the
next person who has to change a row, a key or a confirmation gate without
re-deriving the invariants.

---

## 1. Screen layout

```
┌ worktree-purge ── ~/dev ─────────────────────────────────────────────┐  <- ui::tree (bordered)
│ ▼ markdown-reader                    4 worktrees · 1.2 GB · 2 active │
│   [x] ● feature-auth        412 MB  uncommitted changes              │
│   [ ] ● fix-parser          318 MB  unpushed commits      ⚑ 2        │
│   [ ] ○ old-experiment      290 MB                                   │
│ ▶ worktree-purge                          2 worktrees · 84 MB        │
└──────────────────────────────────────────────────────────────────────┘
 3 repos · 9 worktrees · 2.1 GB    selected 1 (412 MB)   sort: name  ? help   <- ui::statusbar
```

`ui::draw` splits the frame with `Layout::vertical([Min(1), Length(1)])`:

| Region | Renderer | Notes |
|--------|----------|-------|
| top (all remaining rows) | `ui::tree::render` | bordered block, title `worktree-purge ── <scan root, tilde-shortened>` |
| bottom (exactly 1 row) | `ui::statusbar::render` | never bordered — a border would eat the only line |
| full frame (overlay) | `ui::modal::render` / `ui::help::render` | `Clear` + centered bordered block |

Overlays are drawn over the **whole** frame area, not the tree area, so a modal
can be taller than the tree region on a short terminal.

`ui::draw` returns early when the frame is 0×0, and drops the tree entirely when
only one row is left (the totals are more useful than a clipped border). Every
renderer re-checks its own `Rect` before writing, and `ui::centered` clamps to
the parent, so a 1×1 terminal renders almost nothing rather than panicking —
`tiny_rects_render_without_panicking` walks that grid for every mode.

When there is nothing to show, the tree area renders `no linked worktrees under
<root>` (or `scanning…` while enrichment is still running) instead of an empty
box.

### Scrolling

The tree is a `List` + `ListState`. `AppState::scroll` is the *stored* top row;
`tree::render` seeds `ListState::with_offset(state.scroll)` and
`.with_selected(Some(state.cursor))`, lets ratatui do the minimal adjustment
that keeps the cursor visible, then writes `list_state.offset()` back into
`state.scroll`. That is why `tree::render` (and therefore `ui::draw`) takes
`&mut AppState` — the scroll offset is the one piece of view state that only the
renderer knows the viewport height for. Everything else in `ui` is a pure
function of `&AppState`.

**Signature note for the caller:** the stub had `ui::draw(&mut Frame, &AppState)`
and `ui::tree::render(&mut Frame, Rect, &mut AppState)`, which cannot both be
true. `ui::draw` now takes `&mut AppState`. `main.rs` is unaffected — it already
passes a `&mut AppState`.

When the row count exceeds the viewport, a scrollbar is drawn **over the block's
right border**, so it costs no content width and the row geometry below is the
same scrolled or not.

---

## 2. Row grammar

`AppState::rows()` produces `Vec<RowRef>` — the visible lines, top to bottom.
Two shapes:

### Repo header — `RowRef::Repo`

```
 ▼ markdown-reader                    4 worktrees · 1.2 GB · 2 active
^^^                                   ^ right-aligned aggregate
| |name
| arrow: ▼ expanded / ▶ collapsed
leading space (column 0 of the block interior)
```

* Aggregate omits the `· N active` clause when the group has zero active
  worktrees.
* Size is `format::bytes(group.known_size())` — **known sizes only**, so the
  number never over-promises reclaimable space.
* Orphan buckets additionally carry `[orphan]` next to the name.
* When left + right do not both fit, the aggregate is dropped **first**, then
  the name is truncated with `…`. Concretely, the aggregate is shown only when
  the name still gets `min(len(name), HEADER_NAME_MIN = 16)` columns: knowing
  *which* repository you are looking at beats knowing its totals.

### Worktree row — `RowRef::Worktree`

```
   [x] ● feature-auth        412 MB  uncommitted changes  [locked] ⚑ 2
   ^^^ ^^^ ^                 ^^^^^^  ^^^^^^^^^^^^^^^^^^^  ^^^^^^^^ ^^^
   |   |   |                 |       reason                tags    stash hint
   |   |   name (fixed field, truncated with …)
   |   activity glyph
   checkbox
```

Fixed geometry (`ui::tree`):

| Constant | Value | Meaning |
|----------|-------|---------|
| `WT_PREFIX_W` | 9 | `"   [x] ● "` |
| `NAME_W` | 24 | name field when the terminal is wide enough |
| `NAME_MIN` | 4 | floor for the name field on narrow terminals |
| `SIZE_W` | 8 | right-aligned size column (`"1023 PB"` is the widest `format::bytes` output) |
| `GAP` | 2 | between the size column and the reason text |

The size column therefore sits at a stable x for every row of a frame, which is
what makes the column scannable. Everything after the gap (reason, tags, stash
hint) is free-flowing and simply gets clipped by the `List` when the terminal is
narrow. **Rows never wrap** — each `ListItem` is exactly one `Line`.

### Activity glyphs

The glyph carries a real severity distinction; do not collapse these.

| `ActivityState` | Glyph | Colour | Reason text | Meaning |
|-----------------|-------|--------|-------------|---------|
| `Active(Dirty)` | `●` | **red** | `uncommitted changes` | work would genuinely be **lost** |
| `Active(Unpushed)` | `●` | **yellow** | `unpushed commits` | *not* lost — `git worktree remove` deletes neither branches nor commits, they stay in the shared object store. Yellow means "still in use", not "will be destroyed" |
| `Active(Both)` | `●` | **red** | `uncommitted changes + unpushed commits` | red wins: the dirty part is the destructive part |
| `Clean` | `○` | dark gray + `DIM` | — | proven safe |
| `Pending` | spinner (`⠋⠙⠹⠸⠼⠴⠦⠧⠇⠏`, `spinner_tick % 10`) | cyan | `checking…` | not checked **yet** |
| `Unknown` | `?` | magenta | `status unknown` | the check **failed** |

Colour constants live in `ui::mod` (`C_DIRTY`, `C_UNPUSHED`, …) so a change is a
one-line edit rather than a hunt through three renderers.

`Pending` and `Unknown` are deliberately *not* dim-and-quiet: both are unsafe
(`ActivityState::needs_force()` is true for them) and magenta/cyan keep them
visually separate from both "clean" and "active". A size that has not arrived
yet renders as `format::bytes_opt(None)` → `—`; it is **never** rendered as
`0 B`, which would read as "nothing to reclaim".

### Tags and hints

* `[orphan]` — `Worktree::orphaned`, yellow. Owning repo is gone; purged by
  plain recursive delete.
* `[locked]` — `Worktree::locked.is_some()`, yellow. Forces the second gate.
* `[missing]` — `Worktree::prunable`, dark gray. Metadata-only record.
* `[outside]` — `Worktree::outside_scan_root`, dark gray. Lives outside the
  folder the user asked about, so removing it is more surprising than usual.
* `⚑ N` — stash count, dark gray, **informational only**. Stashes live in the
  shared stash stack and survive `git worktree remove`, so this never affects
  selection, sorting, `requires_force`, or anything else.

---

## 3. `AppState` invariants

`AppState` owns no I/O. Everything arrives through `from_scan`, `apply_enrich`
and `apply_purge_results`, which is what makes the whole interaction layer
testable without a disk.

1. **`rows()` is derived, never stored.** It reflects both collapsed groups and
   the current `SortMode`. Sorting is a *view* concern: `group_order()` and
   `worktree_order(g)` return permutations of indices and `groups` itself is
   never reordered. Consequences:
   * setting `state.sort` is sufficient — there is no `apply_sort()` to forget
     (this matters: `main.rs::rescan` assigns `next.sort` directly);
   * `RowRef` indices stay valid against `groups` regardless of sort;
   * selection is a field on `Worktree`, so it survives sorting for free.
2. **The cursor is an index into `rows()`, and never points at a hidden row.**
   Anything that can shrink or reorder `rows()` ends with `clamp_cursor()`.
   Collapsing a group moves the cursor up to that group's header rather than
   leaving it dangling in the hidden range.
3. **Focus is restored by path, not by index.** `focused_path()` returns the
   worktree path under the cursor, or the repo root when the cursor is on a
   header. `focus_path()` looks for that path among visible rows; if the target
   is a worktree hidden inside a collapsed group it falls back to that group's
   header. `cycle_sort()` and `apply_purge_results()` both round-trip through
   this pair, which is what "sorting keeps the cursor on the same worktree"
   means concretely.
4. **`apply_enrich` folds by path and ignores strangers.** A `Size`/`Activity`
   message for a path that no longer exists (removed by a concurrent purge, or
   dropped by a rescan) is a no-op, never a panic — background workers and the
   UI thread are not synchronised and never will be.
   `Stashes` is keyed by `git_common_dir` and maps counts onto worktrees by
   branch name; worktrees with no branch (detached HEAD) get 0.
5. **`apply_purge_results` drops only *successful* removals**, then drops groups
   that became empty, then restores focus by path and clamps. Failed items stay
   in the list, still selected, so the user can see and retry them.
6. **Aggregates count only known sizes.** `selected_size()` / `total_size()`
   sum `SizeState::bytes()`, skipping `Pending` and `Unknown`. We under-promise
   reclaimable space; we never guess.

### Sort orders

| Mode | Groups | Worktrees |
|------|--------|-----------|
| `Name` | name (case-insensitive) | name |
| `SizeDesc` | `known_size()` desc | size desc, **unknown last** — an unmeasured worktree is not "small", it is unmeasured |
| `ActiveFirst` | `active_count()` desc | `Both` → `Dirty` → `Unpushed` → `Unknown` → `Pending` → `Clean` |

Every comparison falls back to the name so the order is total and stable.

---

## 4. Selection semantics

| Key | Scope |
|-----|-------|
| `Space` on a worktree | toggle that worktree |
| `Space` on a repo header | select **all** of that group's worktrees; if they are already all selected, deselect them all |
| `a` | same as `Space`-on-header, but works from any row in the group |
| `A` | everything, everywhere — selects all unless everything is already selected, in which case it clears |

`a` and `A` are toggles rather than pure "select all" so there is always a
one-key way back out of an over-broad selection.

Navigation extras beyond the spec table, all inert inside a modal:
`Home`/`End` (same as `g`/`G`), `PageUp`/`PageDown` (±10 rows), and `Ctrl-C`,
which quits from **any** mode including a confirmation gate.

`←`/`h` on a *child* row walks up to its header rather than collapsing the group
out from under the cursor; on a header it collapses. `→`/`l` expands a collapsed
header and steps into an already-expanded one. `Enter` toggles the group the
cursor is in, matching the SCOPE keybinding table.

## 4b. Status bar

`" N repos · M worktrees · <total>"` on the left (plus a spinner while
enrichment runs, a `· N skipped` warning, and the toast), `selected N (size) ·
sort: <mode> · ? help` on the right, padded apart to fill the width exactly.
When both halves cannot fit, the **hints win** and the totals are clipped with
`…` — the hints are shorter and more actionable. One column is always held back
so the clipped text never runs straight into `sort:`.

---

## 5. The two-stage confirmation

This is the safety-critical path. The shape:

```
Browse --d/x--> Confirm(plan) --y/Enter--> [plan.needs_force()?]
                                              |  no  -> Action::Purge
                                              |  yes -> ForceConfirm(plan) --Shift+F--> Action::Purge
```

Rules, in the order they matter:

1. `d` / `x` in `Browse` calls `build_plan()`: every selected worktree, or —
   when nothing at all is selected — just the worktree under the cursor. A plan
   built from a repo-header cursor with no selection is empty; we show a toast
   and stay in `Browse` rather than opening an empty modal.
2. `PurgeItem::requires_force = activity.needs_force() || locked.is_some()`.
   `ActivityState::needs_force()` is true for `Pending` and `Unknown` as well as
   every `Active` variant: an unfinished or failed check is not evidence of
   safety.
3. Stage one (`Mode::Confirm`) accepts `y` **or** `Enter`. If
   `plan.needs_force()` it advances to `Mode::ForceConfirm` and returns
   `Action::None`. **It does not purge.**
4. Stage two (`Mode::ForceConfirm`) accepts **only uppercase `F`** (Shift+F).
   `y`, `Enter`, lowercase `f` and every navigation key are explicitly inert
   there. This is the whole point: a user who holds `Enter` or double-taps `y`
   out of muscle memory cannot get through both gates. The modal says so in
   plain words: *"Press F to force-remove"*.
5. `handle_key` deliberately leaves `state.mode` on `Confirm`/`ForceConfirm`
   when it returns `Action::Purge` — `main.rs::start_purge` reads the plan back
   out of the mode and is the one that sets `Mode::Purging`.
6. `Esc` / `q` / `n` cancel from either gate straight back to `Browse`.

Both modals show per-item name + size and the summed reclaim. The force modal
lists *only* the at-risk items with their concrete reason, and spells out the
asymmetry that the colours encode: uncommitted changes are destroyed, commits
and branches are not.

`Mode::Purging` swallows every key — there is no cancel once `git worktree
remove` is running, and pretending otherwise would be a lie.

The two gate keys are exported as `events::CONFIRM_KEY` (`'y'`) and
`events::FORCE_KEY` (`'F'`), and both modals *and* the help overlay render them
from those constants, so the UI can never advertise a key the handler does not
honour. `the_force_modal_matches_the_behaviour_it_advertises` asserts exactly
that.

---

## 6. Testing strategy

* **Logic** — unit tests inline in `state.rs` and `events.rs`, built from
  `model` types directly (no scanner, no git, no disk).
* **Rendering** — `tests/ui_snapshot.rs` drives ratatui's `TestBackend` through
  `insta`.

`TestBackend`'s `Display` (what `insta` snapshots) contains **symbols only** —
no style information. So every snapshot that exists to protect a *colour* is
paired with an explicit cell assertion, e.g. "the glyph at (x, y) is `●` and its
`fg` is `Color::Red`". A snapshot alone cannot catch a dirty row silently
turning yellow.

The assertion bar for everything here: a test must fail against a no-op or
trivially-broken implementation. Concretely that means pinning exact cell
content at exact positions, comparing row counts before/after a mutation, or
diffing against a baseline render taken without the feature — never
`output.contains('●')`, since `●`, `─` and `│` all show up in unrelated chrome.

Two helpers in `tests/ui_snapshot.rs` exist purely to keep assertions honest:

* `modal_text()` extracts only what is *inside* the rounded overlay box. Without
  it, "the force gate does not list the clean worktree" would pass or fail on
  whether the browse view behind the modal happened to mention that name.
* `cols()` (in `ui::tree`'s unit tests) slices by display column, not by byte —
  `●`, `○`, `—` and `…` are all multi-byte, so byte slicing both panics and
  silently misaligns.

Column constants used by the cell assertions, for the 78×N fixture:
`ARROW_X = 2` (repo arrow), `CHECK_X = 5` (checkbox marker),
`GLYPH_X = 8` (activity glyph).
