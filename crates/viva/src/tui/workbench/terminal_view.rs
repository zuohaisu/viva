//! Ratatui adapter for vt100 projections. No second emulator or CLI loop.
use crate::terminal::screen::{InputModes, ScreenView};
use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyModifiers, MouseButton, MouseEvent, MouseEventKind,
};
use ratatui::{
    Frame,
    layout::{Position, Rect},
    style::{Color, Modifier, Style},
};
fn color(c: u32) -> Color {
    if c == 0 {
        Color::Reset
    } else if c <= 256 {
        Color::Indexed((c - 1) as u8)
    } else {
        Color::Rgb((c >> 16) as u8, (c >> 8) as u8, c as u8)
    }
}
pub fn draw(frame: &mut Frame, area: Rect, view: &ScreenView, cursor: bool) {
    for (y, row) in view.lines.iter().take(area.height as usize).enumerate() {
        let mut x = area.x;
        for run in row {
            if x >= area.right() {
                break;
            }
            let mut style = Style::new().fg(color(run.2)).bg(color(run.3));
            for (bit, modifier) in [
                (1, Modifier::BOLD),
                (2, Modifier::DIM),
                (4, Modifier::ITALIC),
                (8, Modifier::UNDERLINED),
                (16, Modifier::REVERSED),
            ] {
                if run.4 & bit != 0 {
                    style = style.add_modifier(modifier);
                }
            }
            frame.buffer_mut().set_stringn(
                x,
                area.y + y as u16,
                &run.0,
                usize::from(area.right() - x),
                style,
            );
            x = x.saturating_add(run.1);
        }
    }
    if cursor && view.cursor_visible && view.cursor.0 < area.height && view.cursor.1 < area.width {
        frame.set_cursor_position(Position::new(
            area.x + view.cursor.1,
            area.y + view.cursor.0,
        ));
    }
}
pub fn key(key: KeyEvent, modes: &InputModes) -> Option<Vec<u8>> {
    let m = key.modifiers;
    let modifier = 1
        + u8::from(m.contains(KeyModifiers::SHIFT))
        + 2 * u8::from(m.contains(KeyModifiers::ALT))
        + 4 * u8::from(m.contains(KeyModifiers::CONTROL));
    let arrow = match key.code {
        KeyCode::Up => Some('A'),
        KeyCode::Down => Some('B'),
        KeyCode::Right => Some('C'),
        KeyCode::Left => Some('D'),
        KeyCode::Home => Some('H'),
        KeyCode::End => Some('F'),
        _ => None,
    };
    if let Some(c) = arrow {
        return Some(
            if modifier > 1 {
                format!("\x1b[1;{modifier}{c}")
            } else if modes.application_cursor {
                format!("\x1bO{c}")
            } else {
                format!("\x1b[{c}")
            }
            .into_bytes(),
        );
    }
    let tilde = match key.code {
        KeyCode::Insert => Some(2),
        KeyCode::Delete => Some(3),
        KeyCode::PageUp => Some(5),
        KeyCode::PageDown => Some(6),
        KeyCode::F(5) => Some(15),
        KeyCode::F(6) => Some(17),
        KeyCode::F(7) => Some(18),
        KeyCode::F(8) => Some(19),
        KeyCode::F(9) => Some(20),
        KeyCode::F(10) => Some(21),
        KeyCode::F(11) => Some(23),
        KeyCode::F(12) => Some(24),
        _ => None,
    };
    if let Some(n) = tilde {
        return Some(
            if modifier > 1 {
                format!("\x1b[{n};{modifier}~")
            } else {
                format!("\x1b[{n}~")
            }
            .into_bytes(),
        );
    }
    if let KeyCode::F(n @ 1..=4) = key.code {
        let c = (b'P' + n - 1) as char;
        return Some(
            if modifier > 1 {
                format!("\x1b[1;{modifier}{c}")
            } else {
                format!("\x1bO{c}")
            }
            .into_bytes(),
        );
    }
    let mut bytes = match key.code {
        // Crossterm decodes legacy 0x1c..0x1f as Ctrl+4..7.
        KeyCode::Char(c @ '4'..='7') if m.contains(KeyModifiers::CONTROL) => {
            vec![c as u8 - b'4' + 0x1c]
        }
        KeyCode::Char(c) => super::encode_char(m, c),
        KeyCode::Esc => vec![27],
        KeyCode::Enter if m.contains(KeyModifiers::SHIFT | KeyModifiers::CONTROL) => {
            format!("\x1b[13;{modifier}u").into_bytes()
        }
        KeyCode::Enter if m.contains(KeyModifiers::SHIFT) => b"\x1b[13;2u".to_vec(),
        KeyCode::Enter => vec![13],
        KeyCode::Backspace => vec![127],
        KeyCode::BackTab => b"\x1b[Z".to_vec(),
        KeyCode::Tab => vec![9],
        _ => return None,
    };
    if m.contains(KeyModifiers::ALT) {
        bytes.insert(0, 27);
    }
    Some(bytes)
}
pub fn paste(text: &str, modes: &InputModes) -> Vec<u8> {
    if modes.bracketed_paste {
        format!("\x1b[200~{text}\x1b[201~").into_bytes()
    } else {
        text.as_bytes().to_vec()
    }
}
/// Only called for a focused CLI inner rect. Shift always reserves host input.
pub fn mouse(event: MouseEvent, area: Rect, modes: &InputModes) -> Option<Vec<u8>> {
    if modes.mouse_mode == 0 || event.modifiers.contains(KeyModifiers::SHIFT) {
        return None;
    }
    let mut code = match event.kind {
        MouseEventKind::Down(b) => button(b),
        MouseEventKind::Up(b) if modes.mouse_mode >= 2 => button(b),
        MouseEventKind::Drag(b) if modes.mouse_mode >= 3 => button(b) + 32,
        MouseEventKind::Moved if modes.mouse_mode >= 4 => 35,
        MouseEventKind::ScrollUp => 64,
        MouseEventKind::ScrollDown => 65,
        _ => return None,
    };
    if event.modifiers.contains(KeyModifiers::ALT) {
        code += 8;
    }
    if event.modifiers.contains(KeyModifiers::CONTROL) {
        code += 16;
    }
    let x = event.column.saturating_sub(area.x) + 1;
    let y = event.row.saturating_sub(area.y) + 1;
    let release = matches!(event.kind, MouseEventKind::Up(_));
    match modes.mouse_encoding {
        2 => Some(format!("\x1b[<{code};{x};{y}{}", if release { 'm' } else { 'M' }).into_bytes()),
        1 => {
            let code = if release { 3 } else { code };
            let mut s = "\x1b[M".to_string();
            for n in [code + 32, x + 32, y + 32] {
                s.push(char::from_u32(u32::from(n))?);
            }
            Some(s.into_bytes())
        }
        _ if x <= 223 && y <= 223 => Some(vec![
            27,
            b'[',
            b'M',
            (if release { 3 } else { code }) as u8 + 32,
            x as u8 + 32,
            y as u8 + 32,
        ]),
        _ => None,
    }
}
fn button(b: MouseButton) -> u16 {
    match b {
        MouseButton::Left => 0,
        MouseButton::Middle => 1,
        MouseButton::Right => 2,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn terminal_keys_paste_and_mouse_respect_reported_modes() {
        let modes = InputModes {
            application_cursor: true,
            bracketed_paste: true,
            mouse_mode: 3,
            mouse_encoding: 2,
            ..Default::default()
        };
        assert_eq!(
            key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE), &modes),
            Some(vec![27])
        );
        assert_eq!(
            key(KeyEvent::new(KeyCode::Left, KeyModifiers::NONE), &modes),
            Some(b"\x1bOD".to_vec())
        );
        assert_eq!(
            key(
                KeyEvent::new(KeyCode::Left, KeyModifiers::CONTROL | KeyModifiers::SHIFT),
                &modes
            ),
            Some(b"\x1b[1;6D".to_vec())
        );
        assert_eq!(
            paste("你好\nsecond", &modes),
            "\x1b[200~你好\nsecond\x1b[201~".as_bytes()
        );
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 12,
            row: 9,
            modifiers: KeyModifiers::NONE,
        };
        assert_eq!(
            mouse(event, Rect::new(10, 5, 30, 20), &modes),
            Some(b"\x1b[<0;3;5M".to_vec())
        );
        assert!(
            mouse(
                MouseEvent {
                    modifiers: KeyModifiers::SHIFT,
                    ..event
                },
                Rect::new(10, 5, 30, 20),
                &modes
            )
            .is_none()
        );
        assert!(mouse(event, Rect::new(10, 5, 30, 20), &InputModes::default()).is_none());
    }
    #[test]
    fn styled_wide_cells_and_cursor_reach_ratatui() {
        let mut parser = vt100::Parser::new(3, 12, 0);
        parser.process("\x1b[38;2;12;34;56m\x1b[1;4m中A\x1b[2;5H".as_bytes());
        let view = crate::terminal::screen::project(parser.screen(), 0, 0);
        let mut terminal =
            ratatui::Terminal::new(ratatui::backend::TestBackend::new(12, 3)).unwrap();
        terminal
            .draw(|f| draw(f, Rect::new(0, 0, 12, 3), &view, true))
            .unwrap();
        let buffer = terminal.backend().buffer();
        assert_eq!(buffer[(0, 0)].symbol(), "中");
        assert_eq!(buffer[(2, 0)].symbol(), "A");
        assert_eq!(buffer[(0, 0)].fg, Color::Rgb(12, 34, 56));
        assert!(
            buffer[(0, 0)]
                .modifier
                .contains(Modifier::BOLD | Modifier::UNDERLINED)
        );
    }
}
