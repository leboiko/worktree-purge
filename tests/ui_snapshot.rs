//! Rendering tests: real frames through ratatui's `TestBackend`, snapshotted
//! with `insta`.
//!
//! `TestBackend`'s `Display` — what `insta` records — contains **symbols only**,
//! no style information. So every snapshot that exists to protect a *colour* is
//! paired with an explicit cell assertion (`cell_fg`, `cell`). A snapshot on its
//! own could not catch a dirty row silently turning yellow.
//!
//! State is built directly from `model` types: the scanner and purge engine are
//! being implemented in parallel and calling them here would prove nothing about
//! the UI anyway.

use std::path::PathBuf;

use ratatui::Terminal;
use ratatui::backend::TestBackend;
use ratatui::style::Color;

use worktree_purge::app::events::{Action, handle_key};
use worktree_purge::app::state::{AppState, Mode};
use worktree_purge::model::{
    ActiveReason, ActivityState, PurgeItem, PurgeOutcome, RepoGroup, ScanResult, SizeState,
    Worktree,
};
use worktree_purge::ui;

mod keys {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    pub fn ch(c: char) -> KeyEvent {
        let mods = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        KeyEvent::new(KeyCode::Char(c), mods)
    }

    pub fn code(c: KeyCode) -> KeyEvent {
        KeyEvent::new(c, KeyModifiers::NONE)
    }
}

const MB: u64 = 1024 * 1024;

// -- fixture -------------------------------------------------------------

fn worktree(name: &str, size: SizeState, activity: ActivityState) -> Worktree {
    let mut w = Worktree::new(PathBuf::from(format!("/home/dev/{name}")));
    w.branch = Some(name.to_string());
    w.size = size;
    w.activity = activity;
    w
}

fn group(name: &str, worktrees: Vec<Worktree>) -> RepoGroup {
    let mut g = RepoGroup::new(
        PathBuf::from(format!("/home/dev/{name}")),
        PathBuf::from(format!("/home/dev/{name}/.git")),
    );
    g.worktrees = worktrees;
    g
}

/// A scene with one of every activity state, plus tags and a stash hint.
fn scene() -> AppState {
    let mut stashed = worktree(
        "fix-parser",
        SizeState::Ready(318 * MB),
        ActivityState::Active(ActiveReason::Unpushed),
    );
    stashed.stash_count = 2;

    let mut orphan = worktree(
        "stray-checkout",
        SizeState::Ready(12 * MB),
        ActivityState::Unknown,
    );
    orphan.orphaned = true;

    let mut locked = worktree("release-prep", SizeState::Pending, ActivityState::Pending);
    locked.locked = Some("cutting 1.4".into());
    locked.outside_scan_root = true;

    let mut state = AppState::from_scan(
        PathBuf::from("/home/dev"),
        ScanResult {
            groups: vec![
                group(
                    "markdown-reader",
                    vec![
                        worktree(
                            "feature-auth",
                            SizeState::Ready(412 * MB),
                            ActivityState::Active(ActiveReason::Dirty),
                        ),
                        stashed,
                        // Deliberately the largest, so name order and size
                        // order differ and the sort test can tell them apart.
                        worktree(
                            "old-experiment",
                            SizeState::Ready(690 * MB),
                            ActivityState::Clean,
                        ),
                    ],
                ),
                group("worktree-purge", vec![locked, orphan]),
            ],
            ..Default::default()
        },
    );
    // Pin the spinner so the pending row is deterministic.
    state.spinner_tick = 0;
    state
}

// -- helpers -------------------------------------------------------------

fn terminal(w: u16, h: u16) -> Terminal<TestBackend> {
    Terminal::new(TestBackend::new(w, h)).expect("build test terminal")
}

fn render(state: &mut AppState, w: u16, h: u16) -> Terminal<TestBackend> {
    let mut term = terminal(w, h);
    term.draw(|frame| ui::draw(frame, state)).expect("draw");
    term
}

/// Symbol at a cell.
fn cell(term: &Terminal<TestBackend>, x: u16, y: u16) -> String {
    term.backend().buffer()[(x, y)].symbol().to_string()
}

/// Foreground colour at a cell.
fn cell_fg(term: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
    term.backend().buffer()[(x, y)].fg
}

/// Background colour at a cell.
fn cell_bg(term: &Terminal<TestBackend>, x: u16, y: u16) -> Color {
    term.backend().buffer()[(x, y)].bg
}

/// The whole text of one row.
fn row(term: &Terminal<TestBackend>, y: u16) -> String {
    let buf = term.backend().buffer();
    (0..buf.area.width)
        .map(|x| buf[(x, y)].symbol().to_string())
        .collect()
}

/// Column of the activity glyph on a worktree row: 1 border + 3 indent +
/// 3 checkbox + 1 space.
const GLYPH_X: u16 = 8;
/// Column of the checkbox marker (`x` or a space) on a worktree row.
const CHECK_X: u16 = 5;
/// Column of the expand arrow on a repo header row: 1 border + 1 space.
const ARROW_X: u16 = 1 + 1;

/// Text inside the rounded overlay box, excluding the surrounding browse view.
///
/// Assertions about "what the modal says" must not be able to pass because the
/// tree behind the modal happens to mention the same name.
fn modal_text(term: &Terminal<TestBackend>) -> String {
    let buf = term.backend().buffer();
    let rows: Vec<String> = (0..buf.area.height).map(|y| row(term, y)).collect();
    let top = rows
        .iter()
        .position(|r| r.contains('╭'))
        .expect("no rounded overlay on screen");
    let bottom = rows
        .iter()
        .rposition(|r| r.contains('╰'))
        .expect("overlay bottom");
    let left = rows[top]
        .chars()
        .position(|c| c == '╭')
        .expect("overlay left");
    let right = rows[top]
        .chars()
        .position(|c| c == '╮')
        .expect("overlay right");

    rows[top..=bottom]
        .iter()
        .map(|r| {
            r.chars()
                .skip(left)
                .take(right - left + 1)
                .collect::<String>()
        })
        .collect::<Vec<_>>()
        .join("\n")
}

// -- browse view ---------------------------------------------------------

#[test]
fn browse_view_with_a_mix_of_states() {
    let mut state = scene();
    state.groups[0].worktrees[0].selected = true;
    let term = render(&mut state, 78, 10);
    insta::assert_snapshot!("browse_mixed", term.backend());
}

/// The snapshot above records symbols only. These assertions pin the severity
/// colours, which are the actual safety signal.
#[test]
fn activity_colours_carry_the_severity_distinction() {
    let mut state = scene();
    let term = render(&mut state, 78, 10);

    // Row 1 = header, rows 2..4 = markdown-reader's worktrees.
    assert_eq!(cell(&term, GLYPH_X, 2), "●");
    assert_eq!(
        cell_fg(&term, GLYPH_X, 2),
        Color::Red,
        "feature-auth is dirty: that work is destroyed by a purge"
    );

    assert_eq!(cell(&term, GLYPH_X, 3), "●");
    assert_eq!(
        cell_fg(&term, GLYPH_X, 3),
        Color::Yellow,
        "fix-parser only has unpushed commits, which survive removal — \
         it must not share the red of a dirty worktree"
    );

    assert_eq!(cell(&term, GLYPH_X, 4), "○");
    assert_eq!(cell_fg(&term, GLYPH_X, 4), Color::DarkGray);

    // ...and the two glyph colours genuinely differ.
    assert_ne!(cell_fg(&term, GLYPH_X, 2), cell_fg(&term, GLYPH_X, 3));
}

#[test]
fn pending_and_unknown_rows_are_visually_distinct_from_clean_ones() {
    let mut state = scene();
    let term = render(&mut state, 78, 10);

    // worktree-purge group: header on row 5, then release-prep, stray-checkout.
    assert_eq!(cell(&term, GLYPH_X, 6), "⠋", "pending shows a spinner");
    assert_eq!(cell(&term, GLYPH_X, 7), "?", "a failed check shows `?`");

    let clean_fg = cell_fg(&term, GLYPH_X, 4);
    assert_ne!(
        cell_fg(&term, GLYPH_X, 7),
        clean_fg,
        "an unknown status must not be styled like a proven-clean one"
    );
    assert_ne!(cell_fg(&term, GLYPH_X, 6), clean_fg);
}

#[test]
fn the_spinner_advances_with_the_tick() {
    let mut state = scene();
    let first = render(&mut state, 78, 10);
    let frame0 = cell(&first, GLYPH_X, 6);

    state.spinner_tick = 3;
    let second = render(&mut state, 78, 10);
    let frame3 = cell(&second, GLYPH_X, 6);

    assert_ne!(frame0, frame3, "the pending glyph must animate");
    assert_eq!(frame3, "⠸");
    // Nothing else on the row changed.
    assert_eq!(
        row(&first, 6).replace(&frame0, "@"),
        row(&second, 6).replace(&frame3, "@")
    );
}

#[test]
fn a_pending_size_is_never_rendered_as_zero_bytes() {
    let mut state = scene();
    let term = render(&mut state, 78, 10);
    let pending_row = row(&term, 6);
    assert!(pending_row.contains('—'), "{pending_row:?}");
    assert!(
        !pending_row.contains("0 B"),
        "an unmeasured worktree must not claim there is nothing to reclaim: {pending_row:?}"
    );
}

#[test]
fn the_cursor_row_is_highlighted_and_nothing_else_is() {
    let mut state = scene();
    state.cursor = 2;
    let term = render(&mut state, 78, 10);

    let bg = cell_bg(&term, 5, 3); // inside the tree, on the cursor row
    assert_ne!(bg, Color::Reset, "the cursor row must have a background");
    assert_eq!(
        cell_bg(&term, 5, 2),
        Color::Reset,
        "the row above must not be highlighted"
    );
    assert_eq!(cell_bg(&term, 5, 4), Color::Reset);
}

#[test]
fn selecting_a_row_flips_its_checkbox_and_nothing_else() {
    let mut state = scene();
    let before = render(&mut state, 78, 10);
    let baseline: Vec<String> = (0..10).map(|y| row(&before, y)).collect();

    state.cursor = 1;
    handle_key(&mut state, keys::ch(' '));
    let after = render(&mut state, 78, 10);

    // Exactly one cell of the tree changed: the checkbox marker.
    let changed: Vec<u16> = (0..10)
        .filter(|&y| row(&after, y) != baseline[y as usize])
        .collect();
    assert_eq!(
        changed,
        vec![2, 9],
        "only the row and the status bar change"
    );
    assert_eq!(cell(&before, CHECK_X, 2), " ");
    assert_eq!(cell(&after, CHECK_X, 2), "x");
    assert!(row(&after, 9).contains("selected 1 (412 MB)"));
}

#[test]
fn collapsing_a_group_removes_exactly_its_children() {
    let mut state = scene();
    let expanded = render(&mut state, 78, 10);
    assert_eq!(cell(&expanded, ARROW_X, 1), "▼");

    state.cursor = 0;
    handle_key(&mut state, keys::code(crossterm::event::KeyCode::Left));
    let collapsed = render(&mut state, 78, 10);

    assert_eq!(cell(&collapsed, ARROW_X, 1), "▶");
    // The three children are gone; the second header moved up three rows.
    assert_eq!(row(&expanded, 5).trim_end(), row(&collapsed, 2).trim_end());
    assert!(row(&collapsed, 3).contains("release-prep"));
    // ...and no row still mentions a hidden child.
    for y in 0..10 {
        assert!(
            !row(&collapsed, y).contains("feature-auth"),
            "row {y} still shows a hidden child: {:?}",
            row(&collapsed, y)
        );
    }
    insta::assert_snapshot!("browse_collapsed", collapsed.backend());
}

#[test]
fn an_empty_scan_says_so_instead_of_rendering_a_blank_box() {
    let mut state = AppState::from_scan(PathBuf::from("/home/dev"), ScanResult::default());
    let term = render(&mut state, 60, 8);
    insta::assert_snapshot!("browse_empty", term.backend());
    assert!(
        row(&term, 1).contains("no linked worktrees under /home/dev"),
        "{:?}",
        row(&term, 1)
    );
    assert!(row(&term, 7).contains("0 repos · 0 worktrees · 0 B"));
}

// -- modals --------------------------------------------------------------

/// Walk the real key path into the first gate, so the snapshot cannot drift
/// away from what the event handler actually produces.
fn confirm_state(selected: &[(usize, usize)]) -> AppState {
    let mut state = scene();
    for &(g, w) in selected {
        state.groups[g].worktrees[w].selected = true;
    }
    assert_eq!(handle_key(&mut state, keys::ch('d')), Action::None);
    state
}

#[test]
fn the_confirm_modal_lists_items_sizes_and_the_total() {
    let mut state = confirm_state(&[(0, 0), (0, 2)]);
    assert!(matches!(state.mode, Mode::Confirm(_)));
    let term = render(&mut state, 78, 20);
    insta::assert_snapshot!("modal_confirm", term.backend());

    let text = modal_text(&term);
    assert!(text.contains("feature-auth"), "{text}");
    assert!(text.contains("412 MB"), "{text}");
    assert!(text.contains("old-experiment"), "{text}");
    assert!(text.contains("690 MB"), "{text}");
    assert!(text.contains("reclaims 1.1 GB"), "{text}");
    assert!(
        text.contains("1 worktree marked ! holds work — you will be asked again"),
        "the modal must announce the second gate: {text}"
    );
}

#[test]
fn a_clean_only_confirm_modal_does_not_promise_a_second_gate() {
    let mut state = confirm_state(&[(0, 2)]);
    let term = render(&mut state, 78, 20);
    let text = modal_text(&term);
    assert!(text.contains("reclaims 690 MB"), "{text}");
    assert!(!text.contains("you will be asked again"), "{text}");
    assert!(text.contains("[y] remove"), "{text}");
}

#[test]
fn the_force_modal_names_the_distinct_key_and_only_the_risky_items() {
    let mut state = confirm_state(&[(0, 0), (0, 2)]);
    assert_eq!(handle_key(&mut state, keys::ch('y')), Action::None);
    assert!(matches!(state.mode, Mode::ForceConfirm(_)));

    let term = render(&mut state, 78, 20);
    insta::assert_snapshot!("modal_force_confirm", term.backend());

    let text = modal_text(&term);
    assert!(text.contains("1 worktree holds work"), "{text}");
    assert!(text.contains("feature-auth"), "{text}");
    assert!(
        !text.contains("old-experiment"),
        "the clean item has no business in the force gate: {text}"
    );
    assert!(text.contains("[F] to force-remove"), "{text}");
    assert!(
        text.contains("y and Enter do nothing here"),
        "the modal must say plainly that the usual keys are inert: {text}"
    );
    assert!(
        text.contains("Commits and branches are not"),
        "the modal must state what is *not* destroyed: {text}"
    );
}

/// The rendered second gate and the handler agree: what the modal says is what
/// the keymap does.
#[test]
fn the_force_modal_matches_the_behaviour_it_advertises() {
    let mut state = confirm_state(&[(0, 0)]);
    handle_key(&mut state, keys::ch('y'));

    assert_eq!(handle_key(&mut state, keys::ch('y')), Action::None);
    assert_eq!(
        handle_key(&mut state, keys::code(crossterm::event::KeyCode::Enter)),
        Action::None
    );
    assert!(matches!(state.mode, Mode::ForceConfirm(_)));
    assert_eq!(handle_key(&mut state, keys::ch('F')), Action::Purge);
}

#[test]
fn the_results_modal_shows_per_item_outcomes() {
    let mut state = scene();
    state.mode = Mode::Results(vec![
        PurgeOutcome {
            item: PurgeItem {
                worktree_path: PathBuf::from("/home/dev/feature-auth"),
                repo_root: Some(PathBuf::from("/home/dev/markdown-reader")),
                name: "feature-auth".into(),
                size: Some(412 * MB),
                requires_force: true,
            },
            result: Ok(412 * MB),
        },
        PurgeOutcome {
            item: PurgeItem {
                worktree_path: PathBuf::from("/home/dev/release-prep"),
                repo_root: Some(PathBuf::from("/home/dev/worktree-purge")),
                name: "release-prep".into(),
                size: None,
                requires_force: true,
            },
            result: Err("fatal: 'release-prep' is locked".into()),
        },
    ]);
    state.toast = Some("freed 412 MB · 1 removal failed".into());
    let term = render(&mut state, 78, 20);
    insta::assert_snapshot!("modal_results", term.backend());

    let text = modal_text(&term);
    assert!(text.contains("✓ feature-auth"), "{text}");
    assert!(text.contains("✗ release-prep"), "{text}");
    assert!(text.contains("fatal: 'release-prep' is locked"), "{text}");
    assert!(text.contains("freed 412 MB · 1 removal failed"), "{text}");
    // The toast is already in the status bar underneath, so the outcome stays
    // visible once the modal closes.
    assert!(
        row(&term, 19).contains("freed 412 MB"),
        "{:?}",
        row(&term, 19)
    );
}

#[test]
fn the_help_overlay_documents_every_binding() {
    let mut state = scene();
    state.mode = Mode::Help;
    let term = render(&mut state, 78, 26);
    insta::assert_snapshot!("modal_help", term.backend());

    let text = modal_text(&term);
    for binding in ["j k", "Space", "d x", "s", "r", "g G"] {
        assert!(text.contains(binding), "missing {binding:?} in:\n{text}");
    }
    assert!(
        text.contains("y confirms, then F forces"),
        "the overlay must name both gate keys:\n{text}"
    );
}

// -- degradation ---------------------------------------------------------

#[test]
fn a_very_narrow_terminal_truncates_rather_than_wrapping_or_panicking() {
    let mut state = scene();
    let term = render(&mut state, 32, 9);
    insta::assert_snapshot!("browse_narrow", term.backend());

    let buf = term.backend().buffer();
    assert_eq!(buf.area.width, 32);
    // Every row is exactly one screen line: nothing wrapped into the next.
    for y in 0..9u16 {
        let text = row(&term, y);
        assert_eq!(text.chars().count(), 32, "row {y}: {text:?}");
    }
    // The tree still shows one row per worktree, borders intact.
    assert_eq!(cell(&term, 0, 0), "┌");
    assert_eq!(cell(&term, 31, 0), "┐");
    assert!(row(&term, 2).contains('…'), "{:?}", row(&term, 2));
}

#[test]
fn a_modal_still_fits_a_narrow_terminal() {
    let mut state = confirm_state(&[(0, 0)]);
    let term = render(&mut state, 30, 12);
    insta::assert_snapshot!("modal_confirm_narrow", term.backend());
    for y in 0..12u16 {
        assert_eq!(row(&term, y).chars().count(), 30);
    }
}

#[test]
fn tiny_rects_render_without_panicking() {
    let mut state = scene();
    for (w, h) in [(1, 1), (2, 1), (1, 2), (3, 3), (4, 2), (10, 4)] {
        let mut term = terminal(w, h);
        term.draw(|frame| ui::draw(frame, &mut state))
            .unwrap_or_else(|_| panic!("draw at {w}x{h}"));
    }
    // Same, with every overlay open.
    for mode in [
        Mode::Help,
        Mode::Purging,
        Mode::Results(Vec::new()),
        Mode::Confirm(state.build_plan()),
    ] {
        state.mode = mode;
        for (w, h) in [(1, 1), (5, 3), (12, 5)] {
            let mut term = terminal(w, h);
            term.draw(|frame| ui::draw(frame, &mut state))
                .unwrap_or_else(|_| panic!("draw at {w}x{h}"));
        }
    }
}

#[test]
fn scrolling_keeps_the_cursor_visible_in_a_short_viewport() {
    let mut state = scene();
    // 5 content rows for 7 tree rows.
    state.cursor_bottom();
    let term = render(&mut state, 78, 8);

    let last = state.rows().len() - 1;
    assert_eq!(state.cursor, last);
    assert!(state.scroll > 0, "the view must have scrolled");
    // The last row of the tree area holds the cursor's worktree.
    assert!(
        row(&term, 5).contains("stray-checkout"),
        "{:?}",
        row(&term, 5)
    );
    // The first worktree scrolled off.
    let text: String = (0..8).map(|y| row(&term, y)).collect::<Vec<_>>().join("\n");
    assert!(!text.contains("feature-auth"), "{text}");

    // Coming back to the top brings it into view again.
    state.cursor_top();
    let term = render(&mut state, 78, 8);
    assert_eq!(state.scroll, 0);
    assert!(
        row(&term, 2).contains("feature-auth"),
        "{:?}",
        row(&term, 2)
    );
}

#[test]
fn sorting_visibly_reorders_the_rendered_rows() {
    let mut state = scene();
    state.cursor = 2; // fix-parser, 318 MB
    let by_name = render(&mut state, 78, 10);
    let names_before: Vec<String> = (2..5).map(|y| row(&by_name, y)).collect();

    handle_key(&mut state, keys::ch('s')); // -> size desc
    let by_size = render(&mut state, 78, 10);
    let names_after: Vec<String> = (2..5).map(|y| row(&by_size, y)).collect();

    assert_ne!(names_before, names_after);
    assert!(names_before[0].contains("feature-auth"), "{names_before:?}");
    assert!(
        names_before[2].contains("old-experiment"),
        "{names_before:?}"
    );
    // Largest first: old-experiment (690 MB), feature-auth (412), fix-parser (318).
    assert!(names_after[0].contains("old-experiment"), "{names_after:?}");
    assert!(names_after[1].contains("feature-auth"), "{names_after:?}");
    assert!(names_after[2].contains("fix-parser"), "{names_after:?}");
    // The cursor followed the worktree it was on, not the row index.
    assert_eq!(
        state.focused_path().unwrap(),
        PathBuf::from("/home/dev/fix-parser")
    );
    assert_eq!(state.cursor, 3);
    assert!(row(&by_size, 9).contains("sort: size"));
}
