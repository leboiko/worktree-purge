//! Background enrichment: streams size, activity and stash data to the UI.

use std::sync::mpsc::Sender;

use crate::model::{EnrichMsg, RepoGroup};

/// Handle to a running enrichment pass.
pub struct EnrichHandle {
    _private: (),
}

impl EnrichHandle {
    /// Ask the workers to stop early (used on rescan and on quit).
    pub fn cancel(&self) {
        todo!("scan::enrich::EnrichHandle::cancel")
    }
}

/// Spawn workers that compute size, activity and stash counts for every
/// worktree in `groups`, sending results to `tx` as they complete.
///
/// Returns immediately: the UI must stay responsive during a multi-GB scan.
pub fn spawn(_groups: &[RepoGroup], _tx: Sender<EnrichMsg>) -> EnrichHandle {
    todo!("scan::enrich::spawn")
}
