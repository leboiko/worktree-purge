//! Confirmation and result overlays.
//!
//! The two confirmation modals are the visible half of the two-stage gate. The
//! first one asks "remove these?"; the second one exists only when something in
//! the batch would lose work, and it names a *different* key on purpose so
//! muscle memory cannot carry a user through both.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::app::events::{CONFIRM_KEY, FORCE_KEY};
use crate::app::state::{AppState, Mode};
use crate::format;
use crate::model::{PurgeOutcome, PurgePlan};
use crate::ui::{
    C_ACCENT, C_DANGER, C_MUTED, C_OK, C_UNPUSHED, centered, dim, pad_left, pad_to, spinner_frame,
};

/// Widest a modal gets; it shrinks to fit narrow terminals.
const MAX_W: u16 = 72;
/// Name column inside a modal body.
const ITEM_NAME_W: usize = 30;
const ITEM_SIZE_W: usize = 9;

fn key_hint(key: &str, label: &str) -> Vec<Span<'static>> {
    vec![
        Span::styled(
            format!("[{key}]"),
            Style::new().fg(C_ACCENT).add_modifier(Modifier::BOLD),
        ),
        Span::styled(format!(" {label}"), Style::new().fg(Color::White)),
    ]
}

/// `name … size` plus an optional trailing marker.
fn item_line(name: &str, size: Option<u64>, marker: Option<Span<'static>>) -> Line<'static> {
    let mut spans = vec![
        Span::raw("  "),
        Span::styled(pad_to(name, ITEM_NAME_W), Style::new().fg(Color::White)),
        Span::styled(
            pad_left(&format::bytes_opt(size), ITEM_SIZE_W),
            match size {
                Some(_) => Style::new().fg(Color::White),
                None => dim(C_MUTED),
            },
        ),
    ];
    if let Some(marker) = marker {
        spans.push(Span::raw("  "));
        spans.push(marker);
    }
    Line::from(spans)
}

/// Body of the first gate.
fn confirm_body(plan: &PurgePlan) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for item in &plan.items {
        let marker = item
            .requires_force
            .then(|| Span::styled("!", Style::new().fg(C_DANGER).add_modifier(Modifier::BOLD)));
        lines.push(item_line(&item.name, item.size, marker));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("  reclaims "),
        Span::styled(
            format::bytes(plan.total_size()),
            Style::new().fg(C_OK).add_modifier(Modifier::BOLD),
        ),
    ]));

    let forced = plan.force_items().count();
    if forced > 0 {
        lines.push(Line::raw(""));
        lines.push(Line::from(Span::styled(
            format!(
                "  {} marked ! hold work — you will be asked again",
                format::count_noun(forced, "worktree")
            ),
            Style::new().fg(C_DANGER),
        )));
    }
    lines.push(Line::raw(""));
    let mut footer = vec![Span::raw("  ")];
    footer.extend(key_hint(
        &CONFIRM_KEY.to_string(),
        if forced > 0 { "review" } else { "remove" },
    ));
    footer.push(Span::raw("    "));
    footer.extend(key_hint("Esc", "cancel"));
    lines.push(Line::from(footer));
    lines
}

/// Body of the second gate: only the at-risk items, with concrete reasons.
fn force_body(plan: &PurgePlan) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for item in plan.force_items() {
        lines.push(item_line(
            &item.name,
            item.size,
            Some(Span::styled("force", Style::new().fg(C_DANGER))),
        ));
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        "  Uncommitted changes in these worktrees are destroyed.",
        Style::new().fg(C_DANGER).add_modifier(Modifier::BOLD),
    )));
    lines.push(Line::from(Span::styled(
        "  Commits and branches are not — they stay in the repository.",
        dim(C_MUTED),
    )));
    lines.push(Line::raw(""));
    let mut footer = vec![Span::raw("  Press ")];
    footer.extend(key_hint(&FORCE_KEY.to_string(), "to force-remove"));
    footer.push(Span::raw("    "));
    footer.extend(key_hint("Esc", "cancel"));
    lines.push(Line::from(footer));
    lines.push(Line::from(Span::styled(
        format!("  ({CONFIRM_KEY} and Enter do nothing here — this gate is deliberate)"),
        dim(C_MUTED),
    )));
    lines
}

fn results_body(outcomes: &[PurgeOutcome]) -> Vec<Line<'static>> {
    let mut lines = Vec::new();
    for o in outcomes {
        let (mark, style, detail) = match &o.result {
            Ok(freed) => ("✓", Style::new().fg(C_OK), format::bytes_opt(Some(*freed))),
            Err(msg) => (
                "✗",
                Style::new().fg(C_DANGER),
                msg.lines().next().unwrap_or("failed").to_string(),
            ),
        };
        lines.push(Line::from(vec![
            Span::raw("  "),
            Span::styled(mark, style),
            Span::raw(" "),
            Span::styled(
                pad_to(&o.item.name, ITEM_NAME_W),
                Style::new().fg(Color::White),
            ),
            Span::styled(detail, style),
        ]));
    }
    let freed: u64 = outcomes.iter().filter_map(|o| o.result.as_ref().ok()).sum();
    let failed = outcomes.iter().filter(|o| !o.succeeded()).count();
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("  freed "),
        Span::styled(
            format::bytes(freed),
            Style::new().fg(C_OK).add_modifier(Modifier::BOLD),
        ),
        Span::styled(
            if failed > 0 {
                format!(" · {} failed", format::count_noun(failed, "removal"))
            } else {
                String::new()
            },
            Style::new().fg(C_DANGER),
        ),
    ]));
    lines.push(Line::raw(""));
    let mut footer = vec![Span::raw("  ")];
    footer.extend(key_hint("Enter", "close"));
    lines.push(Line::from(footer));
    lines
}

fn purging_body(tick: usize) -> Vec<Line<'static>> {
    vec![
        Line::raw(""),
        Line::from(vec![
            Span::raw("  "),
            Span::styled(spinner_frame(tick), Style::new().fg(C_ACCENT)),
            Span::styled(" removing worktrees…", Style::new().fg(Color::White)),
        ]),
        Line::raw(""),
        Line::from(Span::styled("  This cannot be cancelled.", dim(C_MUTED))),
    ]
}

/// Render a bordered, centred overlay containing `lines`.
fn overlay(
    frame: &mut Frame,
    area: Rect,
    title: Line<'static>,
    border: Color,
    lines: Vec<Line<'static>>,
) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let content_w = lines.iter().map(Line::width).max().unwrap_or(0) as u16;
    let w = (content_w + 4).clamp(20, MAX_W);
    let h = lines.len() as u16 + 2;
    let rect = centered(area, w, h);

    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(border))
        .title(title);
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

fn title_span(text: String, color: Color) -> Line<'static> {
    Line::from(vec![
        Span::raw(" "),
        Span::styled(text, Style::new().fg(color).add_modifier(Modifier::BOLD)),
        Span::raw(" "),
    ])
}

/// Render whichever modal the current mode calls for. No-op in browse mode.
pub fn render(frame: &mut Frame, area: Rect, state: &AppState) {
    match &state.mode {
        Mode::Browse | Mode::Help => {}
        Mode::Confirm(plan) => overlay(
            frame,
            area,
            title_span(
                format!("Remove {}", format::count_noun(plan.len(), "worktree")),
                if plan.needs_force() {
                    C_UNPUSHED
                } else {
                    C_ACCENT
                },
            ),
            if plan.needs_force() {
                C_UNPUSHED
            } else {
                C_ACCENT
            },
            confirm_body(plan),
        ),
        Mode::ForceConfirm(plan) => overlay(
            frame,
            area,
            title_span(
                format!(
                    "{} hold work",
                    format::count_noun(plan.force_items().count(), "worktree")
                ),
                C_DANGER,
            ),
            C_DANGER,
            force_body(plan),
        ),
        Mode::Purging => overlay(
            frame,
            area,
            title_span("Purging".to_string(), C_ACCENT),
            C_ACCENT,
            purging_body(state.spinner_tick),
        ),
        Mode::Results(outcomes) => overlay(
            frame,
            area,
            title_span("Results".to_string(), C_ACCENT),
            C_ACCENT,
            results_body(outcomes),
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::model::PurgeItem;
    use std::path::PathBuf;

    fn item(name: &str, size: Option<u64>, force: bool) -> PurgeItem {
        PurgeItem {
            worktree_path: PathBuf::from(format!("/scan/{name}")),
            repo_root: None,
            name: name.to_string(),
            size,
            requires_force: force,
        }
    }

    fn joined(lines: &[Line<'_>]) -> String {
        lines
            .iter()
            .map(|l| {
                l.spans
                    .iter()
                    .map(|s| s.content.as_ref())
                    .collect::<String>()
            })
            .collect::<Vec<_>>()
            .join("\n")
    }

    #[test]
    fn the_confirm_body_lists_every_item_with_its_size_and_the_total() {
        let plan = PurgePlan {
            items: vec![item("a", Some(1024), false), item("b", Some(2048), false)],
        };
        let body = joined(&confirm_body(&plan));
        assert!(body.contains("a "), "{body}");
        assert!(body.contains("1.0 KB"), "{body}");
        assert!(body.contains("2.0 KB"), "{body}");
        assert!(body.contains("reclaims 3.0 KB"), "{body}");
        assert!(
            !body.contains("asked again"),
            "a clean plan must not promise a second gate: {body}"
        );
        assert!(body.contains("[y] remove"), "{body}");
    }

    #[test]
    fn the_confirm_body_warns_when_a_second_gate_is_coming() {
        let plan = PurgePlan {
            items: vec![item("a", Some(1), false), item("b", Some(1), true)],
        };
        let body = joined(&confirm_body(&plan));
        assert!(
            body.contains("1 worktree marked ! holds work — you will be asked again")
                || body.contains("1 worktree marked ! hold work — you will be asked again"),
            "{body}"
        );
        assert!(
            body.contains("[y] review"),
            "the key must not say 'remove': {body}"
        );
    }

    #[test]
    fn an_unknown_size_shows_a_placeholder_in_the_modal_too() {
        let plan = PurgePlan {
            items: vec![item("a", None, true)],
        };
        let lines = confirm_body(&plan);
        let item_row: String = lines[0].spans.iter().map(|s| s.content.as_ref()).collect();
        assert!(item_row.contains('—'), "{item_row:?}");
        assert!(
            !item_row.contains("0 B"),
            "an unmeasured worktree must not claim to be empty: {item_row:?}"
        );
        // The *total* really is 0 known bytes, and says so.
        assert!(
            joined(&lines).contains("reclaims 0 B"),
            "{}",
            joined(&lines)
        );
    }

    #[test]
    fn the_force_body_lists_only_the_at_risk_items() {
        let plan = PurgePlan {
            items: vec![
                item("safe-one", Some(1), false),
                item("risky-one", Some(2), true),
            ],
        };
        let body = joined(&force_body(&plan));
        assert!(body.contains("risky-one"), "{body}");
        assert!(
            !body.contains("safe-one"),
            "the second gate is about the dangerous items only: {body}"
        );
    }

    #[test]
    fn the_force_body_names_the_force_key_and_disowns_the_first_gate_keys() {
        let plan = PurgePlan {
            items: vec![item("risky", Some(2), true)],
        };
        let body = joined(&force_body(&plan));
        assert!(body.contains("[F] to force-remove"), "{body}");
        assert!(
            body.contains("y and Enter do nothing here"),
            "the modal must say plainly that the usual keys are inert: {body}"
        );
        assert!(
            body.contains("Uncommitted changes in these worktrees are destroyed."),
            "{body}"
        );
        assert!(
            body.contains("Commits and branches are not"),
            "the asymmetry must be stated, not implied: {body}"
        );
    }

    #[test]
    fn the_results_body_separates_successes_from_failures() {
        let outcomes = vec![
            PurgeOutcome {
                item: item("gone", Some(1024), false),
                result: Ok(1024),
            },
            PurgeOutcome {
                item: item("stuck", Some(2048), false),
                result: Err("fatal: is locked\nsecond line".into()),
            },
        ];
        let body = joined(&results_body(&outcomes));
        assert!(body.contains("✓ gone"), "{body}");
        assert!(body.contains("✗ stuck"), "{body}");
        assert!(body.contains("fatal: is locked"), "{body}");
        assert!(
            !body.contains("second line"),
            "only the first line of stderr belongs in the list: {body}"
        );
        assert!(body.contains("freed 1.0 KB · 1 removal failed"), "{body}");
    }

    #[test]
    fn the_results_body_omits_the_failure_clause_when_everything_worked() {
        let outcomes = vec![PurgeOutcome {
            item: item("gone", Some(1024), false),
            result: Ok(1024),
        }];
        let body = joined(&results_body(&outcomes));
        assert!(body.contains("freed 1.0 KB"), "{body}");
        assert!(!body.contains("failed"), "{body}");
    }
}
