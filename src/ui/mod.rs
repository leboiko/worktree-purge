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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn width_uses_display_columns_not_bytes() {
        // Every glyph the tree renders is one column wide even though several
        // are multi-byte; the row geometry depends on that.
        for glyph in ["●", "○", "⠋", "…", "—", "▼", "▶", "⚑"] {
            assert_eq!(width(glyph), 1, "{glyph} should be one column");
            assert!(glyph.len() > 1, "{glyph} should be multi-byte");
        }
        assert_eq!(width("abc"), 3);
    }

    #[test]
    fn truncate_marks_the_cut_and_never_exceeds_the_budget() {
        assert_eq!(truncate("hello", 10), "hello");
        assert_eq!(truncate("hello", 5), "hello");
        assert_eq!(truncate("hello", 4), "hel…");
        assert_eq!(truncate("hello", 1), "…");
        assert_eq!(truncate("hello", 0), "");
        for max in 0..8 {
            assert!(width(&truncate("abcdefg", max)) <= max, "max {max}");
        }
    }

    #[test]
    fn truncate_does_not_split_a_multi_byte_glyph() {
        let s = "●●●●●";
        assert_eq!(truncate(s, 3), "●●…");
        assert!(truncate(s, 3).is_char_boundary(truncate(s, 3).len()));
    }

    #[test]
    fn padding_produces_exactly_the_requested_field() {
        assert_eq!(pad_to("ab", 5), "ab   ");
        assert_eq!(pad_to("abcdef", 4), "abc…");
        assert_eq!(width(&pad_to("abcdef", 4)), 4);
        assert_eq!(pad_left("1 KB", 8), "    1 KB");
        assert_eq!(width(&pad_left("1 KB", 8)), 8);
        assert_eq!(width(&pad_left("a-very-long-value", 6)), 6);
    }

    #[test]
    fn centered_always_fits_inside_the_parent() {
        let area = Rect::new(0, 0, 80, 24);
        let r = centered(area, 40, 10);
        assert_eq!((r.x, r.y, r.width, r.height), (20, 7, 40, 10));

        // Oversized requests are clamped rather than overflowing.
        let tiny = Rect::new(3, 4, 5, 2);
        let r = centered(tiny, 100, 100);
        assert_eq!((r.x, r.y, r.width, r.height), (3, 4, 5, 2));
        assert!(r.right() <= tiny.right() && r.bottom() <= tiny.bottom());

        // Zero-sized parents produce zero-sized children, not panics.
        let empty = Rect::new(0, 0, 0, 0);
        assert_eq!(centered(empty, 10, 10), Rect::new(0, 0, 0, 0));
    }

    #[test]
    fn the_spinner_wraps_instead_of_indexing_out_of_range() {
        assert_eq!(spinner_frame(0), SPINNER[0]);
        assert_eq!(spinner_frame(SPINNER.len()), SPINNER[0]);
        assert_eq!(
            spinner_frame(usize::MAX),
            SPINNER[usize::MAX % SPINNER.len()]
        );
    }
}
