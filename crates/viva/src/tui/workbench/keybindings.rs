//! Herdr-default workbench keys (herdr `KeysConfig` at e35f3937).
//!
//! Prefix is `ctrl+b`; the next key (with optional shift) is the action.
//! Direct keys apply on navigation surfaces and for pane focus (`h/j/k/l`).

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

/// Whether `key` is the configured prefix (`ctrl+b`).
pub fn is_prefix(key: KeyEvent) -> bool {
    key.modifiers == KeyModifiers::CONTROL
        && matches!(key.code, KeyCode::Char('b') | KeyCode::Char('B'))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PrefixAction {
    ShowHelp,
    NewTab,
    SplitVertical,
    SplitHorizontal,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
    WorkspacePicker,
    Detach,
    NextTab,
    PreviousTab,
    SwitchTab1,
    SwitchTab2,
    SwitchTab3,
    SwitchTab4,
    SwitchTab5,
    SwitchTab6,
    SwitchTab7,
    SwitchTab8,
    SwitchTab9,
    RenameTab,
    CloseTab,
    NewWorkspace,
    RenameWorkspace,
    CloseWorkspace,
    Goto,
    ToggleSidebar,
    SwapPaneLeft,
    SwapPaneDown,
    SwapPaneUp,
    SwapPaneRight,
    CyclePaneNext,
    CyclePanePrevious,
    ClosePane,
    Zoom,
    ResizeMode,
    CopyMode,
}

/// Crossterm on Unix reports Shift+letter as `Char('T')` + SHIFT, not
/// `Char('t')` + SHIFT — normalize to lowercase + a shift flag.
fn letter_binding(key: KeyEvent) -> Option<(char, bool)> {
    let KeyCode::Char(c) = key.code else {
        return None;
    };
    if !c.is_ascii_alphabetic() {
        return None;
    }
    let lower = c.to_ascii_lowercase();
    let shift = key.modifiers.contains(KeyModifiers::SHIFT) || c.is_ascii_uppercase();
    Some((lower, shift))
}

pub fn match_prefix_follow(key: KeyEvent) -> Option<PrefixAction> {
    if key.code == KeyCode::BackTab {
        return Some(PrefixAction::CyclePanePrevious);
    }
    if key.code == KeyCode::Tab && !key.modifiers.contains(KeyModifiers::SHIFT) {
        return Some(PrefixAction::CyclePaneNext);
    }

    if let Some(action) = match key.code {
        KeyCode::Char('?') => Some(PrefixAction::ShowHelp),
        KeyCode::Char('-') => Some(PrefixAction::SplitHorizontal),
        KeyCode::Char('[') => Some(PrefixAction::CopyMode),
        KeyCode::Char('1') => Some(PrefixAction::SwitchTab1),
        KeyCode::Char('2') => Some(PrefixAction::SwitchTab2),
        KeyCode::Char('3') => Some(PrefixAction::SwitchTab3),
        KeyCode::Char('4') => Some(PrefixAction::SwitchTab4),
        KeyCode::Char('5') => Some(PrefixAction::SwitchTab5),
        KeyCode::Char('6') => Some(PrefixAction::SwitchTab6),
        KeyCode::Char('7') => Some(PrefixAction::SwitchTab7),
        KeyCode::Char('8') => Some(PrefixAction::SwitchTab8),
        KeyCode::Char('9') => Some(PrefixAction::SwitchTab9),
        _ => None,
    } {
        return Some(action);
    }

    let (ch, shift) = letter_binding(key)?;
    match (shift, ch) {
        (false, 'c') => Some(PrefixAction::NewTab),
        (false, 'v') => Some(PrefixAction::SplitVertical),
        (false, 'h') => Some(PrefixAction::FocusPaneLeft),
        (false, 'j') => Some(PrefixAction::FocusPaneDown),
        (false, 'k') => Some(PrefixAction::FocusPaneUp),
        (false, 'l') => Some(PrefixAction::FocusPaneRight),
        (false, 'w') => Some(PrefixAction::WorkspacePicker),
        (false, 'q') => Some(PrefixAction::Detach),
        (false, 'n') => Some(PrefixAction::NextTab),
        (false, 'p') => Some(PrefixAction::PreviousTab),
        (false, 'x') => Some(PrefixAction::ClosePane),
        (false, 'z') => Some(PrefixAction::Zoom),
        (false, 'r') => Some(PrefixAction::ResizeMode),
        (false, 'g') => Some(PrefixAction::Goto),
        (false, 'b') => Some(PrefixAction::ToggleSidebar),
        (true, 't') => Some(PrefixAction::RenameTab),
        (true, 'x') => Some(PrefixAction::CloseTab),
        (true, 'n') => Some(PrefixAction::NewWorkspace),
        (true, 'w') => Some(PrefixAction::RenameWorkspace),
        (true, 'd') => Some(PrefixAction::CloseWorkspace),
        (true, 'h') => Some(PrefixAction::SwapPaneLeft),
        (true, 'j') => Some(PrefixAction::SwapPaneDown),
        (true, 'k') => Some(PrefixAction::SwapPaneUp),
        (true, 'l') => Some(PrefixAction::SwapPaneRight),
        _ => None,
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DirectPaneNav {
    Left,
    Down,
    Up,
    Right,
}

pub fn match_direct_pane_nav(key: KeyEvent) -> Option<DirectPaneNav> {
    if key.modifiers != KeyModifiers::empty() {
        return None;
    }
    match key.code {
        KeyCode::Char('h') => Some(DirectPaneNav::Left),
        KeyCode::Char('j') => Some(DirectPaneNav::Down),
        KeyCode::Char('k') => Some(DirectPaneNav::Up),
        KeyCode::Char('l') => Some(DirectPaneNav::Right),
        _ => None,
    }
}

pub fn help_lines() -> &'static [&'static str] {
    &[
        "ctrl+b prefix · ? help · q detach",
        "c new tab · v | right · - down split",
        "h/j/k/l focus pane · tab cycle pane",
        "n/p next/prev tab · 1-9 tab · shift+t rename · shift+x close tab",
        "w workspace picker · shift+n new · shift+w rename · shift+d close · g goto · b sidebar",
        "x close pane · z zoom · r resize · [ copy · shift+h/j/k/l swap",
    ]
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

    #[test]
    fn shift_letter_bindings_match_real_terminal_events() {
        let shift = KeyModifiers::SHIFT;
        assert_eq!(
            match_prefix_follow(KeyEvent::new(KeyCode::Char('T'), shift)),
            Some(PrefixAction::RenameTab)
        );
        assert_eq!(
            match_prefix_follow(KeyEvent::new(KeyCode::Char('X'), shift)),
            Some(PrefixAction::CloseTab)
        );
        assert_eq!(
            match_prefix_follow(KeyEvent::new(KeyCode::Char('N'), shift)),
            Some(PrefixAction::NewWorkspace)
        );
        assert_eq!(
            match_prefix_follow(KeyEvent::new(KeyCode::Char('H'), shift)),
            Some(PrefixAction::SwapPaneLeft)
        );
    }

    #[test]
    fn shift_tab_is_back_tab_on_real_terminals() {
        assert_eq!(
            match_prefix_follow(KeyEvent::new(KeyCode::BackTab, KeyModifiers::empty())),
            Some(PrefixAction::CyclePanePrevious)
        );
    }
}
