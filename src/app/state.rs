//! The view-model: everything the UI renders and every keystroke mutates.
//!
//! `AppState` owns no I/O. Scanning and enrichment happen elsewhere and are
//! folded in via [`AppState::from_scan`] and [`AppState::apply_enrich`], which
//! keeps the whole interaction layer testable without touching a disk.

use std::path::{Path, PathBuf};

use crate::model::{EnrichMsg, PurgeOutcome, PurgePlan, RepoGroup, RowRef, ScanResult, SortMode};

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
    pub fn from_scan(_root: PathBuf, _scan: ScanResult) -> Self {
        todo!("AppState::from_scan")
    }

    /// Visible rows, top to bottom, honouring collapsed groups.
    pub fn rows(&self) -> Vec<RowRef> {
        todo!("AppState::rows")
    }

    /// The row under the cursor, if any.
    pub fn current_row(&self) -> Option<RowRef> {
        todo!("AppState::current_row")
    }

    /// Path of the focused worktree, used to keep the cursor on the same item
    /// across sorting and rescans.
    pub fn focused_path(&self) -> Option<PathBuf> {
        todo!("AppState::focused_path")
    }

    /// Move the cursor to the row showing `path`, if it is visible.
    pub fn focus_path(&mut self, _path: &Path) {
        todo!("AppState::focus_path")
    }

    /// Move the cursor by `delta` rows, clamped to the list.
    pub fn move_cursor(&mut self, _delta: isize) {
        todo!("AppState::move_cursor")
    }

    /// Expand or collapse the group under the cursor.
    pub fn set_expanded(&mut self, _expanded: bool) {
        todo!("AppState::set_expanded")
    }

    /// Toggle expansion of the group under the cursor.
    pub fn toggle_expanded(&mut self) {
        todo!("AppState::toggle_expanded")
    }

    /// Toggle selection of the item under the cursor. On a repo header this
    /// selects or deselects every worktree in that group.
    pub fn toggle_selection(&mut self) {
        todo!("AppState::toggle_selection")
    }

    /// Select or deselect every worktree in the group under the cursor.
    pub fn select_group(&mut self, _selected: bool) {
        todo!("AppState::select_group")
    }

    /// Select or deselect every worktree everywhere.
    pub fn select_all(&mut self, _selected: bool) {
        todo!("AppState::select_all")
    }

    /// Advance the sort mode, keeping the cursor on the same worktree.
    pub fn cycle_sort(&mut self) {
        todo!("AppState::cycle_sort")
    }

    /// Fold one background result into the model.
    pub fn apply_enrich(&mut self, _msg: EnrichMsg) {
        todo!("AppState::apply_enrich")
    }

    /// Build a removal plan from the current selection. Falls back to the
    /// worktree under the cursor when nothing is selected.
    pub fn build_plan(&self) -> PurgePlan {
        todo!("AppState::build_plan")
    }

    /// Drop worktrees that were successfully removed and refresh totals.
    pub fn apply_purge_results(&mut self, _outcomes: &[PurgeOutcome]) {
        todo!("AppState::apply_purge_results")
    }

    pub fn selected_count(&self) -> usize {
        todo!("AppState::selected_count")
    }

    /// Known bytes across selected worktrees.
    pub fn selected_size(&self) -> u64 {
        todo!("AppState::selected_size")
    }

    /// Known bytes across every discovered worktree.
    pub fn total_size(&self) -> u64 {
        todo!("AppState::total_size")
    }

    pub fn worktree_count(&self) -> usize {
        todo!("AppState::worktree_count")
    }
}
