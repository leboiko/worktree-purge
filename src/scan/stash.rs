//! Stash counts, shown as an informational hint.
//!
//! Stashes created inside a worktree live in the repository's shared stash
//! stack and remain recoverable after `git worktree remove`, so they never
//! block a purge — they are surfaced only so the user knows related work
//! exists.

use std::collections::HashMap;
use std::path::Path;

/// Count stash entries per branch name for one repository.
pub fn stash_counts(_repo_root: &Path) -> HashMap<String, usize> {
    todo!("scan::stash::stash_counts")
}
