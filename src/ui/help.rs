//! Keybinding overlay.

use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Color, Modifier, Style};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, BorderType, Clear, Paragraph};

use crate::app::events::{CONFIRM_KEY, FORCE_KEY};
use crate::ui::{C_ACCENT, C_DANGER, C_MUTED, C_UNPUSHED, centered, dim, pad_to};

/// Key column width inside the overlay.
const KEY_W: usize = 12;

/// The binding table, as `(keys, description)`. `None` is a blank separator.
fn bindings() -> Vec<Option<(&'static str, &'static str)>> {
    vec![
        Some(("↑ ↓ j k", "move cursor")),
        Some(("← h", "collapse group / go to header")),
        Some(("→ l", "expand group / enter it")),
        Some(("Enter", "toggle group")),
        Some(("g G", "jump to top / bottom")),
        None,
        Some(("Space", "select worktree (on a header: whole group)")),
        Some(("a", "select all in this repo")),
        Some(("A", "select all everywhere")),
        None,
        Some(("d x", "purge selected (or the row under the cursor)")),
        Some(("s", "cycle sort: name / size / active")),
        Some(("r", "rescan")),
        Some(("? Esc q", "help / close / quit")),
    ]
}

fn body() -> Vec<Line<'static>> {
    let mut lines = vec![Line::raw("")];
    for entry in bindings() {
        match entry {
            Some((keys, what)) => lines.push(Line::from(vec![
                Span::raw("  "),
                Span::styled(
                    pad_to(keys, KEY_W),
                    Style::new().fg(C_ACCENT).add_modifier(Modifier::BOLD),
                ),
                Span::styled(what, Style::new().fg(Color::White)),
            ])),
            None => lines.push(Line::raw("")),
        }
    }
    lines.push(Line::raw(""));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("●", Style::new().fg(C_DANGER)),
        Span::styled(" uncommitted changes — lost on removal", dim(C_MUTED)),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("●", Style::new().fg(C_UNPUSHED)),
        Span::styled(
            " unpushed commits — kept, they live in the repository",
            dim(C_MUTED),
        ),
    ]));
    lines.push(Line::from(vec![
        Span::raw("  "),
        Span::styled("⚑", dim(C_MUTED)),
        Span::styled(" stashes — kept, never blocks a purge", dim(C_MUTED)),
    ]));
    lines.push(Line::raw(""));
    lines.push(Line::from(Span::styled(
        format!("  Removal asks twice: {CONFIRM_KEY} confirms, then {FORCE_KEY} forces anything that holds work."),
        dim(C_MUTED),
    )));
    lines
}

/// Render the help overlay.
pub fn render(frame: &mut Frame, area: Rect) {
    if area.width == 0 || area.height == 0 {
        return;
    }
    let lines = body();
    let w = (lines.iter().map(Line::width).max().unwrap_or(0) as u16 + 4).min(area.width);
    let h = (lines.len() as u16 + 2).min(area.height);
    let rect = centered(area, w, h);

    frame.render_widget(Clear, rect);
    let block = Block::bordered()
        .border_type(BorderType::Rounded)
        .border_style(Style::new().fg(C_ACCENT))
        .title(Line::from(vec![
            Span::raw(" "),
            Span::styled(
                "Keys",
                Style::new().fg(C_ACCENT).add_modifier(Modifier::BOLD),
            ),
            Span::raw(" "),
        ]));
    let inner = block.inner(rect);
    frame.render_widget(block, rect);
    if inner.width == 0 || inner.height == 0 {
        return;
    }
    frame.render_widget(Paragraph::new(lines), inner);
}

#[cfg(test)]
mod tests {
    use super::*;

    fn joined() -> String {
        body()
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
    fn every_documented_binding_is_listed() {
        let text = joined();
        for keys in [
            "↑ ↓ j k",
            "← h",
            "→ l",
            "Enter",
            "g G",
            "Space",
            "a",
            "A",
            "d x",
            "s",
            "r",
            "? Esc q",
        ] {
            assert!(text.contains(keys), "missing binding {keys:?} in:\n{text}");
        }
    }

    #[test]
    fn the_help_explains_the_two_gates_with_the_real_keys() {
        let text = joined();
        assert!(
            text.contains("y confirms, then F forces"),
            "the overlay must name both gate keys:\n{text}"
        );
    }

    #[test]
    fn the_help_explains_what_the_colours_mean() {
        let text = joined();
        assert!(
            text.contains("uncommitted changes — lost on removal"),
            "{text}"
        );
        assert!(
            text.contains("unpushed commits — kept, they live in the repository"),
            "the yellow/red distinction must be explained, not just shown:\n{text}"
        );
        assert!(
            text.contains("stashes — kept, never blocks a purge"),
            "{text}"
        );
    }
}
