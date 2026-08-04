//! Rendering. Every function here is a pure function of `AppState` plus a
//! `Rect`, so the whole UI is snapshot-testable against a `TestBackend`.

pub mod help;
pub mod modal;
pub mod statusbar;
pub mod tree;

use ratatui::Frame;

use crate::app::state::AppState;

/// Draw the whole application for one frame.
pub fn draw(_frame: &mut Frame, _state: &AppState) {
    todo!("ui::draw — implemented in the TUI milestone")
}
