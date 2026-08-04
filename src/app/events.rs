//! Key handling: crossterm events in, state mutations out.

use crossterm::event::KeyEvent;

use crate::app::state::AppState;

/// Something the event loop must do that state alone cannot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    /// Nothing further required.
    None,
    /// Run the confirmed plan.
    Purge,
    /// Re-scan the root folder.
    Rescan,
    Quit,
}

/// Apply a keypress to the state, returning any side effect for the caller.
pub fn handle_key(_state: &mut AppState, _key: KeyEvent) -> Action {
    todo!("app::events::handle_key")
}
