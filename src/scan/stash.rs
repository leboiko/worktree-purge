//! Stash counts, shown as an informational hint.
//!
//! Stashes created inside a worktree live in the repository's shared stash
//! stack and remain recoverable after `git worktree remove`, so they never
//! block a purge — they are surfaced only so the user knows related work
//! exists.

use std::collections::HashMap;
use std::path::Path;

use crate::git;

/// Count stash entries per branch name for one repository.
///
/// One `git stash list` per repository, never per worktree: `refs/stash` is a
/// shared (non-per-worktree) ref, so a single call sees the stashes created in
/// every linked worktree.
///
/// A repository we cannot ask (missing, bare with no worktree to run in,
/// orphaned) yields an empty map — a stash hint is never load-bearing, so
/// "no information" and "no stashes" are allowed to look the same here.
pub fn stash_counts(repo_root: &Path) -> HashMap<String, usize> {
    match git::stdout(repo_root, &["stash", "list", "--pretty=format:%gs"]) {
        Some(output) => parse_stash_subjects(&output),
        None => HashMap::new(),
    }
}

/// Parse `git stash list --pretty=format:%gs` output into branch → count.
fn parse_stash_subjects(output: &str) -> HashMap<String, usize> {
    let mut counts: HashMap<String, usize> = HashMap::new();
    for branch in output.lines().filter_map(branch_of) {
        *counts.entry(branch.to_owned()).or_default() += 1;
    }
    counts
}

/// Pull the branch name out of one stash reflog subject.
///
/// Two shapes exist:
///
/// ```text
/// WIP on <branch>: <sha> <subject>     git stash push
/// On <branch>: <message>               git stash push -m <message>
/// WIP on (no branch): <sha> <subject>  stashed on a detached HEAD
/// ```
///
/// Splitting on the first `:` is exact rather than heuristic: `git
/// check-ref-format` forbids both `:` and space in a refname, so the first
/// colon always terminates the branch name. `(no branch)` is kept as a key —
/// no branch can be named that, so it simply never matches a worktree.
fn branch_of(subject: &str) -> Option<&str> {
    let rest = subject
        .strip_prefix("WIP on ")
        .or_else(|| subject.strip_prefix("On "))?;
    let (branch, _) = rest.split_once(':')?;
    (!branch.is_empty()).then_some(branch)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn counts(output: &str) -> Vec<(String, usize)> {
        let mut v: Vec<_> = parse_stash_subjects(output).into_iter().collect();
        v.sort();
        v
    }

    #[test]
    fn both_subject_shapes_map_to_their_branch() {
        let output = "WIP on feature-a: 0a8b1bb initial\n\
                      On feature-a: hand written message\n\
                      WIP on main: 0a8b1bb initial";
        assert_eq!(
            counts(output),
            vec![("feature-a".to_owned(), 2), ("main".to_owned(), 1)]
        );
    }

    #[test]
    fn branch_names_containing_slashes_survive() {
        // Slashes are legal in refnames; colons and spaces are not, which is
        // why splitting on the first colon is safe.
        let output = "WIP on feat/nested/name: 0a8b1bb initial";
        assert_eq!(counts(output), vec![("feat/nested/name".to_owned(), 1)]);
    }

    #[test]
    fn a_message_containing_colons_does_not_extend_the_branch() {
        let output = "On topic: fix: handle 12:30 timestamps";
        assert_eq!(counts(output), vec![("topic".to_owned(), 1)]);
    }

    #[test]
    fn detached_head_stashes_do_not_collide_with_a_branch() {
        let output = "WIP on (no branch): 0a8b1bb initial\nOn topic: work";
        assert_eq!(
            counts(output),
            vec![("(no branch)".to_owned(), 1), ("topic".to_owned(), 1)]
        );
    }

    #[test]
    fn unparseable_and_empty_output_yield_no_counts() {
        assert!(parse_stash_subjects("").is_empty());
        // `git stash store` accepts an arbitrary message with neither prefix.
        assert!(parse_stash_subjects("some hand-stored stash\n").is_empty());
        // A prefix with no colon is not a subject we understand.
        assert!(parse_stash_subjects("WIP on nothing-here\n").is_empty());
        // An empty branch name is not a branch.
        assert!(parse_stash_subjects("On : message\n").is_empty());
    }
}
