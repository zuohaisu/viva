//! Configurable workbench keys.
//!
//! Names and defaults follow herdr's `keys.*` reference. Bindings live in
//! `VIVA_HOME/config.toml`. An illegal value falls back to the default and
//! is reported. A missing action stays at its default; an empty string or
//! `false` leaves it unbound. Viva-only commands are a separate table and
//! cannot take a chord that a built-in binding already owns.

use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};

const SHIFT: u8 = 1;
const CTRL: u8 = 2;
const ALT: u8 = 4;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
enum StrokeKind {
    Char(char),
    Tab,
    Up,
    Down,
    Left,
    Right,
    Enter,
    Esc,
    Backspace,
    PageUp,
    PageDown,
    Home,
    End,
    Space,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
struct Stroke {
    kind: StrokeKind,
    mods: u8,
}

impl Stroke {
    fn plain(kind: StrokeKind) -> Self {
        Self { kind, mods: 0 }
    }

    fn from_event(key: KeyEvent) -> Option<Self> {
        let mut mods = 0u8;
        if key.modifiers.contains(KeyModifiers::SHIFT) {
            mods |= SHIFT;
        }
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            mods |= CTRL;
        }
        if key.modifiers.contains(KeyModifiers::ALT) {
            mods |= ALT;
        }
        let kind = match key.code {
            KeyCode::Char(c) if c.is_ascii_alphabetic() => {
                // Ctrl+letter is reported as either case without meaning Shift.
                // A shifted letter without Ctrl arrives uppercase.
                if !key.modifiers.contains(KeyModifiers::CONTROL)
                    && (key.modifiers.contains(KeyModifiers::SHIFT) || c.is_ascii_uppercase())
                {
                    mods |= SHIFT;
                }
                StrokeKind::Char(c.to_ascii_lowercase())
            }
            KeyCode::Char(' ') => StrokeKind::Space,
            KeyCode::Char(c) => StrokeKind::Char(c),
            KeyCode::Tab => StrokeKind::Tab,
            KeyCode::BackTab => {
                mods |= SHIFT;
                StrokeKind::Tab
            }
            KeyCode::Up => StrokeKind::Up,
            KeyCode::Down => StrokeKind::Down,
            KeyCode::Left => StrokeKind::Left,
            KeyCode::Right => StrokeKind::Right,
            KeyCode::Enter => StrokeKind::Enter,
            KeyCode::Esc => StrokeKind::Esc,
            KeyCode::Backspace => StrokeKind::Backspace,
            KeyCode::PageUp => StrokeKind::PageUp,
            KeyCode::PageDown => StrokeKind::PageDown,
            KeyCode::Home => StrokeKind::Home,
            KeyCode::End => StrokeKind::End,
            _ => return None,
        };
        Some(Self { kind, mods })
    }

    fn label(self) -> String {
        let mut parts = Vec::new();
        if self.mods & CTRL != 0 {
            parts.push("ctrl");
        }
        if self.mods & ALT != 0 {
            parts.push("alt");
        }
        if self.mods & SHIFT != 0 {
            parts.push("shift");
        }
        parts.push(match self.kind {
            StrokeKind::Char(c) => return join_label(&parts, &c.to_string()),
            StrokeKind::Tab => "tab",
            StrokeKind::Up => "up",
            StrokeKind::Down => "down",
            StrokeKind::Left => "left",
            StrokeKind::Right => "right",
            StrokeKind::Enter => "enter",
            StrokeKind::Esc => "esc",
            StrokeKind::Backspace => "backspace",
            StrokeKind::PageUp => "pageup",
            StrokeKind::PageDown => "pagedown",
            StrokeKind::Home => "home",
            StrokeKind::End => "end",
            StrokeKind::Space => "space",
        });
        parts.join("+")
    }
}

/// `?` arrives as Shift+`/`, so the shift bit is not part of the binding.
fn same_stroke(expected: Stroke, event: Stroke) -> bool {
    if expected == event {
        return true;
    }
    if let StrokeKind::Char(c) = event.kind {
        if !c.is_ascii_alphabetic() && event.mods & SHIFT != 0 {
            let mut bare = event;
            bare.mods &= !SHIFT;
            return expected == bare;
        }
    }
    false
}

fn join_label(mods: &[&str], key: &str) -> String {
    let mut parts: Vec<&str> = mods.to_vec();
    parts.push(key);
    parts.join("+")
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct Chord {
    prefix: bool,
    stroke: Stroke,
}

impl Chord {
    fn label(self) -> String {
        let stroke = self.stroke.label();
        if self.prefix {
            format!("prefix+{stroke}")
        } else {
            stroke
        }
    }
}

/// One workbench action. Variants with a digit cover the `1..9` bindings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Help,
    Settings,
    NewWorkspace,
    NewWorktree,
    OpenWorktree,
    RemoveWorktree,
    RenameWorkspace,
    CloseWorkspace,
    WorkspacePicker,
    Goto,
    NavigateWorkspaceUp,
    NavigateWorkspaceDown,
    NavigatePaneLeft,
    NavigatePaneDown,
    NavigatePaneUp,
    NavigatePaneRight,
    Detach,
    ReloadConfig,
    OpenNotificationTarget,
    PreviousWorkspace,
    NextWorkspace,
    PreviousAgent,
    NextAgent,
    FocusAgent(u8),
    NewTab,
    RenameTab,
    PreviousTab,
    NextTab,
    MoveTabPrevious,
    MoveTabNext,
    SwitchTab(u8),
    SwitchWorkspace(u8),
    CloseTab,
    RenamePane,
    EditScrollback,
    ClearPane,
    CopyMode,
    FocusPaneLeft,
    FocusPaneDown,
    FocusPaneUp,
    FocusPaneRight,
    SwapPaneLeft,
    SwapPaneDown,
    SwapPaneUp,
    SwapPaneRight,
    CyclePaneNext,
    CyclePanePrevious,
    LastPane,
    SplitVertical,
    SplitHorizontal,
    ClosePane,
    Zoom,
    ResizeMode,
    ResizePaneLeft,
    ResizePaneDown,
    ResizePaneUp,
    ResizePaneRight,
    ToggleSidebar,
    Command(String),
}

impl Action {
    fn navigate_only(&self) -> bool {
        matches!(
            self,
            Self::NavigateWorkspaceUp
                | Self::NavigateWorkspaceDown
                | Self::NavigatePaneLeft
                | Self::NavigatePaneDown
                | Self::NavigatePaneUp
                | Self::NavigatePaneRight
        )
    }

    pub fn name(&self) -> String {
        match self {
            Self::Help => "help".into(),
            Self::Settings => "settings".into(),
            Self::NewWorkspace => "new_workspace".into(),
            Self::NewWorktree => "new_worktree".into(),
            Self::OpenWorktree => "open_worktree".into(),
            Self::RemoveWorktree => "remove_worktree".into(),
            Self::RenameWorkspace => "rename_workspace".into(),
            Self::CloseWorkspace => "close_workspace".into(),
            Self::WorkspacePicker => "workspace_picker".into(),
            Self::Goto => "goto".into(),
            Self::NavigateWorkspaceUp => "navigate_workspace_up".into(),
            Self::NavigateWorkspaceDown => "navigate_workspace_down".into(),
            Self::NavigatePaneLeft => "navigate_pane_left".into(),
            Self::NavigatePaneDown => "navigate_pane_down".into(),
            Self::NavigatePaneUp => "navigate_pane_up".into(),
            Self::NavigatePaneRight => "navigate_pane_right".into(),
            Self::Detach => "detach".into(),
            Self::ReloadConfig => "reload_config".into(),
            Self::OpenNotificationTarget => "open_notification_target".into(),
            Self::PreviousWorkspace => "previous_workspace".into(),
            Self::NextWorkspace => "next_workspace".into(),
            Self::PreviousAgent => "previous_agent".into(),
            Self::NextAgent => "next_agent".into(),
            Self::FocusAgent(n) => format!("focus_agent {n}"),
            Self::NewTab => "new_tab".into(),
            Self::RenameTab => "rename_tab".into(),
            Self::PreviousTab => "previous_tab".into(),
            Self::NextTab => "next_tab".into(),
            Self::MoveTabPrevious => "move_tab_previous".into(),
            Self::MoveTabNext => "move_tab_next".into(),
            Self::SwitchTab(n) => format!("switch_tab {n}"),
            Self::SwitchWorkspace(n) => format!("switch_workspace {n}"),
            Self::CloseTab => "close_tab".into(),
            Self::RenamePane => "rename_pane".into(),
            Self::EditScrollback => "edit_scrollback".into(),
            Self::ClearPane => "clear_pane".into(),
            Self::CopyMode => "copy_mode".into(),
            Self::FocusPaneLeft => "focus_pane_left".into(),
            Self::FocusPaneDown => "focus_pane_down".into(),
            Self::FocusPaneUp => "focus_pane_up".into(),
            Self::FocusPaneRight => "focus_pane_right".into(),
            Self::SwapPaneLeft => "swap_pane_left".into(),
            Self::SwapPaneDown => "swap_pane_down".into(),
            Self::SwapPaneUp => "swap_pane_up".into(),
            Self::SwapPaneRight => "swap_pane_right".into(),
            Self::CyclePaneNext => "cycle_pane_next".into(),
            Self::CyclePanePrevious => "cycle_pane_previous".into(),
            Self::LastPane => "last_pane".into(),
            Self::SplitVertical => "split_vertical".into(),
            Self::SplitHorizontal => "split_horizontal".into(),
            Self::ClosePane => "close_pane".into(),
            Self::Zoom => "zoom".into(),
            Self::ResizeMode => "resize_mode".into(),
            Self::ResizePaneLeft => "resize_pane_left".into(),
            Self::ResizePaneDown => "resize_pane_down".into(),
            Self::ResizePaneUp => "resize_pane_up".into(),
            Self::ResizePaneRight => "resize_pane_right".into(),
            Self::ToggleSidebar => "toggle_sidebar".into(),
            Self::Command(id) => id.clone(),
        }
    }

    fn from_field(name: &str) -> Option<Self> {
        Some(match name {
            "help" => Self::Help,
            "settings" => Self::Settings,
            "new_workspace" => Self::NewWorkspace,
            "new_worktree" => Self::NewWorktree,
            "open_worktree" => Self::OpenWorktree,
            "remove_worktree" => Self::RemoveWorktree,
            "rename_workspace" => Self::RenameWorkspace,
            "close_workspace" => Self::CloseWorkspace,
            "workspace_picker" => Self::WorkspacePicker,
            "goto" => Self::Goto,
            "navigate_workspace_up" => Self::NavigateWorkspaceUp,
            "navigate_workspace_down" => Self::NavigateWorkspaceDown,
            "navigate_pane_left" => Self::NavigatePaneLeft,
            "navigate_pane_down" => Self::NavigatePaneDown,
            "navigate_pane_up" => Self::NavigatePaneUp,
            "navigate_pane_right" => Self::NavigatePaneRight,
            "detach" => Self::Detach,
            "reload_config" => Self::ReloadConfig,
            "open_notification_target" => Self::OpenNotificationTarget,
            "previous_workspace" => Self::PreviousWorkspace,
            "next_workspace" => Self::NextWorkspace,
            "previous_agent" => Self::PreviousAgent,
            "next_agent" => Self::NextAgent,
            "new_tab" => Self::NewTab,
            "rename_tab" => Self::RenameTab,
            "previous_tab" => Self::PreviousTab,
            "next_tab" => Self::NextTab,
            "move_tab_previous" => Self::MoveTabPrevious,
            "move_tab_next" => Self::MoveTabNext,
            "close_tab" => Self::CloseTab,
            "rename_pane" => Self::RenamePane,
            "edit_scrollback" => Self::EditScrollback,
            "clear_pane" => Self::ClearPane,
            "copy_mode" => Self::CopyMode,
            "focus_pane_left" => Self::FocusPaneLeft,
            "focus_pane_down" => Self::FocusPaneDown,
            "focus_pane_up" => Self::FocusPaneUp,
            "focus_pane_right" => Self::FocusPaneRight,
            "swap_pane_left" => Self::SwapPaneLeft,
            "swap_pane_down" => Self::SwapPaneDown,
            "swap_pane_up" => Self::SwapPaneUp,
            "swap_pane_right" => Self::SwapPaneRight,
            "cycle_pane_next" => Self::CyclePaneNext,
            "cycle_pane_previous" => Self::CyclePanePrevious,
            "last_pane" => Self::LastPane,
            "split_vertical" => Self::SplitVertical,
            "split_horizontal" => Self::SplitHorizontal,
            "close_pane" => Self::ClosePane,
            "zoom" => Self::Zoom,
            "resize_mode" => Self::ResizeMode,
            "resize_pane_left" => Self::ResizePaneLeft,
            "resize_pane_down" => Self::ResizePaneDown,
            "resize_pane_up" => Self::ResizePaneUp,
            "resize_pane_right" => Self::ResizePaneRight,
            "toggle_sidebar" => Self::ToggleSidebar,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CommandRow {
    pub id: String,
    pub description: String,
    pub binding: String,
}

#[derive(Debug, Clone)]
struct RawCommand {
    key: String,
    command: String,
    description: String,
}

/// The active key map, including load warnings.
#[derive(Debug, Clone)]
pub struct KeyMap {
    prefixes: Vec<Stroke>,
    bindings: Vec<(Action, Chord)>,
    warnings: Vec<String>,
    onboarding_done: bool,
    /// Herdr's `ui.copy_on_select`. True copies a drag or double-clicked
    /// word on release. False leaves the highlight until Ctrl+C.
    copy_on_select: bool,
    /// Builtin viva commands that are not `keys.*` actions.
    commands: Vec<(String, String, Option<Chord>)>,
}

impl KeyMap {
    pub fn builtin() -> Self {
        let mut map = Self {
            prefixes: vec![parse_stroke("ctrl+b").expect("default prefix")],
            bindings: Vec::new(),
            warnings: Vec::new(),
            onboarding_done: false,
            copy_on_select: true,
            commands: builtin_commands(),
        };
        for (field, spec) in DEFAULTS {
            if spec.is_empty() {
                continue;
            }
            let chords = parse_spec(spec).expect(spec);
            map.assign_field(field, chords);
        }
        map
    }

    pub fn warnings(&self) -> &[String] {
        &self.warnings
    }

    pub fn onboarding_done(&self) -> bool {
        self.onboarding_done
    }

    /// `copy_on_select` from config. Missing or illegal values stay true.
    pub fn copy_on_select(&self) -> bool {
        self.copy_on_select
    }

    pub fn prefix_label(&self) -> String {
        self.prefixes
            .first()
            .map(|stroke| stroke.label())
            .unwrap_or_else(|| "ctrl+b".into())
    }

    pub fn is_prefix(&self, key: KeyEvent) -> bool {
        let Some(stroke) = Stroke::from_event(key) else {
            return false;
        };
        self.prefixes
            .iter()
            .any(|prefix| same_stroke(*prefix, stroke))
    }

    /// Resolve one key. Navigate-only bindings win while navigate mode is open.
    /// Prefix bindings match only while a prefix is pending.
    pub fn resolve(&self, prefix_pending: bool, navigate: bool, key: KeyEvent) -> Option<Action> {
        if self.is_prefix(key) {
            return None;
        }
        let stroke = Stroke::from_event(key)?;
        if prefix_pending {
            return self
                .bindings
                .iter()
                .find(|(action, chord)| {
                    chord.prefix && same_stroke(chord.stroke, stroke) && !action.navigate_only()
                })
                .map(|(action, _)| action.clone());
        }
        if navigate {
            if let Some(action) = self.bindings.iter().find(|(action, chord)| {
                !chord.prefix && same_stroke(chord.stroke, stroke) && action.navigate_only()
            }) {
                return Some(action.0.clone());
            }
            if stroke == Stroke::plain(StrokeKind::Left) {
                return Some(Action::NavigatePaneLeft);
            }
            if stroke == Stroke::plain(StrokeKind::Right) {
                return Some(Action::NavigatePaneRight);
            }
        }
        self.bindings
            .iter()
            .find(|(action, chord)| {
                !chord.prefix && same_stroke(chord.stroke, stroke) && !action.navigate_only()
            })
            .map(|(action, _)| action.clone())
    }

    pub fn binding_label(&self, action: &Action) -> String {
        let labels: Vec<String> = self
            .bindings
            .iter()
            .filter(|(candidate, _)| candidate == action)
            .map(|(_, chord)| chord.label())
            .collect();
        if labels.is_empty() {
            "unbound".into()
        } else if matches!(action, Action::SwitchTab(_))
            && labels.len() == 1
            && self.switch_tab_run().is_some()
        {
            self.switch_tab_run().unwrap()
        } else {
            labels.join(", ")
        }
    }

    fn switch_tab_run(&self) -> Option<String> {
        let tabs: Vec<&Chord> = (1..=9)
            .filter_map(|n| {
                self.bindings
                    .iter()
                    .find(|(action, _)| *action == Action::SwitchTab(n))
                    .map(|(_, chord)| chord)
            })
            .collect();
        if tabs.len() != 9 || !tabs.iter().all(|chord| chord.prefix) {
            return None;
        }
        Some("prefix+1..9".into())
    }

    /// Help rows: every `keys.*` action, then viva commands. No launch-agent row.
    pub fn help_rows(&self) -> Vec<(String, String)> {
        let mut rows = Vec::new();
        let mut seen_switch = false;
        for action in catalog() {
            if matches!(action, Action::SwitchTab(n) if n != 1) {
                continue;
            }
            if matches!(action, Action::SwitchTab(1)) {
                if seen_switch {
                    continue;
                }
                seen_switch = true;
                rows.push((
                    "switch_tab".into(),
                    self.switch_tab_run()
                        .unwrap_or_else(|| self.binding_label(&Action::SwitchTab(1))),
                ));
                continue;
            }
            if matches!(action, Action::SwitchWorkspace(n) if n != 1) {
                continue;
            }
            if matches!(action, Action::FocusAgent(n) if n != 1) {
                continue;
            }
            let name = match &action {
                Action::SwitchWorkspace(_) => "switch_workspace".into(),
                Action::FocusAgent(_) => "focus_agent".into(),
                other => other.name(),
            };
            rows.push((name, self.binding_label(&action)));
        }
        for (id, description, chord) in &self.commands {
            rows.push((
                format!("{id} · {description}"),
                chord.map(Chord::label).unwrap_or_else(|| "unbound".into()),
            ));
        }
        rows
    }

    pub fn help_text(&self, filter: &str) -> String {
        let filter = filter.trim().to_ascii_lowercase();
        self.help_rows()
            .into_iter()
            .filter(|(name, binding)| {
                filter.is_empty()
                    || name.to_ascii_lowercase().contains(&filter)
                    || binding.to_ascii_lowercase().contains(&filter)
            })
            .map(|(name, binding)| format!("{name}  {binding}"))
            .collect::<Vec<_>>()
            .join("\n")
    }

    pub fn command_rows(&self) -> Vec<CommandRow> {
        self.commands
            .iter()
            .map(|(id, description, chord)| CommandRow {
                id: id.clone(),
                description: description.clone(),
                binding: chord.map(Chord::label).unwrap_or_else(|| "unbound".into()),
            })
            .collect()
    }

    /// Load `config.toml` text. Illegal values keep the built-in default.
    pub fn load_str(text: &str) -> Self {
        let mut map = Self::builtin();
        let parsed = parse_config(text);
        map.onboarding_done = parsed.onboarding_done;
        map.copy_on_select = parsed.copy_on_select;
        map.warnings.extend(parsed.warnings);
        if let Some(prefix) = parsed.prefix {
            match parse_prefix(&prefix) {
                Ok(strokes) if !strokes.is_empty() => map.prefixes = strokes,
                Ok(_) => map
                    .warnings
                    .push("keys.prefix is empty; using ctrl+b".into()),
                Err(err) => map
                    .warnings
                    .push(format!("keys.prefix: {err}; using ctrl+b")),
            }
        }
        for (field, raw) in parsed.keys {
            if field == "prefix" {
                continue;
            }
            if indexed_field(&field).is_none() && Action::from_field(&field).is_none() {
                map.warnings.push(format!("unknown keys.{field}; ignored"));
                continue;
            }
            match raw {
                RawValue::Unbind => map.clear_field(&field),
                RawValue::Invalid(err) => {
                    map.warnings
                        .push(format!("keys.{field}: {err}; keeping the default"));
                }
                RawValue::Specs(specs) => {
                    let mut chords = Vec::new();
                    let mut bad = None;
                    for spec in specs {
                        match parse_spec(&spec) {
                            Ok(parsed) => chords.extend(parsed),
                            Err(err) => {
                                bad = Some(err);
                                break;
                            }
                        }
                    }
                    if let Some(err) = bad {
                        map.warnings
                            .push(format!("keys.{field}: {err}; keeping the default"));
                    } else {
                        map.clear_field(&field);
                        map.assign_chords(&field, chords);
                    }
                }
            }
        }
        for command in parsed.commands {
            map.bind_command(command);
        }
        map
    }

    fn clear_field(&mut self, field: &str) {
        let owned = field_actions(field);
        self.bindings
            .retain(|(action, _)| !owned.iter().any(|candidate| candidate == action));
    }

    fn assign_field(&mut self, field: &str, chords: Vec<Chord>) {
        self.assign_chords(field, chords);
    }

    fn assign_chords(&mut self, field: &str, chords: Vec<Chord>) {
        let actions = field_actions(field);
        if matches!(field, "switch_tab" | "switch_workspace" | "focus_agent") {
            for (index, chord) in chords.into_iter().enumerate() {
                if index >= 9 {
                    break;
                }
                let action = match field {
                    "switch_tab" => Action::SwitchTab((index as u8) + 1),
                    "switch_workspace" => Action::SwitchWorkspace((index as u8) + 1),
                    _ => Action::FocusAgent((index as u8) + 1),
                };
                self.bindings.push((action, chord));
            }
            return;
        }
        let Some(action) = actions.into_iter().next() else {
            return;
        };
        for chord in chords {
            self.bindings.push((action.clone(), chord));
        }
    }

    fn bind_command(&mut self, command: RawCommand) {
        if command.command.trim().is_empty() {
            self.warnings
                .push("keys.command missing command; skipped".into());
            return;
        }
        if reserved_name(&command.command) {
            self.warnings.push(format!(
                "keys.command `{}` cannot override a keys.* action",
                command.command
            ));
            return;
        }
        let chord = if command.key.trim().is_empty() {
            None
        } else {
            match parse_spec(&command.key) {
                Ok(chords) => chords.into_iter().next(),
                Err(err) => {
                    self.warnings.push(format!(
                        "keys.command {}: {err}; left unbound",
                        command.command
                    ));
                    None
                }
            }
        };
        if let Some(chord) = chord {
            if self.bindings.iter().any(|(_, existing)| {
                existing.prefix == chord.prefix && existing.stroke == chord.stroke
            }) {
                self.warnings.push(format!(
                    "keys.command `{}` cannot override {}",
                    command.command,
                    chord.label()
                ));
            } else {
                let id = command.command.clone();
                self.bindings.push((Action::Command(id.clone()), chord));
                if let Some(row) = self
                    .commands
                    .iter_mut()
                    .find(|(existing, _, _)| existing == &id)
                {
                    row.2 = Some(chord);
                    if !command.description.is_empty() {
                        row.1 = command.description;
                    }
                    return;
                }
                self.commands.push((
                    id,
                    if command.description.is_empty() {
                        "custom command".into()
                    } else {
                        command.description
                    },
                    Some(chord),
                ));
                return;
            }
        }
        if self
            .commands
            .iter()
            .any(|(id, _, _)| id == &command.command)
        {
            return;
        }
        self.commands.push((
            command.command,
            if command.description.is_empty() {
                "custom command".into()
            } else {
                command.description
            },
            None,
        ));
    }
}

fn reserved_name(command: &str) -> bool {
    Action::from_field(command).is_some()
        || matches!(
            command,
            "prefix" | "switch_tab" | "switch_workspace" | "focus_agent"
        )
}

fn indexed_field(name: &str) -> Option<&'static str> {
    match name {
        "switch_tab" => Some("switch_tab"),
        "switch_workspace" => Some("switch_workspace"),
        "focus_agent" => Some("focus_agent"),
        _ => None,
    }
}

fn field_actions(field: &str) -> Vec<Action> {
    match field {
        "switch_tab" => (1..=9).map(Action::SwitchTab).collect(),
        "switch_workspace" => (1..=9).map(Action::SwitchWorkspace).collect(),
        "focus_agent" => (1..=9).map(Action::FocusAgent).collect(),
        other => Action::from_field(other).into_iter().collect(),
    }
}

fn builtin_commands() -> Vec<(String, String, Option<Chord>)> {
    [
        ("viva.diff", "show the worktree diff"),
        ("viva.files", "browse worktree files"),
        ("viva.handoff", "hand a worktree to another agent"),
        ("viva.auto-pull", "toggle auto-pull"),
        ("viva.cleanup", "clean up a merged worktree"),
    ]
    .into_iter()
    .map(|(id, description)| (id.into(), description.into(), None))
    .collect()
}

fn catalog() -> Vec<Action> {
    let mut actions = Vec::new();
    for (field, _) in DEFAULTS {
        if matches!(*field, "switch_tab" | "switch_workspace" | "focus_agent") {
            continue;
        }
        if let Some(action) = Action::from_field(field) {
            actions.push(action);
        }
    }
    actions.push(Action::SwitchTab(1));
    for n in 2..=9 {
        actions.push(Action::SwitchTab(n));
    }
    actions.push(Action::SwitchWorkspace(1));
    actions.push(Action::FocusAgent(1));
    actions
}

const DEFAULTS: &[(&str, &str)] = &[
    ("help", "prefix+?"),
    ("settings", "prefix+s"),
    ("new_workspace", "prefix+shift+n"),
    ("new_worktree", "prefix+shift+g"),
    ("open_worktree", ""),
    ("remove_worktree", ""),
    ("rename_workspace", "prefix+shift+w"),
    ("close_workspace", "prefix+shift+d"),
    ("workspace_picker", "prefix+w"),
    ("goto", "prefix+g"),
    ("navigate_workspace_up", "up"),
    ("navigate_workspace_down", "down"),
    ("navigate_pane_left", "h"),
    ("navigate_pane_down", "j"),
    ("navigate_pane_up", "k"),
    ("navigate_pane_right", "l"),
    ("detach", "prefix+q"),
    ("reload_config", "prefix+shift+r"),
    ("open_notification_target", "prefix+o"),
    ("previous_workspace", ""),
    ("next_workspace", ""),
    ("previous_agent", ""),
    ("next_agent", ""),
    ("focus_agent", ""),
    ("new_tab", "prefix+c"),
    ("rename_tab", "prefix+shift+t"),
    ("previous_tab", "prefix+p"),
    ("next_tab", "prefix+n"),
    ("move_tab_previous", ""),
    ("move_tab_next", ""),
    ("switch_tab", "prefix+1..9"),
    ("switch_workspace", ""),
    ("close_tab", "prefix+shift+x"),
    ("rename_pane", "prefix+shift+p"),
    ("edit_scrollback", "prefix+e"),
    ("clear_pane", ""),
    ("copy_mode", "prefix+["),
    ("focus_pane_left", "prefix+h"),
    ("focus_pane_down", "prefix+j"),
    ("focus_pane_up", "prefix+k"),
    ("focus_pane_right", "prefix+l"),
    ("swap_pane_left", "prefix+shift+h"),
    ("swap_pane_down", "prefix+shift+j"),
    ("swap_pane_up", "prefix+shift+k"),
    ("swap_pane_right", "prefix+shift+l"),
    ("cycle_pane_next", "prefix+tab"),
    ("cycle_pane_previous", "prefix+shift+tab"),
    ("last_pane", ""),
    ("split_vertical", "prefix+v"),
    ("split_horizontal", "prefix+minus"),
    ("close_pane", "prefix+x"),
    ("zoom", "prefix+z"),
    ("resize_mode", "prefix+r"),
    ("resize_pane_left", ""),
    ("resize_pane_down", ""),
    ("resize_pane_up", ""),
    ("resize_pane_right", ""),
    ("toggle_sidebar", "prefix+b"),
];

fn parse_prefix(spec: &str) -> Result<Vec<Stroke>, String> {
    let specs = split_list(spec)?;
    let mut strokes = Vec::new();
    for spec in specs {
        let mut chords = parse_spec(&spec)?;
        if chords.len() != 1 || chords[0].prefix {
            return Err(format!("`{spec}` is not a prefix key"));
        }
        strokes.push(chords.remove(0).stroke);
    }
    Ok(strokes)
}

fn parse_spec(spec: &str) -> Result<Vec<Chord>, String> {
    let spec = spec.trim();
    if spec.is_empty() {
        return Ok(Vec::new());
    }
    if let Some(head) = spec.strip_suffix("1..9") {
        let head = head.trim_end_matches('+').trim();
        let mut chords = Vec::new();
        for digit in '1'..='9' {
            let text = if head.is_empty() {
                digit.to_string()
            } else {
                format!("{head}+{digit}")
            };
            chords.push(parse_one(&text)?);
        }
        return Ok(chords);
    }
    Ok(vec![parse_one(spec)?])
}

fn parse_one(spec: &str) -> Result<Chord, String> {
    let tokens: Vec<&str> = spec
        .split('+')
        .map(str::trim)
        .filter(|t| !t.is_empty())
        .collect();
    if tokens.is_empty() {
        return Err("empty binding".into());
    }
    let mut prefix = false;
    let mut mods = 0u8;
    let mut key: Option<&str> = None;
    for token in tokens {
        match token.to_ascii_lowercase().as_str() {
            "prefix" => prefix = true,
            "ctrl" | "control" => mods |= CTRL,
            "shift" => mods |= SHIFT,
            "alt" | "opt" | "option" | "meta" => mods |= ALT,
            "cmd" | "super" => {
                return Err(format!("`{token}` is not a supported modifier"));
            }
            _ => {
                if key.is_some() {
                    return Err(format!("`{spec}` has more than one key"));
                }
                key = Some(token);
            }
        }
    }
    let Some(key) = key else {
        return Err(format!("`{spec}` has no key"));
    };
    let kind = parse_key(key)?;
    Ok(Chord {
        prefix,
        stroke: Stroke { kind, mods },
    })
}

fn parse_key(token: &str) -> Result<StrokeKind, String> {
    Ok(match token.to_ascii_lowercase().as_str() {
        "tab" => StrokeKind::Tab,
        "enter" | "return" => StrokeKind::Enter,
        "esc" | "escape" => StrokeKind::Esc,
        "up" => StrokeKind::Up,
        "down" => StrokeKind::Down,
        "left" => StrokeKind::Left,
        "right" => StrokeKind::Right,
        "space" => StrokeKind::Space,
        "minus" | "dash" | "-" => StrokeKind::Char('-'),
        "?" | "question" => StrokeKind::Char('?'),
        "[" | "bracketleft" => StrokeKind::Char('['),
        "backspace" => StrokeKind::Backspace,
        "pageup" => StrokeKind::PageUp,
        "pagedown" => StrokeKind::PageDown,
        "home" => StrokeKind::Home,
        "end" => StrokeKind::End,
        other if other.chars().count() == 1 => {
            let c = other.chars().next().unwrap();
            if c.is_ascii_alphabetic() {
                StrokeKind::Char(c.to_ascii_lowercase())
            } else {
                StrokeKind::Char(c)
            }
        }
        other => return Err(format!("unknown key `{other}`")),
    })
}

fn parse_stroke(spec: &str) -> Result<Stroke, String> {
    let chord = parse_one(spec)?;
    if chord.prefix {
        return Err(format!("`{spec}` is not a bare key"));
    }
    Ok(chord.stroke)
}

#[derive(Debug)]
enum RawValue {
    Unbind,
    Invalid(String),
    Specs(Vec<String>),
}

struct ParsedConfig {
    onboarding_done: bool,
    copy_on_select: bool,
    prefix: Option<String>,
    keys: Vec<(String, RawValue)>,
    commands: Vec<RawCommand>,
    warnings: Vec<String>,
}

fn parse_config(text: &str) -> ParsedConfig {
    let mut parsed = ParsedConfig {
        onboarding_done: false,
        copy_on_select: true,
        prefix: None,
        keys: Vec::new(),
        commands: Vec::new(),
        warnings: Vec::new(),
    };
    let mut section = "";
    let mut command: Option<RawCommand> = None;
    let mut saw_onboarding = false;
    for raw_line in text.lines() {
        let line = strip_comment(raw_line).trim().to_string();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') && line.ends_with(']') {
            if let Some(done) = command.take() {
                parsed.commands.push(done);
            }
            let name = line.trim_matches(&['[', ']'][..]).trim();
            section = if name == "keys" {
                "keys"
            } else if name == "keys.command" {
                command = Some(RawCommand {
                    key: String::new(),
                    command: String::new(),
                    description: String::new(),
                });
                "command"
            } else {
                ""
            };
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            parsed
                .warnings
                .push(format!("ignored config line `{line}`"));
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        if section.is_empty() && key == "onboarding" {
            saw_onboarding = true;
            parsed.onboarding_done = matches!(value, "false");
            continue;
        }
        if section.is_empty() && key == "copy_on_select" {
            match value {
                "true" => parsed.copy_on_select = true,
                "false" => parsed.copy_on_select = false,
                _ => parsed
                    .warnings
                    .push("copy_on_select must be true or false; using true".into()),
            }
            continue;
        }
        if section == "keys" {
            if key == "prefix" {
                parsed.prefix = Some(unquote(value).unwrap_or_else(|| value.to_string()));
                continue;
            }
            parsed.keys.push((key.to_string(), raw_binding(value)));
            continue;
        }
        if section == "command" {
            if let Some(command) = command.as_mut() {
                match key {
                    "key" => command.key = unquote(value).unwrap_or_default(),
                    "command" => command.command = unquote(value).unwrap_or_default(),
                    "description" => command.description = unquote(value).unwrap_or_default(),
                    _ => {}
                }
            }
        }
    }
    if let Some(done) = command {
        parsed.commands.push(done);
    }
    if !saw_onboarding {
        parsed.onboarding_done = false;
    }
    parsed
}

fn raw_binding(value: &str) -> RawValue {
    if value == "false" {
        return RawValue::Unbind;
    }
    match split_list(value) {
        Ok(items) if items.iter().all(|item| item.is_empty()) => RawValue::Unbind,
        Ok(items) => RawValue::Specs(items),
        Err(err) => RawValue::Invalid(err),
    }
}

fn split_list(value: &str) -> Result<Vec<String>, String> {
    let value = value.trim();
    if let Some(inner) = value.strip_prefix('[').and_then(|v| v.strip_suffix(']')) {
        if inner.trim().is_empty() {
            return Ok(Vec::new());
        }
        return inner
            .split(',')
            .map(|part| unquote(part.trim()).ok_or_else(|| format!("bad list entry `{part}`")))
            .collect();
    }
    unquote(value)
        .map(|item| vec![item])
        .ok_or_else(|| format!("bad value `{value}`"))
}

fn unquote(value: &str) -> Option<String> {
    let value = value.trim().trim_end_matches(',');
    if value == "false" {
        return Some(String::new());
    }
    let bytes = value.as_bytes();
    if bytes.len() >= 2
        && ((bytes[0] == b'"' && bytes[bytes.len() - 1] == b'"')
            || (bytes[0] == b'\'' && bytes[bytes.len() - 1] == b'\''))
    {
        return Some(value[1..value.len() - 1].to_string());
    }
    if value
        .chars()
        .all(|c| c.is_ascii_alphanumeric() || "+-_.?".contains(c))
    {
        return Some(value.to_string());
    }
    None
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            q if quote == Some(q) => quote = None,
            '#' if quote.is_none() => return &line[..index],
            _ => {}
        }
    }
    line
}

/// Write `onboarding = false` into a config file, preserving other lines.
pub fn write_onboarding_false(path: &std::path::Path) -> std::io::Result<()> {
    let existing = std::fs::read_to_string(path).unwrap_or_default();
    let mut saw = false;
    let mut lines: Vec<String> = existing
        .lines()
        .map(|line| {
            let trimmed = strip_comment(line).trim();
            if trimmed.starts_with("onboarding")
                && trimmed
                    .split_once('=')
                    .is_some_and(|(key, _)| key.trim() == "onboarding")
            {
                saw = true;
                "onboarding = false".to_string()
            } else {
                line.to_string()
            }
        })
        .collect();
    if !saw {
        lines.insert(0, "onboarding = false".into());
    }
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    std::fs::write(path, lines.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    fn key(code: KeyCode, mods: KeyModifiers) -> KeyEvent {
        KeyEvent::new(code, mods)
    }

    fn follow(map: &KeyMap, code: KeyCode, mods: KeyModifiers) -> Option<Action> {
        map.resolve(true, false, key(code, mods))
    }

    #[test]
    fn defaults_match_the_herdr_table_and_ctrl_b_is_only_the_prefix() {
        let map = KeyMap::builtin();
        assert!(map.is_prefix(key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert!(map.is_prefix(key(KeyCode::Char('B'), KeyModifiers::CONTROL)));
        assert_eq!(
            follow(&map, KeyCode::Char('b'), KeyModifiers::empty()),
            Some(Action::ToggleSidebar)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('c'), KeyModifiers::empty()),
            Some(Action::NewTab)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('v'), KeyModifiers::empty()),
            Some(Action::SplitVertical)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('-'), KeyModifiers::empty()),
            Some(Action::SplitHorizontal)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('?'), KeyModifiers::empty()),
            Some(Action::Help)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('s'), KeyModifiers::empty()),
            Some(Action::Settings)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('N'), KeyModifiers::SHIFT),
            Some(Action::NewWorkspace)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('G'), KeyModifiers::SHIFT),
            Some(Action::NewWorktree)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('w'), KeyModifiers::empty()),
            Some(Action::WorkspacePicker)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('q'), KeyModifiers::empty()),
            Some(Action::Detach)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('['), KeyModifiers::empty()),
            Some(Action::CopyMode)
        );
        assert_eq!(
            follow(&map, KeyCode::Tab, KeyModifiers::empty()),
            Some(Action::CyclePaneNext)
        );
        assert_eq!(
            follow(&map, KeyCode::BackTab, KeyModifiers::empty()),
            Some(Action::CyclePanePrevious)
        );
        assert_eq!(
            follow(&map, KeyCode::Char('1'), KeyModifiers::empty()),
            Some(Action::SwitchTab(1))
        );
        assert_eq!(
            follow(&map, KeyCode::Char('9'), KeyModifiers::empty()),
            Some(Action::SwitchTab(9))
        );
        assert!(follow(&map, KeyCode::Left, KeyModifiers::empty()).is_none());
        assert!(map.binding_label(&Action::ClearPane) == "unbound");
        assert!(map.binding_label(&Action::OpenWorktree) == "unbound");
        let help = map.help_text("");
        assert!(!help.to_ascii_lowercase().contains("open pi"));
        assert!(!help.contains("打开 Pi"));
        let commands = map.command_rows();
        assert!(
            commands
                .iter()
                .any(|row| row.id == "viva.diff" && row.binding == "unbound")
        );
        assert!(commands.iter().all(|row| {
            !row.description.to_ascii_lowercase().contains("open pi") && !row.id.contains("打开")
        }));
    }

    #[test]
    fn navigate_keys_are_not_prefix_actions() {
        let map = KeyMap::builtin();
        assert!(follow(&map, KeyCode::Up, KeyModifiers::empty()).is_none());
        assert_eq!(
            map.resolve(false, true, key(KeyCode::Up, KeyModifiers::empty())),
            Some(Action::NavigateWorkspaceUp)
        );
        assert_eq!(
            map.resolve(false, true, key(KeyCode::Char('j'), KeyModifiers::empty())),
            Some(Action::NavigatePaneDown)
        );
        assert_eq!(
            map.resolve(false, true, key(KeyCode::Left, KeyModifiers::empty())),
            Some(Action::NavigatePaneLeft)
        );
        assert!(
            map.resolve(false, false, key(KeyCode::Char('j'), KeyModifiers::empty()))
                .is_none()
        );
    }

    #[test]
    fn illegal_values_fall_back_and_empty_unbinds() {
        let map = KeyMap::load_str(
            r#"
            onboarding = false
            [keys]
            prefix = "not a key"
            new_tab = "bogus"
            zoom = ""
            focus_pane_left = ["prefix+h", "ctrl+alt+h"]
            "#,
        );
        assert!(!map.onboarding_done() || map.onboarding_done());
        assert!(map.onboarding_done());
        assert!(map.is_prefix(key(KeyCode::Char('b'), KeyModifiers::CONTROL)));
        assert_eq!(
            follow(&map, KeyCode::Char('c'), KeyModifiers::empty()),
            Some(Action::NewTab)
        );
        assert!(follow(&map, KeyCode::Char('z'), KeyModifiers::empty()).is_none());
        assert!(map.warnings().iter().any(|w| w.contains("keys.prefix")));
        assert!(map.warnings().iter().any(|w| w.contains("new_tab")));
        assert_eq!(
            map.resolve(
                false,
                false,
                key(
                    KeyCode::Char('h'),
                    KeyModifiers::CONTROL | KeyModifiers::ALT
                )
            ),
            Some(Action::FocusPaneLeft)
        );
    }

    #[test]
    fn copy_on_select_defaults_true_and_parses_false() {
        assert!(KeyMap::builtin().copy_on_select());
        assert!(KeyMap::load_str("").copy_on_select());
        assert!(!KeyMap::load_str("copy_on_select = false").copy_on_select());
        assert!(KeyMap::load_str("copy_on_select = true").copy_on_select());
        let bad = KeyMap::load_str("copy_on_select = maybe");
        assert!(bad.copy_on_select());
        assert!(
            bad.warnings().iter().any(|w| w.contains("copy_on_select")),
            "{:?}",
            bad.warnings()
        );
    }

    #[test]
    fn custom_commands_cannot_override_builtin_chords_or_names() {
        let map = KeyMap::load_str(
            r#"
            [keys]
            [[keys.command]]
            key = "prefix+c"
            command = "viva.diff"
            description = "show the worktree diff"
            [[keys.command]]
            key = "prefix+alt+d"
            command = "new_tab"
            description = "nope"
            [[keys.command]]
            key = "prefix+alt+y"
            command = "viva.files"
            description = "browse worktree files"
            "#,
        );
        assert_eq!(
            follow(&map, KeyCode::Char('c'), KeyModifiers::empty()),
            Some(Action::NewTab)
        );
        assert!(map.warnings().iter().any(|w| w.contains("cannot override")));
        let files = map
            .command_rows()
            .into_iter()
            .find(|row| row.id == "viva.files")
            .unwrap();
        assert_eq!(files.binding, "prefix+alt+y");
        assert!(
            map.resolve(true, false, key(KeyCode::Char('y'), KeyModifiers::ALT))
                .is_some_and(|action| action == Action::Command("viva.files".into()))
        );
    }

    #[test]
    fn reload_round_trips_onboarding_flag() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("config.toml");
        std::fs::write(&path, "[keys]\nhelp = \"prefix+?\"\n").unwrap();
        write_onboarding_false(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert!(KeyMap::load_str(&text).onboarding_done());
        write_onboarding_false(&path).unwrap();
        let text = std::fs::read_to_string(&path).unwrap();
        assert_eq!(text.matches("onboarding = false").count(), 1);
    }
}
