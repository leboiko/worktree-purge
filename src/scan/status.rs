//! "Would purging this lose work?" — dirty tree and unpushed commit detection.

use std::path::Path;

use crate::model::ActivityState;

/// Classify a worktree as clean, active, or unknown.
///
/// Never returns `Clean` on error: an failed probe yields `Unknown`, which the
/// purge flow treats as needing confirmation.
pub fn activity(_worktree: &Path) -> ActivityState {
    todo!("scan::status::activity")
}
