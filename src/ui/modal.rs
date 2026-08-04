//! Confirmation and result overlays.

use ratatui::Frame;
use ratatui::layout::Rect;

use crate::app::state::AppState;

/// Render whichever modal the current mode calls for. No-op in browse mode.
pub fn render(_frame: &mut Frame, _area: Rect, _state: &AppState) {
    todo!("ui::modal::render")
}
