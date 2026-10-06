//! Compact styled rows over vt100. Unicode cell occupancy comes from the
//! emulator; a wide continuation is never emitted as a second glyph.
use serde::{Deserialize, Serialize};
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScreenView {
    pub lines: Vec<Vec<Run>>,
    pub cursor: (u16, u16),
    pub cursor_visible: bool,
    pub modes: InputModes,
    pub scrollback_offset: usize,
    pub retained: usize,
}
/// text, occupied columns, foreground, background, attribute bits.
/// Colors: 0 default; 1..=256 palette; 0x1000000 | RGB true color.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Run(pub String, pub u16, pub u32, pub u32, pub u8);
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct InputModes {
    pub application_cursor: bool,
    pub application_keypad: bool,
    pub bracketed_paste: bool,
    pub alternate: bool,
    pub mouse_mode: u8,
    pub mouse_encoding: u8,
}
fn color(c: vt100::Color) -> u32 {
    match c {
        vt100::Color::Default => 0,
        vt100::Color::Idx(i) => u32::from(i) + 1,
        vt100::Color::Rgb(r, g, b) => {
            0x1000000 | (u32::from(r) << 16) | (u32::from(g) << 8) | u32::from(b)
        }
    }
}
pub fn project(screen: &vt100::Screen, offset: usize, retained: usize) -> ScreenView {
    let (rows, cols) = screen.size();
    let mut lines = Vec::new();
    for y in 0..rows {
        let mut runs: Vec<Run> = Vec::new();
        for x in 0..cols {
            if let Some(cell) = screen.cell(y, x) {
                if cell.is_wide_continuation() {
                    continue;
                }
                let fg = color(cell.fgcolor());
                let bg = color(cell.bgcolor());
                let attrs = u8::from(cell.bold())
                    | (u8::from(cell.dim()) << 1)
                    | (u8::from(cell.italic()) << 2)
                    | (u8::from(cell.underline()) << 3)
                    | (u8::from(cell.inverse()) << 4);
                let text = cell.contents();
                let text = if text.is_empty() { " " } else { text };
                let width = if cell.is_wide() { 2 } else { 1 };
                if let Some(last) = runs
                    .last_mut()
                    .filter(|r| r.2 == fg && r.3 == bg && r.4 == attrs)
                {
                    last.0.push_str(text);
                    last.1 += width;
                } else {
                    runs.push(Run(text.into(), width, fg, bg, attrs));
                }
            }
        }
        lines.push(runs);
    }
    let mouse_mode = match screen.mouse_protocol_mode() {
        vt100::MouseProtocolMode::None => 0,
        vt100::MouseProtocolMode::Press => 1,
        vt100::MouseProtocolMode::PressRelease => 2,
        vt100::MouseProtocolMode::ButtonMotion => 3,
        vt100::MouseProtocolMode::AnyMotion => 4,
    };
    let mouse_encoding = match screen.mouse_protocol_encoding() {
        vt100::MouseProtocolEncoding::Default => 0,
        vt100::MouseProtocolEncoding::Utf8 => 1,
        vt100::MouseProtocolEncoding::Sgr => 2,
    };
    ScreenView {
        lines,
        cursor: screen.cursor_position(),
        cursor_visible: !screen.hide_cursor() && offset == 0,
        modes: InputModes {
            application_cursor: screen.application_cursor(),
            application_keypad: screen.application_keypad(),
            bracketed_paste: screen.bracketed_paste(),
            alternate: screen.alternate_screen(),
            mouse_mode,
            mouse_encoding,
        },
        scrollback_offset: offset,
        retained,
    }
}
/// Terminal queries handled by the emulator callback; replies are bounded and
/// written to this session's PTY, never to the physical outer terminal.
#[derive(Default)]
pub struct Replies {
    pub bytes: Vec<u8>,
}
impl vt100::Callbacks for Replies {
    fn unhandled_csi(
        &mut self,
        screen: &mut vt100::Screen,
        i1: Option<u8>,
        _: Option<u8>,
        params: &[&[u16]],
        c: char,
    ) {
        let p = params.first().and_then(|p| p.first()).copied().unwrap_or(0);
        let reply = match (c, p, i1) {
            ('n', 5, None) => Some("\x1b[0n".into()),
            ('n', 6, None) => {
                let (y, x) = screen.cursor_position();
                Some(format!("\x1b[{};{}R", y + 1, x + 1))
            }
            ('c', 0, None) => Some("\x1b[?1;2c".into()),
            ('c', 0, Some(b'>')) => Some("\x1b[>0;0;0c".into()),
            ('t', 18, None) => {
                let (r, c) = screen.size();
                Some(format!("\x1b[8;{r};{c}t"))
            }
            _ => None,
        };
        if let Some(reply) = reply
            && self.bytes.len() + reply.len() <= 4096
        {
            self.bytes.extend(reply.as_bytes());
        }
    }
}

/// The length of the trailing UNFINISHED escape/UTF-8 sequence in a raw
/// output tail, found by replaying the tail through `vte` — the same
/// state machine vt100 wraps — and reporting where the last fully applied
/// action ended. The bytes after that boundary fired no dispatch: they
/// sit parked inside the parser's in-progress state (or its partial-UTF-8
/// slot), invisible to any screen snapshot, and must be replayed into the
/// adopted parser so a sequence split by a live handoff is reassembled
/// instead of rendered as stray text.
///
/// Best-effort by construction: the replay starts from `Ground`, so a
/// sequence already in progress when the bounded tail window began cannot
/// be fully recovered — the adopted parser then sees only its continuation
/// (a full-screen TUI repaints on the post-adopt resize nudge anyway).
pub fn unfinished_tail_len(tail: &[u8]) -> usize {
    /// `last` = offset just past the most recent byte that completed an
    /// action and returned the machine to (or left it in) a settled state.
    /// DCS hook/put are NOT boundaries: the passthrough body is in flight
    /// until `unhook`, so an interrupted DCS is preserved whole.
    struct Boundary {
        offset: usize,
        last: usize,
    }
    impl vte::Perform for Boundary {
        fn print(&mut self, _: char) {
            self.last = self.offset;
        }
        fn execute(&mut self, _: u8) {
            self.last = self.offset;
        }
        fn csi_dispatch(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) {
            self.last = self.offset;
        }
        fn esc_dispatch(&mut self, _: &[u8], _: bool, _: u8) {
            self.last = self.offset;
        }
        fn osc_dispatch(&mut self, _: &[&[u8]], _: bool) {
            self.last = self.offset;
        }
        fn hook(&mut self, _: &vte::Params, _: &[u8], _: bool, _: char) {}
        fn put(&mut self, _: u8) {}
        fn unhook(&mut self) {
            self.last = self.offset;
        }
    }
    let mut parser = vte::Parser::new();
    let mut tracker = Boundary { offset: 0, last: 0 };
    // Byte-at-a-time advance keeps the callback→byte-offset mapping exact
    // (dispatches report positions, and vte keeps no raw bytes itself).
    // Only ever run over the bounded handoff tail — a 64 KiB scan is
    // microseconds-to-milliseconds of state-machine matches.
    for (i, byte) in tail.iter().enumerate() {
        tracker.offset = i + 1;
        parser.advance(&mut tracker, std::slice::from_ref(byte));
    }
    tail.len() - tracker.last
}

/// Reconstruct both buffers and styled primary scrollback for live handoff
/// (the free-function core of `TerminalHandle::formatted_screen`). A
/// scratch parser operates on a cloned Screen: the live parser/offset and
/// any partially received escape sequence remain untouched.
pub fn formatted_screen_of<CB: vt100::Callbacks>(parser: &vt100::Parser<CB>) -> Vec<u8> {
    let original = parser.screen().clone();
    let (rows, cols) = original.size();
    let mut scratch = vt100::Parser::new(rows, cols, super::SCROLLBACK_LINES);
    *scratch.screen_mut() = original.clone();
    if original.alternate_screen() {
        scratch.process(b"\x1b[?1049l");
    }
    scratch.screen_mut().set_scrollback(usize::MAX);
    let retained = scratch.screen().scrollback();
    let mut bytes = b"\x1b[H".to_vec();
    let mut consumed = 0;
    while consumed < retained {
        scratch.screen_mut().set_scrollback(retained - consumed);
        let count = (retained - consumed).min(rows as usize);
        let view = project(scratch.screen(), 0, retained);
        for row in view.lines.iter().take(count) {
            bytes.extend(formatted_row(row));
            bytes.extend(b"\r\n");
        }
        consumed += count;
    }
    if retained > 0 {
        for _ in 1..rows {
            bytes.extend(b"\r\n");
        }
    }
    scratch.screen_mut().set_scrollback(0);
    bytes.extend(scratch.screen().state_formatted());
    bytes.extend(scratch.screen().cursor_state_formatted());
    if original.alternate_screen() {
        bytes.extend(b"\x1b[?1049h");
        bytes.extend(original.state_formatted());
        bytes.extend(original.cursor_state_formatted());
    }
    bytes.extend(original.input_mode_formatted());
    bytes
}

/// A single row for history replay, with explicit attributes and no cursor
/// addressing. Rows are already emulator-width constrained.
pub fn formatted_row(row: &[Run]) -> Vec<u8> {
    fn color(c: u32, fg: bool) -> String {
        let base = if fg { 38 } else { 48 };
        if c == 0 {
            (if fg { 39 } else { 49 }).to_string()
        } else if c <= 256 {
            format!("{base};5;{}", c - 1)
        } else {
            format!(
                "{base};2;{};{};{}",
                (c >> 16) & 255,
                (c >> 8) & 255,
                c & 255
            )
        }
    }
    let mut out = Vec::new();
    for run in row {
        out.extend(format!("\x1b[0;{};{}", color(run.2, true), color(run.3, false)).as_bytes());
        for (bit, attr) in [(1, 1), (2, 2), (4, 3), (8, 4), (16, 7)] {
            if run.4 & bit != 0 {
                out.extend(format!(";{attr}").as_bytes());
            }
        }
        out.push(b'm');
        out.extend(run.0.as_bytes());
    }
    out.extend(b"\x1b[0m");
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unfinished_tail_boundary_cases() {
        // Settled text: nothing pending.
        assert_eq!(unfinished_tail_len(b"hello\r\nworld"), 0);
        // Mid-SGR: the introducer + params so far are parked in the parser.
        assert_eq!(unfinished_tail_len(b"hi\x1b[38;2;1"), b"\x1b[38;2;1".len());
        // Completed SGR: settled.
        assert_eq!(unfinished_tail_len(b"hi\x1b[38;2;1;2;3m!"), 0);
        // Lone ESC introducer.
        assert_eq!(unfinished_tail_len(b"x\x1b"), 1);
        // OSC in flight (hyperlink), BEL- and ST-terminated forms.
        assert_eq!(
            unfinished_tail_len(b"a\x1b]8;;http://e"),
            b"\x1b]8;;http://e".len()
        );
        assert_eq!(unfinished_tail_len(b"a\x1b]0;title\x07"), 0);
        assert_eq!(unfinished_tail_len(b"a\x1b]0;title\x1b\\"), 0);
        // Partial UTF-8: the lead bytes of 中 (E4 B8 AD) fire no print
        // until the codepoint completes.
        assert_eq!(unfinished_tail_len("a中".as_bytes()), 0);
        assert_eq!(unfinished_tail_len("a\u{4e2d}".as_bytes()), 0);
        let partial = "ok\u{4e2d}".as_bytes();
        assert_eq!(unfinished_tail_len(&partial[..partial.len() - 1]), 2);
        assert_eq!(unfinished_tail_len(&partial[..partial.len() - 2]), 1);
        // A DCS passthrough still in flight is preserved WHOLE (hook and
        // put bytes are not boundaries); completed (unhook) is settled.
        let dcs = b"\x1bPq#0;2;0;0;0~~\x1b\\";
        assert_eq!(
            unfinished_tail_len(b"pre\x1bPq#0;2;0;0"),
            b"\x1bPq#0;2;0;0".len()
        );
        assert_eq!(unfinished_tail_len(dcs), 0);
        // C0 controls in ground settle (execute) — the common \r\n tail.
        assert_eq!(unfinished_tail_len(b"text\r"), 0);
    }

    /// The handoff byte-boundary contract, split at EVERY byte position of
    /// a representative stream: screen replay + pending tail + continuation
    /// must render EXACTLY what an uninterrupted parser renders. This is
    /// the regression for the issue #45 handoff gap where a read stopping
    /// mid-sequence made the adopted parser print the continuation as
    /// plain text.
    #[test]
    fn every_split_point_renders_like_an_uninterrupted_parser() {
        let stream: Vec<u8> = b"line one\r\nline \x1b[1;38;2;10;20;30mtwo\x1b[0m styled\r\n\
             \x1b]8;;http://example.com\x1b\\\\link\x1b]8;;\x1b\\\\\r\n\
             \x1b[?1049halt-screen\x1b[5;13H\x1b[?2004h\x1b[?1h\x1b[?1003h\x1b[?1006h"
            .iter()
            .copied()
            .chain(b"tail".iter().copied())
            .collect();
        // The stream builder above cannot spell non-ASCII; append it here.
        let mut stream = stream;
        stream.extend_from_slice(" \u{4e2d}\u{6587}!".as_bytes());
        let stream = &stream[..];

        let reference = {
            let mut parser = vt100::Parser::new(12, 40, 64);
            parser.process(stream);
            project(parser.screen(), 0, 0)
        };

        for split in 0..=stream.len() {
            let (head, tail) = stream.split_at(split);
            let mut old_parser = vt100::Parser::new(12, 40, 64);
            old_parser.process(head);
            // The handoff pair: the rendered state plus the trailing
            // bytes the parser parked instead of applying.
            let screen = formatted_screen_of(&old_parser);
            let pending = &head[head.len() - unfinished_tail_len(head)..];
            let mut adopted = vt100::Parser::new(12, 40, 64);
            adopted.process(&screen);
            adopted.process(pending);
            adopted.process(tail);
            let after = project(adopted.screen(), 0, 0);
            assert_eq!(
                after.lines, reference.lines,
                "lines differ at split {split}"
            );
            assert_eq!(
                after.cursor, reference.cursor,
                "cursor differs at split {split}"
            );
            assert_eq!(
                after.modes, reference.modes,
                "modes differ at split {split}"
            );
        }
    }
}
