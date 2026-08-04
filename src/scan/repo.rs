//! Repository/worktree identity: `.git` back-pointers and `git worktree list`.

use std::path::{Path, PathBuf};

/// One record from `git worktree list --porcelain`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct WorktreeEntry {
    pub path: PathBuf,
    pub head: Option<String>,
    pub branch: Option<String>,
    pub bare: bool,
    pub detached: bool,
    pub locked: Option<String>,
    pub prunable: bool,
}

/// Parse `git worktree list --porcelain` output.
///
/// The first record is always the main worktree (or the bare repository).
pub fn parse_worktree_list(_output: &str) -> Vec<WorktreeEntry> {
    todo!("scan::repo::parse_worktree_list")
}

/// Read the `gitdir: <path>` pointer out of a linked worktree's `.git` file.
pub fn resolve_gitdir_pointer(_dot_git_file: &Path) -> Option<PathBuf> {
    todo!("scan::repo::resolve_gitdir_pointer")
}
