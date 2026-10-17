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

/// A border plus one content cell needs three rows and three columns.
/// Smaller rects stay full-bleed: drawing the frame would leave a content
/// height of zero.
pub fn content_rect(rect: Rect) -> Rect {
    if rect.width >= 3 && rect.height >= 3 {
        inner_rect(rect)
    } else {
        rect
    }
}

/// A one-column thumb on `track` when `total` rows do not fit in `viewport`.
/// `offset` is the first visible row. The thumb sits at the top when the
/// list is at the start and at the bottom when the last row is visible.
pub fn paint_scrollbar(
    buffer: &mut ratatui::buffer::Buffer,
    track: Rect,
    offset: usize,
    total: usize,
    viewport: usize,
    track_color: ratatui::style::Color,
    thumb_color: ratatui::style::Color,
) {
    let Some(thumb) = scrollbar_thumb(offset, total, viewport, track) else {
        return;
    };
    for y in track.y..track.y.saturating_add(track.height) {
        let color = if y >= thumb.top && y < thumb.top.saturating_add(thumb.len) {
            thumb_color
        } else {
            track_color
        };
        if let Some(cell) = buffer.cell_mut(Position::new(track.x, y)) {
            cell.set_char('▕').set_style(Style::new().fg(color));
        }
    }
}

struct ScrollThumb {
    top: u16,
    len: u16,
}

/// How far into the thumb `row` sits, when the press landed on the thumb.
pub(crate) fn scrollbar_grab(
    offset: usize,
    total: usize,
    viewport: usize,
    track: Rect,
    row: u16,
) -> Option<u16> {
    let thumb = scrollbar_thumb(offset, total, viewport, track)?;
    (row >= thumb.top && row < thumb.top.saturating_add(thumb.len)).then(|| row - thumb.top)
}

/// First visible row when a click should center the thumb on `row`.
pub(crate) fn scrollbar_offset_from_row(
    offset: usize,
    total: usize,
    viewport: usize,
    track: Rect,
    row: u16,
) -> usize {
    let Some(thumb) = scrollbar_thumb(offset, total, viewport, track) else {
        return 0;
    };
    let clamped = row.clamp(track.y, track.bottom().saturating_sub(1));
    let row_offset = usize::from(clamped.saturating_sub(track.y));
    let desired = row_offset.saturating_sub(usize::from(thumb.len) / 2);
    offset_from_thumb_top(offset, total, viewport, track, desired)
}

/// First visible row while dragging the thumb. `grab` is the row inside the
/// thumb that the pointer grabbed, so the thumb does not jump.
pub(crate) fn scrollbar_offset_from_drag(
    offset: usize,
    total: usize,
    viewport: usize,
    track: Rect,
    row: u16,
    grab: u16,
) -> usize {
    let clamped = row.clamp(track.y, track.bottom().saturating_sub(1));
    let row_offset = usize::from(clamped.saturating_sub(track.y));
    let desired = row_offset.saturating_sub(usize::from(grab));
    offset_from_thumb_top(offset, total, viewport, track, desired)
}

fn offset_from_thumb_top(
    offset: usize,
    total: usize,
    viewport: usize,
    track: Rect,
    desired_top: usize,
) -> usize {
    let max_offset = total.saturating_sub(viewport);
    if max_offset == 0 {
        return 0;
    }
    let thumb_len = scrollbar_thumb(offset, total, viewport, track)
        .map(|thumb| usize::from(thumb.len))
        .unwrap_or(1);
    let max_top =
        usize::from(track.height).saturating_sub(thumb_len.min(usize::from(track.height)));
    if max_top == 0 {
        return 0;
    }
    let desired = desired_top.min(max_top);
    ((desired as f32 * max_offset as f32) / max_top as f32).round() as usize
}

fn scrollbar_thumb(
    offset: usize,
    total: usize,
    viewport: usize,
    track: Rect,
) -> Option<ScrollThumb> {
    if viewport == 0 || total <= viewport || track.height == 0 || track.width == 0 {
        return None;
    }
    let max_offset = total - viewport;
    let offset = offset.min(max_offset);
    let track_height = usize::from(track.height);
    let thumb_len = ((viewport * track_height) as f32 / total as f32)
        .round()
        .clamp(1.0, track_height as f32) as u16;
    let max_top = track.height - thumb_len;
    let top = if max_top == 0 {
        0
    } else {
        ((offset as f32 * f32::from(max_top)) / max_offset as f32).round() as u16
    };
    Some(ScrollThumb {
        top: track.y.saturating_add(top),
        len: thumb_len,
    })
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
        // Too small to hold a border plus one content cell: skip. The
        // content pass draws the full rect instead of an empty frame.
        if rect.width < 3 || rect.height < 3 {
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
        let budget = usize::from(rect.width.saturating_sub(4)); // corners + padding
        // Truncate by display width. A CJK title counted in scalars
        // overruns the border and collides with the next pane.
        let full = format!(" {} ", title.text);
        let text = if crate::tui::text::display_width(&full) > budget {
            crate::tui::text::truncate_end(&full, budget)
        } else {
            full
        };
        buffer.set_stringn(rect.x + 1, rect.y, text, budget, style);
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

    #[test]
    fn dragging_the_thumb_down_increases_the_offset() {
        let track = Rect::new(0, 0, 1, 10);
        let top = scrollbar_offset_from_drag(0, 100, 10, track, 0, 0);
        let bottom = scrollbar_offset_from_drag(0, 100, 10, track, 9, 0);
        assert_eq!(top, 0);
        assert!(
            bottom > top,
            "bottom of the track is further down: {bottom}"
        );
        let grabbed = scrollbar_grab(0, 100, 10, track, 0);
        assert_eq!(grabbed, Some(0));
        assert_eq!(scrollbar_grab(0, 100, 10, track, 5), None);
    }
}
