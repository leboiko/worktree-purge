//! Totals and hints along the bottom of the screen.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::Paragraph;

use crate::app::events;
use crate::app::state::AppState;
use crate::format;
use crate::ui::{C_ACCENT, C_MUTED, C_OK, C_UNPUSHED, dim, spinner_frame, truncate, width};

/// Left half: what was found. Only known sizes are summed, so the number never
/// over-promises reclaimable space.
fn totals(state: &AppState) -> Vec<Span<'static>> {
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(
            format::count_noun(state.repo_count(), "repo"),
            Style::new().fg(Color::White),
        ),
        Span::styled(" · ", dim(C_MUTED)),
        Span::styled(
            format::count_noun(state.worktree_count(), "worktree"),
            Style::new().fg(Color::White),
        ),
        Span::styled(" · ", dim(C_MUTED)),
        Span::styled(
            format::bytes(state.total_size()),
            Style::new().fg(Color::White),
        ),
    ];
    if state.enriching {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(
            spinner_frame(state.spinner_tick),
            Style::new().fg(C_ACCENT),
        ));
    }
    if !state.skipped.is_empty() {
        spans.push(Span::styled(
            format!(" · {} skipped", state.skipped.len()),
            Style::new().fg(C_UNPUSHED),
        ));
    }
    spans
}

/// Right half: selection, sort mode and the help hint.
fn hints(state: &AppState) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    if let Some(summary) = events::selection_summary(state) {
        spans.push(Span::styled(
            summary,
            Style::new().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ));
        spans.push(Span::raw("   "));
    }
    spans.push(Span::styled("sort: ", dim(C_MUTED)));
    spans.push(Span::styled(
        state.sort.label(),
        Style::new().fg(Color::White),
    ));
    spans.push(Span::raw("  "));
    spans.push(Span::styled("?", Style::new().fg(C_ACCENT)));
    spans.push(Span::styled(" help ", dim(C_MUTED)));
    spans
}

fn spans_width(spans: &[Span<'_>]) -> usize {
    spans.iter().map(Span::width).sum()
}

/// Clip (or pad) a run of spans to exactly `field` columns, preserving the
/// per-span styling of whatever survives.
fn fit(spans: Vec<Span<'static>>, field: usize) -> Vec<Span<'static>> {
    let mut out = Vec::with_capacity(spans.len() + 1);
    let mut used = 0usize;
    for span in spans {
        if used >= field {
            break;
        }
        let w = span.width();
        if used + w <= field {
            used += w;
            out.push(span);
        } else {
            let text = truncate(span.content.as_ref(), field - used);
            used += width(&text);
            out.push(Span::styled(text, span.style));
            break;
        }
    }
    if used < field {
        out.push(Span::raw(" ".repeat(field - used)));
    }
    out
}

/// Build the status line for a given width. Split out so tests can pin the
/// exact text without a terminal.
pub(crate) fn status_line(state: &AppState, total: usize) -> Line<'static> {
    let mut left = totals(state);
    if let Some(toast) = &state.toast {
        left.push(Span::raw("   "));
        left.push(Span::styled(
            toast.clone(),
            Style::new().fg(C_OK).add_modifier(Modifier::BOLD),
        ));
    }
    let right = hints(state);

    let lw = spans_width(&left);
    let rw = spans_width(&right);

    let mut spans = if lw + rw <= total {
        let mut spans = left;
        spans.push(Span::raw(" ".repeat(total - lw - rw)));
        spans.extend(right);
        spans
    } else if rw < total {
        // Not enough room for both: the hints are the shorter, more actionable
        // half, so they win and the totals get clipped.
        let mut spans = fit(left, total - rw);
        spans.extend(right);
        spans
    } else {
        fit(left, total)
    };
    // Belt and braces: never hand the terminal a line wider than the bar.
    if spans_width(&spans) > total {
        spans = fit(spans, total);
    }
    Line::from(spans)
}

/// Render repo/worktree counts, total and selected size, sort mode and toast.
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    frame.render_widget(
        Paragraph::new(status_line(state, area.width as usize)),
        Rect { height: 1, ..area },
    );
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::tests::{group, sample, state, wt};
    use crate::model::ActivityState;

    fn text(state: &AppState, total: usize) -> String {
        status_line(state, total)
            .spans
            .iter()
            .map(|s| s.content.as_ref())
            .collect()
    }

    #[test]
    fn the_status_line_fills_the_width_exactly() {
        let s = sample();
        for w in [40usize, 60, 80, 120] {
            assert_eq!(width(&text(&s, w)), w, "width {w}");
        }
    }

    #[test]
    fn totals_and_hints_sit_at_opposite_ends() {
        let s = sample();
        let line = text(&s, 80);
        assert!(
            line.starts_with(" 2 repos · 4 worktrees · 1.6 KB"),
            "{line:?}"
        );
        assert!(line.ends_with("sort: name  ? help "), "{line:?}");
    }

    #[test]
    fn the_selection_summary_appears_only_when_something_is_selected() {
        let mut s = sample();
        assert!(!text(&s, 80).contains("selected"));

        s.cursor = 1;
        s.toggle_selection();
        let line = text(&s, 80);
        assert!(line.contains("selected 1 (400 B)"), "{line:?}");
    }

    #[test]
    fn selected_size_ignores_unmeasured_worktrees() {
        let mut s = state(vec![group(
            "r",
            vec![
                wt("known", Some(2048), ActivityState::Clean),
                wt("pending", None, ActivityState::Pending),
            ],
        )]);
        s.select_all(true);
        let line = text(&s, 80);
        assert!(line.contains("selected 2 (2.0 KB)"), "{line:?}");
    }

    #[test]
    fn the_sort_label_tracks_the_mode() {
        let mut s = sample();
        assert!(text(&s, 80).contains("sort: name"));
        s.cycle_sort();
        assert!(text(&s, 80).contains("sort: size"));
        s.cycle_sort();
        assert!(text(&s, 80).contains("sort: active"));
    }

    #[test]
    fn a_toast_is_shown_next_to_the_totals() {
        let mut s = sample();
        s.toast = Some("freed 1.2 GB".into());
        let line = text(&s, 80);
        assert!(line.contains("freed 1.2 GB"), "{line:?}");
        assert_eq!(width(&line), 80);
    }

    #[test]
    fn skipped_directories_are_surfaced() {
        let mut s = sample();
        assert!(!text(&s, 80).contains("skipped"));
        s.skipped = vec!["/no/permission".into(), "/other".into()];
        assert!(text(&s, 80).contains("· 2 skipped"), "{:?}", text(&s, 80));
    }

    #[test]
    fn a_narrow_bar_keeps_the_hints_and_clips_the_totals() {
        let s = sample();
        let line = text(&s, 26);
        assert!(line.ends_with("sort: name  ? help "), "{line:?}");
        assert_eq!(width(&line), 26, "{line:?}");
    }

    #[test]
    fn an_extremely_narrow_bar_does_not_panic_and_stays_in_bounds() {
        let s = sample();
        for w in 0..24usize {
            let line = text(&s, w);
            assert!(width(&line) <= w, "width {w} produced {line:?}");
        }
    }

    #[test]
    fn empty_state_reports_zeroes_rather_than_nothing() {
        let s = state(vec![]);
        let line = text(&s, 60);
        assert!(line.starts_with(" 0 repos · 0 worktrees · 0 B"), "{line:?}");
    }
}
