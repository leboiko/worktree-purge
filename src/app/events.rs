//! Key handling: crossterm events in, state mutations out.
//!
//! The interesting part of this module is the two-stage confirmation. See
//! [`confirm_key`] and [`force_key`] — every other binding is ordinary
//! navigation.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

use crate::app::state::{AppState, Mode};
use crate::format;
use crate::model::RowRef;

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

/// Key that confirms the *first* gate: "yes, remove these".
pub const CONFIRM_KEY: char = 'y';

/// Key that confirms the *second* gate for worktrees that would lose work.
///
/// Deliberately not [`CONFIRM_KEY`] and deliberately not `Enter`: a user
/// holding either of those down, or double-tapping out of muscle memory, must
/// not be able to fall through both gates. It is also uppercase, so it needs
/// Shift — one more piece of evidence that the keypress was intended.
pub const FORCE_KEY: char = 'F';

/// How far `PageUp`/`PageDown` move. A fixed step keeps the binding a pure
/// function of state; the exact viewport height lives in the renderer.
const PAGE: isize = 10;

/// Apply a keypress to the state, returning any side effect for the caller.
pub fn handle_key(state: &mut AppState, key: KeyEvent) -> Action {
    // Ctrl-C is an escape hatch from every mode, including a modal.
    if key.modifiers.contains(KeyModifiers::CONTROL)
        && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
    {
        state.should_quit = true;
        return Action::Quit;
    }

    match state.mode {
        Mode::Browse => browse_key(state, key),
        Mode::Confirm(_) => confirm_key(state, key),
        Mode::ForceConfirm(_) => force_key(state, key),
        Mode::Purging => Action::None,
        Mode::Results(_) => results_key(state, key),
        Mode::Help => help_key(state, key),
    }
}

fn browse_key(state: &mut AppState, key: KeyEvent) -> Action {
    // Any deliberate keypress dismisses the last toast; handlers below are free
    // to set a new one.
    state.toast = None;

    match key.code {
        KeyCode::Up | KeyCode::Char('k') => state.move_cursor(-1),
        KeyCode::Down | KeyCode::Char('j') => state.move_cursor(1),
        KeyCode::PageUp => state.move_cursor(-PAGE),
        KeyCode::PageDown => state.move_cursor(PAGE),
        KeyCode::Home => state.cursor_top(),
        KeyCode::End => state.cursor_bottom(),
        KeyCode::Char('g') => state.cursor_top(),
        KeyCode::Char('G') => state.cursor_bottom(),

        // Left on a child walks up to its header rather than collapsing the
        // group out from under the cursor.
        KeyCode::Left | KeyCode::Char('h') => match state.current_row() {
            Some(RowRef::Worktree { group, .. }) => {
                if let Some(i) = state
                    .rows()
                    .iter()
                    .position(|&r| r == RowRef::Repo { group })
                {
                    state.cursor = i;
                }
            }
            _ => state.set_expanded(false),
        },
        // Right expands, and steps into the group when it is already open.
        KeyCode::Right | KeyCode::Char('l') => {
            if let Some(RowRef::Repo { group }) = state.current_row() {
                if state.groups[group].expanded {
                    if !state.groups[group].worktrees.is_empty() {
                        state.move_cursor(1);
                    }
                } else {
                    state.set_expanded(true);
                }
            }
        }
        KeyCode::Enter => state.toggle_expanded(),

        KeyCode::Char(' ') => state.toggle_selection(),
        KeyCode::Char('a') => {
            let target = !state.current_group_all_selected();
            state.select_group(target);
        }
        KeyCode::Char('A') => {
            let target = !state.all_selected();
            state.select_all(target);
        }

        KeyCode::Char('d') | KeyCode::Char('x') => return open_confirm(state),
        KeyCode::Char('s') => state.cycle_sort(),
        KeyCode::Char('r') => return Action::Rescan,
        KeyCode::Char('?') => state.mode = Mode::Help,

        KeyCode::Char('q') | KeyCode::Esc => {
            state.should_quit = true;
            return Action::Quit;
        }
        _ => {}
    }
    Action::None
}

/// `d`/`x`: build the plan and open the first gate.
fn open_confirm(state: &mut AppState) -> Action {
    let plan = state.build_plan();
    if plan.is_empty() {
        state.toast = Some(if state.worktree_count() == 0 {
            "nothing to purge".to_string()
        } else {
            "nothing selected — press Space on a worktree first".to_string()
        });
        return Action::None;
    }
    state.mode = Mode::Confirm(plan);
    Action::None
}

/// First gate. Confirming a plan that contains anything unsafe advances to the
/// second gate — it never purges directly.
fn confirm_key(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char(CONFIRM_KEY) | KeyCode::Enter => {
            let Mode::Confirm(plan) = &state.mode else {
                return Action::None;
            };
            if plan.needs_force() {
                let plan = plan.clone();
                state.mode = Mode::ForceConfirm(plan);
                Action::None
            } else {
                // The caller reads the plan back out of `state.mode`, so the
                // mode must stay put until the purge has been started.
                Action::Purge
            }
        }
        KeyCode::Char('n') | KeyCode::Char('q') | KeyCode::Esc => {
            state.mode = Mode::Browse;
            Action::None
        }
        // Everything else — navigation included — is swallowed by the modal.
        _ => Action::None,
    }
}

/// Second gate. Only [`FORCE_KEY`] gets through; `y` and `Enter` are inert
/// here by design.
fn force_key(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Char(FORCE_KEY) => Action::Purge,
        KeyCode::Char('n') | KeyCode::Char('q') | KeyCode::Esc => {
            state.mode = Mode::Browse;
            Action::None
        }
        _ => Action::None,
    }
}

fn results_key(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        // The toast set by the event loop ("freed 1.2 GB") is deliberately left
        // in place so the outcome stays visible after the modal closes.
        KeyCode::Enter | KeyCode::Esc | KeyCode::Char('q') | KeyCode::Char(' ') => {
            state.mode = Mode::Browse;
        }
        _ => {}
    }
    Action::None
}

fn help_key(state: &mut AppState, key: KeyEvent) -> Action {
    match key.code {
        KeyCode::Enter
        | KeyCode::Esc
        | KeyCode::Char('q')
        | KeyCode::Char('?')
        | KeyCode::Char(' ') => state.mode = Mode::Browse,
        _ => {}
    }
    Action::None
}

/// One line describing the current selection, used by the status bar.
pub fn selection_summary(state: &AppState) -> Option<String> {
    let n = state.selected_count();
    if n == 0 {
        return None;
    }
    Some(format!(
        "selected {n} ({})",
        format::bytes(state.selected_size())
    ))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::app::state::tests::{group, sample, state, wt};
    use crate::model::{ActiveReason, ActivityState, PurgeItem, PurgeOutcome, SortMode};
    use std::path::PathBuf;

    fn press(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn ch(c: char) -> KeyEvent {
        // Uppercase letters arrive with SHIFT set; carry that so the tests
        // exercise the same events crossterm produces.
        let mods = if c.is_ascii_uppercase() {
            KeyModifiers::SHIFT
        } else {
            KeyModifiers::NONE
        };
        KeyEvent::new(KeyCode::Char(c), mods)
    }

    fn row_names(s: &AppState) -> Vec<String> {
        s.rows()
            .iter()
            .map(|&r| match r {
                RowRef::Repo { group } => format!("repo:{}", s.groups[group].name),
                RowRef::Worktree { group, worktree } => {
                    format!("wt:{}", s.groups[group].worktrees[worktree].name)
                }
            })
            .collect()
    }

    // -- navigation --------------------------------------------------------

    #[test]
    fn vertical_motion_uses_both_arrow_and_vi_keys() {
        let mut s = sample();
        assert_eq!(handle_key(&mut s, ch('j')), Action::None);
        assert_eq!(s.cursor, 1);
        handle_key(&mut s, press(KeyCode::Down));
        assert_eq!(s.cursor, 2);
        handle_key(&mut s, ch('k'));
        assert_eq!(s.cursor, 1);
        handle_key(&mut s, press(KeyCode::Up));
        assert_eq!(s.cursor, 0);
        handle_key(&mut s, press(KeyCode::Up));
        assert_eq!(s.cursor, 0, "clamped at the top");
    }

    #[test]
    fn g_and_shift_g_jump_to_the_extremes() {
        let mut s = sample();
        handle_key(&mut s, ch('G'));
        assert_eq!(s.cursor, s.rows().len() - 1);
        handle_key(&mut s, ch('g'));
        assert_eq!(s.cursor, 0);
    }

    #[test]
    fn left_on_a_child_walks_to_the_header_without_collapsing() {
        let mut s = sample();
        s.cursor = 3;
        let before = s.rows().len();
        handle_key(&mut s, ch('h'));
        assert_eq!(s.cursor, 0);
        assert_eq!(s.rows().len(), before, "the group must still be expanded");
        assert!(s.groups[0].expanded);
    }

    #[test]
    fn left_on_a_header_collapses_and_right_expands_again() {
        let mut s = sample();
        s.cursor = 0;
        handle_key(&mut s, press(KeyCode::Left));
        assert!(!s.groups[0].expanded);
        assert_eq!(row_names(&s), vec!["repo:alpha", "repo:beta", "wt:solo"]);

        handle_key(&mut s, press(KeyCode::Right));
        assert!(s.groups[0].expanded);
        assert_eq!(s.rows().len(), 6);
        assert_eq!(s.cursor, 0, "expanding leaves the cursor on the header");

        // A second Right steps into the group.
        handle_key(&mut s, ch('l'));
        assert_eq!(s.cursor, 1);
    }

    #[test]
    fn enter_toggles_expansion_of_the_group_under_the_cursor() {
        let mut s = sample();
        s.cursor = 4; // repo:beta
        handle_key(&mut s, press(KeyCode::Enter));
        assert!(!s.groups[1].expanded);
        assert_eq!(s.rows().len(), 5);
        handle_key(&mut s, press(KeyCode::Enter));
        assert!(s.groups[1].expanded);
        assert_eq!(s.rows().len(), 6);
    }

    // -- selection ---------------------------------------------------------

    #[test]
    fn space_scopes_to_the_row_under_the_cursor() {
        let mut s = sample();
        s.cursor = 1;
        handle_key(&mut s, ch(' '));
        assert_eq!(s.selected_count(), 1);
        handle_key(&mut s, ch(' '));
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn a_selects_the_current_repo_only_and_shift_a_selects_everything() {
        let mut s = sample();
        s.cursor = 2; // inside alpha

        handle_key(&mut s, ch('a'));
        assert_eq!(s.groups[0].selected_count(), 3);
        assert_eq!(s.groups[1].selected_count(), 0);
        assert_eq!(s.selected_count(), 3);

        handle_key(&mut s, ch('a'));
        assert_eq!(s.selected_count(), 0, "`a` toggles back off");

        handle_key(&mut s, ch('A'));
        assert_eq!(s.groups[0].selected_count(), 3);
        assert_eq!(s.groups[1].selected_count(), 1);
        assert_eq!(s.selected_count(), 4);

        handle_key(&mut s, ch('A'));
        assert_eq!(s.selected_count(), 0);
    }

    #[test]
    fn shift_a_completes_a_partial_selection_before_clearing_it() {
        let mut s = sample();
        s.cursor = 1;
        handle_key(&mut s, ch(' ')); // one of four
        handle_key(&mut s, ch('A'));
        assert_eq!(s.selected_count(), 4, "partial -> all");
        handle_key(&mut s, ch('A'));
        assert_eq!(s.selected_count(), 0, "all -> none");
    }

    // -- sort / rescan / help ---------------------------------------------

    #[test]
    fn s_cycles_the_sort_mode_and_reorders_rows() {
        let mut s = sample();
        s.cursor = 1;
        let before = row_names(&s);
        handle_key(&mut s, ch('s'));
        assert_eq!(s.sort, SortMode::SizeDesc);
        assert_ne!(row_names(&s), before, "size order differs from name order");
        assert_eq!(
            s.focused_path().unwrap(),
            PathBuf::from("/scan/feature-auth"),
            "the cursor stayed on the same worktree"
        );
    }

    #[test]
    fn r_asks_the_event_loop_to_rescan_without_touching_the_model() {
        let mut s = sample();
        let before = s.groups.clone();
        assert_eq!(handle_key(&mut s, ch('r')), Action::Rescan);
        assert_eq!(s.groups, before);
        assert_eq!(s.mode, Mode::Browse);
    }

    #[test]
    fn question_mark_opens_help_and_the_overlay_swallows_navigation() {
        let mut s = sample();
        s.cursor = 2;
        handle_key(&mut s, ch('?'));
        assert_eq!(s.mode, Mode::Help);

        handle_key(&mut s, ch('j'));
        assert_eq!(s.cursor, 2, "help must swallow navigation keys");
        assert_eq!(s.mode, Mode::Help);

        handle_key(&mut s, press(KeyCode::Esc));
        assert_eq!(s.mode, Mode::Browse);
        assert!(!s.should_quit, "Esc closed the overlay, it did not quit");
    }

    #[test]
    fn q_in_help_closes_the_overlay_instead_of_quitting() {
        let mut s = sample();
        s.mode = Mode::Help;
        assert_eq!(handle_key(&mut s, ch('q')), Action::None);
        assert_eq!(s.mode, Mode::Browse);
        assert!(!s.should_quit);
    }

    #[test]
    fn q_and_esc_quit_from_browse() {
        let mut s = sample();
        assert_eq!(handle_key(&mut s, ch('q')), Action::Quit);
        assert!(s.should_quit);

        let mut s = sample();
        assert_eq!(handle_key(&mut s, press(KeyCode::Esc)), Action::Quit);
        assert!(s.should_quit);
    }

    #[test]
    fn ctrl_c_quits_even_from_a_modal() {
        let mut s = sample();
        s.cursor = 1;
        handle_key(&mut s, ch(' '));
        handle_key(&mut s, ch('d'));
        assert!(matches!(s.mode, Mode::Confirm(_)));

        let ctrl_c = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(handle_key(&mut s, ctrl_c), Action::Quit);
        assert!(s.should_quit);
    }

    // -- the two-stage confirmation ---------------------------------------

    /// Select the dirty worktree (`feature-auth`) and open the first gate.
    fn confirm_dirty() -> AppState {
        let mut s = sample();
        s.cursor = 1;
        handle_key(&mut s, ch(' '));
        assert_eq!(handle_key(&mut s, ch('d')), Action::None);
        s
    }

    #[test]
    fn d_opens_a_confirmation_containing_exactly_the_selection() {
        let s = confirm_dirty();
        match &s.mode {
            Mode::Confirm(plan) => {
                assert_eq!(plan.len(), 1);
                assert_eq!(plan.items[0].name, "feature-auth");
                assert!(plan.items[0].requires_force);
                assert_eq!(plan.total_size(), 400);
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn x_is_an_alias_for_d() {
        let mut s = sample();
        s.cursor = 1;
        handle_key(&mut s, ch(' '));
        handle_key(&mut s, ch('x'));
        assert!(matches!(s.mode, Mode::Confirm(_)));
    }

    /// The single most important test in this milestone: a plan that would lose
    /// work cannot reach `Action::Purge` through the first gate, and cannot be
    /// pushed through the second one with the first gate's keys.
    #[test]
    fn a_dirty_plan_cannot_purge_without_passing_the_second_gate() {
        let mut s = confirm_dirty();

        // Stage one: `y` advances, it does not purge.
        assert_eq!(handle_key(&mut s, ch(CONFIRM_KEY)), Action::None);
        assert!(
            matches!(s.mode, Mode::ForceConfirm(_)),
            "confirming a forceful plan must land in the second gate, got {:?}",
            s.mode
        );

        // Stage two: the stage-one key is inert. Pressing it repeatedly — the
        // muscle-memory failure mode — must never purge.
        for _ in 0..5 {
            assert_eq!(
                handle_key(&mut s, ch(CONFIRM_KEY)),
                Action::None,
                "`{CONFIRM_KEY}` must not carry through the force gate"
            );
            assert!(matches!(s.mode, Mode::ForceConfirm(_)));
        }

        // Enter is inert too, for the same reason.
        assert_eq!(handle_key(&mut s, press(KeyCode::Enter)), Action::None);
        assert!(matches!(s.mode, Mode::ForceConfirm(_)));

        // So is the lowercase form of the force key.
        assert_eq!(handle_key(&mut s, ch('f')), Action::None);
        assert!(matches!(s.mode, Mode::ForceConfirm(_)));

        // Only the deliberate uppercase F gets through.
        assert_eq!(handle_key(&mut s, ch(FORCE_KEY)), Action::Purge);
    }

    #[test]
    fn enter_at_the_first_gate_also_stops_at_the_second() {
        let mut s = confirm_dirty();
        assert_eq!(handle_key(&mut s, press(KeyCode::Enter)), Action::None);
        assert!(matches!(s.mode, Mode::ForceConfirm(_)));
    }

    #[test]
    fn a_clean_plan_purges_after_a_single_confirmation() {
        let mut s = sample();
        s.cursor = 3; // wt:old-experiment, Clean
        handle_key(&mut s, ch(' '));
        handle_key(&mut s, ch('d'));
        match &s.mode {
            Mode::Confirm(plan) => assert!(!plan.needs_force()),
            other => panic!("expected Confirm, got {other:?}"),
        }
        assert_eq!(handle_key(&mut s, ch(CONFIRM_KEY)), Action::Purge);
        // The mode still carries the plan: `main::start_purge` reads it back.
        match &s.mode {
            Mode::Confirm(plan) => assert_eq!(plan.items[0].name, "old-experiment"),
            other => panic!("mode must still hold the plan, got {other:?}"),
        }
    }

    #[test]
    fn a_clean_plan_purges_on_enter_too() {
        let mut s = sample();
        s.cursor = 3;
        handle_key(&mut s, ch(' '));
        handle_key(&mut s, ch('d'));
        assert_eq!(handle_key(&mut s, press(KeyCode::Enter)), Action::Purge);
    }

    #[test]
    fn one_unsafe_item_in_a_mostly_clean_batch_still_forces_the_second_gate() {
        let mut s = sample();
        handle_key(&mut s, ch('A')); // everything, including the dirty one
        handle_key(&mut s, ch('d'));
        assert_eq!(handle_key(&mut s, ch(CONFIRM_KEY)), Action::None);
        match &s.mode {
            Mode::ForceConfirm(plan) => {
                assert_eq!(plan.len(), 4);
                assert_eq!(plan.force_items().count(), 2, "dirty + unpushed");
            }
            other => panic!("expected ForceConfirm, got {other:?}"),
        }
    }

    #[test]
    fn a_pending_worktree_alone_is_enough_to_force_the_second_gate() {
        let mut s = state(vec![group(
            "r",
            vec![wt("unchecked", Some(5), ActivityState::Pending)],
        )]);
        s.cursor = 1;
        handle_key(&mut s, ch(' '));
        handle_key(&mut s, ch('d'));
        assert_eq!(handle_key(&mut s, ch(CONFIRM_KEY)), Action::None);
        assert!(
            matches!(s.mode, Mode::ForceConfirm(_)),
            "an unfinished check is not evidence of safety"
        );
    }

    #[test]
    fn both_gates_cancel_back_to_browse_without_purging() {
        for cancel in [KeyCode::Esc, KeyCode::Char('n'), KeyCode::Char('q')] {
            let mut s = confirm_dirty();
            assert_eq!(handle_key(&mut s, press(cancel)), Action::None);
            assert_eq!(s.mode, Mode::Browse, "cancel from the first gate");

            let mut s = confirm_dirty();
            handle_key(&mut s, ch(CONFIRM_KEY));
            assert_eq!(handle_key(&mut s, press(cancel)), Action::None);
            assert_eq!(s.mode, Mode::Browse, "cancel from the second gate");
        }
    }

    #[test]
    fn confirmation_modals_swallow_navigation_and_selection_keys() {
        let mut s = confirm_dirty();
        let cursor = s.cursor;
        let selected = s.selected_count();

        for k in ['j', 'k', 'a', 'A', ' ', 's', 'r', 'g', 'G', 'd', 'x', '?'] {
            assert_eq!(handle_key(&mut s, ch(k)), Action::None, "key {k}");
            assert!(matches!(s.mode, Mode::Confirm(_)), "key {k} left the modal");
            assert_eq!(s.cursor, cursor, "key {k} moved the cursor");
            assert_eq!(
                s.selected_count(),
                selected,
                "key {k} changed the selection"
            );
        }

        handle_key(&mut s, ch(CONFIRM_KEY));
        for k in ['j', 'k', 'a', 'A', ' ', 's', 'r', 'g', 'G', 'd', 'x', '?'] {
            assert_eq!(handle_key(&mut s, ch(k)), Action::None, "key {k}");
            assert!(matches!(s.mode, Mode::ForceConfirm(_)), "key {k}");
            assert_eq!(s.cursor, cursor, "key {k} moved the cursor");
        }
    }

    #[test]
    fn purging_swallows_everything_including_the_force_key() {
        let mut s = sample();
        s.mode = Mode::Purging;
        for k in [CONFIRM_KEY, FORCE_KEY, 'q', 'j', 'd'] {
            assert_eq!(handle_key(&mut s, ch(k)), Action::None, "key {k}");
            assert_eq!(s.mode, Mode::Purging);
        }
        assert_eq!(handle_key(&mut s, press(KeyCode::Esc)), Action::None);
        assert_eq!(s.mode, Mode::Purging);
        assert!(!s.should_quit);
    }

    #[test]
    fn d_with_nothing_selected_and_the_cursor_on_a_header_only_toasts() {
        let mut s = sample();
        s.cursor = 0;
        assert_eq!(handle_key(&mut s, ch('d')), Action::None);
        assert_eq!(s.mode, Mode::Browse, "no empty modal");
        assert_eq!(
            s.toast.as_deref(),
            Some("nothing selected — press Space on a worktree first")
        );
    }

    #[test]
    fn d_on_an_empty_tree_toasts_a_different_message() {
        let mut s = state(vec![]);
        handle_key(&mut s, ch('d'));
        assert_eq!(s.mode, Mode::Browse);
        assert_eq!(s.toast.as_deref(), Some("nothing to purge"));
    }

    #[test]
    fn d_with_no_selection_falls_back_to_the_worktree_under_the_cursor() {
        let mut s = sample();
        s.cursor = 3; // wt:old-experiment, nothing selected
        handle_key(&mut s, ch('d'));
        match &s.mode {
            Mode::Confirm(plan) => {
                assert_eq!(plan.len(), 1);
                assert_eq!(plan.items[0].name, "old-experiment");
            }
            other => panic!("expected Confirm, got {other:?}"),
        }
    }

    #[test]
    fn the_results_modal_closes_on_enter_or_esc() {
        let outcome = PurgeOutcome {
            item: PurgeItem {
                worktree_path: PathBuf::from("/scan/gone"),
                repo_root: None,
                name: "gone".into(),
                size: Some(1),
                requires_force: false,
            },
            result: Ok(1),
        };
        for k in [KeyCode::Enter, KeyCode::Esc, KeyCode::Char('q')] {
            let mut s = sample();
            s.mode = Mode::Results(vec![outcome.clone()]);
            assert_eq!(handle_key(&mut s, press(k)), Action::None);
            assert_eq!(s.mode, Mode::Browse);
            assert!(!s.should_quit);
        }
    }

    #[test]
    fn the_results_modal_swallows_navigation() {
        let mut s = sample();
        s.cursor = 2;
        s.mode = Mode::Results(Vec::new());
        handle_key(&mut s, ch('j'));
        assert_eq!(s.cursor, 2);
        assert!(matches!(s.mode, Mode::Results(_)));
    }

    // -- status bar helper -------------------------------------------------

    #[test]
    fn selection_summary_reports_known_sizes_only() {
        let mut s = state(vec![group(
            "r",
            vec![
                wt("known", Some(2048), ActivityState::Clean),
                wt("pending", None, ActivityState::Pending),
            ],
        )]);
        assert_eq!(selection_summary(&s), None);
        s.select_all(true);
        assert_eq!(
            selection_summary(&s).as_deref(),
            Some("selected 2 (2.0 KB)")
        );
    }

    #[test]
    fn toast_is_cleared_by_the_next_browse_keypress() {
        let mut s = sample();
        s.cursor = 0;
        handle_key(&mut s, ch('d')); // sets the "nothing selected" toast
        assert!(s.toast.is_some());
        handle_key(&mut s, ch('j'));
        assert_eq!(s.toast, None);
    }

    #[test]
    fn activity_does_not_change_from_key_handling_alone() {
        // Guards against a handler accidentally mutating enrichment data.
        let mut s = sample();
        let before: Vec<ActivityState> = s.groups[0].worktrees.iter().map(|w| w.activity).collect();
        for k in ['j', 'k', ' ', 'a', 'A', 's', 'g', 'G', 'h', 'l'] {
            handle_key(&mut s, ch(k));
        }
        let after: Vec<ActivityState> = s.groups[0].worktrees.iter().map(|w| w.activity).collect();
        assert_eq!(before, after);
        assert_eq!(before[0], ActivityState::Active(ActiveReason::Dirty));
    }
}
