//! Core data model shared by the scanner, enrichment workers, TUI and purge
//! engine. Everything here is plain data — no I/O, no git, no rendering — so
//! it can be constructed freely in tests.

use std::collections::HashMap;
use std::path::{Path, PathBuf};

/// Recursive disk usage of a worktree's working files.
///
/// Linked worktrees share the owning repository's object store, so this is
/// only the space actually reclaimed by removing the worktree.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SizeState {
    /// Not computed yet — a worker will fill it in.
    #[default]
    Pending,
    Ready(u64),
    /// Computation failed (permissions, vanished directory, ...).
    Unknown,
}

impl SizeState {
    pub fn bytes(self) -> Option<u64> {
        match self {
            SizeState::Ready(n) => Some(n),
            _ => None,
        }
    }
}

/// Why a worktree holds work that would be lost by purging it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ActiveReason {
    /// Uncommitted changes: modified, staged, or untracked files.
    Dirty,
    /// Commits that exist on no remote.
    Unpushed,
    /// Both dirty and unpushed.
    Both,
}

impl ActiveReason {
    /// Combine two independent signals into a single reason.
    pub fn combine(dirty: bool, unpushed: bool) -> Option<Self> {
        match (dirty, unpushed) {
            (true, true) => Some(ActiveReason::Both),
            (true, false) => Some(ActiveReason::Dirty),
            (false, true) => Some(ActiveReason::Unpushed),
            (false, false) => None,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            ActiveReason::Dirty => "uncommitted changes",
            ActiveReason::Unpushed => "unpushed commits",
            ActiveReason::Both => "uncommitted changes + unpushed commits",
        }
    }
}

/// Whether a worktree is safe to remove.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ActivityState {
    /// Not checked yet.
    #[default]
    Pending,
    /// Checked: nothing would be lost.
    Clean,
    Active(ActiveReason),
    /// The check failed. Treated as active for safety — never let an error
    /// downgrade a warning.
    Unknown,
}

impl ActivityState {
    /// True when purging needs the extra "yes, I really mean it" confirmation.
    ///
    /// `Pending` and `Unknown` both count: we refuse to claim a worktree is
    /// safe before we have actually proven it.
    pub fn needs_force(self) -> bool {
        !matches!(self, ActivityState::Clean)
    }

    pub fn is_active(self) -> bool {
        matches!(self, ActivityState::Active(_))
    }
}

/// A linked git worktree — a candidate for purging.
///
/// The main checkout of a repository is never represented here; it is not
/// purgeable and listing it would invite disaster.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Worktree {
    /// Absolute path to the worktree directory.
    pub path: PathBuf,
    /// Display name — the worktree directory's file name.
    pub name: String,
    /// Checked-out branch, if not detached.
    pub branch: Option<String>,
    /// Short HEAD sha, when known.
    pub head: Option<String>,
    pub size: SizeState,
    pub activity: ActivityState,
    /// Stashes whose branch matches this worktree. Informational only: stashes
    /// live in the shared stash stack and survive worktree removal, so this
    /// never blocks a purge.
    pub stash_count: usize,
    /// `Some(reason)` when `git worktree lock` was used. Locked worktrees
    /// refuse removal without `--force`.
    pub locked: Option<String>,
    /// The directory exists but its owning repository/metadata is gone. Pure
    /// disk waste; purged by plain recursive delete.
    pub orphaned: bool,
    /// Discovered via `git worktree list` rather than the filesystem walk,
    /// meaning it lives outside the scanned folder.
    pub outside_scan_root: bool,
    /// The working tree directory is missing entirely (metadata-only record).
    pub prunable: bool,
    pub selected: bool,
}

impl Worktree {
    /// Build a worktree record with everything unknown/unselected.
    pub fn new(path: PathBuf) -> Self {
        let name = path
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| path.to_string_lossy().into_owned());
        Worktree {
            path,
            name,
            branch: None,
            head: None,
            size: SizeState::Pending,
            activity: ActivityState::Pending,
            stash_count: 0,
            locked: None,
            orphaned: false,
            outside_scan_root: false,
            prunable: false,
            selected: false,
        }
    }
}

/// A repository and the linked worktrees it owns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RepoGroup {
    /// Main checkout directory, or the bare repository directory.
    pub root: PathBuf,
    /// The `.git` directory that owns `worktrees/` metadata — the stable
    /// identity of a repository, shared by every one of its worktrees.
    pub git_common_dir: PathBuf,
    /// Display name — the root directory's file name.
    pub name: String,
    pub worktrees: Vec<Worktree>,
    pub expanded: bool,
    /// The owning repository could not be located (orphan bucket).
    pub orphaned: bool,
}

impl RepoGroup {
    pub fn new(root: PathBuf, git_common_dir: PathBuf) -> Self {
        let name = root
            .file_name()
            .map(|n| n.to_string_lossy().into_owned())
            .unwrap_or_else(|| root.to_string_lossy().into_owned());
        RepoGroup {
            root,
            git_common_dir,
            name,
            worktrees: Vec::new(),
            expanded: true,
            orphaned: false,
        }
    }

    /// Total known size across this repo's worktrees. Pending entries count
    /// as zero, so this only ever under-promises reclaimable space.
    pub fn known_size(&self) -> u64 {
        self.worktrees.iter().filter_map(|w| w.size.bytes()).sum()
    }

    pub fn active_count(&self) -> usize {
        self.worktrees
            .iter()
            .filter(|w| w.activity.is_active())
            .count()
    }

    pub fn selected_count(&self) -> usize {
        self.worktrees.iter().filter(|w| w.selected).count()
    }

    /// True when every worktree in the group is selected (and there is at
    /// least one).
    pub fn all_selected(&self) -> bool {
        !self.worktrees.is_empty() && self.worktrees.iter().all(|w| w.selected)
    }
}

/// Outcome of a filesystem scan.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ScanResult {
    pub groups: Vec<RepoGroup>,
    /// Directories skipped because they could not be read.
    pub skipped: Vec<PathBuf>,
    /// Non-fatal problems worth surfacing in the status bar.
    pub warnings: Vec<String>,
}

impl ScanResult {
    pub fn worktree_count(&self) -> usize {
        self.groups.iter().map(|g| g.worktrees.len()).sum()
    }
}

/// Messages streamed from background enrichment workers to the UI thread.
///
/// Keyed by worktree path, which is unique and stable across re-sorting.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum EnrichMsg {
    Size {
        path: PathBuf,
        state: SizeState,
    },
    Activity {
        path: PathBuf,
        state: ActivityState,
    },
    /// Stash counts for one repository, keyed by branch name.
    Stashes {
        git_common_dir: PathBuf,
        counts: HashMap<String, usize>,
    },
    /// All enrichment work for this scan has finished.
    Done,
}

/// Row ordering in the tree view.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum SortMode {
    #[default]
    Name,
    SizeDesc,
    ActiveFirst,
}

impl SortMode {
    pub fn next(self) -> Self {
        match self {
            SortMode::Name => SortMode::SizeDesc,
            SortMode::SizeDesc => SortMode::ActiveFirst,
            SortMode::ActiveFirst => SortMode::Name,
        }
    }

    pub fn label(self) -> &'static str {
        match self {
            SortMode::Name => "name",
            SortMode::SizeDesc => "size",
            SortMode::ActiveFirst => "active",
        }
    }
}

/// One visible line in the tree, after collapsing is applied.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RowRef {
    Repo { group: usize },
    Worktree { group: usize, worktree: usize },
}

impl RowRef {
    pub fn group(self) -> usize {
        match self {
            RowRef::Repo { group } | RowRef::Worktree { group, .. } => group,
        }
    }

    pub fn worktree(self) -> Option<usize> {
        match self {
            RowRef::Repo { .. } => None,
            RowRef::Worktree { worktree, .. } => Some(worktree),
        }
    }
}

/// One worktree slated for removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeItem {
    pub worktree_path: PathBuf,
    /// Repository to run `git worktree remove` from. `None` means the owning
    /// repo is gone and the directory is deleted directly.
    pub repo_root: Option<PathBuf>,
    pub name: String,
    pub size: Option<u64>,
    /// Requires `--force`: dirty, unpushed, locked, or status unknown.
    pub requires_force: bool,
}

/// Result of attempting one removal.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PurgeOutcome {
    pub item: PurgeItem,
    /// `Ok(freed_bytes)` or `Err(message)`.
    pub result: Result<u64, String>,
}

impl PurgeOutcome {
    pub fn succeeded(&self) -> bool {
        self.result.is_ok()
    }
}

/// A batch of removals awaiting confirmation.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct PurgePlan {
    pub items: Vec<PurgeItem>,
}

impl PurgePlan {
    pub fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    pub fn len(&self) -> usize {
        self.items.len()
    }

    /// Bytes we expect to reclaim (known sizes only).
    pub fn total_size(&self) -> u64 {
        self.items.iter().filter_map(|i| i.size).sum()
    }

    /// Items that need the extra confirmation step.
    pub fn force_items(&self) -> impl Iterator<Item = &PurgeItem> {
        self.items.iter().filter(|i| i.requires_force)
    }

    pub fn needs_force(&self) -> bool {
        self.items.iter().any(|i| i.requires_force)
    }
}

/// Find a worktree by path across all groups.
pub fn find_worktree_mut<'a>(groups: &'a mut [RepoGroup], path: &Path) -> Option<&'a mut Worktree> {
    groups
        .iter_mut()
        .flat_map(|g| g.worktrees.iter_mut())
        .find(|w| w.path == path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn active_reason_combines_signals() {
        assert_eq!(ActiveReason::combine(false, false), None);
        assert_eq!(
            ActiveReason::combine(true, false),
            Some(ActiveReason::Dirty)
        );
        assert_eq!(
            ActiveReason::combine(false, true),
            Some(ActiveReason::Unpushed)
        );
        assert_eq!(ActiveReason::combine(true, true), Some(ActiveReason::Both));
    }

    #[test]
    fn only_proven_clean_skips_force_confirmation() {
        // Pending and Unknown must NOT be treated as safe: an unfinished or
        // failed check is not evidence of a clean worktree.
        assert!(!ActivityState::Clean.needs_force());
        assert!(ActivityState::Pending.needs_force());
        assert!(ActivityState::Unknown.needs_force());
        assert!(ActivityState::Active(ActiveReason::Dirty).needs_force());
    }

    #[test]
    fn group_aggregates_ignore_pending_sizes() {
        let mut g = RepoGroup::new(PathBuf::from("/repo"), PathBuf::from("/repo/.git"));
        let mut a = Worktree::new(PathBuf::from("/repo/../wt-a"));
        a.size = SizeState::Ready(100);
        a.activity = ActivityState::Active(ActiveReason::Dirty);
        let mut b = Worktree::new(PathBuf::from("/repo/../wt-b"));
        b.size = SizeState::Pending;
        g.worktrees = vec![a, b];

        assert_eq!(g.known_size(), 100);
        assert_eq!(g.active_count(), 1);
        assert!(!g.all_selected());
    }

    #[test]
    fn all_selected_is_false_for_empty_group() {
        let g = RepoGroup::new(PathBuf::from("/repo"), PathBuf::from("/repo/.git"));
        assert!(!g.all_selected());
    }

    #[test]
    fn plan_totals_skip_unknown_sizes() {
        let plan = PurgePlan {
            items: vec![
                PurgeItem {
                    worktree_path: PathBuf::from("/a"),
                    repo_root: None,
                    name: "a".into(),
                    size: Some(10),
                    requires_force: false,
                },
                PurgeItem {
                    worktree_path: PathBuf::from("/b"),
                    repo_root: None,
                    name: "b".into(),
                    size: None,
                    requires_force: true,
                },
            ],
        };
        assert_eq!(plan.total_size(), 10);
        assert!(plan.needs_force());
        assert_eq!(plan.force_items().count(), 1);
    }
}
