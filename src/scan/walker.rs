//! Parallel filesystem walk that discovers repositories and linked worktrees.

use std::path::Path;

use crate::model::ScanResult;

/// Walk `root` and return every repository group with its linked worktrees.
///
/// Prunes on hit: once a `.git` entry identifies a directory as a repository or
/// worktree, the walk does not descend into it.
pub fn scan(_root: &Path) -> ScanResult {
    todo!("scan::walker::scan")
}
