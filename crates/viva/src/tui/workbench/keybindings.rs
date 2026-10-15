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

pub fn match_prefix_follow(key: KeyEvent) -> Option<PrefixAction> {
    let shift = key.modifiers.contains(KeyModifiers::SHIFT);
    match (shift, key.code) {
        (false, KeyCode::Char('?')) => Some(PrefixAction::ShowHelp),
        (false, KeyCode::Char('c')) => Some(PrefixAction::NewTab),
        (false, KeyCode::Char('v')) => Some(PrefixAction::SplitVertical),
        (false, KeyCode::Char('-')) => Some(PrefixAction::SplitHorizontal),
        (false, KeyCode::Char('h')) => Some(PrefixAction::FocusPaneLeft),
        (false, KeyCode::Char('j')) => Some(PrefixAction::FocusPaneDown),
        (false, KeyCode::Char('k')) => Some(PrefixAction::FocusPaneUp),
        (false, KeyCode::Char('l')) => Some(PrefixAction::FocusPaneRight),
        (false, KeyCode::Char('w')) => Some(PrefixAction::WorkspacePicker),
        (false, KeyCode::Char('q')) => Some(PrefixAction::Detach),
        (false, KeyCode::Char('n')) => Some(PrefixAction::NextTab),
        (false, KeyCode::Char('p')) => Some(PrefixAction::PreviousTab),
        (false, KeyCode::Char('1')) => Some(PrefixAction::SwitchTab1),
        (false, KeyCode::Char('2')) => Some(PrefixAction::SwitchTab2),
        (false, KeyCode::Char('3')) => Some(PrefixAction::SwitchTab3),
        (false, KeyCode::Char('4')) => Some(PrefixAction::SwitchTab4),
        (false, KeyCode::Char('5')) => Some(PrefixAction::SwitchTab5),
        (false, KeyCode::Char('6')) => Some(PrefixAction::SwitchTab6),
        (false, KeyCode::Char('7')) => Some(PrefixAction::SwitchTab7),
        (false, KeyCode::Char('8')) => Some(PrefixAction::SwitchTab8),
        (false, KeyCode::Char('9')) => Some(PrefixAction::SwitchTab9),
        (false, KeyCode::Char('x')) => Some(PrefixAction::ClosePane),
        (false, KeyCode::Char('z')) => Some(PrefixAction::Zoom),
        (false, KeyCode::Char('r')) => Some(PrefixAction::ResizeMode),
        (false, KeyCode::Char('g')) => Some(PrefixAction::Goto),
        (false, KeyCode::Char('b')) => Some(PrefixAction::ToggleSidebar),
        (false, KeyCode::Tab) => Some(PrefixAction::CyclePaneNext),
        (false, KeyCode::Char('[')) => Some(PrefixAction::CopyMode),
        (true, KeyCode::Char('t')) => Some(PrefixAction::RenameTab),
        (true, KeyCode::Char('x')) => Some(PrefixAction::CloseTab),
        (true, KeyCode::Char('n')) => Some(PrefixAction::NewWorkspace),
        (true, KeyCode::Char('w')) => Some(PrefixAction::RenameWorkspace),
        (true, KeyCode::Char('d')) => Some(PrefixAction::CloseWorkspace),
        (true, KeyCode::Char('h')) => Some(PrefixAction::SwapPaneLeft),
        (true, KeyCode::Char('j')) => Some(PrefixAction::SwapPaneDown),
        (true, KeyCode::Char('k')) => Some(PrefixAction::SwapPaneUp),
        (true, KeyCode::Char('l')) => Some(PrefixAction::SwapPaneRight),
        (true, KeyCode::Tab) => Some(PrefixAction::CyclePanePrevious),
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
