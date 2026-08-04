//! Discovery and enrichment of git worktrees.

pub mod enrich;
pub mod repo;
pub mod size;
pub mod stash;
pub mod status;
pub mod walker;

pub use walker::scan;
