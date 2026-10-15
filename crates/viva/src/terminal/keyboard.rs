//! Kitty keyboard protocol for bytes written into the PTY.
//!
//! The program opts in with `CSI =/>/</? u`. Flags this terminal cannot
//! honor (alternate-key reporting, press/repeat/release event types on the
//! macOS host path) are dropped and are not reported back.
//! A key event whose unshifted identity is not in the host event is dropped
//! too — it is never replaced with a guessed legacy byte.

use std::collections::VecDeque;

use ratatui::crossterm::event::{
    KeyCode, KeyEvent, KeyEventKind, KeyEventState, KeyModifiers, MediaKeyCode, ModifierKeyCode,
};

use super::screen::InputModes;

/// Disambiguate escape codes.
pub(crate) const DISAMBIGUATE: u16 = 1;
/// Report press / repeat / release.
pub(crate) const REPORT_EVENTS: u16 = 2;
/// Report every key as a `CSI u` sequence. Implies [`DISAMBIGUATE`].
pub(crate) const REPORT_ALL: u16 = 8;
/// Embed the produced text in the `CSI u` sequence. Requires [`REPORT_ALL`].
pub(crate) const REPORT_TEXT: u16 = 16;

const ALTERNATE_KEYS: u16 = 4;
const SUPPORTED: u16 = DISAMBIGUATE | REPORT_ALL | REPORT_TEXT;
const STACK_LIMIT: usize = 16;

#[derive(Debug, Clone, Default)]
struct ScreenFlags {
    flags: u16,
    stack: VecDeque<u16>,
}

/// Independent keyboard-mode stacks for the primary and alternate screens.
#[derive(Debug, Clone, Default)]
pub(crate) struct KittyKeyboard {
    primary: ScreenFlags,
    alternate: ScreenFlags,
}

impl KittyKeyboard {
    pub(crate) fn flags(&self, alternate: bool) -> u16 {
        self.slot(alternate).flags
    }

    fn slot(&self, alternate: bool) -> &ScreenFlags {
        if alternate {
            &self.alternate
        } else {
            &self.primary
        }
    }

    fn slot_mut(&mut self, alternate: bool) -> &mut ScreenFlags {
        if alternate {
            &mut self.alternate
        } else {
            &mut self.primary
        }
    }

    /// Apply one Kitty keyboard `CSI … u` request.
    ///
    /// Returns the query reply, when the program asked for the current flags.
    /// An unsupported or ambiguous request changes nothing.
    pub(crate) fn handle(
        &mut self,
        alternate: bool,
        intermediate: Option<u8>,
        extra_intermediate: Option<u8>,
        params: &[&[u16]],
    ) -> Option<Vec<u8>> {
        if extra_intermediate.is_some() {
            return None;
        }
        let slot = self.slot_mut(alternate);
        match intermediate {
            Some(b'=') => {
                let (flags, mode) = parse_set(params)?;
                apply(slot, flags, mode);
                None
            }
            Some(b'>') => {
                let flags = parse_single(params, 0)?;
                push(slot, flags);
                None
            }
            Some(b'<') => {
                let n = parse_single(params, 1)?;
                pop(slot, if n == 0 { 1 } else { n });
                None
            }
            Some(b'?') => {
                if !is_query(params) {
                    return None;
                }
                Some(format!("\x1b[?{}u", slot.flags).into_bytes())
            }
            _ => None,
        }
    }

    /// Bytes that recreate the active screen's flags on a fresh parser.
    pub(crate) fn restore_bytes(&self, alternate: bool) -> Vec<u8> {
        let slot = self.slot(alternate);
        if slot.stack.is_empty() && slot.flags == 0 {
            return Vec::new();
        }
        let mut out = Vec::new();
        if slot.stack.is_empty() {
            out.extend(format!("\x1b[={};1u", slot.flags).into_bytes());
            return out;
        }
        let oldest = slot.stack.front().copied().unwrap_or(0);
        out.extend(format!("\x1b[={oldest};1u").into_bytes());
        for value in slot.stack.iter().skip(1) {
            out.extend(format!("\x1b[>{value}u").into_bytes());
        }
        out.extend(format!("\x1b[>{}u", slot.flags).into_bytes());
        out
    }
}

fn normalize(flags: u16) -> u16 {
    let mut flags = flags & SUPPORTED;
    if flags & REPORT_ALL != 0 {
        flags |= DISAMBIGUATE;
    } else {
        flags &= !REPORT_TEXT;
    }
    flags
}

fn apply(slot: &mut ScreenFlags, requested: u16, mode: u16) {
    let requested = requested & !ALTERNATE_KEYS;
    match mode {
        1 => slot.flags = normalize(requested),
        2 => slot.flags = normalize(slot.flags | requested),
        3 => slot.flags = normalize(slot.flags & !requested),
        _ => {}
    }
}

fn push(slot: &mut ScreenFlags, flags: u16) {
    if slot.stack.len() == STACK_LIMIT {
        slot.stack.pop_front();
    }
    slot.stack.push_back(slot.flags);
    slot.flags = normalize(flags);
}

fn pop(slot: &mut ScreenFlags, n: u16) {
    let mut restored = None;
    for _ in 0..n {
        match slot.stack.pop_back() {
            Some(previous) => {
                restored = Some(previous);
                if slot.stack.is_empty() {
                    // A pop that empties the stack resets every flag.
                    slot.flags = 0;
                    return;
                }
            }
            None => {
                slot.flags = 0;
                return;
            }
        }
    }
    if let Some(previous) = restored {
        slot.flags = previous;
    }
}

fn lone(param: &[u16]) -> Option<u16> {
    match param {
        [n] => Some(*n),
        _ => None,
    }
}

fn parse_set(params: &[&[u16]]) -> Option<(u16, u16)> {
    match params {
        [] => Some((0, 1)),
        [flags] => Some((lone(flags)?, 1)),
        [flags, mode] => {
            let mode = match lone(mode)? {
                0 | 1 => 1,
                2 => 2,
                3 => 3,
                _ => return None,
            };
            Some((lone(flags)?, mode))
        }
        _ => None,
    }
}

fn parse_single(params: &[&[u16]], default: u16) -> Option<u16> {
    match params {
        [] => Some(default),
        [value] => lone(value),
        _ => None,
    }
}

fn is_query(params: &[&[u16]]) -> bool {
    match params {
        [] => true,
        [value] => matches!(value, [0]),
        _ => false,
    }
}

// ---------------------------------------------------------------------------
// Key encoding
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct Policy {
    mods: u16,
    event: u8,
    disambiguate: bool,
    report_all: bool,
    report_text: bool,
    report_events: bool,
    app_cursor: bool,
}

enum Form {
    /// Arrow / Home / End. Unmodified application-cursor uses SS3.
    Nav(char),
    /// F1, F2, F4. Unmodified legacy encoding is always SS3.
    Ss3(char),
    /// F3. Legacy is SS3 R; the protocol form is `CSI 13 ~`.
    F3,
    Tilde(u16),
    U(u32),
}

/// Encode one host key for the PTY.
///
/// `None` means the event is dropped. Release events are dropped unless the
/// program asked for event types, and a shifted non-letter whose base key
/// is not in the host event is dropped instead of guessed.
pub(crate) fn encode(key: KeyEvent, modes: &InputModes) -> Option<Vec<u8>> {
    if modes.kitty_flags == 0
        || (key.kind == KeyEventKind::Release && modes.kitty_flags & REPORT_EVENTS == 0)
    {
        return None;
    }
    let report_all = modes.kitty_flags & REPORT_ALL != 0;
    let report_events = modes.kitty_flags & REPORT_EVENTS != 0;
    let mut code = key.code;
    let mut modifiers = key.modifiers;
    if code == KeyCode::BackTab {
        code = KeyCode::Tab;
        modifiers.insert(KeyModifiers::SHIFT);
    }
    let policy = Policy {
        mods: modifier_bits(modifiers, key.state, report_all),
        event: match key.kind {
            KeyEventKind::Release => 3,
            KeyEventKind::Repeat if report_events => 2,
            _ => 1,
        },
        disambiguate: report_all || modes.kitty_flags & DISAMBIGUATE != 0,
        report_all,
        report_text: report_all && modes.kitty_flags & REPORT_TEXT != 0,
        report_events,
        app_cursor: modes.application_cursor,
    };
    if let KeyCode::Modifier(modifier) = code {
        return encode_modifier_key(modifier, &policy);
    }
    if key.state.contains(KeyEventState::KEYPAD) && policy.disambiguate {
        return keypad_id(code).map(|id| csi_u(id, &policy, None));
    }
    match code {
        KeyCode::Char(c) => encode_char(c, &policy),
        KeyCode::Enter => encode_edit(13, Edit::Enter, &policy),
        KeyCode::Tab => encode_edit(9, Edit::Tab, &policy),
        KeyCode::Backspace => encode_edit(127, Edit::Backspace, &policy),
        KeyCode::Esc => encode_esc(&policy),
        other => encode_functional(other, &policy),
    }
}

fn modifier_bits(modifiers: KeyModifiers, state: KeyEventState, report_all: bool) -> u16 {
    let mut bits = 0u16;
    if modifiers.contains(KeyModifiers::SHIFT) {
        bits |= 1;
    }
    if modifiers.contains(KeyModifiers::ALT) {
        bits |= 2;
    }
    if modifiers.contains(KeyModifiers::CONTROL) {
        bits |= 4;
    }
    if modifiers.contains(KeyModifiers::SUPER) {
        bits |= 8;
    }
    if modifiers.contains(KeyModifiers::HYPER) {
        bits |= 16;
    }
    if modifiers.contains(KeyModifiers::META) {
        bits |= 32;
    }
    if report_all && state.contains(KeyEventState::CAPS_LOCK) {
        bits |= 64;
    }
    if report_all && state.contains(KeyEventState::NUM_LOCK) {
        bits |= 128;
    }
    bits
}

fn modifier_bit(modifier: ModifierKeyCode) -> u16 {
    match modifier {
        ModifierKeyCode::LeftShift | ModifierKeyCode::RightShift => 1,
        ModifierKeyCode::LeftAlt | ModifierKeyCode::RightAlt => 2,
        ModifierKeyCode::LeftControl | ModifierKeyCode::RightControl => 4,
        ModifierKeyCode::LeftSuper | ModifierKeyCode::RightSuper => 8,
        ModifierKeyCode::LeftHyper | ModifierKeyCode::RightHyper => 16,
        ModifierKeyCode::LeftMeta | ModifierKeyCode::RightMeta => 32,
        ModifierKeyCode::IsoLevel3Shift | ModifierKeyCode::IsoLevel5Shift => 0,
    }
}

fn modifier_key_id(modifier: ModifierKeyCode) -> u32 {
    match modifier {
        ModifierKeyCode::LeftShift => 57441,
        ModifierKeyCode::LeftControl => 57442,
        ModifierKeyCode::LeftAlt => 57443,
        ModifierKeyCode::LeftSuper => 57444,
        ModifierKeyCode::LeftHyper => 57445,
        ModifierKeyCode::LeftMeta => 57446,
        ModifierKeyCode::RightShift => 57447,
        ModifierKeyCode::RightControl => 57448,
        ModifierKeyCode::RightAlt => 57449,
        ModifierKeyCode::RightSuper => 57450,
        ModifierKeyCode::RightHyper => 57451,
        ModifierKeyCode::RightMeta => 57452,
        ModifierKeyCode::IsoLevel3Shift => 57453,
        ModifierKeyCode::IsoLevel5Shift => 57454,
    }
}

fn encode_modifier_key(modifier: ModifierKeyCode, policy: &Policy) -> Option<Vec<u8>> {
    if !policy.report_all {
        return None;
    }
    let mut policy = *policy;
    if policy.event != 3 {
        policy.mods |= modifier_bit(modifier);
    }
    Some(csi_u(modifier_key_id(modifier), &policy, None))
}

fn keypad_id(code: KeyCode) -> Option<u32> {
    Some(match code {
        KeyCode::Char(c @ '0'..='9') => 57399 + u32::from(c) - u32::from('0'),
        KeyCode::Char('.') => 57409,
        KeyCode::Char('/') => 57410,
        KeyCode::Char('*') => 57411,
        KeyCode::Char('-') => 57412,
        KeyCode::Char('+') => 57413,
        KeyCode::Enter => 57414,
        KeyCode::Char('=') => 57415,
        KeyCode::Left => 57417,
        KeyCode::Right => 57418,
        KeyCode::Up => 57419,
        KeyCode::Down => 57420,
        KeyCode::PageUp => 57421,
        KeyCode::PageDown => 57422,
        KeyCode::Home => 57423,
        KeyCode::End => 57424,
        KeyCode::Insert => 57425,
        KeyCode::Delete => 57426,
        KeyCode::KeypadBegin => 57427,
        _ => return None,
    })
}

fn encode_char(c: char, policy: &Policy) -> Option<Vec<u8>> {
    if policy.event == 3 && !policy.report_all {
        return None;
    }
    // Repeat of a text key is another press unless every key is reported.
    let mut policy = *policy;
    if !policy.report_all {
        policy.event = 1;
    }
    if policy.report_all || uses_csi_u(c, policy.mods, policy.disambiguate) {
        let code = key_codepoint(c, policy.mods & 1 != 0)?;
        let text = if policy.report_text && policy.event != 3 {
            produced_text(c, policy.mods)
        } else {
            None
        };
        return Some(csi_u(code, &policy, text));
    }
    Some(legacy_text(c, policy.mods))
}

fn uses_csi_u(c: char, mods: u16, disambiguate: bool) -> bool {
    let alt = mods & 2 != 0;
    let ctrl = mods & 4 != 0;
    let shift = mods & 1 != 0;
    let extra = mods & (8 | 16 | 32) != 0;
    if extra {
        return true;
    }
    if disambiguate {
        return alt || ctrl;
    }
    ctrl && shift && is_legacy_text_key(c)
}

fn is_legacy_text_key(c: char) -> bool {
    matches!(
        c.to_ascii_lowercase(),
        ' ' | 'a'..='z'
            | '0'..='9'
            | '`'
            | '-'
            | '='
            | '['
            | ']'
            | '\\'
            | ';'
            | '\''
            | ','
            | '.'
            | '/'
    )
}

/// Unshifted identity of `c`. Shifted symbols outside the unshifted ASCII
/// set are unknown without a layout, so they are refused.
fn key_codepoint(c: char, shift: bool) -> Option<u32> {
    if c.is_ascii_alphabetic() {
        return Some(u32::from(c.to_ascii_lowercase()));
    }
    if shift && !is_legacy_text_key(c) {
        return None;
    }
    if c.is_control() {
        return None;
    }
    Some(u32::from(c))
}

fn produced_text(c: char, mods: u16) -> Option<u32> {
    if mods & (2 | 4 | 8 | 16 | 32) != 0 || c.is_control() {
        return None;
    }
    if c.is_ascii_lowercase() && mods & 1 != 0 {
        return Some(u32::from(c.to_ascii_uppercase()));
    }
    if mods & 1 != 0 && !c.is_ascii_alphabetic() {
        return None;
    }
    Some(u32::from(c))
}

fn legacy_text(c: char, mods: u16) -> Vec<u8> {
    let alt = mods & 2 != 0;
    let mut bytes = if mods & 4 != 0 {
        if let Some(byte) = ctrl_byte(c) {
            vec![byte]
        } else {
            let mut ch = c;
            if mods & 1 != 0 && ch.is_ascii_lowercase() {
                ch = ch.to_ascii_uppercase();
            }
            ch.to_string().into_bytes()
        }
    } else {
        let mut ch = c;
        if mods & 1 != 0 && ch.is_ascii_lowercase() {
            ch = ch.to_ascii_uppercase();
        }
        ch.to_string().into_bytes()
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn ctrl_byte(c: char) -> Option<u8> {
    let upper = c.to_ascii_uppercase();
    match upper {
        '@' | ' ' => Some(0x00),
        'A'..='Z' => Some(upper as u8 - b'A' + 1),
        '[' => Some(0x1b),
        '\\' | '4' => Some(0x1c),
        ']' | '5' => Some(0x1d),
        '^' | '6' => Some(0x1e),
        '_' | '7' => Some(0x1f),
        _ => None,
    }
}

enum Edit {
    Enter,
    Tab,
    Backspace,
}

fn encode_edit(code: u32, kind: Edit, policy: &Policy) -> Option<Vec<u8>> {
    if policy.event == 3 && !policy.report_all {
        return None;
    }
    let extra = policy.mods & (8 | 16 | 32) != 0;
    if policy.report_all || extra {
        return Some(csi_u(code, policy, None));
    }
    Some(legacy_edit(kind, policy.mods))
}

fn legacy_edit(kind: Edit, mods: u16) -> Vec<u8> {
    let alt = mods & 2 != 0;
    let ctrl = mods & 4 != 0;
    let shift = mods & 1 != 0;
    let mut bytes = match kind {
        Edit::Enter => vec![0x0d],
        Edit::Backspace => vec![if ctrl { 0x08 } else { 0x7f }],
        Edit::Tab if shift => b"\x1b[Z".to_vec(),
        Edit::Tab => vec![0x09],
    };
    if alt {
        bytes.insert(0, 0x1b);
    }
    bytes
}

fn encode_esc(policy: &Policy) -> Option<Vec<u8>> {
    let extra = policy.mods & (8 | 16 | 32) != 0;
    if policy.disambiguate || policy.report_all || policy.event != 1 || extra {
        return Some(csi_u(27, policy, None));
    }
    Some(if policy.mods & 2 != 0 {
        vec![0x1b, 0x1b]
    } else {
        vec![0x1b]
    })
}

fn encode_functional(code: KeyCode, policy: &Policy) -> Option<Vec<u8>> {
    let form = match code {
        KeyCode::Up => Form::Nav('A'),
        KeyCode::Down => Form::Nav('B'),
        KeyCode::Right => Form::Nav('C'),
        KeyCode::Left => Form::Nav('D'),
        KeyCode::Home => Form::Nav('H'),
        KeyCode::End => Form::Nav('F'),
        KeyCode::F(1) => Form::Ss3('P'),
        KeyCode::F(2) => Form::Ss3('Q'),
        KeyCode::F(3) => Form::F3,
        KeyCode::F(4) => Form::Ss3('S'),
        KeyCode::F(5) => Form::Tilde(15),
        KeyCode::F(6) => Form::Tilde(17),
        KeyCode::F(7) => Form::Tilde(18),
        KeyCode::F(8) => Form::Tilde(19),
        KeyCode::F(9) => Form::Tilde(20),
        KeyCode::F(10) => Form::Tilde(21),
        KeyCode::F(11) => Form::Tilde(23),
        KeyCode::F(12) => Form::Tilde(24),
        KeyCode::F(n @ 13..=35) => Form::U(57376 + u32::from(n - 13)),
        KeyCode::Insert => Form::Tilde(2),
        KeyCode::Delete => Form::Tilde(3),
        KeyCode::PageUp => Form::Tilde(5),
        KeyCode::PageDown => Form::Tilde(6),
        KeyCode::CapsLock => Form::U(57358),
        KeyCode::ScrollLock => Form::U(57359),
        KeyCode::NumLock => Form::U(57360),
        KeyCode::PrintScreen => Form::U(57361),
        KeyCode::Pause => Form::U(57362),
        KeyCode::Menu => Form::U(57363),
        KeyCode::KeypadBegin => Form::Nav('E'),
        KeyCode::Media(media) => Form::U(media_id(media)),
        _ => return None,
    };
    let show_event = policy.report_events && policy.event != 1;
    let extra = policy.mods & (8 | 16 | 32 | 64 | 128) != 0;
    if !policy.disambiguate && !policy.report_all && !show_event && !extra {
        return Some(legacy_functional(&form, policy));
    }
    Some(match form {
        Form::Nav(letter) | Form::Ss3(letter) => csi_final(1, letter, policy),
        Form::F3 => csi_final(13, '~', policy),
        Form::Tilde(n) => csi_final(n, '~', policy),
        Form::U(code) => csi_u(code, policy, None),
    })
}

fn media_id(media: MediaKeyCode) -> u32 {
    match media {
        MediaKeyCode::Play => 57428,
        MediaKeyCode::Pause => 57429,
        MediaKeyCode::PlayPause => 57430,
        MediaKeyCode::Reverse => 57431,
        MediaKeyCode::Stop => 57432,
        MediaKeyCode::FastForward => 57433,
        MediaKeyCode::Rewind => 57434,
        MediaKeyCode::TrackNext => 57435,
        MediaKeyCode::TrackPrevious => 57436,
        MediaKeyCode::Record => 57437,
        MediaKeyCode::LowerVolume => 57438,
        MediaKeyCode::RaiseVolume => 57439,
        MediaKeyCode::MuteVolume => 57440,
    }
}

fn legacy_ss3(letter: char, wire: u32) -> Vec<u8> {
    if wire > 1 {
        format!("\x1b[1;{wire}{letter}").into_bytes()
    } else {
        format!("\x1bO{letter}").into_bytes()
    }
}

fn legacy_functional(form: &Form, policy: &Policy) -> Vec<u8> {
    let wire = u32::from(policy.mods & 0x3f) + 1;
    match *form {
        Form::Nav(letter) => {
            if wire > 1 {
                format!("\x1b[1;{wire}{letter}").into_bytes()
            } else if policy.app_cursor {
                format!("\x1bO{letter}").into_bytes()
            } else {
                format!("\x1b[{letter}").into_bytes()
            }
        }
        Form::Ss3(letter) => legacy_ss3(letter, wire),
        Form::F3 => legacy_ss3('R', wire),
        Form::Tilde(n) => {
            if wire > 1 {
                format!("\x1b[{n};{wire}~").into_bytes()
            } else {
                format!("\x1b[{n}~").into_bytes()
            }
        }
        Form::U(code) => csi_u(code, policy, None),
    }
}

fn csi_final(number: u16, final_byte: char, policy: &Policy) -> Vec<u8> {
    let wire = u32::from(policy.mods) + 1;
    let show_event = policy.report_events && policy.event != 1;
    if wire == 1 && !show_event {
        if number == 1 && final_byte.is_ascii_alphabetic() {
            return format!("\x1b[{final_byte}").into_bytes();
        }
        return format!("\x1b[{number}{final_byte}").into_bytes();
    }
    let event = if show_event {
        format!(":{}", policy.event)
    } else {
        String::new()
    };
    format!("\x1b[{number};{wire}{event}{final_byte}").into_bytes()
}

fn csi_u(code: u32, policy: &Policy, text: Option<u32>) -> Vec<u8> {
    let wire = u32::from(policy.mods) + 1;
    let show_event = policy.report_events && policy.event != 1;
    let mut out = format!("\x1b[{code}");
    if wire != 1 || show_event || text.is_some() {
        out.push(';');
        out.push_str(&wire.to_string());
        if show_event {
            out.push(':');
            out.push_str(&policy.event.to_string());
        }
        if let Some(text) = text {
            out.push(';');
            out.push_str(&text.to_string());
        }
    }
    out.push('u');
    out.into_bytes()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn press(code: KeyCode, modifiers: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, modifiers)
    }

    fn kind(code: KeyCode, modifiers: KeyModifiers, kind: KeyEventKind) -> KeyEvent {
        KeyEvent::new_with_kind(code, modifiers, kind)
    }

    fn modes(flags: u16) -> InputModes {
        InputModes {
            kitty_flags: flags,
            ..InputModes::default()
        }
    }

    fn flags_of(sequence: &[u8]) -> u16 {
        let mut parser = vt100::Parser::new_with_callbacks(
            2,
            8,
            0,
            crate::terminal::screen::Replies::with_osc52(false),
        );
        parser.process(sequence);
        parser.callbacks().kitty_flags(false)
    }

    #[test]
    fn requests_set_supported_flags_and_drop_the_rest() {
        assert_eq!(flags_of(b"\x1b[=1u"), DISAMBIGUATE);
        // Alternate-key reporting is not honored and is not advertised.
        assert_eq!(flags_of(b"\x1b[=5u"), DISAMBIGUATE);
        // Press/repeat/release reporting needs host release events; not advertised.
        assert_eq!(flags_of(b"\x1b[=2u"), 0);
        assert_eq!(
            flags_of(b"\x1b[=27u"),
            DISAMBIGUATE | REPORT_ALL | REPORT_TEXT
        );
        // Report-all implies disambiguate. Text without report-all is dropped.
        assert_eq!(flags_of(b"\x1b[=8u"), DISAMBIGUATE | REPORT_ALL);
        assert_eq!(flags_of(b"\x1b[=16u"), 0);
        // Mode 2 sets bits; mode 3 clears them. An unknown mode is ignored.
        let mut parser = vt100::Parser::new_with_callbacks(
            2,
            8,
            0,
            crate::terminal::screen::Replies::with_osc52(false),
        );
        parser.process(b"\x1b[=1u\x1b[=2;2u");
        assert_eq!(
            parser.callbacks().kitty_flags(false),
            DISAMBIGUATE,
            "report-events bit is not stored"
        );
        parser.process(b"\x1b[=7;4u");
        assert_eq!(
            parser.callbacks().kitty_flags(false),
            DISAMBIGUATE,
            "an unknown mode is ignored"
        );
        parser.process(b"\x1b[=1;3u");
        assert_eq!(parser.callbacks().kitty_flags(false), 0);
    }

    #[test]
    fn push_pop_and_query_round_trip() {
        let mut parser = vt100::Parser::new_with_callbacks(
            2,
            8,
            0,
            crate::terminal::screen::Replies::with_osc52(false),
        );
        parser.process(b"\x1b[>1u");
        assert_eq!(parser.callbacks().kitty_flags(false), DISAMBIGUATE);
        parser.callbacks_mut().bytes.clear();
        parser.process(b"\x1b[?u");
        assert_eq!(parser.callbacks().bytes, b"\x1b[?1u");
        parser.process(b"\x1b[<u");
        assert_eq!(parser.callbacks().kitty_flags(false), 0);
        // Emptying the stack resets. It does not restore the saved flags (1).
        parser.process(b"\x1b[=1u\x1b[>2u\x1b[<u");
        assert_eq!(parser.callbacks().kitty_flags(false), 0);
        parser.process(b"\x1b[=1u\x1b[>2u\x1b[>3u\x1b[<1u");
        assert_eq!(parser.callbacks().kitty_flags(false), 0);
    }

    #[test]
    fn alternate_screen_keeps_its_own_flags() {
        let mut parser = vt100::Parser::new_with_callbacks(
            2,
            8,
            0,
            crate::terminal::screen::Replies::with_osc52(false),
        );
        parser.process(b"\x1b[=1u\x1b[?1049h\x1b[=2u");
        assert_eq!(parser.callbacks().kitty_flags(true), 0);
        assert_eq!(parser.callbacks().kitty_flags(false), DISAMBIGUATE);
        let restore = parser.callbacks().kitty_restore(true);
        let mut adopted = vt100::Parser::new_with_callbacks(
            2,
            8,
            0,
            crate::terminal::screen::Replies::with_osc52(false),
        );
        adopted.process(b"\x1b[?1049h");
        adopted.process(&restore);
        assert_eq!(adopted.callbacks().kitty_flags(true), 0);
    }

    #[test]
    fn full_stack_evicts_the_oldest_entry() {
        let mut keyboard = KittyKeyboard::default();
        for _ in 0..(STACK_LIMIT + 4) {
            keyboard.handle(false, Some(b'>'), None, &[&[DISAMBIGUATE]]);
        }
        let mut pops = 0;
        while keyboard.flags(false) != 0 {
            keyboard.handle(false, Some(b'<'), None, &[&[1]]);
            pops += 1;
            assert!(pops <= STACK_LIMIT, "keyboard stack grew past its cap");
        }
        assert_eq!(pops, STACK_LIMIT);
    }

    #[test]
    fn disambiguate_sends_the_requested_key_and_drops_unknown_ones() {
        let d = modes(DISAMBIGUATE);
        assert_eq!(
            encode(press(KeyCode::Esc, KeyModifiers::NONE), &d),
            Some(b"\x1b[27u".to_vec())
        );
        assert_eq!(
            encode(press(KeyCode::Char('c'), KeyModifiers::CONTROL), &d),
            Some(b"\x1b[99;5u".to_vec())
        );
        assert_eq!(
            encode(press(KeyCode::Char('['), KeyModifiers::CONTROL), &d),
            Some(b"\x1b[91;5u".to_vec()),
            "ctrl+[ is not delivered as a bare escape"
        );
        assert_eq!(
            encode(press(KeyCode::Char('a'), KeyModifiers::NONE), &d),
            Some(b"a".to_vec())
        );
        assert_eq!(
            encode(press(KeyCode::Enter, KeyModifiers::SHIFT), &d),
            Some(vec![0x0d])
        );
        let mut app = d.clone();
        app.application_cursor = true;
        assert_eq!(
            encode(press(KeyCode::Left, KeyModifiers::NONE), &app),
            Some(b"\x1b[D".to_vec()),
            "disambiguate uses CSI, not SS3"
        );
        assert_eq!(
            encode(press(KeyCode::Left, KeyModifiers::CONTROL), &d),
            Some(b"\x1b[1;5D".to_vec())
        );
        assert_eq!(
            encode(
                press(KeyCode::Char('!'), KeyModifiers::SHIFT),
                &modes(REPORT_ALL)
            ),
            None,
            "the unshifted key of a shifted symbol is not guessed"
        );
        assert!(encode(press(KeyCode::F(99), KeyModifiers::NONE), &d).is_none());
        assert!(
            encode(
                press(
                    KeyCode::Modifier(ModifierKeyCode::LeftControl),
                    KeyModifiers::NONE
                ),
                &d
            )
            .is_none()
        );
    }

    #[test]
    fn report_all_and_event_types_use_the_protocol_form() {
        let all = modes(REPORT_ALL | REPORT_TEXT);
        assert_eq!(
            encode(press(KeyCode::Char('a'), KeyModifiers::NONE), &all),
            Some(b"\x1b[97;1;97u".to_vec())
        );
        assert_eq!(
            encode(press(KeyCode::Char('a'), KeyModifiers::SHIFT), &all),
            Some(b"\x1b[97;2;65u".to_vec())
        );
        assert_eq!(
            encode(press(KeyCode::Char('A'), KeyModifiers::SHIFT), &all),
            Some(b"\x1b[97;2;65u".to_vec())
        );
        assert_eq!(
            encode(press(KeyCode::Enter, KeyModifiers::NONE), &all),
            Some(b"\x1b[13u".to_vec())
        );
        assert_eq!(
            encode(
                press(KeyCode::Char('a'), KeyModifiers::SUPER),
                &modes(DISAMBIGUATE)
            ),
            Some(b"\x1b[97;9u".to_vec())
        );
        let disambiguate = modes(DISAMBIGUATE);
        assert!(
            encode(
                kind(KeyCode::Left, KeyModifiers::NONE, KeyEventKind::Release),
                &disambiguate
            )
            .is_none(),
            "release events are dropped when report-events is not advertised"
        );
        assert_eq!(
            encode(
                kind(KeyCode::Left, KeyModifiers::NONE, KeyEventKind::Repeat),
                &disambiguate
            ),
            encode(press(KeyCode::Left, KeyModifiers::NONE), &disambiguate),
            "repeat is encoded as a press when event types are unsupported"
        );
        let keypad = KeyEvent::new_with_kind_and_state(
            KeyCode::Char('0'),
            KeyModifiers::NONE,
            KeyEventKind::Press,
            KeyEventState::KEYPAD,
        );
        assert_eq!(
            encode(keypad, &modes(DISAMBIGUATE)),
            Some(b"\x1b[57399u".to_vec())
        );
        assert_eq!(
            encode(
                press(
                    KeyCode::Modifier(ModifierKeyCode::LeftSuper),
                    KeyModifiers::SUPER
                ),
                &modes(REPORT_ALL)
            ),
            Some(b"\x1b[57444;9u".to_vec())
        );
    }
}
