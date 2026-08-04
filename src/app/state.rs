//! The view-model: everything the UI renders and every keystroke mutates.
//!
//! `AppState` owns no I/O. Scanning and enrichment happen elsewhere and are
//! folded in via [`AppState::from_scan`] and [`AppState::apply_enrich`], which
//! keeps the whole interaction layer testable without touching a disk.

use std::path::{Path, PathBuf};

use crate::model::{
    ActivityState, EnrichMsg, PurgeItem, PurgeOutcome, PurgePlan, RepoGroup, RowRef, ScanResult,
    SortMode, Worktree,
};

/// What the app is currently showing. Modal states capture the data they need
/// so the browse view underneath stays untouched.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Mode {
    Browse,
    /// Reviewing a batch before removal.
    Confirm(PurgePlan),
    /// Second gate, shown only when the batch contains worktrees that would
    /// lose work. Requires a distinct keypress from the first confirmation.
    ForceConfirm(PurgePlan),
    /// Removal in flight.
    Purging,
    /// Per-item results of the last batch.
    Results(Vec<PurgeOutcome>),
    Help,
}

impl Mode {
    /// True while an overlay owns the keyboard, so the browse bindings must not
    /// fire underneath it.
    pub fn is_modal(&self) -> bool {
        !matches!(self, Mode::Browse)
    }
}

/// Complete application state.
#[derive(Debug, Clone)]
pub struct AppState {
    /// Folder the user asked us to scan.
    pub root: PathBuf,
    pub groups: Vec<RepoGroup>,
    /// Index into the currently visible rows (see [`AppState::rows`]).
    pub cursor: usize,
    /// First visible row, maintained by the tree renderer.
    pub scroll: usize,
    pub sort: SortMode,
    pub mode: Mode,
    /// True while background enrichment is still streaming in.
    pub enriching: bool,
    /// Directories that could not be read during the scan.
    pub skipped: Vec<PathBuf>,
    pub warnings: Vec<String>,
    /// Transient message shown in the status bar (e.g. "freed 1.2 GB").
    pub toast: Option<String>,
    /// Animation counter for pending-value spinners.
    pub spinner_tick: usize,
    /// Set when the user asks to quit.
    pub should_quit: bool,
}

impl AppState {
    /// Build state from a completed scan.
    pub fn from_scan(root: PathBuf, scan: ScanResult) -> Self {
        AppState {
            root,
            groups: scan.groups,
            cursor: 0,
            scroll: 0,
            sort: SortMode::default(),
            mode: Mode::Browse,
            enriching: false,
            skipped: scan.skipped,
            warnings: scan.warnings,
            toast: None,
            spinner_tick: 0,
            should_quit: false,
        }
    }

    // -- row model ---------------------------------------------------------
    //
    // Sorting is a *view* concern: `groups` is never reordered, only the order
    // in which rows are emitted changes. That keeps `RowRef` indices valid
    // across a sort change and means assigning `state.sort` is sufficient —
    // there is no separate "apply" step for a caller to forget.

    /// Group indices in display order.
    fn group_order(&self) -> Vec<usize> {
        let mut order: Vec<usize> = (0..self.groups.len()).collect();
        let sort = self.sort;
        order.sort_by(|&a, &b| {
            let (ga, gb) = (&self.groups[a], &self.groups[b]);
            let primary = match sort {
                SortMode::Name => std::cmp::Ordering::Equal,
                SortMode::SizeDesc => gb.known_size().cmp(&ga.known_size()),
                SortMode::ActiveFirst => gb.active_count().cmp(&ga.active_count()),
            };
            primary.then_with(|| name_key(&ga.name).cmp(&name_key(&gb.name)))
        });
        order
    }

    /// Worktree indices within `group`, in display order.
    fn worktree_order(&self, group: usize) -> Vec<usize> {
        let Some(g) = self.groups.get(group) else {
            return Vec::new();
        };
        let mut order: Vec<usize> = (0..g.worktrees.len()).collect();
        let sort = self.sort;
        order.sort_by(|&a, &b| {
            let (wa, wb) = (&g.worktrees[a], &g.worktrees[b]);
            let primary = match sort {
                SortMode::Name => std::cmp::Ordering::Equal,
                // Unknown sizes sort last rather than as zero: we do not know
                // that they are small, only that we have not measured them.
                SortMode::SizeDesc => size_key(wa).cmp(&size_key(wb)),
                SortMode::ActiveFirst => {
                    activity_rank(wa.activity).cmp(&activity_rank(wb.activity))
                }
            };
            primary.then_with(|| name_key(&wa.name).cmp(&name_key(&wb.name)))
        });
        order
    }

    /// Visible rows, top to bottom, honouring collapsed groups.
    pub fn rows(&self) -> Vec<RowRef> {
        let mut rows = Vec::new();
        for group in self.group_order() {
            rows.push(RowRef::Repo { group });
            if self.groups[group].expanded {
                for worktree in self.worktree_order(group) {
                    rows.push(RowRef::Worktree { group, worktree });
                }
            }
        }
        rows
    }

    /// The row under the cursor, if any.
    pub fn current_row(&self) -> Option<RowRef> {
        self.rows().get(self.cursor).copied()
    }

    /// The worktree under the cursor, if the cursor is on one.
    pub fn current_worktree(&self) -> Option<&Worktree> {
        match self.current_row()? {
            RowRef::Repo { .. } => None,
            RowRef::Worktree { group, worktree } => self.groups[group].worktrees.get(worktree),
        }
    }

    /// The path a row stands for: the worktree's, or the repo's root.
    fn row_path(&self, row: RowRef) -> Option<&Path> {
        match row {
            RowRef::Repo { group } => self.groups.get(group).map(|g| g.root.as_path()),
            RowRef::Worktree { group, worktree } => self
                .groups
                .get(group)
                .and_then(|g| g.worktrees.get(worktree))
                .map(|w| w.path.as_path()),
        }
    }

    /// Path of the focused worktree, used to keep the cursor on the same item
    /// across sorting and rescans.
    ///
    /// Falls back to the repository root when the cursor is on a group header,
    /// so a rescan restores the cursor in that case too.
    pub fn focused_path(&self) -> Option<PathBuf> {
        let row = self.current_row()?;
        self.row_path(row).map(Path::to_path_buf)
    }

    /// Move the cursor to the row showing `path`, if it is visible.
    ///
    /// When `path` is a worktree hidden inside a collapsed group, the cursor
    /// lands on that group's header instead — the closest visible row that
    /// still represents it. Collapsing is never undone behind the user's back.
    pub fn focus_path(&mut self, path: &Path) {
        let rows = self.rows();
        if let Some(i) = rows
            .iter()
            .position(|&r| self.row_path(r) == Some(path))
            .or_else(|| {
                let g = self
                    .groups
                    .iter()
                    .position(|g| g.worktrees.iter().any(|w| w.path == path))?;
                rows.iter().position(|&r| r == RowRef::Repo { group: g })
            })
        {
            self.cursor = i;
        }
    }

    /// Keep the cursor inside the visible rows. Called after anything that can
    /// shrink or reorder the row list.
    fn clamp_cursor(&mut self) {
        let len = self.rows().len();
        self.cursor = self.cursor.min(len.saturating_sub(1));
    }

    /// Move the cursor by `delta` rows, clamped to the list.
    pub fn move_cursor(&mut self, delta: isize) {
        let len = self.rows().len();
        if len == 0 {
            self.cursor = 0;
            return;
        }
        let last = (len - 1) as isize;
        let next = (self.cursor as isize).saturating_add(delta).clamp(0, last);
        self.cursor = next as usize;
    }

    /// Jump to the first row.
    pub fn cursor_top(&mut self) {
        self.cursor = 0;
    }

    /// Jump to the last visible row.
    pub fn cursor_bottom(&mut self) {
        self.cursor = self.rows().len().saturating_sub(1);
    }

    /// Group index under the cursor, if any.
    pub fn current_group(&self) -> Option<usize> {
        self.current_row().map(RowRef::group)
    }

    /// Move the cursor onto the header of `group`, if that header is visible.
    fn focus_group_header(&mut self, group: usize) {
        if let Some(i) = self
            .rows()
            .iter()
            .position(|&r| r == RowRef::Repo { group })
        {
            self.cursor = i;
        }
    }

    /// Expand or collapse the group under the cursor.
    ///
    /// Collapsing pulls the cursor up onto the group header, because the row it
    /// was on is about to stop existing.
    pub fn set_expanded(&mut self, expanded: bool) {
        let Some(group) = self.current_group() else {
            return;
        };
        self.groups[group].expanded = expanded;
        if !expanded {
            self.focus_group_header(group);
        }
        self.clamp_cursor();
    }

    /// Toggle expansion of the group under the cursor.
    pub fn toggle_expanded(&mut self) {
        let Some(group) = self.current_group() else {
            return;
        };
        let expanded = !self.groups[group].expanded;
        self.set_expanded(expanded);
    }

    /// Toggle selection of the item under the cursor. On a repo header this
    /// selects or deselects every worktree in that group.
    pub fn toggle_selection(&mut self) {
        match self.current_row() {
            Some(RowRef::Worktree { group, worktree }) => {
                if let Some(w) = self.groups[group].worktrees.get_mut(worktree) {
                    w.selected = !w.selected;
                }
            }
            Some(RowRef::Repo { group }) => {
                let target = !self.groups[group].all_selected();
                for w in &mut self.groups[group].worktrees {
                    w.selected = target;
                }
            }
            None => {}
        }
    }

    /// Select or deselect every worktree in the group under the cursor.
    pub fn select_group(&mut self, selected: bool) {
        let Some(group) = self.current_group() else {
            return;
        };
        for w in &mut self.groups[group].worktrees {
            w.selected = selected;
        }
    }

    /// True when every worktree in the group under the cursor is selected.
    pub fn current_group_all_selected(&self) -> bool {
        self.current_group()
            .map(|g| self.groups[g].all_selected())
            .unwrap_or(false)
    }

    /// Select or deselect every worktree everywhere.
    pub fn select_all(&mut self, selected: bool) {
        for g in &mut self.groups {
            for w in &mut g.worktrees {
                w.selected = selected;
            }
        }
    }

    /// True when every discovered worktree is selected (and there is at least
    /// one).
    pub fn all_selected(&self) -> bool {
        self.worktree_count() > 0 && self.selected_count() == self.worktree_count()
    }

    /// Advance the sort mode, keeping the cursor on the same worktree.
    pub fn cycle_sort(&mut self) {
        let focused = self.focused_path();
        self.sort = self.sort.next();
        if let Some(path) = focused {
            self.focus_path(&path);
        }
        self.clamp_cursor();
    }

    /// Fold one background result into the model.
    ///
    /// Messages naming a path (or repository) we no longer know about are
    /// dropped: workers are not synchronised with rescans or purges, so a
    /// stale message is normal, not exceptional.
    pub fn apply_enrich(&mut self, msg: EnrichMsg) {
        match msg {
            EnrichMsg::Size { path, state } => {
                if let Some(w) = crate::model::find_worktree_mut(&mut self.groups, &path) {
                    w.size = state;
                }
            }
            EnrichMsg::Activity { path, state } => {
                if let Some(w) = crate::model::find_worktree_mut(&mut self.groups, &path) {
                    w.activity = state;
                }
            }
            EnrichMsg::Stashes {
                git_common_dir,
                counts,
            } => {
                if let Some(g) = self
                    .groups
                    .iter_mut()
                    .find(|g| g.git_common_dir == git_common_dir)
                {
                    for w in &mut g.worktrees {
                        w.stash_count = w
                            .branch
                            .as_deref()
                            .and_then(|b| counts.get(b))
                            .copied()
                            .unwrap_or(0);
                    }
                }
            }
            EnrichMsg::Done => self.enriching = false,
        }
    }

    /// Turn one worktree into a plan item.
    fn plan_item(&self, group: usize, worktree: usize) -> Option<PurgeItem> {
        let g = self.groups.get(group)?;
        let w = g.worktrees.get(worktree)?;
        Some(PurgeItem {
            worktree_path: w.path.clone(),
            // No owning repository left to run `git worktree remove` from.
            repo_root: if g.orphaned || w.orphaned {
                None
            } else {
                Some(g.root.clone())
            },
            name: w.name.clone(),
            size: w.size.bytes(),
            // Pending and Unknown count as needing force: an unfinished or
            // failed check is not evidence that a worktree is safe.
            requires_force: w.activity.needs_force() || w.locked.is_some(),
        })
    }

    /// Build a removal plan from the current selection. Falls back to the
    /// worktree under the cursor when nothing is selected.
    pub fn build_plan(&self) -> PurgePlan {
        let mut items = Vec::new();
        if self.selected_count() > 0 {
            for group in self.group_order() {
                for worktree in self.worktree_order(group) {
                    if self.groups[group].worktrees[worktree].selected
                        && let Some(item) = self.plan_item(group, worktree)
                    {
                        items.push(item);
                    }
                }
            }
        } else if let Some(RowRef::Worktree { group, worktree }) = self.current_row()
            && let Some(item) = self.plan_item(group, worktree)
        {
            items.push(item);
        }
        PurgePlan { items }
    }

    /// Drop worktrees that were successfully removed and refresh totals.
    ///
    /// Failures are left in place (and left selected) so the user can see what
    /// did not go away and try again.
    pub fn apply_purge_results(&mut self, outcomes: &[PurgeOutcome]) {
        let removed: Vec<&Path> = outcomes
            .iter()
            .filter(|o| o.succeeded())
            .map(|o| o.item.worktree_path.as_path())
            .collect();
        if removed.is_empty() {
            self.clamp_cursor();
            return;
        }

        let focused = self.focused_path();
        for g in &mut self.groups {
            g.worktrees.retain(|w| !removed.contains(&w.path.as_path()));
        }
        self.groups.retain(|g| !g.worktrees.is_empty());

        if let Some(path) = focused.as_deref()
            && !removed.contains(&path)
        {
            self.focus_path(path);
        }
        self.clamp_cursor();
    }

    pub fn selected_count(&self) -> usize {
        self.groups.iter().map(RepoGroup::selected_count).sum()
    }

    /// Known bytes across selected worktrees.
    pub fn selected_size(&self) -> u64 {
        self.groups
            .iter()
            .flat_map(|g| g.worktrees.iter())
            .filter(|w| w.selected)
            .filter_map(|w| w.size.bytes())
            .sum()
    }

    /// Known bytes across every discovered worktree.
    pub fn total_size(&self) -> u64 {
        self.groups.iter().map(RepoGroup::known_size).sum()
    }

    pub fn worktree_count(&self) -> usize {
        self.groups.iter().map(|g| g.worktrees.len()).sum()
    }

    pub fn repo_count(&self) -> usize {
        self.groups.len()
    }
}

/// Case-insensitive display ordering, with the original as a tie-break so the
/// order is total and stable.
fn name_key(name: &str) -> (String, String) {
    (name.to_lowercase(), name.to_string())
}

/// Sort key for size-descending: known sizes first (largest first), unknown
/// last.
fn size_key(w: &Worktree) -> (u8, std::cmp::Reverse<u64>) {
    match w.size.bytes() {
        Some(n) => (0, std::cmp::Reverse(n)),
        None => (1, std::cmp::Reverse(0)),
    }
}

/// Severity ordering for `ActiveFirst`: the rows most likely to *lose* work
/// float to the top, then the ones we could not prove safe, then clean ones.
fn activity_rank(a: ActivityState) -> u8 {
    use crate::model::ActiveReason::{Both, Dirty, Unpushed};
    match a {
        ActivityState::Active(Both) => 0,
        ActivityState::Active(Dirty) => 1,
        ActivityState::Active(Unpushed) => 2,
        ActivityState::Unknown => 3,
        ActivityState::Pending => 4,
        ActivityState::Clean => 5,
    }
}

#[cfg(test)]
pub(crate) mod tests {
    use super::*;
    use crate::model::{ActiveReason, ScanResult, SizeState};
    use std::collections::HashMap;

    /// Build a worktree with an explicit size/activity, rooted under `/scan`.
    pub(crate) fn wt(name: &str, size: Option<u64>, activity: ActivityState) -> Worktree {
        let mut w = Worktree::new(PathBuf::from(format!("/scan/{name}")));
        w.branch = Some(name.to_string());
        w.size = size.map_or(SizeState::Pending, SizeState::Ready);
        w.activity = activity;
        w
    }

    pub(crate) fn group(name: &str, worktrees: Vec<Worktree>) -> RepoGroup {
        let mut g = RepoGroup::new(
            PathBuf::from(format!("/scan/{name}")),
            PathBuf::from(format!("/scan/{name}/.git")),
        );
        g.worktrees = worktrees;
        g
    }

    pub(crate) fn state(groups: Vec<RepoGroup>) -> AppState {
        AppState::from_scan(
            PathBuf::from("/scan"),
            ScanResult {
                groups,
                ..Default::default()
            },
        )
    }

    /// Two groups: `alpha` (3 worktrees) and `beta` (1).
    pub(crate) fn sample() -> AppState {
        state(vec![
            group(
                "alpha",
                vec![
                    wt(
                        "feature-auth",
                        Some(400),
                        ActivityState::Active(ActiveReason::Dirty),
                    ),
                    wt(
                        "fix-parser",
                        Some(300),
                        ActivityState::Active(ActiveReason::Unpushed),
                    ),
                    wt("old-experiment", Some(900), ActivityState::Clean),
                ],
            ),
            group("beta", vec![wt("solo", Some(50), ActivityState::Clean)]),
        ])
    }

    /// Human-readable row list, for assertions that must pin exact structure.
    fn row_names(s: &AppState) -> Vec<String> {
        s.rows()
            .iter()
            .map(|&r| match r {
                RowRef::Repo { group } => format!("repo:{}", s.groups[group].name),
                RowRef::Worktree { group, worktree } => {
                    format!("wt:{}", s.groups[group].worktrees[worktree].name)
                }
            })
            .collect()
    }

    #[test]
    fn rows_list_headers_then_children_in_name_order() {
        let s = sample();
        assert_eq!(
            row_names(&s),
            vec![
                "repo:alpha",
                "wt:feature-auth",
                "wt:fix-parser",
                "wt:old-experiment",
                "repo:beta",
                "wt:solo",
            ]
        );
    }

    #[test]
    fn collapsing_hides_children_and_parks_the_cursor_on_the_header() {
        let mut s = sample();
        let before = s.rows().len();
        // Cursor on the last worktree of `alpha`.
        s.cursor = 3;
        assert_eq!(
            s.current_row(),
            Some(RowRef::Worktree {
                group: 0,
                worktree: 2
            })
        );

        s.set_expanded(false);

        // Exactly the three children disappeared — not "some rows changed".
        assert_eq!(s.rows().len(), before - 3);
        assert_eq!(row_names(&s), vec!["repo:alpha", "repo:beta", "wt:solo"]);
        // ...and the cursor is on alpha's header, which is a *visible* row.
        assert_eq!(s.cursor, 0);
        assert_eq!(s.current_row(), Some(RowRef::Repo { group: 0 }));
        assert!(s.cursor < s.rows().len());
    }

    #[test]
    fn cursor_never_lands_on_a_hidden_row_after_collapsing_the_last_group() {
        let mut s = sample();
        s.cursor = 5; // wt:solo, the last row
        s.set_expanded(false);
        assert_eq!(
            row_names(&s),
            vec![
                "repo:alpha",
                "wt:feature-auth",
                "wt:fix-parser",
                "wt:old-experiment",
                "repo:beta"
            ]
        );
        assert_eq!(s.cursor, 4);
        assert_eq!(s.current_row(), Some(RowRef::Repo { group: 1 }));
    }

    #[test]
    fn expanding_restores_the_children() {
        let mut s = sample();
        s.cursor = 0;
        s.set_expanded(false);
        assert_eq!(s.rows().len(), 3);
        s.set_expanded(true);
        assert_eq!(s.rows().len(), 6);
        assert_eq!(row_names(&s)[1], "wt:feature-auth");
    }

    #[test]
    fn move_cursor_clamps_at_both_ends() {
        let mut s = sample();
        s.move_cursor(-5);
        assert_eq!(s.cursor, 0);
        s.move_cursor(100);
        assert_eq!(s.cursor, s.rows().len() - 1);
        // isize::MIN must not overflow the arithmetic.
        s.move_cursor(isize::MIN);
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn move_cursor_on_an_empty_tree_is_a_noop() {
        let mut s = state(vec![]);
        s.move_cursor(3);
        assert_eq!(s.cursor, 0);
        assert_eq!(s.current_row(), None);
        assert!(s.build_plan().is_empty());
    }

    #[test]
    fn space_on_a_header_selects_then_deselects_the_whole_group() {
        let mut s = sample();
        s.cursor = 0; // repo:alpha

        s.toggle_selection();
        assert_eq!(s.groups[0].selected_count(), 3);
        assert_eq!(s.groups[1].selected_count(), 0, "must not touch beta");
        assert_eq!(s.selected_count(), 3);

        s.toggle_selection();
        assert_eq!(s.groups[0].selected_count(), 0);
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn header_toggle_selects_the_remainder_when_partially_selected() {
        let mut s = sample();
        s.cursor = 1;
        s.toggle_selection(); // one child selected
        assert_eq!(s.groups[0].selected_count(), 1);
        s.cursor = 0;
        s.toggle_selection(); // header: not all selected -> select all
        assert_eq!(s.groups[0].selected_count(), 3);
    }

    #[test]
    fn space_on_a_worktree_toggles_only_that_worktree() {
        let mut s = sample();
        s.cursor = 2; // wt:fix-parser
        s.toggle_selection();
        assert!(s.groups[0].worktrees[1].selected);
        assert!(!s.groups[0].worktrees[0].selected);
        assert!(!s.groups[0].worktrees[2].selected);
        assert_eq!(s.selected_count(), 1);
    }

    #[test]
    fn select_group_and_select_all_have_different_scope() {
        let mut s = sample();
        s.cursor = 2; // inside alpha
        s.select_group(true);
        assert_eq!(s.groups[0].selected_count(), 3);
        assert_eq!(s.groups[1].selected_count(), 0);
        assert_eq!(s.selected_count(), 3);

        s.select_all(true);
        assert_eq!(s.groups[1].selected_count(), 1);
        assert_eq!(s.selected_count(), 4);

        s.select_all(false);
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn selection_survives_collapse_and_expand() {
        let mut s = sample();
        s.cursor = 1;
        s.toggle_selection();
        let path = s.groups[0].worktrees[0].path.clone();

        s.cursor = 0;
        s.set_expanded(false);
        s.set_expanded(true);

        let w = s.groups[0]
            .worktrees
            .iter()
            .find(|w| w.path == path)
            .expect("worktree still present");
        assert!(w.selected);
        assert_eq!(s.selected_count(), 1);
    }

    #[test]
    fn sorting_by_size_reorders_rows_and_keeps_the_cursor_on_the_same_worktree() {
        let mut s = sample();
        s.cursor = 1; // wt:feature-auth (400)
        let focused = s.focused_path().unwrap();
        assert_eq!(focused, PathBuf::from("/scan/feature-auth"));

        s.cycle_sort(); // Name -> SizeDesc
        assert_eq!(s.sort, SortMode::SizeDesc);

        // alpha (1600 known) still before beta (50); children by size desc.
        assert_eq!(
            row_names(&s),
            vec![
                "repo:alpha",
                "wt:old-experiment",
                "wt:feature-auth",
                "wt:fix-parser",
                "repo:beta",
                "wt:solo",
            ]
        );
        // The cursor followed the worktree, not the index.
        assert_eq!(s.cursor, 2);
        assert_eq!(s.focused_path().unwrap(), focused);
    }

    #[test]
    fn sorting_by_active_puts_the_most_destructive_rows_first() {
        let mut s = sample();
        s.sort = SortMode::ActiveFirst;
        assert_eq!(
            row_names(&s),
            vec![
                "repo:alpha",
                "wt:feature-auth",   // Dirty
                "wt:fix-parser",     // Unpushed
                "wt:old-experiment", // Clean
                "repo:beta",
                "wt:solo",
            ]
        );
    }

    #[test]
    fn sort_cycles_back_to_name_and_restores_the_original_order() {
        let mut s = sample();
        let original = row_names(&s);
        s.cursor = 3; // wt:old-experiment
        let focused = s.focused_path().unwrap();

        s.cycle_sort(); // SizeDesc
        s.cycle_sort(); // ActiveFirst
        s.cycle_sort(); // Name

        assert_eq!(s.sort, SortMode::Name);
        assert_eq!(row_names(&s), original);
        assert_eq!(s.focused_path().unwrap(), focused);
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn unknown_sizes_sort_after_known_ones_not_as_zero() {
        let mut s = state(vec![group(
            "r",
            vec![
                wt("pending", None, ActivityState::Clean),
                wt("tiny", Some(1), ActivityState::Clean),
            ],
        )]);
        s.sort = SortMode::SizeDesc;
        assert_eq!(row_names(&s), vec!["repo:r", "wt:tiny", "wt:pending"]);
    }

    #[test]
    fn sorting_keeps_the_cursor_on_a_repo_header() {
        let mut s = sample();
        s.cursor = 4; // repo:beta
        s.sort = SortMode::SizeDesc;
        s.focus_path(&PathBuf::from("/scan/beta"));
        assert_eq!(s.current_row(), Some(RowRef::Repo { group: 1 }));
    }

    #[test]
    fn focus_path_falls_back_to_the_header_when_the_target_is_collapsed() {
        let mut s = sample();
        s.cursor = 0;
        s.set_expanded(false);
        s.cursor = 2; // wt:solo in beta
        s.focus_path(&PathBuf::from("/scan/fix-parser"));
        assert_eq!(s.current_row(), Some(RowRef::Repo { group: 0 }));
    }

    #[test]
    fn focus_path_leaves_the_cursor_alone_for_an_unknown_path() {
        let mut s = sample();
        s.cursor = 2;
        s.focus_path(&PathBuf::from("/scan/does-not-exist"));
        assert_eq!(s.cursor, 2);
    }

    #[test]
    fn build_plan_falls_back_to_the_cursor_row_when_nothing_is_selected() {
        let mut s = sample();
        s.cursor = 2; // wt:fix-parser
        assert_eq!(s.selected_count(), 0);

        let plan = s.build_plan();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.items[0].name, "fix-parser");
        assert_eq!(
            plan.items[0].worktree_path,
            PathBuf::from("/scan/fix-parser")
        );
        assert_eq!(plan.items[0].size, Some(300));
        assert_eq!(plan.items[0].repo_root, Some(PathBuf::from("/scan/alpha")));
        assert_eq!(plan.total_size(), 300);
    }

    #[test]
    fn build_plan_ignores_the_cursor_once_anything_is_selected() {
        let mut s = sample();
        s.cursor = 3; // wt:old-experiment
        s.toggle_selection();
        s.cursor = 1; // move onto a *different*, unselected worktree

        let plan = s.build_plan();
        assert_eq!(plan.len(), 1);
        assert_eq!(plan.items[0].name, "old-experiment");
    }

    #[test]
    fn build_plan_on_a_header_with_no_selection_is_empty() {
        let mut s = sample();
        s.cursor = 0;
        assert!(s.build_plan().is_empty());
    }

    #[test]
    fn build_plan_collects_selections_across_groups() {
        let mut s = sample();
        s.select_all(true);
        let plan = s.build_plan();
        assert_eq!(
            plan.items
                .iter()
                .map(|i| i.name.as_str())
                .collect::<Vec<_>>(),
            vec!["feature-auth", "fix-parser", "old-experiment", "solo"]
        );
        assert_eq!(plan.total_size(), 400 + 300 + 900 + 50);
    }

    #[test]
    fn pending_and_unknown_activity_require_force() {
        let mut s = state(vec![group(
            "r",
            vec![
                wt("a-pending", Some(1), ActivityState::Pending),
                wt("b-unknown", Some(2), ActivityState::Unknown),
                wt("c-clean", Some(3), ActivityState::Clean),
            ],
        )]);
        s.select_all(true);
        let plan = s.build_plan();
        assert_eq!(
            plan.items
                .iter()
                .map(|i| (i.name.as_str(), i.requires_force))
                .collect::<Vec<_>>(),
            vec![("a-pending", true), ("b-unknown", true), ("c-clean", false)]
        );
        assert!(plan.needs_force());
        assert_eq!(plan.force_items().count(), 2);
    }

    #[test]
    fn a_locked_but_clean_worktree_still_requires_force() {
        let mut w = wt("locked", Some(10), ActivityState::Clean);
        w.locked = Some("in use".into());
        w.selected = true;
        let s = state(vec![group("r", vec![w])]);
        let plan = s.build_plan();
        assert!(plan.items[0].requires_force);
    }

    #[test]
    fn orphans_plan_without_a_repo_root() {
        let mut w = wt("stray", Some(10), ActivityState::Clean);
        w.orphaned = true;
        w.selected = true;
        let s = state(vec![group("r", vec![w])]);
        let plan = s.build_plan();
        assert_eq!(plan.items[0].repo_root, None);
    }

    #[test]
    fn apply_enrich_updates_the_named_row_only() {
        let mut s = sample();
        s.apply_enrich(EnrichMsg::Size {
            path: PathBuf::from("/scan/fix-parser"),
            state: SizeState::Ready(4242),
        });
        assert_eq!(s.groups[0].worktrees[1].size, SizeState::Ready(4242));
        // The neighbours are untouched.
        assert_eq!(s.groups[0].worktrees[0].size, SizeState::Ready(400));
        assert_eq!(s.groups[0].worktrees[2].size, SizeState::Ready(900));

        s.apply_enrich(EnrichMsg::Activity {
            path: PathBuf::from("/scan/old-experiment"),
            state: ActivityState::Active(ActiveReason::Both),
        });
        assert_eq!(
            s.groups[0].worktrees[2].activity,
            ActivityState::Active(ActiveReason::Both)
        );
        assert_eq!(
            s.groups[0].worktrees[0].activity,
            ActivityState::Active(ActiveReason::Dirty)
        );
    }

    #[test]
    fn apply_enrich_ignores_a_path_we_no_longer_know() {
        let mut s = sample();
        let before = s.groups.clone();
        s.apply_enrich(EnrichMsg::Size {
            path: PathBuf::from("/scan/vanished"),
            state: SizeState::Ready(999),
        });
        s.apply_enrich(EnrichMsg::Activity {
            path: PathBuf::from("/scan/vanished"),
            state: ActivityState::Clean,
        });
        s.apply_enrich(EnrichMsg::Stashes {
            git_common_dir: PathBuf::from("/nowhere/.git"),
            counts: HashMap::from([("feature-auth".to_string(), 7)]),
        });
        assert_eq!(s.groups, before, "state must be byte-identical");
    }

    #[test]
    fn stash_counts_map_by_branch_and_default_to_zero() {
        let mut s = sample();
        s.groups[0].worktrees[2].branch = None; // detached HEAD
        s.apply_enrich(EnrichMsg::Stashes {
            git_common_dir: PathBuf::from("/scan/alpha/.git"),
            counts: HashMap::from([("feature-auth".to_string(), 2)]),
        });
        assert_eq!(s.groups[0].worktrees[0].stash_count, 2);
        assert_eq!(s.groups[0].worktrees[1].stash_count, 0);
        assert_eq!(s.groups[0].worktrees[2].stash_count, 0);
        // The other repo is untouched.
        assert_eq!(s.groups[1].worktrees[0].stash_count, 0);
    }

    #[test]
    fn stash_counts_never_affect_safety() {
        let mut s = sample();
        s.groups[1].worktrees[0].stash_count = 5;
        s.groups[1].worktrees[0].selected = true;
        let plan = s.build_plan();
        assert_eq!(plan.len(), 1);
        assert!(
            !plan.items[0].requires_force,
            "a stash survives worktree removal and must not gate the purge"
        );
    }

    #[test]
    fn apply_enrich_done_clears_the_enriching_flag() {
        let mut s = sample();
        s.enriching = true;
        s.apply_enrich(EnrichMsg::Done);
        assert!(!s.enriching);
    }

    fn outcome(name: &str, ok: bool) -> PurgeOutcome {
        PurgeOutcome {
            item: PurgeItem {
                worktree_path: PathBuf::from(format!("/scan/{name}")),
                repo_root: None,
                name: name.to_string(),
                size: Some(1),
                requires_force: false,
            },
            result: if ok { Ok(1) } else { Err("boom".into()) },
        }
    }

    #[test]
    fn apply_purge_results_removes_only_successes() {
        let mut s = sample();
        let before = s.worktree_count();
        s.apply_purge_results(&[outcome("feature-auth", true), outcome("fix-parser", false)]);

        assert_eq!(s.worktree_count(), before - 1);
        assert_eq!(
            row_names(&s),
            vec![
                "repo:alpha",
                "wt:fix-parser",
                "wt:old-experiment",
                "repo:beta",
                "wt:solo"
            ]
        );
    }

    #[test]
    fn apply_purge_results_drops_emptied_groups_and_clamps_the_cursor() {
        let mut s = sample();
        s.cursor = 5; // wt:solo, the last row
        s.apply_purge_results(&[outcome("solo", true)]);

        assert_eq!(s.repo_count(), 1, "beta had only one worktree");
        assert_eq!(
            row_names(&s),
            vec![
                "repo:alpha",
                "wt:feature-auth",
                "wt:fix-parser",
                "wt:old-experiment"
            ]
        );
        assert_eq!(s.cursor, 3, "clamped to the new last row");
        assert!(s.current_row().is_some());
    }

    #[test]
    fn apply_purge_results_keeps_the_cursor_on_a_surviving_worktree() {
        let mut s = sample();
        s.cursor = 3; // wt:old-experiment
        s.apply_purge_results(&[outcome("feature-auth", true)]);
        assert_eq!(s.cursor, 2);
        assert_eq!(
            s.focused_path().unwrap(),
            PathBuf::from("/scan/old-experiment")
        );
    }

    #[test]
    fn apply_purge_results_of_only_failures_changes_nothing() {
        let mut s = sample();
        s.cursor = 3;
        let before = s.groups.clone();
        s.apply_purge_results(&[outcome("feature-auth", false)]);
        assert_eq!(s.groups, before);
        assert_eq!(s.cursor, 3);
    }

    #[test]
    fn purging_everything_leaves_an_empty_but_usable_state() {
        let mut s = sample();
        s.cursor = 5;
        let all: Vec<PurgeOutcome> = ["feature-auth", "fix-parser", "old-experiment", "solo"]
            .iter()
            .map(|n| outcome(n, true))
            .collect();
        s.apply_purge_results(&all);
        assert_eq!(s.repo_count(), 0);
        assert_eq!(s.rows().len(), 0);
        assert_eq!(s.cursor, 0);
        assert_eq!(s.current_row(), None);
    }

    #[test]
    fn aggregates_count_only_known_sizes() {
        let mut s = state(vec![group(
            "r",
            vec![
                wt("known", Some(1000), ActivityState::Clean),
                wt("pending", None, ActivityState::Pending),
            ],
        )]);
        assert_eq!(s.total_size(), 1000);
        assert_eq!(s.selected_size(), 0);

        s.select_all(true);
        assert_eq!(s.selected_count(), 2);
        assert_eq!(
            s.selected_size(),
            1000,
            "the pending worktree must not contribute a guessed 0 or anything else"
        );

        // An Unknown size is not a zero-size either.
        s.groups[0].worktrees[1].size = SizeState::Unknown;
        assert_eq!(s.total_size(), 1000);
    }

    #[test]
    fn all_selected_is_false_on_an_empty_tree() {
        let s = state(vec![]);
        assert!(!s.all_selected());
    }

    #[test]
    fn cursor_top_and_bottom_jump_to_visible_extremes() {
        let mut s = sample();
        s.cursor_bottom();
        assert_eq!(s.cursor, 5);
        assert_eq!(
            s.current_row(),
            Some(RowRef::Worktree {
                group: 1,
                worktree: 0
            })
        );
        s.cursor_top();
        assert_eq!(s.cursor, 0);

        // With everything collapsed the extremes are the two headers.
        s.set_expanded(false);
        s.cursor_bottom();
        s.set_expanded(false);
        assert_eq!(s.rows().len(), 2);
        assert_eq!(s.cursor, 1);
    }
}
