//! Rendering. Every function here is a pure function of `AppState` plus a
//! `Rect`, so the whole UI is snapshot-testable against a `TestBackend`.
//!
//! The one exception is [`tree::render`], which writes the scroll offset back
//! into `AppState::scroll` — the renderer is the only place that knows the
//! viewport height. Everything else takes `&AppState`.

pub mod help;
pub mod modal;
pub mod statusbar;
pub mod tree;

use ratatui::Frame;
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::Span;

use crate::app::state::{AppState, Mode};

// Shared palette. The severity distinction between `Dirty` (work is lost) and
// `Unpushed` (work survives in the object store) is the whole point of the
// colour choices — see `docs/notes-ui.md`.
pub(crate) const C_DIRTY: Color = Color::Red;
pub(crate) const C_UNPUSHED: Color = Color::Yellow;
pub(crate) const C_CLEAN: Color = Color::DarkGray;
pub(crate) const C_PENDING: Color = Color::Cyan;
pub(crate) const C_UNKNOWN: Color = Color::Magenta;
pub(crate) const C_MUTED: Color = Color::DarkGray;
pub(crate) const C_ACCENT: Color = Color::Cyan;
pub(crate) const C_DANGER: Color = Color::Red;
pub(crate) const C_OK: Color = Color::Green;

/// Background of the row under the cursor.
pub(crate) const CURSOR_BG: Color = Color::Indexed(238);

/// Braille spinner used wherever a background worker has not answered yet.
pub(crate) const SPINNER: [&str; 10] = ["⠋", "⠙", "⠹", "⠸", "⠼", "⠴", "⠦", "⠧", "⠇", "⠏"];

pub(crate) fn spinner_frame(tick: usize) -> &'static str {
    SPINNER[tick % SPINNER.len()]
}

pub(crate) fn dim(color: Color) -> Style {
    Style::new().fg(color).add_modifier(Modifier::DIM)
}

/// Display width of a string, using the same unicode rules ratatui renders
/// with. Goes through [`Span`] so we do not take a direct dependency on
/// `unicode-width`.
pub(crate) fn width(s: &str) -> usize {
    Span::raw(s).width()
}

fn char_width(ch: char) -> usize {
    let mut buf = [0u8; 4];
    width(ch.encode_utf8(&mut buf))
}

/// Truncate `s` to at most `max` display columns, marking the cut with `…`.
///
/// Returns an empty string for `max == 0` rather than panicking: narrow
/// terminals are a supported configuration, not an error.
pub(crate) fn truncate(s: &str, max: usize) -> String {
    if width(s) <= max {
        return s.to_string();
    }
    if max == 0 {
        return String::new();
    }
    if max == 1 {
        return "…".to_string();
    }
    let mut out = String::new();
    let mut w = 0usize;
    for ch in s.chars() {
        let cw = char_width(ch);
        if w + cw > max - 1 {
            break;
        }
        out.push(ch);
        w += cw;
    }
    out.push('…');
    out
}

/// Truncate to `field` columns and pad with spaces to exactly `field` columns.
pub(crate) fn pad_to(s: &str, field: usize) -> String {
    let mut out = truncate(s, field);
    let pad = field.saturating_sub(width(&out));
    out.push_str(&" ".repeat(pad));
    out
}

/// Right-align `s` inside `field` columns.
pub(crate) fn pad_left(s: &str, field: usize) -> String {
    let t = truncate(s, field);
    let pad = field.saturating_sub(width(&t));
    format!("{}{t}", " ".repeat(pad))
}

/// A centred rectangle `w` × `h`, clamped so it always fits inside `area`.
pub(crate) fn centered(area: Rect, w: u16, h: u16) -> Rect {
    let w = w.min(area.width);
    let h = h.min(area.height);
    Rect {
        x: area.x + (area.width.saturating_sub(w)) / 2,
        y: area.y + (area.height.saturating_sub(h)) / 2,
        width: w,
        height: h,
    }
}

/// Draw the whole application for one frame.
pub fn draw(frame: &mut Frame, state: &mut AppState) {
    let area = frame.area();
    if area.width == 0 || area.height == 0 {
        return;
    }

    if area.height >= 2 {
        let [top, bottom] =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).areas::<2>(area);
        tree::render(frame, top, state);
        statusbar::render(frame, bottom, state);
    } else {
        // One row left: the totals are more useful than a clipped border.
        statusbar::render(frame, area, state);
    }

    match state.mode {
        Mode::Help => help::render(frame, area),
        Mode::Browse => {}
        _ => modal::render(frame, area, state),
    }
}
