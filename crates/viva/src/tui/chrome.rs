//! Merged pane borders for the workbench grid (herdr-style chrome; route
//! A: our own implementation, no herdr code).
//!
//! Every leaf rect already includes its border cells (see
//! [`crate::tui::layout::PaneNode::render_layout_chrome`]): sibling rects
//! overlap by exactly one shared divider cell, so the grid renders ONE
//! line between neighbors — never a double wall — and the focused pane's
//! touching cells carry the accent color while the rest stay muted. With
//! fewer than two panes nothing is drawn: a single pane is full-bleed,
//! like herdr's `auto` pane-borders mode.
//!
//! Junction glyphs come from cell connectivity, not from per-pane blocks:
//! each border cell records which line kinds (horizontal/vertical) pass
//! through it, and the glyph follows the directions that continue into
//! neighboring cells (`┌ ┐ └ ┘ ├ ┤ ┬ ┴ ┼ │ ─`).

use std::collections::HashMap;

use ratatui::Frame;
use ratatui::layout::{Position, Rect};
use ratatui::style::Style;

use crate::tui::theme::Palette;

/// Connectivity bits for one border cell.
const UP: u8 = 1;
const DOWN: u8 = 2;
const LEFT: u8 = 4;
const RIGHT: u8 = 8;

#[derive(Debug, Clone, Copy, Default)]
struct CellFlags {
    /// A horizontal line passes through (a top/bottom edge of some pane).
    h: bool,
    /// A vertical line passes through (a left/right edge of some pane).
    v: bool,
    /// The cell touches the focused pane's border.
    focused: bool,
}

/// A border title: the pane index it belongs to and the text. Rendered on
/// the pane's top border row, skipping the corner cells.
pub struct PaneTitle {
    pub pane: usize,
    pub text: String,
}

/// Shrink a rect by one cell on every side (saturating). This is a leaf's
/// content area: the border cells belong to the chrome.
pub fn inner_rect(rect: Rect) -> Rect {
    Rect {
        x: rect.x.saturating_add(1),
        y: rect.y.saturating_add(1),
        width: rect.width.saturating_sub(2),
        height: rect.height.saturating_sub(2),
    }
}

/// Draw the merged border grid for all pane rects. `focused` is the index
/// of the focused pane (its border cells get the accent color); titles are
/// stamped onto the named panes' top border rows.
pub fn render_pane_borders(
    frame: &mut Frame,
    rects: &[Rect],
    focused: Option<usize>,
    palette: &Palette,
    titles: &[PaneTitle],
) {
    // herdr's `auto` mode: a single pane has no chrome at all.
    if rects.len() < 2 {
        return;
    }
    let mut cells: HashMap<(u16, u16), CellFlags> = HashMap::new();
    for (index, rect) in rects.iter().enumerate() {
        // Too small to hold a box: skip (the content still renders).
        if rect.width < 2 || rect.height < 2 {
            continue;
        }
        let focused = focused == Some(index);
        let right = rect.x + rect.width - 1;
        let bottom = rect.y + rect.height - 1;
        for x in rect.x..=right {
            for y in [rect.y, bottom] {
                let cell = cells.entry((x, y)).or_default();
                cell.h = true;
                cell.focused |= focused;
            }
        }
        for y in rect.y..=bottom {
            for x in [rect.x, right] {
                let cell = cells.entry((x, y)).or_default();
                cell.v = true;
                cell.focused |= focused;
            }
        }
    }

    let buffer = frame.buffer_mut();
    for ((x, y), flags) in &cells {
        let mut dirs = 0u8;
        if flags.v && *y > 0 && cells.get(&(*x, y.saturating_sub(1))).is_some_and(|c| c.v) {
            dirs |= UP;
        }
        if flags.v && *y < u16::MAX && cells.get(&(*x, y + 1)).is_some_and(|c| c.v) {
            dirs |= DOWN;
        }
        if flags.h && *x > 0 && cells.get(&(x - 1, *y)).is_some_and(|c| c.h) {
            dirs |= LEFT;
        }
        if flags.h && cells.get(&(x + 1, *y)).is_some_and(|c| c.h) {
            dirs |= RIGHT;
        }
        let glyph = junction_glyph(dirs, *flags);
        let style = Style::new().fg(if flags.focused {
            palette.accent
        } else {
            palette.overlay0
        });
        if let Some(cell) = buffer.cell_mut(Position::new(*x, *y)) {
            cell.set_char(glyph).set_style(style);
        }
    }

    // Titles go on top of the border row, inside the corners.
    for title in titles {
        let Some(rect) = rects.get(title.pane) else {
            continue;
        };
        if rect.width < 6 {
            continue;
        }
        let focused = focused == Some(title.pane);
        let style = Style::new().fg(if focused {
            palette.accent
        } else {
            palette.overlay1
        });
        let budget = rect.width.saturating_sub(4) as usize; // corners + padding
        // Truncate with an ellipsis instead of cutting mid-word: the
        // reported/screen labels in titles are an honesty boundary (QA P3)
        // and must not silently lose their closing paren.
        let full: String = format!(" {} ", title.text);
        let text: String = if full.chars().count() > budget {
            format!(
                "{}…",
                full.chars()
                    .take(budget.saturating_sub(1))
                    .collect::<String>()
            )
        } else {
            full
        };
        for (offset, ch) in text.chars().enumerate() {
            let x = rect.x + 1 + offset as u16;
            if x >= rect.x + rect.width - 1 {
                break;
            }
            if let Some(cell) = buffer.cell_mut(Position::new(x, rect.y)) {
                cell.set_char(ch).set_style(style);
            }
        }
    }
}

/// The glyph for one border cell from its continuing directions.
fn junction_glyph(dirs: u8, flags: CellFlags) -> char {
    match dirs {
        d if d == UP | DOWN => '│',
        d if d == LEFT | RIGHT => '─',
        d if d == DOWN | RIGHT => '┌',
        d if d == DOWN | LEFT => '┐',
        d if d == UP | RIGHT => '└',
        d if d == UP | LEFT => '┘',
        d if d == UP | DOWN | RIGHT => '├',
        d if d == UP | DOWN | LEFT => '┤',
        d if d == DOWN | LEFT | RIGHT => '┬',
        d if d == UP | LEFT | RIGHT => '┴',
        d if d == UP | DOWN | LEFT | RIGHT => '┼',
        // A cell whose line ends here (1-wide segment): keep the kind.
        0 if flags.h => '─',
        0 if flags.v => '│',
        0 => '─',
        _ => '─',
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn draw_with(rects: Vec<Rect>, focused: Option<usize>) -> ratatui::buffer::Buffer {
        let backend = TestBackend::new(40, 12);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                render_pane_borders(frame, &rects, focused, &Palette::catppuccin_mocha(), &[])
            })
            .expect("draw");
        terminal.backend().buffer().clone()
    }

    fn cell_char(buffer: &ratatui::buffer::Buffer, x: u16, y: u16) -> char {
        buffer
            .cell(Position::new(x, y))
            .expect("cell")
            .symbol()
            .chars()
            .next()
            .unwrap_or(' ')
    }

    #[test]
    fn two_side_by_side_panes_share_one_divider_column() {
        // [0,0,10x10] and [10,0,10x10] overlap on x=9 by the layout steal.
        let rects = vec![Rect::new(0, 0, 10, 10), Rect::new(9, 0, 11, 10)];
        let buffer = draw_with(rects, None);
        assert_eq!(cell_char(&buffer, 9, 5), '│', "shared divider");
        // Outer frame corners and edges exist once.
        assert_eq!(cell_char(&buffer, 0, 0), '┌');
        assert_eq!(cell_char(&buffer, 19, 0), '┐');
        assert_eq!(cell_char(&buffer, 0, 9), '└');
        assert_eq!(cell_char(&buffer, 19, 9), '┘');
        // No double wall: the column left of the divider is content space.
        assert_eq!(cell_char(&buffer, 8, 5), ' ');
        assert_eq!(cell_char(&buffer, 10, 5), ' ');
    }

    #[test]
    fn four_panes_meet_in_a_cross_junction() {
        // 2x2 grid sharing both dividers (x=9 column, y=5 row).
        let rects = vec![
            Rect::new(0, 0, 10, 6),
            Rect::new(9, 0, 11, 6),
            Rect::new(0, 5, 10, 6),
            Rect::new(9, 5, 11, 6),
        ];
        let buffer = draw_with(rects, None);
        assert_eq!(cell_char(&buffer, 9, 5), '┼', "four-way crossing");
        assert_eq!(cell_char(&buffer, 9, 0), '┬', "divider meets top frame");
        assert_eq!(cell_char(&buffer, 9, 10), '┴', "divider meets bottom frame");
        assert_eq!(cell_char(&buffer, 0, 5), '├');
        assert_eq!(cell_char(&buffer, 19, 5), '┤');
    }

    #[test]
    fn titles_are_stamped_inside_the_top_border() {
        let rects = vec![Rect::new(0, 0, 20, 8), Rect::new(19, 0, 20, 8)];
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                render_pane_borders(
                    frame,
                    &rects,
                    Some(0),
                    &Palette::catppuccin_mocha(),
                    &[PaneTitle {
                        pane: 0,
                        text: "terminal t1".into(),
                    }],
                )
            })
            .expect("draw");
        let text = terminal.backend().to_string();
        assert!(text.contains("terminal t1"), "title on the border: {text}");
    }

    #[test]
    fn a_single_pane_gets_no_chrome_at_all() {
        let buffer = draw_with(vec![Rect::new(0, 0, 20, 8)], Some(0));
        assert_eq!(cell_char(&buffer, 0, 0), ' ', "full-bleed single pane");
    }

    #[test]
    fn titles_truncate_with_an_ellipsis_not_mid_word() {
        let rects = vec![Rect::new(0, 0, 20, 8), Rect::new(19, 0, 20, 8)];
        let backend = TestBackend::new(40, 10);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal
            .draw(|frame| {
                render_pane_borders(
                    frame,
                    &rects,
                    Some(0),
                    &Palette::catppuccin_mocha(),
                    &[PaneTitle {
                        pane: 0,
                        text: "terminal t0 · pi:working(reported)".into(),
                    }],
                )
            })
            .expect("draw");
        let view = terminal.backend().to_string();
        assert!(
            view.contains("…"),
            "a too-long title truncates gracefully: {view}"
        );
    }
}
