//! Removing worktrees: planning, execution and reporting.
//!
//! Removal goes through `git worktree remove` so the repository's
//! `.git/worktrees/<name>` metadata is cleaned up too; a plain directory
//! delete would leave the repo referencing a worktree that no longer exists.

use std::path::Path;

use crate::model::{PurgeItem, PurgeOutcome};

/// Remove a single worktree.
///
/// `force` is required for worktrees that are dirty, hold unpushed commits,
/// are locked, or whose status could not be determined.
pub fn purge_one(_item: &PurgeItem, _force: bool) -> PurgeOutcome {
    todo!("app::purge::purge_one")
}

/// Remove every item in the plan, reporting each outcome to `progress` as it
/// completes.
pub fn purge_all(
    _items: &[PurgeItem],
    _force: bool,
    _progress: &mut dyn FnMut(&PurgeOutcome),
) -> Vec<PurgeOutcome> {
    todo!("app::purge::purge_all")
}

/// Drop stale `worktrees/` metadata in a repository after removals.
pub fn prune_repo(_repo_root: &Path) {
    todo!("app::purge::prune_repo")
}
