//! OSC 52 clipboard writes.
//!
//! A pane can ask the host to place text on the macOS clipboard. The text
//! is handed to `pbcopy` and is not written to Viva's disk log, handoff
//! manifest, or database. Set `VIVA_OSC52=0` (also `off`, `false`, `no`)
//! to ignore the request. Anything else, including an unset variable,
//! leaves the feature on.

#[cfg(target_os = "macos")]
use std::io::Write;

const MAX_CLIPBOARD_BYTES: usize = 1024 * 1024;

/// `true` unless the environment explicitly turns OSC 52 off.
pub(crate) fn enabled_from_env() -> bool {
    enabled_from_value(std::env::var("VIVA_OSC52").ok().as_deref())
}

pub(crate) fn enabled_from_value(value: Option<&str>) -> bool {
    !matches!(
        value
            .map(str::trim)
            .map(|s| s.to_ascii_lowercase())
            .as_deref(),
        Some("0" | "off" | "false" | "no")
    )
}

/// macOS has one system clipboard. `c` and `s` name it; an empty selector
/// is the xterm default (every clipboard). Primary (`p`) and the other
/// X11 selections are not that clipboard, so they are ignored.
pub(crate) fn system_clipboard_selector(ty: &[u8]) -> bool {
    ty.is_empty() || ty.contains(&b'c') || ty.contains(&b's')
}

/// Decode an OSC 52 payload. Invalid base64, non-UTF-8, and oversized
/// payloads are refused rather than repaired.
pub(crate) fn decode_clipboard(data: &[u8]) -> Option<String> {
    if data.len() > MAX_CLIPBOARD_BYTES / 3 * 4 + 4 {
        return None;
    }
    let bytes = decode_base64(data)?;
    if bytes.len() > MAX_CLIPBOARD_BYTES {
        return None;
    }
    String::from_utf8(bytes).ok()
}

/// Hand decoded clipboard texts to the macOS pasteboard.
///
/// The write runs off the PTY reader so a slow pasteboard cannot stall
/// output. Failures are ignored and the text is not logged.
pub(crate) fn publish(texts: Vec<String>) {
    if texts.is_empty() {
        return;
    }
    std::thread::spawn(move || {
        for text in texts {
            let _ = write_clipboard(&text);
        }
    });
}

fn write_clipboard(text: &str) -> std::io::Result<()> {
    #[cfg(target_os = "macos")]
    {
        let mut child = std::process::Command::new("/usr/bin/pbcopy")
            .stdin(std::process::Stdio::piped())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn()?;
        if let Some(mut stdin) = child.stdin.take() {
            stdin.write_all(text.as_bytes())?;
            drop(stdin);
        }
        let _ = child.wait();
        Ok(())
    }
    #[cfg(not(target_os = "macos"))]
    {
        let _ = text;
        Ok(())
    }
}

/// Drop an unfinished OSC 52 from a handoff pending tail.
///
/// The tail is persisted in the handoff manifest. A clipboard payload that
/// the parser has not finished must not be stored with it. Other unfinished
/// sequences are left unchanged.
pub(crate) fn strip_unfinished_clipboard(pending: &[u8]) -> Vec<u8> {
    if let Some(start) = osc52_start(pending) {
        pending[..start].to_vec()
    } else {
        pending.to_vec()
    }
}

fn osc52_start(bytes: &[u8]) -> Option<usize> {
    bytes
        .windows(4)
        .position(|window| window == b"\x1b]52")
        .or_else(|| bytes.windows(3).position(|window| window == b"\x9d52"))
}

/// Streaming removal of OSC 52 from the raw PTY transcript.
///
/// Every other byte, including other escape sequences, is kept. An OSC 52
/// that is still open at [`Osc52Stripper::flush`] is discarded so a chunk
/// boundary cannot leak the payload into the disk log.
#[derive(Debug, Default)]
pub(crate) struct Osc52Stripper {
    stage: Stage,
    hold: Vec<u8>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
enum Stage {
    #[default]
    Ground,
    Esc,
    Osc,
    Param,
    Drop,
    DropEsc,
    Pass,
    PassEsc,
}

impl Osc52Stripper {
    pub(crate) fn push(&mut self, chunk: &[u8]) -> Vec<u8> {
        let mut out = Vec::with_capacity(chunk.len());
        for &byte in chunk {
            self.feed(byte, &mut out);
        }
        out
    }

    pub(crate) fn flush(&mut self) -> Vec<u8> {
        let out = match self.stage {
            Stage::Drop | Stage::DropEsc => Vec::new(),
            _ => std::mem::take(&mut self.hold),
        };
        self.hold.clear();
        self.stage = Stage::Ground;
        out
    }

    fn feed(&mut self, byte: u8, out: &mut Vec<u8>) {
        match self.stage {
            Stage::Ground => match byte {
                0x1b => {
                    self.hold = vec![0x1b];
                    self.stage = Stage::Esc;
                }
                0x9d => {
                    self.hold = vec![0x9d];
                    self.stage = Stage::Osc;
                }
                _ => out.push(byte),
            },
            Stage::Esc => {
                if byte == b']' {
                    self.hold.push(byte);
                    self.stage = Stage::Osc;
                } else if byte == 0x1b {
                    self.emit_hold(out);
                    self.hold = vec![0x1b];
                } else {
                    self.emit_hold(out);
                    self.stage = Stage::Ground;
                    self.feed(byte, out);
                }
            }
            Stage::Osc => match byte {
                b'0'..=b'9' => {
                    self.hold.push(byte);
                    self.stage = Stage::Param;
                }
                0x1b => {
                    self.emit_hold(out);
                    self.hold = vec![0x1b];
                    self.stage = Stage::Esc;
                }
                0x07 | 0x18 | 0x1a | 0x9c => {
                    self.emit_hold(out);
                    out.push(byte);
                    self.stage = Stage::Ground;
                }
                _ => {
                    self.emit_hold(out);
                    out.push(byte);
                    self.stage = Stage::Pass;
                }
            },
            Stage::Param => match byte {
                b'0'..=b'9' if self.param().len() >= 2 => {
                    self.emit_hold(out);
                    out.push(byte);
                    self.stage = Stage::Pass;
                }
                b'0'..=b'9' => self.hold.push(byte),
                b';' if self.param() == b"52" => {
                    self.hold.clear();
                    self.stage = Stage::Drop;
                }
                b';' => {
                    self.emit_hold(out);
                    out.push(byte);
                    self.stage = Stage::Pass;
                }
                0x1b => {
                    self.emit_hold(out);
                    self.hold = vec![0x1b];
                    self.stage = Stage::Esc;
                }
                0x07 | 0x18 | 0x1a | 0x9c => {
                    self.emit_hold(out);
                    out.push(byte);
                    self.stage = Stage::Ground;
                }
                _ => {
                    self.emit_hold(out);
                    out.push(byte);
                    self.stage = Stage::Pass;
                }
            },
            Stage::Drop => match byte {
                0x07 | 0x18 | 0x1a | 0x9c => self.stage = Stage::Ground,
                0x1b => self.stage = Stage::DropEsc,
                _ => {}
            },
            Stage::DropEsc => {
                if byte == b'\\' {
                    self.hold.clear();
                    self.stage = Stage::Ground;
                } else if byte == 0x1b {
                    self.hold = vec![0x1b];
                    self.stage = Stage::Esc;
                } else {
                    self.hold = vec![0x1b];
                    self.stage = Stage::Esc;
                    self.feed(byte, out);
                }
            }
            Stage::Pass => match byte {
                0x07 | 0x18 | 0x1a | 0x9c => {
                    out.push(byte);
                    self.stage = Stage::Ground;
                }
                0x1b => self.stage = Stage::PassEsc,
                _ => out.push(byte),
            },
            Stage::PassEsc => {
                if byte == b'\\' {
                    out.push(0x1b);
                    out.push(byte);
                    self.stage = Stage::Ground;
                } else {
                    self.stage = Stage::Esc;
                    self.hold = vec![0x1b];
                    self.feed(byte, out);
                }
            }
        }
    }

    fn param(&self) -> &[u8] {
        if self.hold.first() == Some(&0x9d) {
            &self.hold[1..]
        } else {
            self.hold.get(2..).unwrap_or(&[])
        }
    }

    fn emit_hold(&mut self, out: &mut Vec<u8>) {
        out.extend(std::mem::take(&mut self.hold));
    }
}

fn decode_base64(input: &[u8]) -> Option<Vec<u8>> {
    if input.is_empty() {
        return Some(Vec::new());
    }
    if input.len() % 4 != 0 {
        return None;
    }
    let mut out = Vec::with_capacity(input.len() / 4 * 3);
    for chunk in input.chunks_exact(4) {
        let pad = usize::from(chunk[2] == b'=') + usize::from(chunk[3] == b'=');
        if chunk[2] == b'=' && chunk[3] != b'=' {
            return None;
        }
        let a = b64_val(chunk[0])?;
        let b = b64_val(chunk[1])?;
        let c = if chunk[2] == b'=' {
            0
        } else {
            b64_val(chunk[2])?
        };
        let d = if chunk[3] == b'=' {
            0
        } else {
            b64_val(chunk[3])?
        };
        out.push((a << 2) | (b >> 4));
        if pad < 2 {
            out.push((b << 4) | (c >> 2));
        }
        if pad == 0 {
            out.push((c << 6) | d);
        }
    }
    Some(out)
}

fn b64_val(byte: u8) -> Option<u8> {
    match byte {
        b'A'..=b'Z' => Some(byte - b'A'),
        b'a'..=b'z' => Some(byte - b'a' + 26),
        b'0'..=b'9' => Some(byte - b'0' + 52),
        b'+' => Some(62),
        b'/' => Some(63),
        _ => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn osc52_config_defaults_on_and_recognizes_the_off_switch() {
        assert!(enabled_from_value(None));
        assert!(enabled_from_value(Some("")));
        assert!(enabled_from_value(Some("1")));
        assert!(enabled_from_value(Some("on")));
        assert!(!enabled_from_value(Some("0")));
        assert!(!enabled_from_value(Some(" OFF ")));
        assert!(!enabled_from_value(Some("false")));
        assert!(!enabled_from_value(Some("no")));
    }

    #[test]
    fn clipboard_payload_decodes_only_when_it_is_utf8_text() {
        assert_eq!(decode_clipboard(b"aGVsbG8=").as_deref(), Some("hello"));
        assert_eq!(decode_clipboard(b"").as_deref(), Some(""));
        assert!(decode_clipboard(b"!!!!").is_none());
        assert!(decode_clipboard(b"YQ==").is_some());
        // 0xff is not UTF-8.
        assert!(decode_clipboard(b"/w==").is_none());
        assert!(system_clipboard_selector(b"c"));
        assert!(system_clipboard_selector(b""));
        assert!(system_clipboard_selector(b"cs"));
        assert!(!system_clipboard_selector(b"p"));
    }

    #[test]
    fn stripper_removes_osc52_across_chunk_boundaries_and_keeps_neighbors() {
        let secret = "viva-osc52-secret";
        let payload = base64_encode(secret.as_bytes());
        let mut raw = b"preface ".to_vec();
        raw.extend(format!("\x1b]52;c;{payload}\x07").into_bytes());
        raw.extend(b" epilogue \x1b]0;kept-title\x07 tail");
        raw.extend(format!("\x1b]52;c;{payload}\x1b\\").into_bytes());
        raw.extend(b" done");
        for split in 0..=raw.len() {
            let mut stripper = Osc52Stripper::default();
            let mut out = stripper.push(&raw[..split]);
            out.extend(stripper.push(&raw[split..]));
            out.extend(stripper.flush());
            let text = String::from_utf8(out).expect("log stays valid utf-8");
            assert!(text.contains("preface"), "{text}");
            assert!(text.contains("epilogue"), "{text}");
            assert!(text.contains("kept-title"), "{text}");
            assert!(text.contains("done"), "{text}");
            assert!(!text.contains(secret), "split {split}: {text}");
            assert!(!text.contains(&payload), "split {split}: {text}");
        }
    }

    #[test]
    fn unfinished_clipboard_sequence_is_cut_out_of_a_pending_tail() {
        let pending = b"\x1b]52;c;viva-osc52-secret";
        assert_eq!(strip_unfinished_clipboard(pending), b"");
        assert_eq!(strip_unfinished_clipboard(b"\x1b[31"), b"\x1b[31");
        let mixed = b"\x1b[31\x1b]52;c;secret";
        assert_eq!(strip_unfinished_clipboard(mixed), b"\x1b[31");
        assert_eq!(strip_unfinished_clipboard(b"ok\x9d52;c;secret"), b"ok");
    }

    #[test]
    fn c1_osc52_is_removed_and_does_not_swallow_the_following_bytes() {
        let secret = "viva-osc52-secret";
        let payload = base64_encode(secret.as_bytes());
        let mut raw = vec![0x9d];
        raw.extend(format!("52;c;{payload}").into_bytes());
        raw.push(0x9c);
        raw.extend(b"kept");
        raw.extend(format!("\x1b]52;c;{payload}\x07").into_bytes());
        raw.extend(b"tail");
        let mut stripper = Osc52Stripper::default();
        let mut out = stripper.push(&raw);
        out.extend(stripper.flush());
        let text = String::from_utf8(out).expect("log stays valid utf-8");
        assert_eq!(text, "kepttail");
        assert!(!text.contains(secret));
        assert!(!text.contains(&payload));
    }

    #[test]
    fn osc52_is_queued_only_when_enabled_and_never_drawn() {
        let secret = "viva-osc52-secret";
        let payload = base64_encode(secret.as_bytes());
        let sequence = format!("before\x1b]52;c;{payload}\x07after");
        let mut enabled = vt100::Parser::new_with_callbacks(
            2,
            40,
            0,
            crate::terminal::screen::Replies::with_osc52(true),
        );
        enabled.process(sequence.as_bytes());
        let drawn = drawn_text(enabled.screen());
        assert!(drawn.contains("before"));
        assert!(drawn.contains("after"));
        assert!(!drawn.contains(secret), "{drawn}");
        assert!(enabled.callbacks().bytes.is_empty());
        assert_eq!(
            enabled.callbacks_mut().take_clipboard(),
            vec![secret.to_string()]
        );
        // A paste query does not read the clipboard or echo the text.
        enabled.process(b"\x1b]52;c;?\x07");
        assert!(enabled.callbacks().bytes.is_empty());
        assert!(enabled.callbacks_mut().take_clipboard().is_empty());
        // Primary selection is not the macOS clipboard.
        enabled.process(b"\x1b]52;p;aGVsbG8=\x07");
        assert!(enabled.callbacks_mut().take_clipboard().is_empty());

        let mut disabled = vt100::Parser::new_with_callbacks(
            2,
            40,
            0,
            crate::terminal::screen::Replies::with_osc52(false),
        );
        disabled.process(sequence.as_bytes());
        assert!(disabled.callbacks_mut().take_clipboard().is_empty());
        assert!(!drawn_text(disabled.screen()).contains(secret));
    }

    fn drawn_text(screen: &vt100::Screen) -> String {
        let view = crate::terminal::screen::project(screen, 0, 0);
        view.lines
            .into_iter()
            .flat_map(|row| row.into_iter().map(|run| run.0))
            .collect()
    }

    fn base64_encode(input: &[u8]) -> String {
        const ALPH: &[u8] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
        let mut out = String::new();
        for chunk in input.chunks(3) {
            let a = chunk[0];
            let b = chunk.get(1).copied().unwrap_or(0);
            let c = chunk.get(2).copied().unwrap_or(0);
            out.push(ALPH[(a >> 2) as usize] as char);
            out.push(ALPH[(((a & 0x03) << 4) | (b >> 4)) as usize] as char);
            if chunk.len() > 1 {
                out.push(ALPH[(((b & 0x0f) << 2) | (c >> 6)) as usize] as char);
            } else {
                out.push('=');
            }
            if chunk.len() > 2 {
                out.push(ALPH[(c & 0x3f) as usize] as char);
            } else {
                out.push('=');
            }
        }
        out
    }
}
