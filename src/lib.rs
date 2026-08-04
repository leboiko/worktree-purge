//! worktree-purge — find and reclaim disk space from stale git worktrees.
//!
//! The crate is split so the TUI is a thin shell over testable logic:
//!
//! - [`scan`]  — discovering repositories and their linked worktrees, then
//!   enriching them with size / activity / stash information in the background.
//! - [`app`]   — the view-model ([`app::state::AppState`]), key handling and
//!   the purge engine.
//! - [`ui`]    — pure rendering against [`app::state::AppState`].
//! - [`model`] — plain data shared by all of the above.

pub mod app;
pub mod format;
pub mod git;
pub mod model;
pub mod scan;
pub mod ui;
