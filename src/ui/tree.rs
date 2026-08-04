//! The repository/worktree tree.
//!
//! Row geometry is fixed so the size column lands on the same x for every row
//! of a frame; see `docs/notes-ui.md` for the grammar.

use ratatui::Frame;
use ratatui::layout::{Margin, Rect};
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{
    Block, List, ListItem, ListState, Paragraph, Scrollbar, ScrollbarOrientation, ScrollbarState,
};

use crate::app::state::AppState;
use crate::format;
use crate::model::{ActiveReason, ActivityState, RepoGroup, RowRef, Worktree};
use crate::ui::{
    C_ACCENT, C_CLEAN, C_DIRTY, C_MUTED, C_PENDING, C_UNKNOWN, C_UNPUSHED, CURSOR_BG, dim,
    pad_left, pad_to, spinner_frame, truncate, width,
};

/// `"   [x] ● "` — everything before the name on a worktree row.
const WT_PREFIX_W: usize = 9;
/// Name field on a worktree row when the terminal is wide enough.
const NAME_W: usize = 24;
/// Right-aligned size column. `format::bytes` never exceeds 7 columns.
const SIZE_W: usize = 8;
/// Between the size column and the reason text.
const GAP: usize = 2;
/// Floor for the name field on very narrow terminals.
const NAME_MIN: usize = 4;
/// A repo header shows its aggregate only when the name still gets at least
/// this many columns (or all it needs, whichever is smaller).
const HEADER_NAME_MIN: usize = 16;

/// Width of the name field for a given viewport width.
fn name_field(total: usize) -> usize {
    let free = total.saturating_sub(WT_PREFIX_W + SIZE_W + GAP);
    free.clamp(NAME_MIN, NAME_W)
}

/// Glyph + colour for an activity state. The severity distinction here is
/// load-bearing: red means work is destroyed, yellow means work is merely not
/// yet published.
pub(crate) fn activity_glyph(activity: ActivityState, tick: usize) -> Span<'static> {
    match activity {
        ActivityState::Active(ActiveReason::Dirty | ActiveReason::Both) => {
            Span::styled("●", Style::new().fg(C_DIRTY))
        }
        ActivityState::Active(ActiveReason::Unpushed) => {
            Span::styled("●", Style::new().fg(C_UNPUSHED))
        }
        ActivityState::Clean => Span::styled("○", dim(C_CLEAN)),
        // Never a value-shaped glyph: nothing has been measured yet.
        ActivityState::Pending => Span::styled(spinner_frame(tick), Style::new().fg(C_PENDING)),
        // The check *failed*. Distinct from both "clean" and "active" so an
        // error can never read as reassurance.
        ActivityState::Unknown => Span::styled("?", Style::new().fg(C_UNKNOWN)),
    }
}

/// Human-readable reason, matching the glyph's colour.
fn activity_reason(activity: ActivityState) -> Option<Span<'static>> {
    match activity {
        ActivityState::Active(r @ (ActiveReason::Dirty | ActiveReason::Both)) => {
            Some(Span::styled(r.label(), Style::new().fg(C_DIRTY)))
        }
        ActivityState::Active(r @ ActiveReason::Unpushed) => {
            Some(Span::styled(r.label(), Style::new().fg(C_UNPUSHED)))
        }
        ActivityState::Clean => None,
        ActivityState::Pending => Some(Span::styled("checking…", dim(C_PENDING))),
        ActivityState::Unknown => Some(Span::styled("status unknown", Style::new().fg(C_UNKNOWN))),
    }
}

/// Tags and the (purely informational) stash hint.
fn worktree_tags(w: &Worktree) -> Vec<Span<'static>> {
    let mut spans = Vec::new();
    let mut tag = |text: &str, style: Style| {
        spans.push(Span::raw(" "));
        spans.push(Span::styled(text.to_string(), style));
    };
    if w.orphaned {
        tag("[orphan]", Style::new().fg(C_UNPUSHED));
    }
    if w.locked.is_some() {
        tag("[locked]", Style::new().fg(C_UNPUSHED));
    }
    if w.prunable {
        tag("[missing]", dim(C_MUTED));
    }
    if w.outside_scan_root {
        tag("[outside]", dim(C_MUTED));
    }
    if w.stash_count > 0 {
        // Stashes live in the shared stash stack and survive removal, so this
        // is a hint and nothing more.
        tag(&format!("⚑ {}", w.stash_count), dim(C_MUTED));
    }
    spans
}

/// Right-hand aggregate on a repo header, e.g. `4 worktrees · 1.2 GB · 2 active`.
fn group_summary(g: &RepoGroup) -> String {
    let mut s = format!(
        "{} · {}",
        format::count_noun(g.worktrees.len(), "worktree"),
        format::bytes(g.known_size())
    );
    let active = g.active_count();
    if active > 0 {
        s.push_str(&format!(" · {active} active"));
    }
    s
}

/// Build the line for a repo header row.
pub(crate) fn repo_line(g: &RepoGroup, total: usize) -> Line<'static> {
    let arrow = if g.expanded { "▼" } else { "▶" };
    let name = if g.orphaned {
        format!("{} [orphan]", g.name)
    } else {
        g.name.clone()
    };

    let summary = group_summary(g);
    let fixed = 1 + 1 + 1; // leading space, arrow, space
    let room_for_name = total.saturating_sub(fixed);
    // The aggregate is dropped before the name is squeezed: knowing *which*
    // repository you are looking at matters more than knowing its totals.
    let floor = width(&name).min(HEADER_NAME_MIN);
    let (name_w, right) = if room_for_name >= floor + 2 + width(&summary) {
        (room_for_name - width(&summary) - 2, Some(summary))
    } else {
        (room_for_name, None)
    };

    let name = truncate(&name, name_w);
    let mut spans = vec![
        Span::raw(" "),
        Span::styled(arrow, Style::new().fg(C_ACCENT)),
        Span::raw(" "),
        Span::styled(
            name.clone(),
            Style::new().fg(Color::White).add_modifier(Modifier::BOLD),
        ),
    ];
    if let Some(right) = right {
        let used = fixed + width(&name);
        let pad = total.saturating_sub(used + width(&right));
        spans.push(Span::raw(" ".repeat(pad)));
        spans.push(Span::styled(right, dim(C_MUTED)));
    }
    Line::from(spans)
}

/// Build the line for a worktree row.
pub(crate) fn worktree_line(w: &Worktree, total: usize, tick: usize) -> Line<'static> {
    let name_w = name_field(total);
    let checkbox = if w.selected { "[x]" } else { "[ ]" };
    let checkbox_style = if w.selected {
        Style::new().fg(C_ACCENT).add_modifier(Modifier::BOLD)
    } else {
        dim(C_MUTED)
    };

    let mut spans = vec![
        Span::raw("   "),
        Span::styled(checkbox, checkbox_style),
        Span::raw(" "),
        activity_glyph(w.activity, tick),
        Span::raw(" "),
        Span::raw(pad_to(&w.name, name_w)),
        // A size we have not measured shows as `—`, never as `0 B`, which
        // would read as "there is nothing here to reclaim".
        Span::styled(
            pad_left(&format::bytes_opt(w.size.bytes()), SIZE_W),
            match w.size.bytes() {
                Some(_) => Style::new().fg(Color::White),
                None => dim(C_MUTED),
            },
        ),
        Span::raw("  "),
    ];
    if let Some(reason) = activity_reason(w.activity) {
        spans.push(reason);
    }
    spans.extend(worktree_tags(w));
    Line::from(spans)
}

/// Line for any visible row. Panics-free for out-of-range refs.
pub(crate) fn row_line(state: &AppState, row: RowRef, total: usize) -> Line<'static> {
    match row {
        RowRef::Repo { group } => state
            .groups
            .get(group)
            .map(|g| repo_line(g, total))
            .unwrap_or_default(),
        RowRef::Worktree { group, worktree } => state
            .groups
            .get(group)
            .and_then(|g| g.worktrees.get(worktree))
            .map(|w| worktree_line(w, total, state.spinner_tick))
            .unwrap_or_default(),
    }
}

fn title(state: &AppState) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(
            "worktree-purge",
            Style::new().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(" ── ", dim(C_MUTED)),
        Span::styled(format::tilde(&state.root), Style::new().fg(Color::White)),
        Span::raw(" "),
    ])
}

/// Render the tree, including the scrollbar and per-row indicators.
pub fn render(frame: &mut Frame, area: Rect, state: &mut AppState) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let block = Block::bordered()
        .border_style(dim(C_MUTED))
        .title(title(state));
    let inner = block.inner(area);
    frame.render_widget(block, area);
    if inner.width == 0 || inner.height == 0 {
        return;
    }

    let rows = state.rows();
    if rows.is_empty() {
        let msg = if state.enriching {
            "scanning…".to_string()
        } else {
            format!("no linked worktrees under {}", format::tilde(&state.root))
        };
        frame.render_widget(
            Paragraph::new(Line::from(Span::styled(msg, dim(C_MUTED)))).centered(),
            inner,
        );
        state.scroll = 0;
        return;
    }

    let total = inner.width as usize;
    let items: Vec<ListItem> = rows
        .iter()
        .map(|&r| ListItem::new(row_line(state, r, total)))
        .collect();

    let list = List::new(items).highlight_style(Style::new().bg(CURSOR_BG));
    let mut list_state = ListState::default()
        .with_offset(state.scroll.min(rows.len().saturating_sub(1)))
        .with_selected(Some(state.cursor.min(rows.len() - 1)));
    frame.render_stateful_widget(list, inner, &mut list_state);
    // The renderer is the only place that knows the viewport height, so it owns
    // the scroll offset. Everything else in `ui` stays a pure read of state.
    state.scroll = list_state.offset();

    if rows.len() > inner.height as usize {
        // Drawn over the block's right border so it costs no content width.
        let mut sb_state = ScrollbarState::new(rows.len())
            .position(state.scroll)
            .viewport_content_length(inner.height as usize);
        frame.render_stateful_widget(
            Scrollbar::new(ScrollbarOrientation::VerticalRight)
                .begin_symbol(None)
                .end_symbol(None)
                .track_symbol(None)
                .thumb_style(Style::new().fg(C_MUTED)),
            area.inner(Margin {
                horizontal: 0,
                vertical: 1,
            }),
            &mut sb_state,
        );
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::tests::{group, sample, state, wt};
    use crate::model::SizeState;
    use crate::ui::SPINNER;

    fn text(line: &Line<'_>) -> String {
        line.spans.iter().map(|s| s.content.as_ref()).collect()
    }

    /// Columns `[start, start + len)` of a row. Every glyph we render is one
    /// column wide, so char indices and column indices coincide; byte indices
    /// do not, which is why this helper exists.
    fn cols(s: &str, start: usize, len: usize) -> String {
        s.chars().skip(start).take(len).collect()
    }

    #[test]
    fn worktree_row_places_the_size_column_at_a_fixed_offset() {
        let a = wt("short", Some(400 * 1024 * 1024), ActivityState::Clean);
        let b = wt(
            "a-much-longer-branch-name",
            Some(1024),
            ActivityState::Clean,
        );
        let la = text(&worktree_line(&a, 78, 0));
        let lb = text(&worktree_line(&b, 78, 0));

        let field = WT_PREFIX_W + name_field(78);
        assert_eq!(field, 33);
        assert_eq!(cols(&la, 0, field), "   [ ] ○ short                   ");
        // Both rows put the size in exactly the same columns, whatever the
        // name length — that is the point of the fixed field.
        assert_eq!(cols(&la, field, SIZE_W), "  400 MB");
        assert_eq!(cols(&lb, field, SIZE_W), "  1.0 KB");
        assert_eq!(cols(&lb, WT_PREFIX_W, 24), "a-much-longer-branch-na…");
    }

    #[test]
    fn a_long_name_is_truncated_with_an_ellipsis_and_never_pushes_the_size() {
        let w = wt(
            "an-extremely-long-worktree-directory-name",
            Some(1),
            ActivityState::Clean,
        );
        let s = text(&worktree_line(&w, 78, 0));
        let field = WT_PREFIX_W + name_field(78);
        assert_eq!(
            cols(&s, WT_PREFIX_W, name_field(78)),
            "an-extremely-long-workt…"
        );
        assert_eq!(width(&cols(&s, 0, field)), field);
        assert_eq!(cols(&s, field, SIZE_W), "     1 B");
    }

    #[test]
    fn a_pending_size_renders_as_a_placeholder_not_zero_bytes() {
        let mut w = wt("wip", None, ActivityState::Pending);
        w.size = SizeState::Pending;
        let s = text(&worktree_line(&w, 78, 0));
        let field = WT_PREFIX_W + name_field(78);
        assert_eq!(cols(&s, field, SIZE_W), "       —");
        assert!(!s.contains("0 B"));
    }

    #[test]
    fn an_unknown_size_is_also_a_placeholder() {
        let mut w = wt("failed", None, ActivityState::Clean);
        w.size = SizeState::Unknown;
        let s = text(&worktree_line(&w, 78, 0));
        assert!(s.contains('—'));
        assert!(!s.contains("0 B"));
    }

    #[test]
    fn glyphs_and_colours_encode_severity() {
        use ratatui::style::Color;

        let dirty = activity_glyph(ActivityState::Active(ActiveReason::Dirty), 0);
        assert_eq!(dirty.content, "●");
        assert_eq!(dirty.style.fg, Some(Color::Red));

        let unpushed = activity_glyph(ActivityState::Active(ActiveReason::Unpushed), 0);
        assert_eq!(unpushed.content, "●");
        assert_eq!(
            unpushed.style.fg,
            Some(Color::Yellow),
            "unpushed commits survive removal — they must not be red"
        );

        // Both is dominated by the destructive half.
        let both = activity_glyph(ActivityState::Active(ActiveReason::Both), 0);
        assert_eq!(both.style.fg, Some(Color::Red));

        let clean = activity_glyph(ActivityState::Clean, 0);
        assert_eq!(clean.content, "○");
        assert_eq!(clean.style.fg, Some(Color::DarkGray));

        let unknown = activity_glyph(ActivityState::Unknown, 0);
        assert_eq!(unknown.content, "?");
        assert_ne!(
            unknown.style.fg, clean.style.fg,
            "a failed check must not look like a clean one"
        );
    }

    #[test]
    fn the_pending_glyph_animates_with_the_spinner_tick() {
        let frames: Vec<String> = (0..4)
            .map(|t| {
                activity_glyph(ActivityState::Pending, t)
                    .content
                    .into_owned()
            })
            .collect();
        assert_eq!(frames, vec!["⠋", "⠙", "⠹", "⠸"]);
        // ...and wraps rather than panicking.
        assert_eq!(
            activity_glyph(ActivityState::Pending, 10).content,
            frames[0].as_str()
        );
        assert_eq!(
            activity_glyph(ActivityState::Pending, usize::MAX).content,
            SPINNER[usize::MAX % SPINNER.len()]
        );
    }

    #[test]
    fn both_reasons_are_spelled_out_in_full() {
        let w = wt("x", Some(1), ActivityState::Active(ActiveReason::Both));
        let s = text(&worktree_line(&w, 100, 0));
        assert!(
            s.contains("uncommitted changes + unpushed commits"),
            "{s:?}"
        );
    }

    #[test]
    fn the_checkbox_reflects_selection() {
        let mut w = wt("x", Some(1), ActivityState::Clean);
        assert!(text(&worktree_line(&w, 60, 0)).starts_with("   [ ] "));
        w.selected = true;
        assert!(text(&worktree_line(&w, 60, 0)).starts_with("   [x] "));
    }

    #[test]
    fn tags_appear_only_when_the_flag_is_set() {
        let plain = wt("x", Some(1), ActivityState::Clean);
        let base = text(&worktree_line(&plain, 100, 0));
        assert!(!base.contains("[orphan]"));
        assert!(!base.contains("[locked]"));
        assert!(!base.contains("[outside]"));
        assert!(!base.contains('⚑'));

        let mut w = plain.clone();
        w.orphaned = true;
        w.locked = Some("busy".into());
        w.outside_scan_root = true;
        w.prunable = true;
        w.stash_count = 2;
        let s = text(&worktree_line(&w, 120, 0));
        assert!(s.contains("[orphan]"), "{s:?}");
        assert!(s.contains("[locked]"), "{s:?}");
        assert!(s.contains("[missing]"), "{s:?}");
        assert!(s.contains("[outside]"), "{s:?}");
        assert!(s.contains("⚑ 2"), "{s:?}");
    }

    #[test]
    fn repo_header_right_aligns_the_aggregate() {
        let g = group(
            "markdown-reader",
            vec![
                wt("a", Some(1024), ActivityState::Active(ActiveReason::Dirty)),
                wt("b", Some(1024), ActivityState::Clean),
            ],
        );
        let line = repo_line(&g, 60);
        let s = text(&line);
        assert_eq!(width(&s), 60, "the header must fill the row exactly");
        assert!(s.starts_with(" ▼ markdown-reader"), "{s:?}");
        assert!(s.ends_with("2 worktrees · 2.0 KB · 1 active"), "{s:?}");
    }

    #[test]
    fn repo_header_omits_the_active_clause_when_nothing_is_active() {
        let g = group("quiet", vec![wt("a", Some(1024), ActivityState::Clean)]);
        let s = text(&repo_line(&g, 60));
        assert!(s.ends_with("1 worktree · 1.0 KB"), "{s:?}");
        assert!(!s.contains("active"));
    }

    #[test]
    fn repo_header_arrow_tracks_expansion() {
        let mut g = group("r", vec![wt("a", Some(1), ActivityState::Clean)]);
        assert!(text(&repo_line(&g, 40)).starts_with(" ▼ r"));
        g.expanded = false;
        assert!(text(&repo_line(&g, 40)).starts_with(" ▶ r"));
    }

    #[test]
    fn repo_header_drops_the_aggregate_before_truncating_the_name() {
        let g = group("a-repository-with-a-long-name", vec![]);
        let s = text(&repo_line(&g, 24));
        assert!(!s.contains("worktree"), "aggregate must go first: {s:?}");
        assert!(s.starts_with(" ▼ a-repository"), "{s:?}");
        assert!(width(&s) <= 24, "{s:?} is {} wide", width(&s));
    }

    #[test]
    fn repo_header_size_counts_only_known_sizes() {
        let g = group(
            "r",
            vec![
                wt("known", Some(1024), ActivityState::Clean),
                wt("pending", None, ActivityState::Pending),
            ],
        );
        let s = text(&repo_line(&g, 60));
        assert!(s.ends_with("2 worktrees · 1.0 KB"), "{s:?}");
    }

    #[test]
    fn rows_never_exceed_the_viewport_on_a_narrow_terminal() {
        let mut w = wt("some-fairly-long-name", Some(4096), ActivityState::Clean);
        w.stash_count = 3;
        for total in 1..40usize {
            let line = worktree_line(&w, total, 0);
            // The line may be wider than the viewport (the List clips it), but
            // the name field must never collapse below the floor or panic.
            assert!(name_field(total) >= NAME_MIN);
            assert!(!text(&line).contains('\n'), "rows must never wrap");
        }
    }

    #[test]
    fn row_line_dispatches_on_the_row_kind() {
        let s = sample();
        let header = text(&row_line(&s, RowRef::Repo { group: 0 }, 60));
        assert!(header.starts_with(" ▼ alpha"), "{header:?}");
        let child = text(&row_line(
            &s,
            RowRef::Worktree {
                group: 0,
                worktree: 0,
            },
            60,
        ));
        assert!(child.starts_with("   [ ] ● feature-auth"), "{child:?}");
    }

    #[test]
    fn row_line_is_blank_for_an_out_of_range_ref() {
        let s = state(vec![]);
        assert_eq!(text(&row_line(&s, RowRef::Repo { group: 9 }, 40)), "");
        assert_eq!(
            text(&row_line(
                &s,
                RowRef::Worktree {
                    group: 9,
                    worktree: 9
                },
                40
            )),
            ""
        );
    }
}
