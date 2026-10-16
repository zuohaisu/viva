//! The parallel-development workbench (V14, issue #27): one Viva spanning
//! many projects, tasks and worktrees, with real terminals per worktree.
//!
//! Since the resident-server split (S1, issue #43; ADR 0012) this module is
//! the CLIENT: facts arrive as `WorkbenchView` projections over the control
//! channel, actions go back as requests, and `q` detaches while the server
//! keeps every terminal running. The server side of the projections lives
//! in [`assemble_view`], called by the server. The scene / tab / pane tree
//! lives there too: this client caches it, commits the tab it is looking
//! at, and refreshes the other tabs from the server.
//!
//! Composition rules (this module only combines; the domains stay theirs):
//! - Facts come from the existing registries (projects/tasks V02–V03,
//!   worktrees V08, terminals V05). Lists, dirty flags and needs-attention
//!   markers are projections — there is no second workflow state here, and
//!   unknown states are shown as unknown, never guessed.
//! - A new pane is a terminal. Pi, Claude, Codex, and any other TUI agent
//!   start when the user runs that command in the pane. The workbench has
//!   no action that launches one of them.
//! - Diffs and status are real git facts, read-only; editing is delegated
//!   to the user's own editor — the workbench builds no code editor.
//! - Quit is a detach: worktree contents and records are left exactly as
//!   they are, nothing is re-run when the client reattaches.

mod copy_mode;
mod keys;
mod polling;
pub mod scenes;
pub mod terminal_view;
pub mod tree;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use scenes::{FLOATING, MAX_SCENES, MAX_TABS, Scene, SceneBook, TabView};

use std::process::Command;

use crate::foundation::error::Result;
use crate::terminal::TerminalSnapshot;
use crate::tui::layout::{Direction, PaneContent, PaneNode, SplitAxis};
use crate::tui::theme::Palette;

/// Reference area for geometric pane-focus moves: adjacency decisions are
/// proportional, so one fixed virtual size is stable for any real size.
const PANE_REF_AREA: ratatui::layout::Rect = ratatui::layout::Rect {
    x: 0,
    y: 0,
    width: 100,
    height: 40,
};

/// Sidebar width (herdr's default); the chrome hides it on narrow
/// terminals instead of squeezing the pane grid to death.
const SIDEBAR_WIDTH: u16 = 26;

/// Sidebar drag bounds (herdr's validated defaults): the edge drag clamps
/// into this band, and double-click resets to [`SIDEBAR_WIDTH`].
const SIDEBAR_MIN_WIDTH: u16 = 18;
const SIDEBAR_MAX_WIDTH: u16 = 36;

/// Two clicks on the same divider cell within this window are a
/// double-click (reset to the default split/width).
const DOUBLE_CLICK_WINDOW: std::time::Duration = std::time::Duration::from_millis(500);

/// Minimum usable pane size (P3): a split that would leave any pane below
/// this is refused instead of crushing a neighbor into a sliver.
const MIN_PANE_WIDTH: u16 = 8;
const MIN_PANE_HEIGHT: u16 = 3;

/// Clip `text` to `max` characters (display width is approximated by char
/// count; the sidebar rows are ASCII in practice).
fn fit(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

// ---------------------------------------------------------------------------
// Projections (read-only facts assembled by the store-backed source)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct ProjectRow {
    pub project_id: String,
    pub name: String,
    pub repo_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct WorktreeRow {
    pub worktree_id: String,
    pub project_id: String,
    pub path: String,
    pub branch: String,
    /// Real git status: `Some(true)` dirty, `Some(false)` clean, `None`
    /// unknown (unreadable tree) — displayed as unknown, never guessed.
    pub dirty: Option<bool>,
    pub task_id: Option<String>,
    /// created (Viva allocated) or adopted (explicitly selected).
    pub source: String,
    /// V15-2: the branch's PR state via gh (`open`/`merged`/`closed`).
    #[serde(default)]
    pub pr_state: Option<String>,
    /// V15-4: distance vs `origin/<branch>`; None = unknown.
    #[serde(default)]
    pub ahead: Option<u32>,
    #[serde(default)]
    pub behind: Option<u32>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct TaskRow {
    pub task_id: String,
    pub goal: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct TerminalRow {
    pub terminal_id: String,
    pub worktree_id: Option<String>,
    pub purpose: String,
    pub owner_label: String,
    /// Viva's real knowledge; `None` is shown as unknown.
    pub live: Option<bool>,
    /// Agent status observations, PER SOURCE (S4): controlled reports are
    /// authoritative, screen inference is auxiliary. They sit side by
    /// side, never merged into one claim.
    #[serde(default)]
    pub agent_status: Vec<crate::agents::AgentStatusRecord>,
}

/// A needs-attention marker: a fact recorded by Viva that a human
/// should look at (a failed execution, an unresolved launch). Projections
/// only — the workbench never decides what to do about them.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct AttentionMarker {
    pub kind: String,
    pub detail: String,
}

/// The workbench snapshot the view renders. Serialized over the control
/// channel since the client/server split (#43): the server assembles it
/// from the real registries, the client renders it.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct WorkbenchModel {
    #[serde(default)]
    pub agents: Vec<crate::agents::AgentSessionRow>,
    #[serde(default)]
    pub workspaces: Vec<crate::workspaces::navigation::WorkspaceRow>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub workspace_diagnostics: Vec<String>,
    pub projects: Vec<ProjectRow>,
    pub worktrees: Vec<WorktreeRow>,
    pub tasks: Vec<TaskRow>,
    pub terminals: Vec<TerminalRow>,
    pub attention: Vec<AttentionMarker>,
    /// V15-4: the auto_pull switch state, surfaced in the workbench.
    #[serde(default)]
    pub auto_pull: bool,
    /// V15-4: a human summary of the last sync cycle.
    #[serde(default)]
    pub sync_note: Option<String>,
    /// This computer plus each saved SSH machine. Empty for older views.
    /// The sidebar projects these rows; it does not add a key set.
    #[serde(default)]
    pub machines: Vec<crate::machines::MachineView>,
}

// ---------------------------------------------------------------------------
// View state machine (pure; the run loop executes actions)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    /// The `spaces` sidebar section: projects and their worktrees.
    Worktrees,
    Tasks,
    /// The `agents` sidebar section: terminals and their agent states.
    Terminals,
    /// Right-hand pane navigation, independent of sidebar selection.
    Panes,
}

/// What one key press decided.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum KeyOutcome {
    Handled,
    Ignored,
    /// The user asked to quit; the client run loop detaches from the
    /// resident server (the server keeps running — ADR 0012).
    QuitRequested,
    /// Terminal-mode: these bytes belong to the focused terminal's stdin.
    Forward(Vec<u8>),
    /// A workbench action for the run loop to execute against the real
    /// registries, then feed back via `set_model` / `set_terminal_view`.
    Action(WorkbenchAction),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum WorkbenchAction {
    /// Enter keyboard-forwarding mode on a terminal.
    EnterTerminal(String),
    /// Stop exactly one terminal (its process group only).
    StopTerminal(String),
    /// Show the real diff of a worktree.
    ShowDiff(String),
    /// Ask the run loop to refresh the model from the registries.
    Refresh,
    /// Split the focused pane right, spawning a shell in the new leaf.
    SplitRight,
    /// Split the focused pane below, spawning a shell in the new leaf.
    SplitBelow,
    /// Open a shell at the selected worktree (the `o` action).
    OpenWorktreeShell(String),
    /// Create the selected task's worktree (reachable as a command, not a single key).
    CreateTaskWorktree(String),
    /// V15-1: open the file browser for a worktree.
    BrowseFiles(String),
    /// V15-1: view one file's diff/content in the panel.
    ViewFile {
        worktree_id: String,
        path: String,
    },
    /// V15-1: open the viewed file with the system opener.
    OpenInEditor {
        full_path: String,
    },
    /// V15-2: release + remove a worktree whose PR is merged.
    CleanupWorktree(String),
    /// V15-3: hand the worktree to the picked agent.
    HandoffTo {
        worktree_id: String,
        agent: String,
    },
    /// V15-4: toggle the auto_pull switch.
    ToggleAutoPull,
    SelectWorktree(String),
    /// Workspace or agent on a saved machine. The local socket is not retargeted.
    UseMachine {
        machine_id: String,
        workspace_id: Option<String>,
        terminal_id: Option<String>,
    },
    NewTab,
    Workspace {
        action: String,
        value: Option<String>,
        project_id: Option<String>,
    },
}

/// An in-progress mouse drag. A pane-split drag mutates the local ratio
/// immediately. The layout save commits that tab's grid to the resident
/// tree; other tabs stay as the server last recorded them.
#[derive(Debug, Clone, PartialEq, Eq)]
enum DragState {
    None,
    /// Dragging one split boundary. `grab_offset` glues the grabbed point
    /// to the pointer (divider position at grab minus pointer coordinate
    /// at grab), so the line never jumps on grab. `leaves` snapshots the
    /// topology: a split/close under a live drag invalidates the path and
    /// cancels the drag instead of resizing the wrong node.
    PaneSplit {
        path: Vec<bool>,
        axis: SplitAxis,
        area: Rect,
        grab_offset: i32,
        leaves: Vec<PaneContent>,
    },
    /// Dragging the sidebar's right edge to resize it.
    Sidebar,
}

// ---------------------------------------------------------------------------
// V15 workbench UI blocks (issues V15-1/V15-3): file panel, handoff
// picker, two-step cleanup. Inserted into tui/workbench/mod.rs.
// ---------------------------------------------------------------------------

/// The file browser state for one worktree (V15-1): the inventory rows,
/// the selection, and optionally the file currently being viewed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FilePanel {
    pub worktree_id: String,
    /// `(path, status)` rows as served by Viva (bounded: may be a
    /// truncation of the full inventory — see `truncated`/`total`).
    pub rows: Vec<(String, String)>,
    pub truncated: bool,
    pub total: usize,
    pub selected: usize,
    /// When set: this file's diff/content is being viewed instead of the
    /// list. Esc returns to the list first. `full_path` backs the `e`
    /// open-in-editor action.
    pub viewing: Option<String>,
    pub full_path: Option<String>,
    /// QA F6 residual: scroll offset into the viewed material.
    pub scroll: usize,
}

/// The `h` handoff target picker (V15-3): a minimal overlay over the
/// detectable agent list.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct HandoffPicker {
    pub worktree_id: String,
    pub agents: Vec<String>,
    pub selected: usize,
}

/// The one-line input of the workspace actions (`W` open, `N` new, `A` add,
/// `S` save as). For `open`, the most recent workspaces are listed under
/// the input; ↑/↓ pick one and Enter on an empty input switches to it.
#[derive(Debug, Clone, PartialEq, Eq)]
struct WorkspacePrompt {
    action: &'static str,
    text: String,
    pick: Option<usize>,
}

impl WorkspacePrompt {
    fn title(&self) -> &'static str {
        match self.action {
            "open" => {
                " Open workspace: a repo, a folder or a .code-workspace file · ↑↓ recent · Enter · Esc "
            }
            "new" => " New workspace name · Enter · Esc ",
            "rename" => " Rename workspace · Enter · Esc ",
            "worktree" => " New worktree branch · Enter · Esc ",
            "add" => " Add project: path of a repo or folder · Enter · Esc ",
            _ => " Save As: path of the .code-workspace file · Enter · Esc ",
        }
    }
}

/// How many recent workspaces the open prompt lists.
const RECENT_LIMIT: usize = 8;

const DEFAULT_STATUS_HINT: &str = "";

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum MenuAction {
    SplitRight,
    SplitDown,
    Close,
    Rename,
    Zoom,
}

struct ContextMenu {
    selected: usize,
    column: u16,
    row: u16,
}

struct GotoPicker {
    query: String,
    searching: bool,
    cursor: usize,
    /// `b` `w` `i` `d` filter agent rows; `None` lists every terminal.
    kind: Option<String>,
}

struct GotoRow {
    id: String,
    group: String,
    label: String,
}

fn menu_actions() -> [MenuAction; 5] {
    [
        MenuAction::SplitRight,
        MenuAction::SplitDown,
        MenuAction::Close,
        MenuAction::Rename,
        MenuAction::Zoom,
    ]
}

struct DragSelect {
    id: String,
    area: Rect,
    start_col: u16,
    start_row: u16,
    end_col: u16,
    end_row: u16,
    moved: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MachineSpace {
    machine_id: String,
    label: String,
    connected: bool,
    enabled: bool,
    workspace_id: String,
    name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct MachineAgent {
    machine_id: String,
    label: String,
    connected: bool,
    terminal_id: String,
    name: String,
}

/// The workbench application state. Pure view state + explicit caches;
/// drive it with [`WorkbenchApp::on_key`], the setters, and [`Self::draw`].
pub struct WorkbenchApp {
    model: WorkbenchModel,
    focus: Focus,
    selected: usize,
    /// The client-side pane tree (S2): leaves render the browser or one
    /// terminal each. The terminals themselves live in the resident
    /// server; closing a pane never stops one.
    grid: PaneNode,
    pane_focus: PaneContent,
    zoomed: bool,
    /// Per-terminal snapshot cache the pane tree renders; the run loop
    /// fetches one per terminal leaf per cycle (bounded by the pane cap).
    snapshots: std::collections::HashMap<String, TerminalSnapshot>,
    /// When set: keyboard bytes forward to this terminal; Esc releases.
    terminal_mode: Option<String>,
    /// Set when `terminal_mode` belongs to a remote machine. Local panes leave this empty.
    remote_terminal: Option<(String, String)>,
    diff_view: Option<String>,
    status_line: String,
    quit_requested: bool,
    /// Set by every pane-tree mutation; the run loop persists the layout
    /// to the server (QA F5) when it sees the flag.
    layout_dirty: bool,
    /// V15-1: the file browser overlay for one worktree.
    file_panel: Option<FilePanel>,
    /// V15-1: the viewed file's material (diff or bounded content).
    file_view: Option<String>,
    /// V15-3: the handoff target picker.
    handoff_picker: Option<HandoffPicker>,
    /// V15-2: armed cleanup — the first `D` names the exact path, the
    /// second confirms. Any OTHER keypress disarms (stale arming must not
    /// turn a later `D` into an unintended deletion).
    cleanup_armed: Option<String>,
    /// QA F4: scroll offset into the diff overlay.
    diff_scroll: usize,
    /// The pane area as last drawn: split refuses to create ANY pane below
    /// the minimum usable size at this real terminal size (P3). Invariant:
    /// a split before the first draw is only cap-checked — the product run
    /// loop always draws before reading keys, so it never hits that gap;
    /// tests may set the area directly.
    last_pane_area: Option<Rect>,
    /// The sidebar's right edge as last drawn (the pane area's first
    /// column); None while the sidebar is hidden (narrow terminal). The
    /// sidebar drag band sits on this column and the one before it.
    last_sidebar_edge: Option<u16>,
    /// The user-chosen sidebar width; the draw applies the
    /// narrow-terminal clamp on top of it.
    sidebar_width: u16,
    /// Mouse drag state; see [`DragState`].
    drag: DragState,
    /// Whether the live drag has actually moved something. A drag that
    /// moved the layout must never count as the first click of a
    /// double-click reset: grab → nudge → release → grab again is how a
    /// human repositions a divider, and the second grab must START a drag,
    /// not reset the split to 50/50.
    drag_moved: bool,
    /// The last divider click for double-click detection: (when, column,
    /// row). Cleared by any click that is not on a divider.
    last_divider_click: Option<(std::time::Instant, u16, u16)>,
    workspace_prompt: Option<WorkspacePrompt>,
    /// Click targets of the top workspace switcher (`None` = All projects).
    workspace_hits: Vec<(Option<String>, Rect)>,
    scenes: SceneBook,
    tab_prompt: Option<String>,
    tab_hits: Vec<(String, Rect)>,
    terminal_picker: Option<usize>,
    global_attention: bool,
    sidebar_hits: Vec<(Focus, usize, Rect)>,
    mouse_action: Option<WorkbenchAction>,
    pane_geometry: Vec<(String, Rect)>,
    scroll_offsets: std::collections::HashMap<String, usize>,
    /// Herdr-style prefix mode: `ctrl+b` then one action key.
    prefix_pending: bool,
    show_key_help: bool,
    help_filter: String,
    help_filtering: bool,
    sidebar_collapsed: bool,
    copy_mode: bool,
    copy_state: copy_mode::CopyMode,
    resize_mode: bool,
    navigate: bool,
    goto: Option<GotoPicker>,
    settings_open: bool,
    onboarding_pending: bool,
    keymap: keys::KeyMap,
    config_path: Option<std::path::PathBuf>,
    context_menu: Option<ContextMenu>,
    menu_hits: Vec<(MenuAction, Rect)>,
    tree_ops: Vec<crate::server::WorkbenchTreeOp>,
    pane_prompt: Option<String>,
    drag_select: Option<DragSelect>,
    last_copy: String,
    scrollback_edit: Option<String>,
    workspace_nav: usize,
}

impl WorkbenchApp {
    pub fn new() -> Self {
        Self {
            model: WorkbenchModel::default(),
            focus: Focus::Worktrees,
            selected: 0,
            grid: PaneNode::leaf(PaneContent::Browser),
            pane_focus: PaneContent::Browser,
            zoomed: false,
            snapshots: std::collections::HashMap::new(),
            terminal_mode: None,
            remote_terminal: None,
            diff_view: None,
            status_line: DEFAULT_STATUS_HINT.into(),
            quit_requested: false,
            layout_dirty: false,
            file_panel: None,
            file_view: None,
            handoff_picker: None,
            cleanup_armed: None,
            diff_scroll: 0,
            last_pane_area: None,
            last_sidebar_edge: None,
            sidebar_width: SIDEBAR_WIDTH,
            drag: DragState::None,
            drag_moved: false,
            last_divider_click: None,
            workspace_prompt: None,
            workspace_hits: vec![],
            scenes: SceneBook::default(),
            tab_prompt: None,
            tab_hits: vec![],
            terminal_picker: None,
            global_attention: false,
            sidebar_hits: vec![],
            mouse_action: None,
            pane_geometry: vec![],
            scroll_offsets: std::collections::HashMap::new(),
            prefix_pending: false,
            show_key_help: false,
            help_filter: String::new(),
            help_filtering: false,
            sidebar_collapsed: false,
            copy_mode: false,
            copy_state: copy_mode::CopyMode::default(),
            resize_mode: false,
            navigate: false,
            goto: None,
            settings_open: false,
            onboarding_pending: false,
            keymap: keys::KeyMap::builtin(),
            config_path: None,
            context_menu: None,
            menu_hits: Vec::new(),
            tree_ops: Vec::new(),
            pane_prompt: None,
            drag_select: None,
            last_copy: String::new(),
            scrollback_edit: None,
            workspace_nav: 0,
        }
    }

    pub fn navigate_mode(&self) -> bool {
        self.navigate
    }

    pub fn settings_open(&self) -> bool {
        self.settings_open
    }

    pub fn goto_open(&self) -> bool {
        self.goto.is_some()
    }

    pub fn help_open(&self) -> bool {
        self.show_key_help
    }

    pub fn help_text(&self) -> String {
        self.keymap.help_text(&self.help_filter)
    }

    pub fn command_catalog(&self) -> Vec<keys::CommandRow> {
        self.keymap.command_rows()
    }

    pub fn config_warnings(&self) -> &[String] {
        self.keymap.warnings()
    }

    pub fn last_copy(&self) -> &str {
        &self.last_copy
    }

    pub fn scrollback_edit_path(&self) -> Option<&str> {
        self.scrollback_edit.as_deref()
    }

    pub fn take_tree_ops(&mut self) -> Vec<crate::server::WorkbenchTreeOp> {
        std::mem::take(&mut self.tree_ops)
    }

    pub fn load_config_str(&mut self, text: &str, path: Option<std::path::PathBuf>) {
        self.keymap = keys::KeyMap::load_str(text);
        self.config_path = path;
        self.onboarding_pending = !self.keymap.onboarding_done();
        if self.onboarding_pending {
            self.settings_open = true;
        }
        if !self.keymap.warnings().is_empty() {
            self.set_status(self.keymap.warnings().join("; "));
        }
    }

    pub fn load_home_config(&mut self) {
        let path = crate::foundation::paths::viva_home(None).join("config.toml");
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        self.load_config_str(&text, Some(path));
    }

    fn queue_tree(&mut self, op: crate::server::WorkbenchTreeOp) {
        self.tree_ops.push(op);
    }

    fn active_tab_id(&self) -> Option<String> {
        self.scenes
            .scenes
            .get(&self.scenes.selected)
            .map(|scene| scene.active.clone())
    }

    fn pane_key(content: &PaneContent) -> String {
        match content {
            PaneContent::Browser => "browser".into(),
            PaneContent::Terminal(id) => id.clone(),
        }
    }

    fn queue_focus(&mut self, content: &PaneContent) {
        self.queue_tree(crate::server::WorkbenchTreeOp::FocusPane {
            scene_id: Some(self.scenes.selected.clone()),
            tab_id: self.active_tab_id(),
            pane_id: Self::pane_key(content),
        });
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    pub fn terminal_mode(&self) -> Option<&str> {
        self.terminal_mode.as_deref()
    }

    pub fn set_model(&mut self, model: WorkbenchModel) {
        if !model.workspace_diagnostics.is_empty()
            && (model.workspace_id != self.model.workspace_id
                || model.workspace_diagnostics != self.model.workspace_diagnostics)
        {
            self.set_status(model.workspace_diagnostics.join(" · "));
        }
        if let Some((machine, _)) = self.remote_terminal.clone() {
            let usable = model
                .machines
                .iter()
                .any(|view| view.id == machine && view.connected && view.enabled);
            if !usable {
                let label = model
                    .machines
                    .iter()
                    .find(|view| view.id == machine)
                    .map(|view| view.label.clone())
                    .unwrap_or(machine);
                self.terminal_mode = None;
                self.remote_terminal = None;
                self.set_status(format!("{label} is disconnected; input is refused"));
            }
        }
        self.model = model;
        self.clamp_selection();
    }

    pub fn model(&self) -> &WorkbenchModel {
        &self.model
    }

    pub fn set_diff_view(&mut self, diff: Option<String>) {
        if diff.is_some() {
            self.diff_scroll = 0;
        }
        self.diff_view = diff;
    }

    pub fn set_status(&mut self, line: impl Into<String>) {
        self.status_line = line.into();
    }

    pub fn set_snapshot(&mut self, terminal_id: impl Into<String>, snapshot: TerminalSnapshot) {
        let id = terminal_id.into();
        if self.terminal_leaves().contains(&id) {
            self.snapshots.insert(id, snapshot);
        }
    }

    pub fn visible_geometry(&self) -> &[(String, Rect)] {
        &self.pane_geometry
    }
    pub fn scroll_offset(&self, id: &str) -> usize {
        self.scroll_offsets.get(id).copied().unwrap_or(0)
    }
    fn input_modes(&self) -> crate::terminal::screen::InputModes {
        self.terminal_mode
            .as_ref()
            .and_then(|id| self.snapshots.get(id))
            .and_then(|s| s.screen.as_ref())
            .map(|s| s.modes.clone())
            .unwrap_or_default()
    }
    fn scroll_terminal(&mut self, id: String, delta: isize) {
        let old = self.scroll_offset(&id);
        let retained = self
            .snapshots
            .get(&id)
            .and_then(|s| s.screen.as_ref())
            .map(|s| s.retained)
            .unwrap_or(crate::terminal::SCROLLBACK_LINES);
        let value = old.saturating_add_signed(delta).min(retained);
        self.scroll_offsets.insert(id, value);
    }
    /// `None` (All projects) followed by every workspace in creation order.
    fn switcher_order(&self) -> Vec<Option<String>> {
        std::iter::once(None)
            .chain(
                self.model
                    .workspaces
                    .iter()
                    .map(|w| Some(w.workspace_id.clone())),
            )
            .collect()
    }

    fn select_workspace(id: Option<String>) -> WorkbenchAction {
        WorkbenchAction::Workspace {
            action: "select".into(),
            value: Some(id.unwrap_or_else(|| "all".into())),
            project_id: None,
        }
    }

    /// Most recently opened workspaces, newest first.
    fn recent_workspaces(&self) -> Vec<&crate::workspaces::navigation::WorkspaceRow> {
        // Reversed first: the sort is stable, so equal timestamps keep the
        // newest-created first, exactly like the store's own ordering.
        let mut recent: Vec<_> = self.model.workspaces.iter().rev().collect();
        recent.sort_by(|a, b| b.opened_at.cmp(&a.opened_at));
        recent.truncate(RECENT_LIMIT);
        recent
    }

    fn on_workspace_prompt_key(&mut self, key: KeyEvent) -> KeyOutcome {
        use ratatui::crossterm::event::KeyModifiers;
        let recent_len = self.recent_workspaces().len();
        let Some(prompt) = self.workspace_prompt.as_mut() else {
            return KeyOutcome::Ignored;
        };
        match key.code {
            KeyCode::Esc => self.workspace_prompt = None,
            KeyCode::Backspace => {
                prompt.text.pop();
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                prompt.text.clear();
            }
            KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                prompt.text.push(c);
                prompt.pick = None;
            }
            KeyCode::Down if prompt.action == "open" && recent_len > 0 => {
                prompt.pick = Some(prompt.pick.map_or(0, |i| (i + 1).min(recent_len - 1)));
            }
            KeyCode::Up if prompt.action == "open" => {
                prompt.pick = prompt.pick.and_then(|i| i.checked_sub(1));
            }
            KeyCode::Enter => {
                let (action, text, pick) =
                    (prompt.action, prompt.text.trim().to_string(), prompt.pick);
                if let (true, true, Some(i)) = (action == "open", text.is_empty(), pick) {
                    let id = self
                        .recent_workspaces()
                        .get(i)
                        .map(|w| w.workspace_id.clone());
                    self.workspace_prompt = None;
                    return match id {
                        Some(id) => KeyOutcome::Action(Self::select_workspace(Some(id))),
                        None => KeyOutcome::Handled,
                    };
                }
                if text.is_empty() {
                    self.set_status("type a value first (Esc cancels)");
                    return KeyOutcome::Handled;
                }
                if action == "worktree" {
                    self.workspace_prompt = None;
                    self.set_status(format!("new worktree `{text}`"));
                    return KeyOutcome::Handled;
                }
                self.workspace_prompt = None;
                return KeyOutcome::Action(WorkbenchAction::Workspace {
                    action: action.into(),
                    value: Some(text),
                    project_id: None,
                });
            }
            _ => {}
        }
        KeyOutcome::Handled
    }

    pub fn on_paste(&mut self, text: &str) -> KeyOutcome {
        if let Some(prompt) = self.workspace_prompt.as_mut() {
            // A pasted path lands in the prompt, newlines dropped.
            prompt.text.extend(text.chars().filter(|c| !c.is_control()));
            prompt.pick = None;
            return KeyOutcome::Handled;
        }
        if self.terminal_mode.is_none() {
            return KeyOutcome::Ignored;
        }
        if self.blocked_remote() {
            return KeyOutcome::Handled;
        }
        if text.len() > 64 * 1024 {
            self.set_status("paste exceeds 64 KiB; nothing sent");
            return KeyOutcome::Handled;
        }
        KeyOutcome::Forward(terminal_view::paste(text, &self.input_modes()))
    }
    pub fn terminal_mouse(
        &mut self,
        event: ratatui::crossterm::event::MouseEvent,
    ) -> Option<(String, Vec<u8>)> {
        use ratatui::crossterm::event::{KeyModifiers, MouseEventKind};
        if matches!(
            event.kind,
            MouseEventKind::Down(ratatui::crossterm::event::MouseButton::Right)
                | MouseEventKind::Up(ratatui::crossterm::event::MouseButton::Right)
        ) {
            return None;
        }
        if self.file_panel.is_some()
            || self.diff_view.is_some()
            || self.handoff_picker.is_some()
            || self.workspace_prompt.is_some()
            || self.tab_prompt.is_some()
            || self.terminal_picker.is_some()
            || !matches!(self.drag, DragState::None)
        {
            return None;
        }
        let (id, rect) = self
            .pane_geometry
            .iter()
            .find(|(_, r)| contains_point(*r, event.column, event.row))
            .cloned()?;
        // Divider grab bands always belong to Viva, including their tolerance.
        if self.last_pane_area.is_some_and(|area| {
            !self.zoomed
                && self.grid.splits(area).iter().any(|hit| match hit.axis {
                    SplitAxis::Horizontal => event.column.abs_diff(hit.pos) <= 1,
                    SplitAxis::Vertical => event.row.abs_diff(hit.pos) <= 1,
                })
        }) {
            return None;
        }
        if self.terminal_mode.as_deref() == Some(&id)
            && !event.modifiers.contains(KeyModifiers::SHIFT)
            && self.scroll_offset(&id) == 0
        {
            let modes = self.input_modes();
            if let Some(bytes) = terminal_view::mouse(event, rect, &modes) {
                return Some((id, bytes));
            }
        }
        let delta = match event.kind {
            MouseEventKind::ScrollUp => 3,
            MouseEventKind::ScrollDown => -3,
            _ => return None,
        };
        self.scroll_terminal(id, delta);
        None
    }
    /// Terminal ids currently placed in the pane tree (fetch budget: the
    /// tree never exceeds the layout cap).
    pub fn terminal_leaves(&self) -> Vec<String> {
        self.grid
            .leaves()
            .into_iter()
            .filter_map(|c| match c {
                PaneContent::Terminal(id) => Some(id),
                _ => None,
            })
            .collect()
    }

    pub fn pane_focus(&self) -> &PaneContent {
        &self.pane_focus
    }

    pub fn zoomed(&self) -> bool {
        self.zoomed
    }

    pub fn layout_dirty(&self) -> bool {
        self.layout_dirty
    }

    pub fn clear_layout_dirty(&mut self) {
        self.layout_dirty = false;
    }

    /// Take a fresher server tree without jumping to the other client's tab.
    /// The tab this client is looking at picks up that tab's latest grid,
    /// focus and zoom. Sidebar width stays local.
    pub fn adopt_shared_tree(&mut self, json: &str) {
        let Ok(book) = serde_json::from_str::<SceneBook>(json) else {
            return;
        };
        if !book.valid() {
            return;
        }
        let selected = self.scenes.selected.clone();
        let active = self
            .scenes
            .scenes
            .get(&selected)
            .map(|scene| scene.active.clone());
        let prev_grid = self.grid.clone();
        self.scenes = book;
        if self.scenes.scenes.contains_key(&selected) {
            self.scenes.selected = selected;
            if let Some(active) = active {
                if let Some(scene) = self.scenes.scenes.get_mut(&self.scenes.selected) {
                    if scene.tabs.iter().any(|tab| tab.id == active && !tab.hidden) {
                        scene.active = active;
                    }
                }
            }
        }
        let tab = self
            .scenes
            .scenes
            .get(&self.scenes.selected)
            .and_then(|scene| {
                scene
                    .tabs
                    .iter()
                    .find(|tab| tab.id == scene.active && !tab.hidden)
                    .cloned()
            });
        let Some(tab) = tab else {
            self.load_scene();
            return;
        };
        let grid_changed = tab.grid != prev_grid;
        self.grid = tab.grid;
        self.pane_focus = tab.focus;
        self.zoomed = tab.zoom;
        if grid_changed {
            let live: std::collections::HashSet<String> =
                self.terminal_leaves().into_iter().collect();
            self.snapshots.retain(|id, _| live.contains(id));
            if self
                .terminal_mode
                .as_ref()
                .is_some_and(|id| !live.contains(id))
            {
                self.terminal_mode = None;
            }
            self.drag = DragState::None;
        }
    }

    /// The persisted layout payload: the pane tree plus the focused pane
    /// and the sidebar width (restored with a default when absent, so
    /// older saved layouts keep loading).
    fn save_scene(&mut self) {
        let key = self.scenes.selected.clone();
        let scene = self.scenes.scenes.entry(key).or_default();
        if let Some(tab) = scene.tabs.iter_mut().find(|t| t.id == scene.active) {
            tab.grid = self.grid.clone();
            tab.focus = self.pane_focus.clone();
            tab.zoom = self.zoomed;
        }
    }
    fn load_scene(&mut self) {
        let tab = self
            .scenes
            .scenes
            .get(&self.scenes.selected)
            .and_then(|s| s.tabs.iter().find(|t| t.id == s.active && !t.hidden));
        if let Some(t) = tab {
            self.grid = t.grid.clone();
            self.pane_focus = t.focus.clone();
            self.zoomed = t.zoom;
        } else {
            self.grid = PaneNode::leaf(PaneContent::Browser);
            self.pane_focus = PaneContent::Browser;
            self.zoomed = false;
        }
        self.terminal_mode = None;
        self.drag = DragState::None;
        self.snapshots.clear();
    }
    pub fn select_worktree(&mut self, key: String) {
        self.save_scene();
        if !self.scenes.scenes.contains_key(&key) && self.scenes.scenes.len() >= MAX_SCENES {
            self.set_status("scene limit reached");
            return;
        }
        self.scenes.selected = key;
        self.load_scene();
        self.layout_dirty = true;
        self.set_status(
            "existing scene · ctrl+b c new tab · no process started until you open a shell",
        );
    }
    pub fn selected_scene(&self) -> &str {
        &self.scenes.selected
    }
    pub fn new_tab(&mut self, name: String) -> bool {
        self.save_scene();
        let scene = self
            .scenes
            .scenes
            .entry(self.scenes.selected.clone())
            .or_default();
        if scene.tabs.len() >= MAX_TABS {
            self.set_status("tab limit reached (including hidden tabs)");
            return false;
        }
        let tab = TabView::new(name);
        scene.active = tab.id.clone();
        scene.tabs.push(tab);
        self.load_scene();
        self.layout_dirty = true;
        true
    }
    pub fn switch_tab(&mut self, id: &str) -> bool {
        self.save_scene();
        let Some(scene) = self.scenes.scenes.get_mut(&self.scenes.selected) else {
            return false;
        };
        if !scene.tabs.iter().any(|t| t.id == id && !t.hidden) {
            return false;
        }
        scene.active = id.into();
        self.load_scene();
        self.focus = Focus::Panes;
        self.layout_dirty = true;
        true
    }
    pub fn close_tab(&mut self) {
        self.save_scene();
        if let Some(scene) = self.scenes.scenes.get_mut(&self.scenes.selected) {
            if let Some(t) = scene.tabs.iter_mut().find(|t| t.id == scene.active) {
                t.hidden = true;
            }
            scene.active = scene
                .tabs
                .iter()
                .find(|t| !t.hidden)
                .map(|t| t.id.clone())
                .unwrap_or_default();
        }
        self.load_scene();
        self.layout_dirty = true;
        self.set_status("tab hidden · t finds sessions · processes continue");
    }
    pub fn terminal_location(&self, id: &str) -> Option<scenes::TerminalLocation> {
        let mut book = self.scenes.clone();
        if let Some(s) = book.scenes.get_mut(&book.selected) {
            if let Some(t) = s.tabs.iter_mut().find(|t| t.id == s.active) {
                t.grid = self.grid.clone();
                t.focus = self.pane_focus.clone();
            }
        }
        book.locate(id)
    }
    pub fn serialize_layout(&self) -> String {
        let mut book = self.scenes.clone();
        book.sidebar = self.sidebar_width;
        if let Some(scene) = book.scenes.get_mut(&book.selected) {
            if let Some(tab) = scene.tabs.iter_mut().find(|t| t.id == scene.active) {
                tab.grid = self.grid.clone();
                tab.focus = self.pane_focus.clone();
                tab.zoom = self.zoomed;
            }
        }
        serde_json::to_string(&book).expect("scene serialization")
    }
    pub fn restore_layout(&mut self, json: &str, _live: &std::collections::HashSet<String>) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            self.set_status("unavailable layout: invalid JSON; original retained");
            return;
        };
        if value.get("version").is_some() {
            match serde_json::from_value::<SceneBook>(value) {
                Ok(book) if book.valid() => {
                    self.scenes = book;
                    self.sidebar_width = self
                        .scenes
                        .sidebar
                        .clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                    self.load_scene();
                    self.layout_dirty = false;
                }
                _ => self
                    .set_status("unavailable layout: invalid scene references; original retained"),
            };
            return;
        }
        let Ok(grid) =
            serde_json::from_value::<PaneNode>(value.get("grid").cloned().unwrap_or_default())
        else {
            self.set_status("unavailable legacy layout; original retained");
            return;
        };
        let mut book = SceneBook {
            legacy: Some(value.clone()),
            ..Default::default()
        };
        // Preserve the old tree, partitioned by terminal ownership; the full
        // original payload remains embedded and is backed up server-side.
        let mut keys = std::collections::BTreeSet::new();
        for c in grid.leaves() {
            if let PaneContent::Terminal(id) = c {
                keys.insert(
                    self.model
                        .terminals
                        .iter()
                        .find(|t| t.terminal_id == id)
                        .and_then(|t| t.worktree_id.clone())
                        .unwrap_or_else(|| FLOATING.into()),
                );
            }
        }
        if keys.is_empty() {
            keys.insert(FLOATING.into());
        }
        for key in keys {
            let mut tree = grid.clone();
            let own = self
                .model
                .terminals
                .iter()
                .filter(|t| t.worktree_id.as_deref().unwrap_or(FLOATING) == key)
                .map(|t| t.terminal_id.clone())
                .collect::<std::collections::HashSet<_>>();
            // Dead/unavailable references stay in floating, never replayed.
            let keep = tree
                .leaves()
                .into_iter()
                .filter_map(|c| match c {
                    PaneContent::Terminal(id)
                        if own.contains(&id)
                            || (key == FLOATING
                                && !self.model.terminals.iter().any(|t| t.terminal_id == id)) =>
                    {
                        Some(id)
                    }
                    _ => None,
                })
                .collect();
            tree.prune_dead_terminals(&keep);
            let mut tab = TabView::new("Migrated".into());
            tab.grid = tree;
            tab.focus = value
                .get("focus")
                .and_then(|f| serde_json::from_value(f.clone()).ok())
                .filter(|f| tab.grid.leaves().contains(f))
                .unwrap_or_else(|| tab.grid.leaves()[0].clone());
            book.scenes.insert(
                key.clone(),
                Scene {
                    active: tab.id.clone(),
                    tabs: vec![tab],
                    ..Scene::default()
                },
            );
            book.selected = key;
        }
        self.scenes = book;
        self.load_scene();
        self.layout_dirty = true;
        self.set_status(
            "legacy layout migrated · original retained · unavailable terminals never replay",
        );
    }

    /// Move pane focus geometrically (Ctrl+Arrows in the keymap).
    pub fn move_pane_focus(&mut self, direction: Direction) -> bool {
        match self.grid.neighbor(
            self.last_pane_area.unwrap_or(PANE_REF_AREA),
            &self.pane_focus,
            direction,
        ) {
            Some(next) => {
                self.pane_focus = next;
                self.focus = Focus::Panes;
                self.layout_dirty = true;
                true
            }
            None => false,
        }
    }

    pub fn cycle_pane(&mut self) {
        let leaves = self.grid.leaves();
        if leaves.len() < 2 {
            return;
        }
        let index = leaves
            .iter()
            .position(|c| *c == self.pane_focus)
            .unwrap_or(0);
        self.pane_focus = leaves[(index + 1) % leaves.len()].clone();
        self.focus = Focus::Panes;
        self.layout_dirty = true;
    }

    /// Locate the original scene/tab/pane, including hidden views. An
    /// unplaced terminal gets a new leaf or tab; neighbors are never replaced.
    pub fn attach_terminal(&mut self, terminal_id: String) {
        self.save_scene();
        if let Some(location) = self.scenes.locate(&terminal_id) {
            self.select_worktree(location.scene_id.clone());
            if let Some(scene) = self.scenes.scenes.get_mut(&location.scene_id) {
                scene.active = location.tab_id.clone();
                if let Some(tab) = scene.tabs.iter_mut().find(|t| t.id == location.tab_id) {
                    tab.hidden = false;
                }
            }
            self.load_scene();
            let content = PaneContent::Terminal(terminal_id.clone());
            if !self.grid.leaves().contains(&content) {
                if self.grid == PaneNode::leaf(PaneContent::Browser) {
                    self.grid = PaneNode::leaf(content.clone());
                } else if !self
                    .grid
                    .split(&self.pane_focus, SplitAxis::Horizontal, content.clone())
                {
                    if !self.new_tab("Recovered terminal".into()) {
                        return;
                    }
                    self.grid = PaneNode::leaf(content.clone());
                }
            }
            if let Some(s) = self.scenes.scenes.get_mut(&self.scenes.selected) {
                for t in &mut s.tabs {
                    t.closed.retain(|id| id != &terminal_id);
                }
            }
            self.pane_focus = content;
            self.layout_dirty = true;
            return;
        }
        let key = self
            .model
            .terminals
            .iter()
            .find(|t| t.terminal_id == terminal_id)
            .and_then(|t| t.worktree_id.clone())
            .unwrap_or_else(|| self.scenes.selected.clone());
        if key != self.scenes.selected {
            self.select_worktree(key);
        }
        let has_tab = self
            .scenes
            .scenes
            .get(&self.scenes.selected)
            .is_some_and(|s| s.tabs.iter().any(|t| t.id == s.active && !t.hidden));
        if !has_tab && !self.new_tab("Terminal".into()) {
            return;
        }
        let content = PaneContent::Terminal(terminal_id);
        if self.grid == PaneNode::leaf(PaneContent::Browser) {
            self.grid = PaneNode::leaf(content.clone());
        } else if !self
            .grid
            .split(&self.pane_focus, SplitAxis::Horizontal, content.clone())
        {
            if !self.new_tab("Terminal".into()) {
                return;
            }
            self.grid = PaneNode::leaf(content.clone());
        }
        self.pane_focus = content;
        self.layout_dirty = true;
    }

    /// Whether the pane tree can take one more leaf (the run loop checks
    /// BEFORE spawning a terminal, so no terminal is ever created that no
    /// pane can hold — QA F6).
    pub fn can_split(&self) -> bool {
        self.grid.leaves().len() < crate::tui::layout::MAX_PANES
    }

    /// Split the focused pane and put a new terminal in the new leaf.
    /// Returns false (with a status message) at the pane cap, or when the
    /// split would crush a pane below the minimum usable size at the last
    /// drawn terminal size — the split is undone in that case (P3).
    pub fn split_pane(&mut self, axis: SplitAxis, terminal_id: String) -> bool {
        self.layout_dirty = true;
        let content = PaneContent::Terminal(terminal_id);
        if self.grid.split(&self.pane_focus, axis, content.clone()) {
            if let Some(area) = self.last_pane_area {
                // Every leaf must stay usable — not just the new one: the
                // ratio split can hand the KEPT (focused) pane the smaller
                // half on odd widths.
                let too_small = self
                    .grid
                    .render_layout_chrome(area)
                    .iter()
                    .any(|(_, r)| r.width < MIN_PANE_WIDTH || r.height < MIN_PANE_HEIGHT);
                if too_small {
                    // Undo: the new leaf collapses back into its sibling.
                    self.grid.close(&content);
                    self.set_status(format!(
                        "pane too small to split at this terminal size (min {MIN_PANE_WIDTH}×{MIN_PANE_HEIGHT})"
                    ));
                    return false;
                }
            }
            let split_target = Self::pane_key(&self.pane_focus);
            let spawned = match &content {
                PaneContent::Terminal(id) => id.clone(),
                PaneContent::Browser => "browser".into(),
            };
            self.pane_focus = content;
            self.queue_tree(crate::server::WorkbenchTreeOp::Split {
                scene_id: Some(self.scenes.selected.clone()),
                tab_id: self.active_tab_id(),
                pane_id: Some(split_target),
                direction: match axis {
                    SplitAxis::Horizontal => "right",
                    SplitAxis::Vertical => "down",
                }
                .into(),
                terminal_id: Some(spawned),
            });
            true
        } else {
            self.set_status(format!(
                "pane limit reached ({} panes) — close one first",
                crate::tui::layout::MAX_PANES
            ));
            false
        }
    }

    /// Close the focused terminal pane. The terminal keeps running in the
    /// server — closing is a view operation, never a stop.
    pub fn close_pane(&mut self) {
        if let PaneContent::Terminal(id) = self.pane_focus.clone() {
            self.save_scene();
            if let Some(scene) = self.scenes.scenes.get_mut(&self.scenes.selected) {
                if let Some(t) = scene.tabs.iter_mut().find(|t| t.id == scene.active) {
                    if !t.closed.contains(&id) {
                        if t.closed.len() >= 256 {
                            self.set_status(
                                "closed-pane reference limit reached; view remains open",
                            );
                            return;
                        }
                        t.closed.push(id.clone());
                    }
                }
            }
            self.layout_dirty = true;
            let removed = if self.grid == PaneNode::leaf(PaneContent::Terminal(id.clone())) {
                self.grid = PaneNode::leaf(PaneContent::Browser);
                Some(PaneContent::Terminal(id))
            } else {
                self.grid.close(&PaneContent::Terminal(id))
            };
            if let Some(removed) = removed {
                if let PaneContent::Terminal(removed_id) = removed {
                    self.snapshots.remove(&removed_id);
                    self.set_status(format!(
                        "pane closed — terminal {removed_id} keeps running server-side"
                    ));
                }
                self.pane_focus = self
                    .grid
                    .leaves()
                    .first()
                    .cloned()
                    .unwrap_or(PaneContent::Browser);
            }
        }
    }

    pub fn toggle_zoom(&mut self) {
        self.zoomed = !self.zoomed;
        self.layout_dirty = true;
    }

    /// The focused terminal's worktree, for split-cwd decisions.
    pub fn focused_worktree_id(&self) -> Option<String> {
        if let PaneContent::Terminal(id) = &self.pane_focus {
            return self
                .model
                .terminals
                .iter()
                .find(|t| t.terminal_id == *id)?
                .worktree_id
                .clone();
        }
        if self.scenes.selected != FLOATING {
            Some(self.scenes.selected.clone())
        } else {
            None
        }
    }

    /// Explicitly enter terminal-forwarding mode (run loop decides when —
    /// usually the Enter key on a terminals row, executed as
    /// [`WorkbenchAction::EnterTerminal`]).
    pub fn enter_terminal_mode(&mut self, terminal_id: impl Into<String>) {
        self.focus = Focus::Panes;
        self.remote_terminal = None;
        self.terminal_mode = Some(terminal_id.into());
        self.status_line = "terminal · only the prefix key is intercepted".into();
    }

    pub fn leave_terminal_mode(&mut self) {
        self.terminal_mode = None;
        self.remote_terminal = None;
        self.copy_mode = false;
        self.prefix_pending = false;
        self.status_line = "terminal released".into();
    }

    /// While keyboard-forwarding is active, keep the PTY target aligned with
    /// pane focus after prefix focus/cycle moves (swap intentionally does not
    /// retarget — input stays on the terminal that had focus before the swap).
    fn sync_terminal_mode_to_pane_focus(&mut self) {
        if self.terminal_mode.is_none() {
            return;
        }
        match &self.pane_focus {
            PaneContent::Terminal(id) => {
                if self.terminal_mode.as_deref() != Some(id.as_str()) {
                    self.enter_terminal_mode(id.clone());
                }
            }
            PaneContent::Browser => self.leave_terminal_mode(),
        }
    }

    fn clear_prefix(&mut self) {
        self.prefix_pending = false;
        if self.status_line.starts_with("prefix mode") {
            self.status_line = DEFAULT_STATUS_HINT.into();
        }
    }

    fn arm_prefix(&mut self) {
        self.prefix_pending = true;
        self.set_status("prefix mode · ? help · next key is a workbench action");
    }

    fn switch_tab_index(&mut self, index: usize) -> KeyOutcome {
        let tabs = self
            .scenes
            .scenes
            .get(&self.scenes.selected)
            .map(|s| {
                s.tabs
                    .iter()
                    .filter(|t| !t.hidden)
                    .map(|t| t.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if tabs.is_empty() {
            return KeyOutcome::Ignored;
        }
        let i = index.min(tabs.len().saturating_sub(1));
        self.switch_tab(&tabs[i]);
        KeyOutcome::Handled
    }

    fn focus_dir(&mut self, direction: Direction) -> KeyOutcome {
        if self.move_pane_focus(direction) {
            self.queue_focus(&self.pane_focus.clone());
            self.sync_terminal_mode_to_pane_focus();
            KeyOutcome::Handled
        } else {
            KeyOutcome::Ignored
        }
    }

    fn prompt(&mut self, action: &'static str) -> KeyOutcome {
        self.workspace_prompt = Some(WorkspacePrompt {
            action,
            text: String::new(),
            pick: None,
        });
        KeyOutcome::Handled
    }

    fn enter_copy_mode(&mut self) {
        let lines = self.focused_copy_lines();
        self.copy_mode = true;
        self.copy_state = copy_mode::CopyMode::default();
        self.copy_state.place_at_end(&lines);
    }

    fn focused_copy_lines(&self) -> Vec<String> {
        let id = match self.terminal_mode.as_deref() {
            Some(id) => id,
            None => match &self.pane_focus {
                PaneContent::Terminal(id) => id.as_str(),
                PaneContent::Browser => return Vec::new(),
            },
        };
        self.snapshots
            .get(id)
            .map(|snap| {
                snap.scrollback
                    .iter()
                    .chain(snap.visible.iter())
                    .cloned()
                    .collect()
            })
            .unwrap_or_default()
    }

    fn apply_action(&mut self, action: keys::Action) -> KeyOutcome {
        use keys::Action;
        match action {
            Action::Help => {
                self.show_key_help = !self.show_key_help;
                self.help_filter.clear();
                self.help_filtering = false;
                KeyOutcome::Handled
            }
            Action::Settings => {
                self.settings_open = true;
                KeyOutcome::Handled
            }
            Action::Detach => {
                self.quit_requested = true;
                KeyOutcome::QuitRequested
            }
            Action::NewTab => KeyOutcome::Action(WorkbenchAction::NewTab),
            Action::SplitVertical => KeyOutcome::Action(WorkbenchAction::SplitRight),
            Action::SplitHorizontal => KeyOutcome::Action(WorkbenchAction::SplitBelow),
            Action::FocusPaneLeft | Action::NavigatePaneLeft => self.focus_dir(Direction::Left),
            Action::FocusPaneDown | Action::NavigatePaneDown => self.focus_dir(Direction::Down),
            Action::FocusPaneUp | Action::NavigatePaneUp => self.focus_dir(Direction::Up),
            Action::FocusPaneRight | Action::NavigatePaneRight => self.focus_dir(Direction::Right),
            Action::WorkspacePicker => {
                self.navigate = true;
                self.prefix_pending = false;
                self.workspace_nav = self
                    .model
                    .workspaces
                    .iter()
                    .position(|w| Some(&w.workspace_id) == self.model.workspace_id.as_ref())
                    .unwrap_or(0);
                KeyOutcome::Handled
            }
            Action::NewWorkspace => self.prompt("new"),
            Action::RenameWorkspace => self.prompt("rename"),
            Action::NewWorktree => self.prompt("worktree"),
            Action::OpenWorktree => self.prompt("open"),
            Action::CloseWorkspace => KeyOutcome::Action(WorkbenchAction::Workspace {
                action: "close".into(),
                value: None,
                project_id: None,
            }),
            Action::RemoveWorktree => {
                let project_id = self
                    .model
                    .worktrees
                    .get(self.selected)
                    .map(|w| w.project_id.clone());
                KeyOutcome::Action(WorkbenchAction::Workspace {
                    action: "remove".into(),
                    value: None,
                    project_id,
                })
            }
            Action::Goto => {
                self.goto = Some(GotoPicker {
                    query: String::new(),
                    searching: false,
                    cursor: 0,
                    kind: None,
                });
                KeyOutcome::Handled
            }
            Action::ToggleSidebar => {
                self.sidebar_collapsed = !self.sidebar_collapsed;
                self.layout_dirty = true;
                KeyOutcome::Handled
            }
            Action::NextTab => self.cycle_tab_relative(1),
            Action::PreviousTab => self.cycle_tab_relative(-1),
            Action::SwitchTab(n) => self.switch_tab_index(n.saturating_sub(1) as usize),
            Action::SwitchWorkspace(n) => self.switch_workspace_index(n.saturating_sub(1) as usize),
            Action::RenameTab => {
                self.tab_prompt = Some(String::new());
                KeyOutcome::Handled
            }
            Action::CloseTab => {
                let tab_id = self.active_tab_id().unwrap_or_default();
                self.close_tab();
                self.queue_tree(crate::server::WorkbenchTreeOp::CloseTab {
                    scene_id: Some(self.scenes.selected.clone()),
                    tab_id,
                });
                KeyOutcome::Handled
            }
            Action::CyclePaneNext => {
                self.cycle_pane();
                self.queue_focus(&self.pane_focus.clone());
                self.sync_terminal_mode_to_pane_focus();
                KeyOutcome::Handled
            }
            Action::CyclePanePrevious => {
                let leaves = self.grid.leaves();
                if leaves.len() < 2 {
                    return KeyOutcome::Ignored;
                }
                let index = leaves
                    .iter()
                    .position(|c| *c == self.pane_focus)
                    .unwrap_or(0);
                self.pane_focus = leaves[(index + leaves.len() - 1) % leaves.len()].clone();
                self.focus = Focus::Panes;
                self.layout_dirty = true;
                self.queue_focus(&self.pane_focus.clone());
                self.sync_terminal_mode_to_pane_focus();
                KeyOutcome::Handled
            }
            Action::ClosePane => {
                let pane_id = Self::pane_key(&self.pane_focus);
                self.close_pane();
                self.queue_tree(crate::server::WorkbenchTreeOp::ClosePane {
                    scene_id: Some(self.scenes.selected.clone()),
                    tab_id: self.active_tab_id(),
                    pane_id,
                });
                KeyOutcome::Handled
            }
            Action::Zoom => {
                self.toggle_zoom();
                self.queue_tree(crate::server::WorkbenchTreeOp::Zoom {
                    scene_id: Some(self.scenes.selected.clone()),
                    tab_id: self.active_tab_id(),
                    on: Some(self.zoomed),
                });
                KeyOutcome::Handled
            }
            Action::ResizeMode => {
                self.resize_mode = !self.resize_mode;
                KeyOutcome::Handled
            }
            Action::ResizePaneLeft => self.nudge_split(-5, true),
            Action::ResizePaneRight => self.nudge_split(5, true),
            Action::ResizePaneUp => self.nudge_split(-5, false),
            Action::ResizePaneDown => self.nudge_split(5, false),
            Action::CopyMode => {
                if matches!(self.pane_focus, PaneContent::Terminal(_))
                    || self.terminal_mode.is_some()
                {
                    if self.copy_mode {
                        self.copy_mode = false;
                    } else {
                        self.enter_copy_mode();
                    }
                } else {
                    self.set_status("copy mode needs a focused terminal pane");
                }
                KeyOutcome::Handled
            }
            Action::RenamePane => {
                self.pane_prompt = Some(String::new());
                KeyOutcome::Handled
            }
            Action::EditScrollback => self.edit_scrollback(),
            Action::ClearPane => {
                self.set_status("clear pane is not bound by default");
                KeyOutcome::Handled
            }
            Action::ReloadConfig => self.reload_config(),
            Action::OpenNotificationTarget => self.open_notification(),
            Action::NavigateWorkspaceUp | Action::PreviousWorkspace => self.step_workspace(-1),
            Action::NavigateWorkspaceDown | Action::NextWorkspace => self.step_workspace(1),
            Action::PreviousAgent => self.step_agent(-1),
            Action::NextAgent => self.step_agent(1),
            Action::FocusAgent(n) => self.focus_agent_index(n.saturating_sub(1) as usize),
            Action::MoveTabPrevious => self.move_tab(-1),
            Action::MoveTabNext => self.move_tab(1),
            Action::LastPane => {
                self.cycle_pane();
                self.queue_focus(&self.pane_focus.clone());
                self.sync_terminal_mode_to_pane_focus();
                KeyOutcome::Handled
            }
            Action::SwapPaneLeft => self.swap_pane(Direction::Left),
            Action::SwapPaneDown => self.swap_pane(Direction::Down),
            Action::SwapPaneUp => self.swap_pane(Direction::Up),
            Action::SwapPaneRight => self.swap_pane(Direction::Right),
            Action::Command(id) => self.run_command(&id),
        }
    }

    fn step_workspace(&mut self, delta: isize) -> KeyOutcome {
        let len = self.model.workspaces.len();
        if len == 0 {
            return KeyOutcome::Ignored;
        }
        let next = self.workspace_nav as isize + delta;
        self.workspace_nav = next.clamp(0, len as isize - 1) as usize;
        let Some(id) = self
            .model
            .workspaces
            .get(self.workspace_nav)
            .map(|w| w.workspace_id.clone())
        else {
            return KeyOutcome::Ignored;
        };
        KeyOutcome::Action(Self::select_workspace(Some(id)))
    }

    fn switch_workspace_index(&mut self, index: usize) -> KeyOutcome {
        let Some(id) = self
            .model
            .workspaces
            .get(index)
            .map(|w| w.workspace_id.clone())
        else {
            return KeyOutcome::Ignored;
        };
        self.workspace_nav = index;
        KeyOutcome::Action(Self::select_workspace(Some(id)))
    }

    fn step_agent(&mut self, delta: isize) -> KeyOutcome {
        let len = self.agent_rows().len();
        if len == 0 {
            return KeyOutcome::Ignored;
        }
        self.focus = Focus::Terminals;
        let next = self.selected as isize + delta;
        self.selected = next.clamp(0, len as isize - 1) as usize;
        KeyOutcome::Handled
    }

    fn focus_agent_index(&mut self, index: usize) -> KeyOutcome {
        let rows = self.agent_rows();
        if index >= rows.len() {
            return KeyOutcome::Ignored;
        }
        let id = rows[index].terminal_id.clone();
        self.focus = Focus::Terminals;
        self.selected = index;
        KeyOutcome::Action(WorkbenchAction::EnterTerminal(id))
    }

    fn move_tab(&mut self, delta: isize) -> KeyOutcome {
        let Some(scene) = self.scenes.scenes.get_mut(&self.scenes.selected) else {
            return KeyOutcome::Ignored;
        };
        let Some(index) = scene.tabs.iter().position(|t| t.id == scene.active) else {
            return KeyOutcome::Ignored;
        };
        let next = index as isize + delta;
        if next < 0 || next >= scene.tabs.len() as isize {
            return KeyOutcome::Ignored;
        }
        scene.tabs.swap(index, next as usize);
        self.layout_dirty = true;
        KeyOutcome::Handled
    }

    fn nudge_split(&mut self, delta: i16, horizontal: bool) -> KeyOutcome {
        let area = self.last_pane_area.unwrap_or(PANE_REF_AREA);
        let Some(hit) = self
            .grid
            .splits(area)
            .into_iter()
            .find(|hit| horizontal == matches!(hit.axis, SplitAxis::Horizontal))
        else {
            return KeyOutcome::Ignored;
        };
        let ratio = (hit.pos as i16).clamp(10, 90);
        let _ = ratio;
        let current = 50i16;
        let next = (current + delta).clamp(10, 90) as u16;
        if self.grid.set_ratio_at_path(&hit.path, next) {
            self.layout_dirty = true;
            self.queue_tree(crate::server::WorkbenchTreeOp::Resize {
                scene_id: Some(self.scenes.selected.clone()),
                tab_id: self.active_tab_id(),
                path: hit.path,
                ratio: next,
            });
            KeyOutcome::Handled
        } else {
            KeyOutcome::Ignored
        }
    }

    fn edit_scrollback(&mut self) -> KeyOutcome {
        let lines = self.focused_copy_lines();
        let id = self
            .terminal_mode
            .clone()
            .or_else(|| match &self.pane_focus {
                PaneContent::Terminal(id) => Some(id.clone()),
                PaneContent::Browser => None,
            });
        let Some(id) = id else {
            self.set_status("edit scrollback needs a focused terminal");
            return KeyOutcome::Handled;
        };
        let path = std::env::temp_dir().join(format!("viva-scrollback-{id}.txt"));
        if std::fs::write(&path, lines.join("\n")).is_err() {
            self.set_status("could not write scrollback");
            return KeyOutcome::Handled;
        }
        self.scrollback_edit = Some(path.display().to_string());
        self.set_status("scrollback written; the pane keeps running");
        KeyOutcome::Handled
    }

    fn reload_config(&mut self) -> KeyOutcome {
        let Some(path) = self.config_path.clone() else {
            self.keymap = keys::KeyMap::builtin();
            self.set_status("no config file; defaults restored");
            return KeyOutcome::Handled;
        };
        let text = std::fs::read_to_string(&path).unwrap_or_default();
        self.keymap = keys::KeyMap::load_str(&text);
        self.onboarding_pending = !self.keymap.onboarding_done();
        if self.keymap.warnings().is_empty() {
            self.set_status(format!("reloaded {}", path.display()));
        } else {
            self.set_status(self.keymap.warnings().join("; "));
        }
        KeyOutcome::Handled
    }

    fn open_notification(&mut self) -> KeyOutcome {
        if let Some(marker) = self.model.attention.first() {
            self.focus = Focus::Terminals;
            self.set_status(format!("notification: {}: {}", marker.kind, marker.detail));
            return KeyOutcome::Handled;
        }
        if let Some(index) = self
            .agent_rows()
            .iter()
            .position(|row| row.needs_attention())
        {
            self.focus = Focus::Terminals;
            self.selected = index;
            self.set_status("notification target focused");
            return KeyOutcome::Handled;
        }
        self.set_status("no notification target");
        KeyOutcome::Handled
    }

    fn run_command(&mut self, id: &str) -> KeyOutcome {
        let worktree = self.focused_worktree_id();
        match id {
            "viva.diff" => match worktree {
                Some(id) => KeyOutcome::Action(WorkbenchAction::ShowDiff(id)),
                None => {
                    self.set_status("diff needs a worktree");
                    KeyOutcome::Handled
                }
            },
            "viva.files" => match worktree {
                Some(id) => KeyOutcome::Action(WorkbenchAction::BrowseFiles(id)),
                None => {
                    self.set_status("files need a worktree");
                    KeyOutcome::Handled
                }
            },
            "viva.handoff" => match worktree {
                Some(id) => {
                    self.handoff_picker = Some(HandoffPicker {
                        worktree_id: id,
                        agents: Vec::new(),
                        selected: 0,
                    });
                    KeyOutcome::Handled
                }
                None => {
                    self.set_status("handoff needs a worktree");
                    KeyOutcome::Handled
                }
            },
            "viva.auto-pull" => KeyOutcome::Action(WorkbenchAction::ToggleAutoPull),
            "viva.cleanup" => {
                self.cleanup_armed = worktree;
                self.set_status(if self.cleanup_armed.is_some() {
                    "cleanup armed · Enter confirms · Esc cancels"
                } else {
                    "cleanup needs a worktree"
                });
                KeyOutcome::Handled
            }
            other => {
                self.set_status(format!("command {other}"));
                KeyOutcome::Handled
            }
        }
    }

    fn finish_onboarding(&mut self) {
        self.settings_open = false;
        self.onboarding_pending = false;
        if let Some(path) = &self.config_path {
            if let Err(err) = keys::write_onboarding_false(path) {
                self.set_status(format!("could not write onboarding flag: {err}"));
                return;
            }
            self.keymap = keys::KeyMap::load_str(
                &std::fs::read_to_string(path).unwrap_or_else(|_| "onboarding = false\n".into()),
            );
        }
        self.set_status("settings saved");
    }

    fn cycle_tab_relative(&mut self, delta: i32) -> KeyOutcome {
        let tabs = self
            .scenes
            .scenes
            .get(&self.scenes.selected)
            .map(|s| {
                s.tabs
                    .iter()
                    .filter(|t| !t.hidden)
                    .map(|t| t.id.clone())
                    .collect::<Vec<_>>()
            })
            .unwrap_or_default();
        if tabs.is_empty() {
            return KeyOutcome::Ignored;
        }
        let current = self.scenes.scenes[&self.scenes.selected].active.clone();
        let i = tabs.iter().position(|t| t == &current).unwrap_or(0);
        let len = tabs.len() as i32;
        let j = ((i as i32 + delta).rem_euclid(len)) as usize;
        self.switch_tab(&tabs[j]);
        KeyOutcome::Handled
    }

    fn swap_pane(&mut self, direction: Direction) -> KeyOutcome {
        let area = self.last_pane_area.unwrap_or(PANE_REF_AREA);
        if self
            .grid
            .swap_with_neighbor(area, &self.pane_focus, direction)
        {
            self.layout_dirty = true;
            KeyOutcome::Handled
        } else {
            KeyOutcome::Ignored
        }
    }

    fn handle_prefix_follow(&mut self, key: KeyEvent) -> KeyOutcome {
        self.clear_prefix();
        if let Some(action) = self.keymap.resolve(true, self.navigate, key) {
            self.apply_action(action)
        } else {
            self.set_status("unknown prefix key");
            KeyOutcome::Ignored
        }
    }

    fn on_navigation_surface_key(&mut self, key: KeyEvent) -> KeyOutcome {
        use ratatui::crossterm::event::KeyModifiers;
        if self.keymap.is_prefix(key) {
            self.arm_prefix();
            return KeyOutcome::Handled;
        }
        if self.prefix_pending {
            return self.handle_prefix_follow(key);
        }
        if self.navigate {
            if matches!(key.code, KeyCode::Esc)
                || (key.modifiers.contains(KeyModifiers::CONTROL)
                    && matches!(key.code, KeyCode::Char('[')))
            {
                self.navigate = false;
                return KeyOutcome::Handled;
            }
            if key.code == KeyCode::Char('/') && key.modifiers.is_empty() {
                return self.prompt("open");
            }
            if let Some(action) = self.keymap.resolve(false, true, key) {
                return self.apply_action(action);
            }
            return KeyOutcome::Ignored;
        }
        if self.show_key_help && matches!(key.code, KeyCode::Esc | KeyCode::Char('?')) {
            self.show_key_help = false;
            self.help_filtering = false;
            return KeyOutcome::Handled;
        }
        if self.focus == Focus::Panes && matches!(key.code, KeyCode::Enter) {
            if let PaneContent::Terminal(id) = &self.pane_focus {
                self.enter_terminal_mode(id.clone());
                return KeyOutcome::Handled;
            }
        }
        if matches!(
            self.focus,
            Focus::Worktrees | Focus::Terminals | Focus::Tasks
        ) {
            match key.code {
                KeyCode::Down | KeyCode::Char('j') if self.row_count() > 0 => {
                    self.selected = (self.selected + 1).min(self.row_count() - 1);
                    return KeyOutcome::Handled;
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.selected = self.selected.saturating_sub(1);
                    return KeyOutcome::Handled;
                }
                KeyCode::Enter if self.focus == Focus::Terminals => {
                    return self.activate_agent();
                }
                KeyCode::Enter if self.focus == Focus::Worktrees => {
                    return self.activate_space();
                }
                KeyCode::Tab => {
                    self.cycle_pane();
                    return KeyOutcome::Handled;
                }
                _ => {}
            }
        }
        KeyOutcome::Ignored
    }

    fn enter_remote_terminal(&mut self, machine_id: String, terminal_id: String) {
        self.focus = Focus::Panes;
        self.terminal_mode = Some(terminal_id.clone());
        self.remote_terminal = Some((machine_id, terminal_id));
        self.copy_mode = false;
        self.prefix_pending = false;
        self.status_line =
            "remote terminal · keys go to that machine · this computer's socket is unchanged"
                .into();
    }

    fn row_count(&self) -> usize {
        match self.focus {
            Focus::Worktrees => self.model.worktrees.len() + self.machine_workspaces().len(),
            Focus::Tasks => self.model.tasks.len(),
            Focus::Terminals => self.agent_rows().len() + self.machine_agents().len(),
            Focus::Panes => 0,
        }
    }

    fn clamp_selection(&mut self) {
        let len = self.row_count();
        self.selected = if len == 0 {
            0
        } else {
            self.selected.min(len - 1)
        };
    }

    fn agent_rows(&self) -> Vec<&crate::agents::AgentSessionRow> {
        self.model
            .agents
            .iter()
            .filter(|a| {
                if self.global_attention {
                    a.needs_attention()
                } else {
                    a.in_workspace
                }
            })
            .collect()
    }
    fn selected_terminal(&self) -> Option<String> {
        self.agent_rows()
            .get(self.selected)
            .map(|a| a.terminal_id.clone())
    }
    pub fn take_mouse_action(&mut self) -> Option<WorkbenchAction> {
        self.mouse_action.take()
    }

    fn selected_worktree(&self) -> Option<String> {
        self.model
            .worktrees
            .get(self.selected)
            .map(|w| w.worktree_id.clone())
    }

    fn machine_workspaces(&self) -> Vec<MachineSpace> {
        let mut rows = Vec::new();
        for machine in &self.model.machines {
            for workspace in &machine.workspaces {
                rows.push(MachineSpace {
                    machine_id: machine.id.clone(),
                    label: machine.label.clone(),
                    connected: machine.connected && machine.enabled,
                    enabled: machine.enabled,
                    workspace_id: workspace.workspace_id.clone(),
                    name: workspace.name.clone(),
                });
            }
        }
        rows
    }

    fn machine_agents(&self) -> Vec<MachineAgent> {
        let mut rows = Vec::new();
        for machine in &self.model.machines {
            if machine.id == crate::machines::LOCAL_MACHINE_ID {
                continue;
            }
            for agent in &machine.agents {
                rows.push(MachineAgent {
                    machine_id: machine.id.clone(),
                    label: machine.label.clone(),
                    connected: machine.connected && machine.enabled,
                    terminal_id: agent.terminal_id.clone(),
                    name: agent.name.clone(),
                });
            }
        }
        rows
    }

    fn machine_workspace_row(&self, selected: usize) -> Option<MachineSpace> {
        let offset = selected.checked_sub(self.model.worktrees.len())?;
        self.machine_workspaces().get(offset).cloned()
    }

    fn machine_agent_row(&self, selected: usize) -> Option<MachineAgent> {
        let offset = selected.checked_sub(self.agent_rows().len())?;
        self.machine_agents().get(offset).cloned()
    }

    fn machine_accepts_input(&self, id: &str) -> bool {
        self.model
            .machines
            .iter()
            .any(|machine| machine.id == id && machine.connected && machine.enabled)
    }

    /// Drop remote keyboard forwarding when that machine can no longer take input.
    fn blocked_remote(&mut self) -> bool {
        let Some((machine, _)) = self.remote_terminal.clone() else {
            return false;
        };
        if self.machine_accepts_input(&machine) {
            return false;
        }
        let label = self
            .model
            .machines
            .iter()
            .find(|view| view.id == machine)
            .map(|view| view.label.clone())
            .unwrap_or(machine);
        self.terminal_mode = None;
        self.remote_terminal = None;
        self.set_status(format!("{label} is disconnected; input is refused"));
        true
    }

    fn activate_space(&mut self) -> KeyOutcome {
        if let Some(row) = self.machine_workspace_row(self.selected) {
            if row.machine_id == crate::machines::LOCAL_MACHINE_ID {
                return KeyOutcome::Action(WorkbenchAction::Workspace {
                    action: "select".into(),
                    value: Some(row.workspace_id),
                    project_id: None,
                });
            }
            if !row.connected {
                let why = if row.enabled {
                    "disconnected"
                } else {
                    "disabled"
                };
                self.set_status(format!("{} is {why}; input is refused", row.label));
                return KeyOutcome::Handled;
            }
            return KeyOutcome::Action(WorkbenchAction::UseMachine {
                machine_id: row.machine_id,
                workspace_id: Some(row.workspace_id),
                terminal_id: None,
            });
        }
        match self.selected_worktree() {
            Some(id) => KeyOutcome::Action(WorkbenchAction::SelectWorktree(id)),
            None => KeyOutcome::Ignored,
        }
    }

    fn activate_agent(&mut self) -> KeyOutcome {
        if let Some(row) = self.machine_agent_row(self.selected) {
            if !row.connected {
                self.set_status(format!("{} is disconnected; input is refused", row.label));
                return KeyOutcome::Handled;
            }
            return KeyOutcome::Action(WorkbenchAction::UseMachine {
                machine_id: row.machine_id,
                workspace_id: None,
                terminal_id: Some(row.terminal_id),
            });
        }
        match self.selected_terminal() {
            Some(id) => KeyOutcome::Action(WorkbenchAction::EnterTerminal(id)),
            None => KeyOutcome::Ignored,
        }
    }

    fn activate_space_or_agent(&mut self) -> KeyOutcome {
        match self.focus {
            Focus::Worktrees => self.activate_space(),
            Focus::Terminals => self.activate_agent(),
            Focus::Tasks | Focus::Panes => KeyOutcome::Ignored,
        }
    }

    /// Handle one key event.
    pub fn on_key(&mut self, key: KeyEvent) -> KeyOutcome {
        use ratatui::crossterm::event::KeyEventKind;
        // Releases never drive workbench shortcuts. In a terminal pane they
        // are forwarded only when the program asked for key-release events.
        if key.kind == KeyEventKind::Release {
            if self.terminal_mode.is_none() {
                return KeyOutcome::Ignored;
            }
            return match terminal_view::key(key, &self.input_modes()) {
                Some(bytes) => KeyOutcome::Forward(bytes),
                None => KeyOutcome::Ignored,
            };
        }
        if let Some(index) = &mut self.terminal_picker {
            match key.code {
                KeyCode::Esc => self.terminal_picker = None,
                KeyCode::Up => *index = index.saturating_sub(1),
                KeyCode::Down => {
                    *index = (*index + 1).min(self.model.terminals.len().saturating_sub(1))
                }
                KeyCode::Enter | KeyCode::Char('s') => {
                    let id = self
                        .model
                        .terminals
                        .get(*index)
                        .map(|t| t.terminal_id.clone());
                    self.terminal_picker = None;
                    if let Some(id) = id {
                        return KeyOutcome::Action(if key.code == KeyCode::Enter {
                            WorkbenchAction::EnterTerminal(id)
                        } else {
                            WorkbenchAction::StopTerminal(id)
                        });
                    }
                }
                _ => {}
            }
            return KeyOutcome::Handled;
        }
        if let Some(text) = &mut self.tab_prompt {
            match key.code {
                KeyCode::Esc => self.tab_prompt = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if text.len() < 252 => text.push(c),
                KeyCode::Enter => {
                    let name = text.trim().to_string();
                    self.tab_prompt = None;
                    if !name.is_empty() {
                        self.save_scene();
                        if let Some(s) = self.scenes.scenes.get_mut(&self.scenes.selected) {
                            if let Some(t) = s.tabs.iter_mut().find(|t| t.id == s.active) {
                                t.name = name;
                                self.layout_dirty = true;
                            }
                        }
                    }
                }
                _ => {}
            }
            return KeyOutcome::Handled;
        }
        if self.workspace_prompt.is_some() {
            return self.on_workspace_prompt_key(key);
        }
        if let Some(text) = &mut self.pane_prompt {
            match key.code {
                KeyCode::Esc => self.pane_prompt = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) if text.len() < 252 => text.push(c),
                KeyCode::Enter => {
                    let name = text.trim().to_string();
                    self.pane_prompt = None;
                    if !name.is_empty() {
                        let pane_id = Self::pane_key(&self.pane_focus);
                        if let Some(scene) = self.scenes.scenes.get_mut(&self.scenes.selected) {
                            if let Some(tab) = scene.tabs.iter_mut().find(|t| t.id == scene.active)
                            {
                                tab.pane_names.insert(pane_id.clone(), name.clone());
                            }
                        }
                        self.layout_dirty = true;
                        self.queue_tree(crate::server::WorkbenchTreeOp::RenamePane {
                            scene_id: Some(self.scenes.selected.clone()),
                            tab_id: self.active_tab_id(),
                            pane_id,
                            name,
                        });
                    }
                }
                _ => {}
            }
            return KeyOutcome::Handled;
        }
        if self.cleanup_armed.is_some() {
            if key.code == KeyCode::Enter {
                let id = self.cleanup_armed.take().unwrap();
                return KeyOutcome::Action(WorkbenchAction::CleanupWorktree(id));
            }
            if key.code == KeyCode::Esc {
                self.cleanup_armed = None;
                self.set_status("cleanup cancelled");
                return KeyOutcome::Handled;
            }
            self.cleanup_armed = None;
        }
        if self.settings_open {
            if matches!(key.code, KeyCode::Enter | KeyCode::Esc) {
                self.finish_onboarding();
            }
            return KeyOutcome::Handled;
        }
        if self.context_menu.is_some() {
            return self.on_menu_key(key);
        }
        if self.goto.is_some() {
            return self.on_goto_key(key);
        }
        if self.show_key_help {
            return self.on_help_key(key);
        }
        // Overlays own the keyboard first (V15-1 file panel).
        if self.file_panel.is_some() {
            let viewing = self.file_panel.as_ref().unwrap().viewing.is_some();
            return match key.code {
                KeyCode::Esc => {
                    let panel = self.file_panel.as_mut().unwrap();
                    if viewing {
                        panel.viewing = None;
                        panel.full_path = None;
                        panel.scroll = 0;
                        self.file_view = None;
                        self.set_status("back to the file list");
                    } else {
                        self.file_panel = None;
                        self.file_view = None;
                        self.set_status("file panel closed");
                    }
                    KeyOutcome::Handled
                }
                KeyCode::Up | KeyCode::Char('k') if !viewing => {
                    let panel = self.file_panel.as_mut().unwrap();
                    panel.selected = panel.selected.saturating_sub(1);
                    KeyOutcome::Handled
                }
                KeyCode::Down | KeyCode::Char('j') if !viewing => {
                    let panel = self.file_panel.as_mut().unwrap();
                    if panel.selected + 1 < panel.rows.len() {
                        panel.selected += 1;
                    }
                    KeyOutcome::Handled
                }
                KeyCode::Enter if !viewing => {
                    let panel = self.file_panel.as_ref().unwrap();
                    match panel.rows.get(panel.selected) {
                        Some((path, _)) => {
                            let (worktree_id, path) = (panel.worktree_id.clone(), path.clone());
                            KeyOutcome::Action(WorkbenchAction::ViewFile { worktree_id, path })
                        }
                        None => KeyOutcome::Ignored,
                    }
                }
                KeyCode::Char('e') if viewing => {
                    let full_path = self
                        .file_panel
                        .as_ref()
                        .unwrap()
                        .full_path
                        .clone()
                        .unwrap_or_default();
                    KeyOutcome::Action(WorkbenchAction::OpenInEditor { full_path })
                }
                // V15 QA F6 residual: scroll the viewed material.
                KeyCode::Up if viewing => {
                    if let Some(panel) = self.file_panel.as_mut() {
                        panel.scroll = panel.scroll.saturating_sub(1);
                    }
                    KeyOutcome::Handled
                }
                KeyCode::Down if viewing => {
                    if let Some(panel) = self.file_panel.as_mut() {
                        panel.scroll = panel.scroll.saturating_add(1);
                    }
                    KeyOutcome::Handled
                }
                KeyCode::PageUp if viewing => {
                    if let Some(panel) = self.file_panel.as_mut() {
                        panel.scroll = panel.scroll.saturating_sub(20);
                    }
                    KeyOutcome::Handled
                }
                KeyCode::PageDown if viewing => {
                    if let Some(panel) = self.file_panel.as_mut() {
                        panel.scroll = panel.scroll.saturating_add(20);
                    }
                    KeyOutcome::Handled
                }
                _ => KeyOutcome::Ignored,
            };
        }
        // V15-3 handoff picker.
        if self.handoff_picker.is_some() {
            return match key.code {
                KeyCode::Esc => {
                    self.handoff_picker = None;
                    self.set_status("handoff cancelled");
                    KeyOutcome::Handled
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    let picker = self.handoff_picker.as_mut().unwrap();
                    picker.selected = picker.selected.saturating_sub(1);
                    KeyOutcome::Handled
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    let picker = self.handoff_picker.as_mut().unwrap();
                    if picker.selected + 1 < picker.agents.len() {
                        picker.selected += 1;
                    }
                    KeyOutcome::Handled
                }
                KeyCode::Enter => {
                    let picker = self.handoff_picker.as_ref().unwrap();
                    if picker.agents.is_empty() {
                        self.handoff_picker = None;
                        self.set_status("no handoff targets in harness matrix");
                        return KeyOutcome::Handled;
                    }
                    let (worktree_id, agent) = (
                        picker.worktree_id.clone(),
                        picker.agents[picker.selected].clone(),
                    );
                    self.handoff_picker = None;
                    KeyOutcome::Action(WorkbenchAction::HandoffTo { worktree_id, agent })
                }
                _ => KeyOutcome::Ignored,
            };
        }
        // QA F4: the diff overlay is modal. It used to be unclosable (the
        // setter only ever set it) and unscrollable (the truncation note
        // sat below the fold) — Esc closes, ↑↓/PageUp/PageDown scroll.
        if self.diff_view.is_some() {
            return match key.code {
                KeyCode::Esc => {
                    self.diff_view = None;
                    self.diff_scroll = 0;
                    self.set_status("diff closed");
                    KeyOutcome::Handled
                }
                KeyCode::Up | KeyCode::Char('k') => {
                    self.diff_scroll = self.diff_scroll.saturating_sub(1);
                    KeyOutcome::Handled
                }
                KeyCode::Down | KeyCode::Char('j') => {
                    self.diff_scroll = self.diff_scroll.saturating_add(1);
                    KeyOutcome::Handled
                }
                KeyCode::PageUp => {
                    self.diff_scroll = self.diff_scroll.saturating_sub(20);
                    KeyOutcome::Handled
                }
                KeyCode::PageDown => {
                    self.diff_scroll = self.diff_scroll.saturating_add(20);
                    KeyOutcome::Handled
                }
                _ => KeyOutcome::Ignored,
            };
        }
        if self.navigate || self.terminal_mode.is_some() || self.copy_mode || self.resize_mode {
            if self.terminal_mode.is_some() && self.blocked_remote() {
                return KeyOutcome::Handled;
            }
            if self.keymap.is_prefix(key) {
                self.arm_prefix();
                return KeyOutcome::Handled;
            }
            if self.prefix_pending {
                return self.handle_prefix_follow(key);
            }
            if self.navigate {
                return self.on_navigation_surface_key(key);
            }
            if self.copy_mode {
                let lines = self.focused_copy_lines();
                let page = self
                    .last_pane_area
                    .map(|area| area.height as usize)
                    .unwrap_or(20);
                return match self.copy_state.on_key(key, &lines, page) {
                    copy_mode::CopyEffect::Exit => {
                        self.copy_mode = false;
                        KeyOutcome::Handled
                    }
                    copy_mode::CopyEffect::Copied(text) => {
                        self.copy_to_clipboard(&text);
                        self.copy_mode = false;
                        KeyOutcome::Handled
                    }
                    copy_mode::CopyEffect::Stay => KeyOutcome::Handled,
                };
            }
            if self.resize_mode {
                if key.code == KeyCode::Esc {
                    self.resize_mode = false;
                    return KeyOutcome::Handled;
                }
                if let Some(action) = self.keymap.resolve(false, false, key) {
                    if matches!(
                        action,
                        keys::Action::ResizePaneLeft
                            | keys::Action::ResizePaneRight
                            | keys::Action::ResizePaneUp
                            | keys::Action::ResizePaneDown
                    ) {
                        return self.apply_action(action);
                    }
                }
                return KeyOutcome::Ignored;
            }
            if let Some(action) = self.keymap.resolve(false, false, key) {
                return self.apply_action(action);
            }
            if self.terminal_mode.is_some() {
                if let Some(id) = self.terminal_mode.clone() {
                    self.scroll_offsets.insert(id, 0);
                }
                return match terminal_view::key(key, &self.input_modes()) {
                    Some(bytes) => KeyOutcome::Forward(bytes),
                    None => KeyOutcome::Ignored,
                };
            }
        }
        self.on_navigation_surface_key(key)
    }

    fn on_help_key(&mut self, key: KeyEvent) -> KeyOutcome {
        use ratatui::crossterm::event::KeyModifiers;
        if self.keymap.is_prefix(key) {
            self.arm_prefix();
            return KeyOutcome::Handled;
        }
        if self.prefix_pending {
            return self.handle_prefix_follow(key);
        }
        match key.code {
            KeyCode::Esc => {
                self.show_key_help = false;
                self.help_filtering = false;
                self.help_filter.clear();
            }
            KeyCode::Char('/') if !self.help_filtering && key.modifiers.is_empty() => {
                self.help_filtering = true;
            }
            KeyCode::Char('u') if key.modifiers.contains(KeyModifiers::CONTROL) => {
                self.help_filter.clear();
            }
            KeyCode::Backspace if self.help_filtering => {
                self.help_filter.pop();
            }
            KeyCode::Char(c)
                if self.help_filtering && !key.modifiers.contains(KeyModifiers::CONTROL) =>
            {
                self.help_filter.push(c);
            }
            _ => {}
        }
        KeyOutcome::Handled
    }

    fn on_menu_key(&mut self, key: KeyEvent) -> KeyOutcome {
        let Some(menu) = self.context_menu.as_mut() else {
            return KeyOutcome::Ignored;
        };
        match key.code {
            KeyCode::Esc | KeyCode::Char('q') => self.context_menu = None,
            KeyCode::Up | KeyCode::Char('k') => menu.selected = menu.selected.saturating_sub(1),
            KeyCode::Down | KeyCode::Char('j') => menu.selected = (menu.selected + 1).min(4),
            KeyCode::Enter => {
                let selected = menu.selected;
                self.context_menu = None;
                return self.activate_menu(menu_actions()[selected]);
            }
            _ => {}
        }
        KeyOutcome::Handled
    }

    fn on_goto_key(&mut self, key: KeyEvent) -> KeyOutcome {
        use ratatui::crossterm::event::KeyModifiers;
        if self.keymap.is_prefix(key) {
            self.arm_prefix();
            return KeyOutcome::Handled;
        }
        if self.prefix_pending {
            return self.handle_prefix_follow(key);
        }
        let rows = self.goto_rows();
        let Some(goto) = self.goto.as_mut() else {
            return KeyOutcome::Ignored;
        };
        if goto.searching {
            match key.code {
                KeyCode::Esc => goto.searching = false,
                KeyCode::Enter => goto.searching = false,
                KeyCode::Backspace => {
                    goto.query.pop();
                }
                KeyCode::Left | KeyCode::Right => {}
                KeyCode::Char(c) if !key.modifiers.contains(KeyModifiers::CONTROL) => {
                    goto.query.push(c);
                }
                _ => {}
            }
            goto.cursor = 0;
            return KeyOutcome::Handled;
        }
        match key.code {
            KeyCode::Esc => self.goto = None,
            KeyCode::Char('/') => {
                if let Some(goto) = self.goto.as_mut() {
                    goto.searching = true;
                }
            }
            KeyCode::Up | KeyCode::Char('k') => {
                if let Some(goto) = self.goto.as_mut() {
                    goto.cursor = goto.cursor.saturating_sub(1);
                }
            }
            KeyCode::Down | KeyCode::Char('j') => {
                if let Some(goto) = self.goto.as_mut() {
                    let max = rows.len().saturating_sub(1);
                    goto.cursor = (goto.cursor + 1).min(max);
                }
            }
            KeyCode::Left => self.goto_section(-1),
            KeyCode::Right => self.goto_section(1),
            KeyCode::Char('b') => self.set_goto_filter(Some("blocked".into())),
            KeyCode::Char('w') => self.set_goto_filter(Some("working".into())),
            KeyCode::Char('i') => self.set_goto_filter(Some("idle".into())),
            KeyCode::Char('d') => self.set_goto_filter(Some("done".into())),
            KeyCode::Char('a') => self.set_goto_filter(None),
            KeyCode::Enter => {
                let cursor = self.goto.as_ref().map(|g| g.cursor).unwrap_or(0);
                self.goto = None;
                if let Some(id) = rows.get(cursor).map(|row| row.id.clone()) {
                    return KeyOutcome::Action(WorkbenchAction::EnterTerminal(id));
                }
            }
            _ => {}
        }
        KeyOutcome::Handled
    }

    fn set_goto_filter(&mut self, kind: Option<String>) {
        if let Some(goto) = self.goto.as_mut() {
            goto.kind = kind;
            goto.cursor = 0;
        }
    }

    fn goto_section(&mut self, delta: isize) {
        let rows = self.goto_rows();
        let Some(goto) = self.goto.as_mut() else {
            return;
        };
        let current = rows.get(goto.cursor).map(|row| row.group.clone());
        let groups: Vec<&str> =
            rows.iter()
                .map(|row| row.group.as_str())
                .fold(Vec::new(), |mut acc, group| {
                    if acc.last().copied() != Some(group) {
                        acc.push(group);
                    }
                    acc
                });
        let index = current
            .as_deref()
            .and_then(|group| groups.iter().position(|item| *item == group))
            .unwrap_or(0);
        let next = index as isize + delta;
        if next < 0 || next >= groups.len() as isize {
            return;
        }
        let target = groups[next as usize];
        if let Some(pos) = rows.iter().position(|row| row.group == target) {
            goto.cursor = pos;
        }
    }

    fn goto_rows(&self) -> Vec<GotoRow> {
        let filter = self.goto.as_ref();
        let query = filter
            .map(|goto| goto.query.to_lowercase())
            .unwrap_or_default();
        let kind = filter.and_then(|goto| goto.kind.clone());
        let mut rows = Vec::new();
        for terminal in &self.model.terminals {
            let group = terminal.worktree_id.as_deref().unwrap_or("floating");
            let agent = self
                .model
                .agents
                .iter()
                .find(|agent| agent.terminal_id == terminal.terminal_id);
            let status = agent
                .and_then(|agent| agent.records.first())
                .map(|record| record.status.as_str().to_string())
                .unwrap_or_else(|| "terminal".into());
            if let Some(kind) = &kind {
                if status != *kind {
                    continue;
                }
            }
            let label = agent
                .map(|agent| format!("{} · {status}", agent.name))
                .unwrap_or_else(|| format!("{} · terminal", terminal.terminal_id));
            let hay = format!("{label} {group} {status}").to_lowercase();
            if !query.is_empty() && !hay.contains(&query) {
                continue;
            }
            rows.push(GotoRow {
                id: terminal.terminal_id.clone(),
                group: group.to_string(),
                label,
            });
        }
        rows
    }

    fn activate_menu(&mut self, action: MenuAction) -> KeyOutcome {
        match action {
            MenuAction::SplitRight => KeyOutcome::Action(WorkbenchAction::SplitRight),
            MenuAction::SplitDown => KeyOutcome::Action(WorkbenchAction::SplitBelow),
            MenuAction::Close => {
                let pane_id = Self::pane_key(&self.pane_focus);
                self.close_pane();
                self.queue_tree(crate::server::WorkbenchTreeOp::ClosePane {
                    scene_id: Some(self.scenes.selected.clone()),
                    tab_id: self.active_tab_id(),
                    pane_id,
                });
                KeyOutcome::Handled
            }
            MenuAction::Rename => {
                self.pane_prompt = Some(String::new());
                KeyOutcome::Handled
            }
            MenuAction::Zoom => {
                self.toggle_zoom();
                self.queue_tree(crate::server::WorkbenchTreeOp::Zoom {
                    scene_id: Some(self.scenes.selected.clone()),
                    tab_id: self.active_tab_id(),
                    on: Some(self.zoomed),
                });
                KeyOutcome::Handled
            }
        }
    }

    fn copy_to_clipboard(&mut self, text: &str) {
        self.last_copy = text.to_string();
        #[cfg(target_os = "macos")]
        {
            if let Ok(mut child) = std::process::Command::new("pbcopy")
                .stdin(std::process::Stdio::piped())
                .spawn()
            {
                if let Some(stdin) = child.stdin.as_mut() {
                    use std::io::Write;
                    let _ = stdin.write_all(text.as_bytes());
                }
                let _ = child.wait();
            }
        }
    }

    /// Handle one mouse event (herdr-parity interaction). Left button
    /// drags a pane divider, drags the sidebar edge, clicks a pane into
    /// focus, and double-clicks a divider to reset. Scroll wheels and
    /// other buttons are not wired (later interaction slices).
    pub fn on_mouse(&mut self, mouse: ratatui::crossterm::event::MouseEvent) {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        match mouse.kind {
            MouseEventKind::Down(MouseButton::Left) => self.on_left_down(mouse.column, mouse.row),
            MouseEventKind::Drag(MouseButton::Left) => self.on_left_drag(mouse.column, mouse.row),
            MouseEventKind::Up(MouseButton::Left) => {
                if let Some(select) = self.drag_select.take() {
                    if select.moved {
                        let text = self.selection_text(&select);
                        if !text.is_empty() {
                            self.copy_to_clipboard(&text);
                        }
                    }
                }
                // A drag that moved the layout breaks any double-click
                // sequence: the next grab starts a fresh drag.
                if self.drag_moved {
                    self.last_divider_click = None;
                }
                self.drag_moved = false;
                self.drag = DragState::None;
            }
            MouseEventKind::Down(MouseButton::Right) => self.on_right_down(mouse.column, mouse.row),
            _ => {}
        }
    }

    fn on_right_down(&mut self, column: u16, row: u16) {
        if let Some((action, _)) = self
            .menu_hits
            .iter()
            .find(|(_, rect)| contains_point(*rect, column, row))
            .map(|(action, _)| (*action, ()))
        {
            let outcome = self.activate_menu(action);
            if let KeyOutcome::Action(action) = outcome {
                self.mouse_action = Some(action);
            }
            return;
        }
        self.context_menu = Some(ContextMenu {
            selected: 0,
            column,
            row,
        });
    }

    fn selection_text(&self, select: &DragSelect) -> String {
        let Some(snap) = self.snapshots.get(&select.id) else {
            return String::new();
        };
        let lines: Vec<&str> = snap
            .scrollback
            .iter()
            .chain(snap.visible.iter())
            .map(String::as_str)
            .collect();
        if lines.is_empty() {
            return String::new();
        }
        let row_of = |y: u16| -> usize {
            let rel = y.saturating_sub(select.area.y) as usize;
            let visible = snap.visible.len().max(1);
            let start = lines.len().saturating_sub(visible);
            (start + rel).min(lines.len().saturating_sub(1))
        };
        let mut r1 = row_of(select.start_row.min(select.end_row));
        let mut r2 = row_of(select.start_row.max(select.end_row));
        let c1 = select.start_col.saturating_sub(select.area.x) as usize;
        let c2 = select.end_col.saturating_sub(select.area.x) as usize;
        if r1 > r2 {
            std::mem::swap(&mut r1, &mut r2);
        }
        let (c1, c2) = if select.start_row < select.end_row
            || (select.start_row == select.end_row && select.start_col <= select.end_col)
        {
            (c1, c2)
        } else {
            (c2, c1)
        };
        if r1 == r2 {
            let line = lines[r1];
            return line
                .chars()
                .skip(c1.min(c2))
                .take(c1.abs_diff(c2).max(1))
                .collect();
        }
        let mut out = Vec::new();
        out.push(lines[r1].chars().skip(c1).collect::<String>());
        for line in lines.iter().take(r2).skip(r1 + 1) {
            out.push((*line).to_string());
        }
        out.push(lines[r2].chars().take(c2.max(1)).collect());
        out.join("\n")
    }

    fn on_left_down(&mut self, column: u16, row: u16) {
        // Overlays are modal: clicks must not fall through to the grid.
        if self.workspace_prompt.is_some()
            || self.tab_prompt.is_some()
            || self.file_panel.is_some()
            || self.handoff_picker.is_some()
            || self.diff_view.is_some()
        {
            return;
        }
        if let Some((id, _)) = self
            .workspace_hits
            .iter()
            .find(|(_, r)| contains_point(*r, column, row))
            .cloned()
        {
            self.mouse_action = Some(Self::select_workspace(id));
            return;
        }
        if let Some((id, _)) = self
            .tab_hits
            .iter()
            .find(|(_, r)| contains_point(*r, column, row))
        {
            let id = id.clone();
            self.switch_tab(&id);
            return;
        }
        if let Some((focus, index, _)) = self
            .sidebar_hits
            .iter()
            .find(|(_, _, r)| contains_point(*r, column, row))
            .cloned()
        {
            self.leave_terminal_mode();
            self.focus = focus;
            self.selected = index;
            self.mouse_action = match self.activate_space_or_agent() {
                KeyOutcome::Action(action) => Some(action),
                _ => None,
            };
            return;
        }
        // Sidebar edge first: the band is the sidebar's last column and
        // the pane area's first column (the pane border the user sees).
        if let Some(edge) = self.last_sidebar_edge {
            if column.saturating_add(1) == edge || column == edge {
                let now = std::time::Instant::now();
                let double_click = self.last_divider_click.take().is_some_and(|(at, x, y)| {
                    now.duration_since(at) <= DOUBLE_CLICK_WINDOW && x == column && y == row
                });
                if double_click {
                    self.sidebar_width = SIDEBAR_WIDTH;
                    self.layout_dirty = true;
                    self.set_status("sidebar width reset");
                    return;
                }
                self.last_divider_click = Some((now, column, row));
                self.drag_moved = false;
                self.drag = DragState::Sidebar;
                return;
            }
        }
        let Some(pane_area) = self.last_pane_area else {
            return;
        };
        if !contains_point(pane_area, column, row) {
            return;
        }
        // Divider grabs come before pane clicks: the grab band overlaps
        // the panes' border cells on purpose.
        if !self.zoomed {
            let hit = self
                .grid
                .splits(pane_area)
                .into_iter()
                .find(|hit| contains_point(hit.hit_rect, column, row));
            if let Some(hit) = hit {
                let now = std::time::Instant::now();
                let double_click = self.last_divider_click.take().is_some_and(|(at, x, y)| {
                    now.duration_since(at) <= DOUBLE_CLICK_WINDOW && x == column && y == row
                });
                if double_click {
                    if self.grid.set_ratio_at_path(&hit.path, 50) {
                        self.layout_dirty = true;
                        self.set_status("split reset to 50/50");
                    }
                    return;
                }
                self.last_divider_click = Some((now, column, row));
                let pointer = match hit.axis {
                    SplitAxis::Horizontal => i32::from(column),
                    SplitAxis::Vertical => i32::from(row),
                };
                self.drag_moved = false;
                self.drag = DragState::PaneSplit {
                    path: hit.path,
                    axis: hit.axis,
                    area: hit.area,
                    grab_offset: i32::from(hit.pos) - pointer,
                    leaves: self.grid.leaves(),
                };
                return;
            }
        }
        // Click a pane to focus it (herdr behavior). Switching panes while
        // a terminal is in keyboard-forwarding mode releases the mode:
        // typed keys must never silently keep flowing to the previously
        // focused terminal. A click that is not on a divider also breaks
        // any double-click sequence.
        self.last_divider_click = None;
        let hit_leaf = self
            .grid
            .render_layout_chrome(pane_area)
            .into_iter()
            .find(|(_, rect)| contains_point(*rect, column, row))
            .map(|(content, _)| content);
        if let Some(content) = hit_leaf {
            self.focus = Focus::Panes;
            self.queue_focus(&content);
            if self.pane_focus != content {
                self.pane_focus = content.clone();
                self.layout_dirty = true;
            }
            match &content {
                PaneContent::Terminal(id) => self.enter_terminal_mode(id.clone()),
                PaneContent::Browser => self.leave_terminal_mode(),
            }
            if let PaneContent::Terminal(id) = &content {
                if let Some((_, rect)) = self
                    .pane_geometry
                    .iter()
                    .find(|(pane, _)| pane == id)
                    .cloned()
                {
                    self.drag_select = Some(DragSelect {
                        id: id.clone(),
                        area: rect,
                        start_col: column,
                        start_row: row,
                        end_col: column,
                        end_row: row,
                        moved: false,
                    });
                }
            }
        }
    }

    fn on_left_drag(&mut self, column: u16, row: u16) {
        if let Some(select) = self.drag_select.as_mut() {
            if column != select.start_col || row != select.start_row {
                select.moved = true;
                select.end_col = column;
                select.end_row = row;
            }
            if select.moved {
                return;
            }
        }
        match self.drag.clone() {
            DragState::None => {}
            DragState::Sidebar => {
                let width = column.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                if self.sidebar_width != width {
                    self.sidebar_width = width;
                    self.layout_dirty = true;
                    self.drag_moved = true;
                }
            }
            DragState::PaneSplit {
                path,
                axis,
                area,
                grab_offset,
                leaves,
            } => {
                // The topology must still be the one the grab happened on:
                // a split/close under a live drag leaves the path pointing
                // at a different node, which must never silently resize.
                if self.grid.leaves() != leaves {
                    self.drag = DragState::None;
                    return;
                }
                let pointer = match axis {
                    SplitAxis::Horizontal => i32::from(column),
                    SplitAxis::Vertical => i32::from(row),
                };
                if let Some(ratio) =
                    crate::tui::layout::ratio_for_divider(axis, pointer + grab_offset, area)
                {
                    if self.grid.set_ratio_at_path(&path, ratio) {
                        self.layout_dirty = true;
                        self.drag_moved = true;
                        self.queue_tree(crate::server::WorkbenchTreeOp::Resize {
                            scene_id: Some(self.scenes.selected.clone()),
                            tab_id: self.active_tab_id(),
                            path: path.clone(),
                            ratio,
                        });
                    }
                }
            }
        }
    }

    /// Draw one frame from the caches, herdr-style: the sidebar (spaces /
    /// agents / tasks) on the left, the pane grid on the right with merged
    /// borders, one status/mode bar at the bottom. The browser leaf is a
    /// facts card now — the lists live in the sidebar. Overlays (file
    /// panel, handoff picker, diff) sit on top of everything.
    pub fn draw(&mut self, frame: &mut Frame) {
        let palette = crate::tui::theme::Palette::catppuccin_mocha();
        let rows = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());
        self.draw_workspace_bar(frame, rows[0], &palette);
        let main = rows[1];

        // herdr hides the sidebar on narrow terminals; so do we. The
        // user-chosen width applies, clamped so the pane grid keeps a
        // usable minimum.
        let sidebar_width = if self.sidebar_collapsed || main.width < 60 {
            0
        } else {
            self.sidebar_width.min(main.width.saturating_sub(20))
        };
        let columns =
            Layout::horizontal([Constraint::Length(sidebar_width), Constraint::Min(0)]).split(main);
        let right = Layout::vertical([Constraint::Length(1), Constraint::Min(0)]).split(columns[1]);
        self.tab_hits.clear();
        let mut x = right[0].x;
        if let Some(scene) = self.scenes.scenes.get(&self.scenes.selected) {
            for tab in scene.tabs.iter().filter(|t| !t.hidden) {
                let label = format!(
                    " {}{} ",
                    if tab.id == scene.active { "● " } else { "" },
                    tab.name
                );
                let w = (label.chars().count() as u16).min(right[0].right().saturating_sub(x));
                let rect = Rect::new(x, right[0].y, w, 1);
                frame.render_widget(Paragraph::new(label), rect);
                self.tab_hits.push((tab.id.clone(), rect));
                x += w;
            }
        }
        if self.tab_hits.is_empty() {
            frame.render_widget(Paragraph::new(" 0 Unassigned · ctrl+b c new tab"), right[0]);
        }
        let pane_area = right[1];
        self.last_pane_area = Some(pane_area);
        self.last_sidebar_edge = if sidebar_width > 0 {
            Some(sidebar_width)
        } else {
            None
        };
        if sidebar_width > 0 {
            self.draw_sidebar(frame, columns[0], &palette);
        }

        // The chrome pass yields leaf rects that INCLUDE their border
        // cells (shared divider cells between siblings). A single pane
        // stays full-bleed, so zoom and one-pane views have no chrome.
        let layout = if self.zoomed {
            vec![(self.pane_focus.clone(), pane_area)]
        } else {
            self.grid.render_layout_chrome(pane_area)
        };
        let rects: Vec<Rect> = layout.iter().map(|(_, rect)| *rect).collect();
        let focused_index = layout
            .iter()
            .position(|(content, _)| *content == self.pane_focus);
        let titles: Vec<crate::tui::chrome::PaneTitle> = layout
            .iter()
            .enumerate()
            .map(|(index, (content, _))| match content {
                PaneContent::Browser => crate::tui::chrome::PaneTitle {
                    pane: index,
                    text: "viva".into(),
                },
                PaneContent::Terminal(id) => crate::tui::chrome::PaneTitle {
                    pane: index,
                    text: self.terminal_title(id),
                },
            })
            .collect();
        crate::tui::chrome::render_pane_borders(frame, &rects, focused_index, &palette, &titles);
        self.pane_geometry.clear();
        for (index, (content, rect)) in layout.iter().enumerate() {
            let content_area = if layout.len() >= 2 {
                crate::tui::chrome::inner_rect(*rect)
            } else {
                rects[index]
            };
            match content {
                PaneContent::Browser => self.draw_overview(frame, content_area),
                PaneContent::Terminal(id) => {
                    self.pane_geometry.push((id.clone(), content_area));
                    self.draw_terminal_pane(frame, content_area, id);
                }
            }
        }

        // A live divider drag lights up the dragged line (herdr feedback):
        // restyle the shared divider cells over the drawn chrome. Overlays
        // still render on top.
        let drag_path = match &self.drag {
            DragState::PaneSplit { path, .. } if !self.zoomed => Some(path.clone()),
            _ => None,
        };
        if let Some(path) = drag_path {
            let hit = self
                .grid
                .splits(pane_area)
                .into_iter()
                .find(|hit| hit.path == path);
            if let Some(hit) = hit {
                let style = ratatui::style::Style::new().fg(palette.mauve).bold();
                let buffer = frame.buffer_mut();
                match hit.axis {
                    SplitAxis::Horizontal => {
                        for y in hit.area.y..hit.area.y + hit.area.height {
                            if let Some(cell) =
                                buffer.cell_mut(ratatui::layout::Position::new(hit.pos, y))
                            {
                                cell.set_style(style);
                            }
                        }
                    }
                    SplitAxis::Vertical => {
                        for x in hit.area.x..hit.area.x + hit.area.width {
                            if let Some(cell) =
                                buffer.cell_mut(ratatui::layout::Position::new(x, hit.pos))
                            {
                                cell.set_style(style);
                            }
                        }
                    }
                }
            }
        }

        if self.file_panel.is_some() {
            self.draw_file_panel(frame);
        }
        if self.handoff_picker.is_some() {
            self.draw_handoff_picker(frame);
        }
        if self.show_key_help {
            self.draw_key_help(frame, main, &palette);
        }
        if self.settings_open {
            self.draw_settings(frame, main, &palette);
        }
        if self.goto.is_some() {
            self.draw_goto(frame, main, &palette);
        }
        if self.context_menu.is_some() {
            self.draw_context_menu(frame, &palette);
        }
        if self.pane_prompt.is_some() {
            self.draw_pane_prompt(frame, &palette);
        }
        self.draw_status_bar(frame, rows[2], &palette);
        if let Some(selected) = self.terminal_picker {
            let area = centered(frame.area(), 90, frame.area().height.saturating_sub(4));
            frame.render_widget(Clear, area);
            let items = self
                .model
                .terminals
                .iter()
                .enumerate()
                .skip(selected.saturating_sub(area.height.saturating_sub(3) as usize))
                .map(|(i, t)| {
                    ListItem::new(format!(
                        "{} {} · {} · {} · {}",
                        if i == selected { "▶" } else { " " },
                        t.terminal_id,
                        t.purpose,
                        t.worktree_id.as_deref().unwrap_or("floating/directory"),
                        if t.live == Some(true) {
                            "running"
                        } else {
                            "exited/unavailable"
                        }
                    ))
                })
                .collect::<Vec<_>>();
            frame.render_widget(
                List::new(items).block(Block::bordered().title(
                    " All terminals (including hidden) · Enter locate · s Stop session · Esc ",
                )),
                area,
            );
        }
        if let Some(text) = &self.tab_prompt {
            let area = centered(frame.area(), 60, 3);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(text.as_str())
                    .block(Block::bordered().title(" Rename tab · Enter / Esc ")),
                area,
            );
        }
        if let Some(prompt) = &self.workspace_prompt {
            let recent = self.recent_workspaces();
            let listed = if prompt.action == "open" {
                recent.len()
            } else {
                0
            };
            let area = centered(frame.area(), 80, 3 + listed as u16 + u16::from(listed > 0));
            frame.render_widget(Clear, area);
            let mut lines = vec![Line::from(format!("{}▏", prompt.text))];
            if listed > 0 {
                lines.push(Line::from(Span::styled(
                    "recent",
                    ratatui::style::Style::new().fg(palette.subtext0),
                )));
                for (i, w) in recent.iter().enumerate() {
                    let marker = if prompt.pick == Some(i) { "›" } else { " " };
                    let current = if Some(&w.workspace_id) == self.model.workspace_id.as_ref() {
                        " (current)"
                    } else {
                        ""
                    };
                    let place = w.source_path.as_deref().unwrap_or("not saved to a file");
                    let mut line = Line::from(format!("{marker} {}{current} — {place}", w.name));
                    if prompt.pick == Some(i) {
                        line = line.style(ratatui::style::Style::new().reversed());
                    }
                    lines.push(line);
                }
            }
            frame.render_widget(
                Paragraph::new(lines).block(Block::bordered().title(prompt.title())),
                area,
            );
        }
        if let Some(diff) = &self.diff_view {
            self.draw_diff(frame, frame.area(), diff);
        }
    }

    /// The browser leaf after the sidebar took over the lists: a small
    /// facts card. Counts of the real model plus key hints — nothing more.
    fn draw_overview(&self, frame: &mut Frame, area: Rect) {
        let palette = crate::tui::theme::Palette::catppuccin_mocha();
        let live = self
            .model
            .terminals
            .iter()
            .filter(|t| t.live == Some(true))
            .count();
        let facts = format!(
            "{} projects · {} worktrees · {} terminals ({} live) · {} tasks",
            self.model.projects.len(),
            self.model.worktrees.len(),
            self.model.terminals.len(),
            live,
            self.model.tasks.len()
        );
        let lines = vec![
            Line::from(Span::styled(
                " viva workbench",
                ratatui::style::Style::new().bold().fg(palette.text),
            )),
            Line::from(""),
            Line::from(Span::styled(
                facts,
                ratatui::style::Style::new().fg(palette.subtext0),
            )),
            Line::from(Span::styled(
                "ctrl+b prefix · a new pane is a terminal",
                ratatui::style::Style::new().fg(palette.overlay0),
            )),
            Line::from(Span::styled(
                "prefix+? lists bindings",
                ratatui::style::Style::new().fg(palette.overlay0),
            )),
        ];
        let height = 5.min(area.height);
        let y = area.y + area.height.saturating_sub(height) / 2;
        frame.render_widget(
            Paragraph::new(lines),
            Rect {
                x: area.x.saturating_add(2),
                y,
                width: area.width.saturating_sub(4),
                height,
            },
        );
    }

    /// One terminal leaf: the server snapshot, bottom-anchored so a
    /// reattached client sees the server-kept history. Borders and the
    /// title are chrome (see `render_pane_borders`); this draws content.
    fn draw_terminal_pane(&self, frame: &mut Frame, area: Rect, id: &str) {
        if let Some(view) = self.snapshots.get(id).and_then(|s| s.screen.as_ref()) {
            let cursor = self.terminal_mode.as_deref() == Some(id)
                && self.file_panel.is_none()
                && self.handoff_picker.is_none()
                && self.diff_view.is_none()
                && self.tab_prompt.is_none()
                && self.workspace_prompt.is_none()
                && self.terminal_picker.is_none();
            terminal_view::draw(frame, area, view, cursor);
            return;
        }
        let lines: Vec<Line> = match self.snapshots.get(id) {
            Some(view) => {
                let mut lines: Vec<Line> = view
                    .scrollback
                    .iter()
                    .chain(view.visible.iter())
                    .map(|row| Line::from(row.clone()))
                    .collect();
                let max_lines = area.height.saturating_sub(1) as usize;
                if lines.len() > max_lines {
                    let skip = lines.len() - max_lines;
                    lines.drain(..skip);
                }
                lines.push(Line::from(format!(
                    "— {}×{} · scrollback: {} lines{} · output bytes: {} —",
                    view.cols,
                    view.rows,
                    view.scrollback.len(),
                    if view.scrollback_capped {
                        " (capped)"
                    } else {
                        ""
                    },
                    view.total_output_bytes
                )));
                lines
            }
            None => vec![Line::from(
                if self
                    .model
                    .terminals
                    .iter()
                    .any(|t| t.terminal_id == id && t.live == Some(true))
                {
                    " waiting for the first server snapshot… "
                } else {
                    " exited/unavailable terminal reference · no command replayed · x hides view "
                },
            )],
        };
        frame.render_widget(Paragraph::new(lines), area);
    }

    /// The pane title: id plus the source-labeled agent statuses.
    fn terminal_title(&self, id: &str) -> String {
        let statuses = self
            .model
            .terminals
            .iter()
            .find(|t| t.terminal_id == id)
            .map(|t| Self::format_agent_status(&t.agent_status))
            .unwrap_or_default();
        if statuses.is_empty() {
            format!("terminal {id}")
        } else {
            format!("terminal {id} · {statuses}")
        }
    }

    /// The 1-row status/mode bar (herdr's mode bar). Facts come first —
    /// mode chips, needs-attention markers, sync note — and the key hints
    /// go last, truncated with an ellipsis. Honesty never loses its row to
    /// decoration on a narrow terminal (QA F5). The client version sits
    /// dim at the right edge: provenance without noise.
    /// The top switcher: "All projects" plus every workspace as a clickable
    /// tab in a stable order, the current one highlighted, then key hints.
    fn draw_workspace_bar(&mut self, frame: &mut Frame, area: Rect, palette: &Palette) {
        self.workspace_hits.clear();
        let order = self.switcher_order();
        let label_of = |id: &Option<String>| -> String {
            match id {
                None => " All projects ".to_string(),
                Some(id) => self
                    .model
                    .workspaces
                    .iter()
                    .find(|w| &w.workspace_id == id)
                    .map_or_else(|| " ? ".to_string(), |w| format!(" {} ", w.name)),
            }
        };
        let labels: Vec<(Option<String>, String)> =
            order.iter().map(|id| (id.clone(), label_of(id))).collect();
        let width = |s: &str| s.chars().count() as u16;
        let active = labels
            .iter()
            .position(|(id, _)| *id == self.model.workspace_id)
            .unwrap_or(0);
        // Scroll the strip just far enough that the current tab is visible.
        let budget = area.width.saturating_sub(2);
        let mut first = 0;
        while first < active
            && labels[first..=active]
                .iter()
                .map(|(_, l)| width(l) + 1)
                .sum::<u16>()
                > budget
        {
            first += 1;
        }
        let mut x = area.x;
        for (i, (id, label)) in labels.iter().enumerate().skip(first) {
            let w = width(label).min(area.right().saturating_sub(x));
            if w == 0 {
                break;
            }
            let rect = Rect::new(x, area.y, w, 1);
            let style = if i == active {
                ratatui::style::Style::new().bold().reversed()
            } else {
                ratatui::style::Style::new().fg(palette.subtext0)
            };
            frame.render_widget(Paragraph::new(Span::styled(label.clone(), style)), rect);
            self.workspace_hits.push((id.clone(), rect));
            x += w + 1;
        }
    }

    fn draw_key_help(&self, frame: &mut Frame, area: Rect, palette: &Palette) {
        let body = self.keymap.help_text(&self.help_filter);
        let mut lines = vec![Line::from(Span::styled(
            format!(" keys · prefix {} · / filter ", self.keymap.prefix_label()),
            ratatui::style::Style::new()
                .fg(palette.text)
                .bg(palette.panel_bg)
                .bold(),
        ))];
        if self.help_filtering {
            lines.push(Line::from(format!(" /{}", self.help_filter)));
        }
        for row in body.lines().take(area.height.saturating_sub(4) as usize) {
            lines.push(Line::from(Span::styled(
                row.to_string(),
                ratatui::style::Style::new().fg(palette.subtext0),
            )));
        }
        let height = (lines.len() as u16 + 2).min(area.height.saturating_sub(1));
        let popup = centered(area, 90, height);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(ratatui::style::Style::new().fg(palette.accent)),
            ),
            popup,
        );
    }

    fn draw_settings(&self, frame: &mut Frame, area: Rect, palette: &Palette) {
        let warnings = self.config_warnings();
        let mut body = format!(
            " settings\n prefix {}\n\n Enter or Esc continues and writes onboarding = false",
            self.keymap.prefix_label()
        );
        if !warnings.is_empty() {
            body.push_str("\n\n");
            body.push_str(&warnings.join("\n"));
        }
        let popup = centered(area, 72, 14);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(body).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(ratatui::style::Style::new().fg(palette.accent)),
            ),
            popup,
        );
    }

    fn draw_goto(&self, frame: &mut Frame, area: Rect, palette: &Palette) {
        let rows = self.goto_rows();
        let cursor = self.goto.as_ref().map(|goto| goto.cursor).unwrap_or(0);
        let query = self
            .goto
            .as_ref()
            .map(|goto| goto.query.clone())
            .unwrap_or_default();
        let mut lines = vec![Line::from(format!(" goto  /{query}"))];
        let mut last_group = String::new();
        for (index, row) in rows
            .iter()
            .enumerate()
            .take(area.height.saturating_sub(4) as usize)
        {
            if row.group != last_group {
                last_group = row.group.clone();
                lines.push(Line::from(Span::styled(
                    format!(" {}", row.group),
                    ratatui::style::Style::new().fg(palette.overlay0),
                )));
            }
            let mark = if index == cursor { "▶" } else { " " };
            lines.push(Line::from(format!(" {mark} {}", row.label)));
        }
        if rows.is_empty() {
            lines.push(Line::from(" no terminals"));
        }
        let popup = centered(area, 80, 18);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(ratatui::style::Style::new().fg(palette.accent)),
            ),
            popup,
        );
    }

    fn draw_context_menu(&mut self, frame: &mut Frame, palette: &Palette) {
        let Some(menu) = &self.context_menu else {
            return;
        };
        let labels = ["split right", "split down", "close", "rename", "zoom"];
        let selected = menu.selected;
        let column = menu.column;
        let row = menu.row;
        let width = 18u16;
        let height = 7u16;
        let screen = frame.area();
        let x = column.min(screen.x.saturating_add(screen.width).saturating_sub(width));
        let y = row.min(
            screen
                .y
                .saturating_add(screen.height)
                .saturating_sub(height),
        );
        let popup = Rect::new(x, y, width, height);
        let mut lines = vec![Line::from(" pane")];
        self.menu_hits.clear();
        for (index, label) in labels.iter().enumerate() {
            let style = if index == selected {
                ratatui::style::Style::new()
                    .fg(palette.chip_fg())
                    .bg(palette.accent)
            } else {
                ratatui::style::Style::new().fg(palette.text)
            };
            lines.push(Line::from(Span::styled(format!(" {label}"), style)));
            self.menu_hits.push((
                menu_actions()[index],
                Rect::new(x, y.saturating_add(1 + index as u16), width, 1),
            ));
        }
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(ratatui::style::Style::new().fg(palette.accent)),
            ),
            popup,
        );
    }

    fn draw_pane_prompt(&self, frame: &mut Frame, palette: &Palette) {
        let value = self.pane_prompt.clone().unwrap_or_default();
        let popup = centered(frame.area(), 60, 3);
        frame.render_widget(Clear, popup);
        frame.render_widget(
            Paragraph::new(format!(" rename pane: {value}")).block(
                Block::default()
                    .borders(Borders::ALL)
                    .border_style(ratatui::style::Style::new().fg(palette.accent)),
            ),
            popup,
        );
    }

    fn draw_status_bar(&self, frame: &mut Frame, area: Rect, palette: &Palette) {
        let mode = if self.prefix_pending {
            "PREFIX"
        } else if self.navigate {
            "NAVIGATE"
        } else if self.copy_mode {
            "COPY"
        } else if self.resize_mode {
            "RESIZE"
        } else if self.settings_open {
            "SETTINGS"
        } else if self.goto.is_some() {
            "GOTO"
        } else if self.show_key_help {
            "HELP"
        } else if self.terminal_mode.is_some() {
            "TERMINAL"
        } else {
            "WORKBENCH"
        };
        let mode = if self.zoomed {
            format!("{mode} ZOOM")
        } else {
            mode.to_string()
        };
        let line = format!(" {}  {mode}", self.keymap.prefix_label());
        frame.render_widget(
            Paragraph::new(Span::styled(
                line,
                ratatui::style::Style::new().fg(palette.text),
            )),
            area,
        );
    }

    fn workspace_rollup(&self, id: &str) -> &'static str {
        if self.model.workspace_id.as_deref() != Some(id) {
            return "unknown";
        }
        let mut best = 0u8;
        let mut label = "unknown";
        for agent in self.agent_rows() {
            let status = agent
                .records
                .first()
                .map(|record| record.status.as_str())
                .unwrap_or("unknown");
            let rank = match status {
                "blocked" | "rate_limited" => 5,
                "working" => 4,
                "done" => 3,
                "idle" => 2,
                _ => 1,
            };
            if rank > best {
                best = rank;
                label = match status {
                    "rate_limited" => "blocked",
                    other => other,
                };
            }
        }
        label
    }

    /// The herdr-style sidebar: `spaces` (projects → worktrees as a tree),
    /// `agents` (terminals with status dots), `tasks`. The active
    /// section's header is accented; the selected row carries the
    /// selection background across its whole entity block. The viewport
    /// follows the keyboard selection (QA F2) — with no mouse, a selected
    /// row that scrolled off-screen would be unreachable. Facts only —
    /// unknown stays unknown.
    fn draw_sidebar(&mut self, frame: &mut Frame, area: Rect, palette: &Palette) {
        let width = area.width as usize;
        // Each line carries the entity it belongs to (section + model
        // index); headers and dividers carry None. The viewport window is
        // computed over the SELECTED entity's whole block.
        let mut lines: Vec<(Line<'static>, Option<(Focus, usize)>)> = Vec::new();
        if !self.status_line.is_empty() {
            lines.push((
                Line::from(Span::styled(
                    fit(&self.status_line, width.saturating_sub(1)),
                    ratatui::style::Style::new().fg(palette.overlay0),
                )),
                None,
            ));
        }
        for marker in &self.model.attention {
            lines.push((
                Line::from(Span::styled(
                    "needs attention",
                    ratatui::style::Style::new().fg(palette.red),
                )),
                None,
            ));
            lines.push((
                Line::from(Span::styled(
                    fit(&marker.kind, width.saturating_sub(1)),
                    ratatui::style::Style::new().fg(palette.red),
                )),
                None,
            ));
            lines.push((
                Line::from(Span::styled(
                    fit(&marker.detail, width.saturating_sub(1)),
                    ratatui::style::Style::new().fg(palette.red),
                )),
                None,
            ));
        }
        lines.push((
            Self::section_header(" workspaces", self.navigate, palette),
            None,
        ));
        if self.model.workspaces.is_empty() {
            lines.push((
                Line::from(Span::styled(
                    " no workspace",
                    ratatui::style::Style::new().fg(palette.overlay0),
                )),
                None,
            ));
        }
        for (index, workspace) in self.model.workspaces.iter().enumerate() {
            let rollup = self.workspace_rollup(&workspace.workspace_id);
            let selected = self.navigate && self.workspace_nav == index;
            lines.push((
                Self::row(
                    vec![
                        Span::styled(
                            fit(&workspace.name, width.saturating_sub(12)),
                            ratatui::style::Style::new().fg(palette.text),
                        ),
                        Span::styled(
                            format!(" {rollup}"),
                            ratatui::style::Style::new().fg(palette.subtext0),
                        ),
                    ],
                    selected,
                    width,
                    palette,
                ),
                None,
            ));
        }

        // -- spaces --
        lines.push((
            Self::section_header(" spaces", self.focus == Focus::Worktrees, palette),
            None,
        ));
        for (name, indices) in self.sidebar_space_groups() {
            lines.push((
                Self::row(
                    vec![
                        Span::styled(
                            fit(&name, width.saturating_sub(4)),
                            ratatui::style::Style::new().bold().fg(palette.text),
                        ),
                        Span::styled(
                            format!(" {}", indices.len()),
                            ratatui::style::Style::new().fg(palette.overlay0),
                        ),
                    ],
                    false,
                    width,
                    palette,
                ),
                None,
            ));
            let count = indices.len();
            for (position, index) in indices.iter().enumerate() {
                let worktree = &self.model.worktrees[*index];
                let last = position + 1 == count;
                let (prefix, continuation) = if last {
                    ("└─ ", "   ")
                } else {
                    ("├─ ", "│  ")
                };
                let (dot, dot_color) = match worktree.dirty {
                    Some(true) => ("●", palette.yellow),
                    Some(false) => ("●", palette.green),
                    None => ("·", palette.overlay0),
                };
                let selected = self.focus == Focus::Worktrees && self.selected == *index;
                lines.push((
                    Self::row(
                        vec![
                            Span::styled(
                                prefix.to_string(),
                                ratatui::style::Style::new().fg(palette.overlay0),
                            ),
                            Span::styled(
                                dot.to_string(),
                                ratatui::style::Style::new().fg(dot_color),
                            ),
                            Span::styled(
                                format!(" {}", fit(&worktree.branch, width.saturating_sub(5))),
                                ratatui::style::Style::new().fg(palette.mauve),
                            ),
                        ],
                        selected,
                        width,
                        palette,
                    ),
                    Some((Focus::Worktrees, *index)),
                ));
                let mut detail = format!(
                    "{continuation}{}",
                    match worktree.dirty {
                        Some(true) => "dirty",
                        Some(false) => "clean",
                        None => "unknown",
                    }
                );
                if let Some(task) = &worktree.task_id {
                    detail.push_str(&format!(" · task:{task}"));
                }
                if let Some(pr) = &worktree.pr_state {
                    detail.push_str(&format!(" · pr:{pr}"));
                }
                if let (Some(ahead), Some(behind)) = (worktree.ahead, worktree.behind) {
                    detail.push_str(&format!(" · ↑{ahead}↓{behind}"));
                }
                lines.push((
                    Self::row(
                        vec![Span::styled(
                            fit(&detail, width.saturating_sub(1)),
                            ratatui::style::Style::new().fg(palette.overlay0),
                        )],
                        selected,
                        width,
                        palette,
                    ),
                    Some((Focus::Worktrees, *index)),
                ));
            }
        }
        self.append_machine_spaces(&mut lines, width, palette);

        let agent_start = lines.len();
        // -- agents --
        lines.push((Self::section_divider(width, palette), None));
        lines.push((
            Self::section_header(
                if self.global_attention {
                    " agents · global attention (! returns)"
                } else {
                    " agents · workspace (! global)"
                },
                self.focus == Focus::Terminals,
                palette,
            ),
            None,
        ));
        for (index, terminal) in self.agent_rows().into_iter().enumerate() {
            let selected = self.focus == Focus::Terminals && self.selected == index;
            // The dot shows the best observation's state. Honesty (S4,
            // ruling §5.3): only a controlled report may look authoritative
            // — screen/process-tree dots stay muted.
            let (dot, dot_color) = match terminal.records.first() {
                Some(record) => {
                    let color = if record.source.is_authoritative() && !terminal.stale {
                        crate::tui::theme::status_color(record.status, palette)
                    } else {
                        palette.overlay0
                    };
                    (crate::tui::theme::status_glyph(record.status), color)
                }
                None => ("·", palette.overlay0),
            };
            lines.push((
                Self::row(
                    vec![
                        Span::styled(dot.to_string(), ratatui::style::Style::new().fg(dot_color)),
                        Span::styled(
                            format!(" {}", fit(&terminal.name, width.saturating_sub(3))),
                            ratatui::style::Style::new().fg(palette.text),
                        ),
                    ],
                    selected,
                    width,
                    palette,
                ),
                Some((Focus::Terminals, index)),
            ));
            let live_word = match terminal.live {
                Some(true) => "live",
                Some(false) => "exited",
                None => "unknown",
            };
            let mut detail = format!(
                "{} · {live_word} · {}{}",
                terminal.location,
                terminal.report_capability,
                if terminal.stale {
                    " · stale report"
                } else {
                    ""
                }
            );
            let records = Self::format_agent_status(&terminal.records);
            if !records.is_empty() {
                detail.push_str(&format!(" · {records}"));
            }
            lines.push((
                Self::row(
                    vec![Span::styled(
                        fit(&detail, width.saturating_sub(1)),
                        ratatui::style::Style::new().fg(palette.overlay0),
                    )],
                    selected,
                    width,
                    palette,
                ),
                Some((Focus::Terminals, index)),
            ));
        }
        self.append_machine_agents(&mut lines, width, palette);

        let task_start = lines.len();
        // -- tasks --
        lines.push((Self::section_divider(width, palette), None));
        lines.push((
            Self::section_header(" tasks", self.focus == Focus::Tasks, palette),
            None,
        ));
        for (index, task) in self.model.tasks.iter().enumerate() {
            let selected = self.focus == Focus::Tasks && self.selected == index;
            // Two rows per task (herdr's two-row workspaces): the colored
            // status word, then the goal on its own line so it fits.
            lines.push((
                Self::row(
                    vec![Span::styled(
                        task.status.clone(),
                        ratatui::style::Style::new()
                            .fg(Self::task_status_color(&task.status, palette)),
                    )],
                    selected,
                    width,
                    palette,
                ),
                Some((Focus::Tasks, index)),
            ));
            lines.push((
                Self::row(
                    vec![Span::styled(
                        format!(" {}", fit(&task.goal, width.saturating_sub(2))),
                        ratatui::style::Style::new().fg(palette.text),
                    )],
                    selected,
                    width,
                    palette,
                ),
                Some((Focus::Tasks, index)),
            ));
        }

        // Two stable regions; Tasks are an explicit alternate lower view.
        let regions =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
        self.sidebar_hits.clear();
        for (range, rect) in [
            (0..agent_start, regions[0]),
            (
                if self.focus == Focus::Tasks {
                    task_start..lines.len()
                } else {
                    agent_start..task_start
                },
                regions[1],
            ),
        ] {
            let section = &lines[range];
            let height = rect.height as usize;
            let selected_key = Some((self.focus, self.selected));
            let last = section
                .iter()
                .rposition(|(_, key)| *key == selected_key)
                .unwrap_or(0);
            let offset = (last + 1).saturating_sub(height);
            let mut visible = Vec::new();
            for (y, (line, key)) in section.iter().skip(offset).take(height).enumerate() {
                visible.push(line.clone());
                if let Some((focus, index)) = key {
                    self.sidebar_hits.push((
                        *focus,
                        *index,
                        Rect::new(rect.x, rect.y + y as u16, rect.width.saturating_sub(1), 1),
                    ));
                }
            }
            frame.render_widget(Paragraph::new(visible), rect);
        }
        // herdr's sidebar separator: one dim vertical line on the right.
        let buffer = frame.buffer_mut();
        let x = area.x + area.width.saturating_sub(1);
        for y in area.y..area.y + area.height {
            if let Some(cell) = buffer.cell_mut(ratatui::layout::Position::new(x, y)) {
                cell.set_char('│')
                    .set_style(ratatui::style::Style::new().fg(palette.surface_dim));
            }
        }
    }

    fn append_machine_spaces(
        &self,
        lines: &mut Vec<(Line<'static>, Option<(Focus, usize)>)>,
        width: usize,
        palette: &Palette,
    ) {
        let mut index = self.model.worktrees.len();
        for machine in &self.model.machines {
            lines.push((self.machine_header(machine, width, palette), None));
            for workspace in &machine.workspaces {
                let selected = self.focus == Focus::Worktrees && self.selected == index;
                let style =
                    self.machine_style(machine.connected && machine.enabled, palette.text, palette);
                lines.push((
                    Self::row(
                        vec![Span::styled(
                            fit(&format!("└─ {}", workspace.name), width.saturating_sub(1)),
                            style,
                        )],
                        selected,
                        width,
                        palette,
                    ),
                    Some((Focus::Worktrees, index)),
                ));
                index += 1;
            }
        }
    }

    fn append_machine_agents(
        &self,
        lines: &mut Vec<(Line<'static>, Option<(Focus, usize)>)>,
        width: usize,
        palette: &Palette,
    ) {
        let mut index = self.agent_rows().len();
        for machine in &self.model.machines {
            if machine.id == crate::machines::LOCAL_MACHINE_ID {
                continue;
            }
            lines.push((self.machine_header(machine, width, palette), None));
            for agent in &machine.agents {
                let selected = self.focus == Focus::Terminals && self.selected == index;
                let live = machine.connected && machine.enabled;
                lines.push((
                    Self::row(
                        vec![Span::styled(
                            fit(&format!("└─ {}", agent.name), width.saturating_sub(1)),
                            self.machine_style(live, palette.text, palette),
                        )],
                        selected,
                        width,
                        palette,
                    ),
                    Some((Focus::Terminals, index)),
                ));
                index += 1;
            }
        }
    }

    fn machine_header(
        &self,
        machine: &crate::machines::MachineView,
        width: usize,
        palette: &Palette,
    ) -> Line<'static> {
        let state = if machine.connected {
            String::new()
        } else if machine.enabled {
            " disconnected".into()
        } else {
            " disabled".into()
        };
        let text = format!(" {}{state}", machine.label);
        Self::row(
            vec![Span::styled(
                fit(&text, width.saturating_sub(1)),
                self.machine_style(machine.connected && machine.enabled, palette.text, palette)
                    .bold(),
            )],
            false,
            width,
            palette,
        )
    }

    fn machine_style(
        &self,
        live: bool,
        color: ratatui::style::Color,
        palette: &Palette,
    ) -> ratatui::style::Style {
        ratatui::style::Style::new().fg(if live { color } else { palette.overlay0 })
    }

    /// Worktrees grouped under their project's name, as (name, indices
    /// into `model.worktrees`), projects in model order, unmatched
    /// worktrees last under "(no project)".
    fn sidebar_space_groups(&self) -> Vec<(String, Vec<usize>)> {
        let mut groups: Vec<(String, Vec<usize>)> = self
            .model
            .projects
            .iter()
            .map(|project| {
                let rows: Vec<usize> = self
                    .model
                    .worktrees
                    .iter()
                    .enumerate()
                    .filter(|(_, w)| w.project_id == project.project_id)
                    .map(|(index, _)| index)
                    .collect();
                (project.name.clone(), rows)
            })
            .collect();
        let orphans: Vec<usize> = self
            .model
            .worktrees
            .iter()
            .enumerate()
            .filter(|(_, w)| {
                !self
                    .model
                    .projects
                    .iter()
                    .any(|p| p.project_id == w.project_id)
            })
            .map(|(index, _)| index)
            .collect();
        if !orphans.is_empty() {
            groups.push(("(no project)".into(), orphans));
        }
        groups
    }

    fn section_header(text: &str, active: bool, palette: &Palette) -> Line<'static> {
        let style = if active {
            ratatui::style::Style::new().fg(palette.accent).bold()
        } else {
            ratatui::style::Style::new().fg(palette.subtext0)
        };
        Line::from(Span::styled(format!("{text} "), style))
    }

    fn section_divider(width: usize, palette: &Palette) -> Line<'static> {
        Line::from(Span::styled(
            "─".repeat(width),
            ratatui::style::Style::new().fg(palette.surface_dim),
        ))
    }

    /// One sidebar row: pad with the selection background when selected so
    /// the highlight fills the row.
    fn row(
        mut spans: Vec<Span<'static>>,
        selected: bool,
        width: usize,
        palette: &Palette,
    ) -> Line<'static> {
        if selected {
            for span in spans.iter_mut() {
                span.style = span.style.bg(palette.selection_bg);
            }
            let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
            let pad = width.saturating_sub(used);
            if pad > 0 {
                spans.push(Span::styled(
                    " ".repeat(pad),
                    ratatui::style::Style::new().bg(palette.selection_bg),
                ));
            }
        }
        Line::from(spans)
    }

    /// Task status is a free string from the registry; known words map to
    /// semantic colors, everything else stays muted.
    fn task_status_color(status: &str, palette: &Palette) -> ratatui::style::Color {
        match status {
            "done" | "completed" | "released" => palette.green,
            "in_progress" | "active" | "working" => palette.yellow,
            "blocked" | "failed" => palette.red,
            _ => palette.overlay0,
        }
    }

    /// One-line, source-labeled agent status: `(reported)` = the agent's
    /// own controlled word; `(screen)` = auxiliary rules, never a fact;
    /// `(detected)` = process-tree identification.
    fn format_agent_status(records: &[crate::agents::AgentStatusRecord]) -> String {
        records
            .iter()
            .map(|record| {
                let source = match record.source {
                    crate::agents::StatusSource::ControlledReport => "reported",
                    crate::agents::StatusSource::ScreenInference => "screen",
                    crate::agents::StatusSource::ProcessTree => "detected",
                };
                format!("{}:{}({})", record.agent, record.status.as_str(), source)
            })
            .collect::<Vec<_>>()
            .join(" ")
    }

    /// Draw the file browser overlay (V15-1): either the inventory list or
    /// the viewed file's material, always bounded to the frame.
    fn draw_file_panel(&self, frame: &mut Frame) {
        let Some(panel) = &self.file_panel else {
            return;
        };
        let area = centered(
            frame.area(),
            90,
            frame.area().height.saturating_sub(4).max(6),
        );
        frame.render_widget(Clear, area);
        match (&panel.viewing, self.file_view.as_ref()) {
            (Some(path), Some(text)) => {
                let lines: Vec<Line> = text.lines().map(Line::from).collect();
                let view_height = area.height.saturating_sub(2).max(1) as usize;
                let max_scroll = lines.len().saturating_sub(view_height);
                let offset = panel.scroll.min(max_scroll);
                let total_lines = lines.len();
                let shown: Vec<Line> = lines
                    .iter()
                    .skip(offset)
                    .take(view_height.max(1))
                    .cloned()
                    .collect();
                let shown_count = shown.len();
                frame.render_widget(
                    Paragraph::new(shown).block(
                        Block::new()
                            .title(format!(
                                " {} — Esc back · ↑↓ scroll{} ",
                                path,
                                if total_lines > view_height {
                                    format!(
                                        " · lines {}..{} / {}",
                                        offset + 1,
                                        offset + shown_count,
                                        total_lines
                                    )
                                } else {
                                    String::new()
                                }
                            ))
                            .borders(Borders::ALL),
                    ),
                    area,
                );
            }
            _ => {
                let mut items: Vec<ListItem> = Vec::new();
                if panel.truncated {
                    items.push(ListItem::new(Line::from(Span::styled(
                        format!(
                            "— showing {} of {} files (truncated) —",
                            panel.rows.len(),
                            panel.total
                        ),
                        ratatui::style::Style::new().gray(),
                    ))));
                }
                for (i, (path, status)) in panel.rows.iter().enumerate() {
                    let marker = if panel.selected == i { "▶ " } else { "  " };
                    items.push(ListItem::new(Line::from(format!(
                        "{marker}{status:<6} {path}"
                    ))));
                }
                frame.render_widget(
                    List::new(items).block(
                        Block::new()
                            .title(format!(
                                " files · {} — Enter view · Esc close ",
                                panel.worktree_id
                            ))
                            .borders(Borders::ALL),
                    ),
                    area,
                );
            }
        }
    }

    /// Draw the handoff target picker (V15-3).
    fn draw_handoff_picker(&self, frame: &mut Frame) {
        let Some(picker) = &self.handoff_picker else {
            return;
        };
        let area = centered(frame.area(), 50, (picker.agents.len() as u16 + 2).max(5));
        frame.render_widget(Clear, area);
        let items: Vec<ListItem> = picker
            .agents
            .iter()
            .enumerate()
            .map(|(i, agent)| {
                let marker = if picker.selected == i { "▶ " } else { "  " };
                ListItem::new(Line::from(format!("{marker}{agent}")))
            })
            .collect();
        frame.render_widget(
            List::new(items).block(
                Block::new()
                    .title(format!(
                        " handoff {} to — Enter confirm · Esc cancel ",
                        picker.worktree_id
                    ))
                    .borders(Borders::ALL),
            ),
            area,
        );
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect, diff: &str) {
        let inner = centered(area, 80, area.height.saturating_sub(4).max(5));
        let lines: Vec<Line> = diff.lines().map(Line::from).collect();
        // Scrolled (QA F4): the bounded material may be truncated — the
        // position/truncation note must be reachable, so the view scrolls
        // instead of clipping.
        let view_height = inner.height.saturating_sub(2) as usize;
        let max_scroll = lines.len().saturating_sub(view_height);
        let offset = self.diff_scroll.min(max_scroll);
        let shown: Vec<Line> = lines
            .iter()
            .skip(offset)
            .take(view_height.max(1))
            .cloned()
            .collect();
        let shown_count = shown.len();
        frame.render_widget(Clear, inner);
        frame.render_widget(
            Paragraph::new(shown).block(
                Block::new()
                    .title(format!(
                        " worktree diff (HEAD) — Esc close · ↑↓ scroll{} ",
                        if lines.len() > view_height {
                            format!(
                                " · lines {}..{} / {}",
                                offset + 1,
                                offset + shown_count,
                                lines.len()
                            )
                        } else {
                            String::new()
                        }
                    ))
                    .borders(Borders::ALL),
            ),
            inner,
        );
    }
}

impl Default for WorkbenchApp {
    fn default() -> Self {
        Self::new()
    }
}

/// Encode one character for the focused terminal. Control combos map to
/// their real control bytes; a Ctrl combo with no control-byte meaning
/// (Ctrl+Space → NUL is defined, Ctrl+9 is not) forwards the raw character
/// instead of panicking or sending garbage.
fn encode_char(modifiers: ratatui::crossterm::event::KeyModifiers, c: char) -> Vec<u8> {
    use ratatui::crossterm::event::KeyModifiers;
    if modifiers.contains(KeyModifiers::CONTROL) {
        let upper = c.to_ascii_uppercase();
        let control: Option<u8> = match upper {
            '@' | ' ' => Some(0x00),
            'A'..='Z' => Some(upper as u8 - b'A' + 1),
            '[' => Some(0x1b), // Esc
            '\\' => Some(0x1c),
            ']' => Some(0x1d),
            '^' => Some(0x1e),
            '_' => Some(0x1f),
            _ => None,
        };
        if let Some(byte) = control {
            return vec![byte];
        }
    }
    let mut buf = [0u8; 4];
    c.encode_utf8(&mut buf).as_bytes().to_vec()
}

fn centered(area: Rect, percent_x: u16, height: u16) -> Rect {
    // u32 math: 90% of 730+ columns overflows u16 in debug builds and
    // wraps silently in release.
    let width = ((area.width as u32) * (percent_x as u32) / 100) as u16;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

/// Whether the cell (`x`, `y`) lies inside `rect` (zero-size rects contain
/// nothing).
fn contains_point(rect: Rect, x: u16, y: u16) -> bool {
    x >= rect.x
        && y >= rect.y
        && x < rect.x.saturating_add(rect.width)
        && y < rect.y.saturating_add(rect.height)
}

// ---------------------------------------------------------------------------
// Store-backed source: the composition layer that assembles real facts and
// executes actions against the owning registries
// ---------------------------------------------------------------------------

/// The workbench's data + action layer over the real store. It owns
/// no state of its own beyond the references it borrows; every fact is read
/// from the registries, every action goes through the owning domain.
pub struct WorkbenchStore<'a> {
    pub store: &'a crate::foundation::store::Store,
    pub terminals: &'a crate::terminal::TerminalRegistry,
    /// Protected refs for worktree creation/adoption (V08 policy).
    pub protected: crate::git::worktrees::ProtectedRefs,
}

impl<'a> WorkbenchStore<'a> {
    /// Assemble the model from real records. Failures of read-only git
    /// probes become `unknown` facts (never guesses), while store errors
    /// propagate.
    pub fn refresh(&self) -> Result<WorkbenchModel> {
        use crate::projects::ProjectRegistry;
        use crate::tasks::TaskRegistry;

        let projects_registry = ProjectRegistry::new(self.store);
        let tasks_registry = TaskRegistry::new(self.store);

        let projects = projects_registry
            .list(true)?
            .into_iter()
            .map(|p| ProjectRow {
                project_id: p.project_id.to_string(),
                name: p.display_name,
                repo_path: p.repo_path.to_string_lossy().into_owned(),
            })
            .collect::<Vec<_>>();

        let task_records = tasks_registry.list_tasks(0, 200)?;
        // Worktree grouping (S2): a worktree groups under its task's
        // project, so the render can show per-project headers.
        let project_of_task: std::collections::HashMap<String, String> = task_records
            .iter()
            .filter_map(|t| {
                t.project_id
                    .as_ref()
                    .map(|p| (t.task_id.to_string(), p.to_string()))
            })
            .collect();
        let tasks = task_records
            .into_iter()
            .map(|t| TaskRow {
                task_id: t.task_id.to_string(),
                goal: t.goal,
                status: t.status.as_str().to_string(),
            })
            .collect::<Vec<_>>();

        // Worktree records + real dirty state per record.
        let worktree_service =
            crate::git::worktrees::WorktreeService::new(self.store, self.protected.clone());
        // A task worktree belongs to its task's project; a task without a
        // project still hangs under the repo the worktree was cut from, so
        // it never falls out of the tree.
        let project_of_repo: std::collections::HashMap<std::path::PathBuf, String> = projects
            .iter()
            .map(|p| {
                let path = std::path::PathBuf::from(&p.repo_path);
                (
                    std::fs::canonicalize(&path).unwrap_or(path),
                    p.project_id.clone(),
                )
            })
            .collect();
        let mut worktrees = worktree_service
            .all_records()?
            .into_iter()
            .filter(|r| r.released_at.is_none())
            .map(|r| {
                let dirty = self.dirty_state(&r.worktree_path);
                let repo = std::fs::canonicalize(&r.repo_root).unwrap_or(r.repo_root.clone());
                WorktreeRow {
                    worktree_id: r.worktree_id.to_string(),
                    project_id: project_of_task
                        .get(&r.task_id.to_string())
                        .or_else(|| project_of_repo.get(&repo))
                        .cloned()
                        .unwrap_or_default(),
                    path: r.worktree_path.to_string_lossy().into_owned(),
                    branch: r.branch,
                    dirty,
                    task_id: Some(r.task_id.to_string()),
                    source: r.source.as_str().to_string(),
                    pr_state: None,
                    ahead: None,
                    behind: None,
                }
            })
            .collect::<Vec<_>>();
        // Same-project rows stay adjacent (the render inserts one header
        // per group).
        worktrees.sort_by(|a, b| a.project_id.cmp(&b.project_id).then(a.path.cmp(&b.path)));

        let terminals = self
            .terminals
            .list()
            .into_iter()
            .map(|entry| {
                // An unreadable wait state stays unknown — never guessed
                // as live.
                let live = self
                    .terminals
                    .handle(&entry.terminal_id)
                    .ok()
                    .flatten()
                    .and_then(|h| h.try_wait().ok().map(|exit| exit.is_none()));
                TerminalRow {
                    terminal_id: entry.terminal_id.to_string(),
                    worktree_id: entry.worktree_id.map(|w| w.to_string()),
                    purpose: entry.purpose,
                    owner_label: owner_label(&entry.owner),
                    live,
                    agent_status: Vec::new(),
                }
            })
            .collect::<Vec<_>>();

        // Needs-attention markers: recorded facts that deserve eyes.
        let mut attention = Vec::new();
        let failed: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM executions WHERE status = 'failed'",
            [],
            |row| row.get(0),
        )?;
        if failed > 0 {
            attention.push(AttentionMarker {
                kind: "execution_failed".into(),
                detail: format!("{failed} execution(s) failed — see Viva's records"),
            });
        }
        let unresolved: i64 = self.store.connection().query_row(
            "SELECT COUNT(*) FROM launch_intents WHERE state = 'unresolved'",
            [],
            |row| row.get(0),
        )?;
        if unresolved > 0 {
            attention.push(AttentionMarker {
                kind: "launch_unresolved".into(),
                detail: format!("{unresolved} launch(es) unresolved — whether their processes started is unknown"),
            });
        }

        let nav = crate::workspaces::navigation::Navigation { store: self.store };
        let workspaces = nav.list()?;
        let selection = nav.selection()?;
        let workspace_id = selection.as_ref().map(|s| s.workspace_id.clone());
        let folders = selection
            .as_ref()
            .map(|s| s.folders.clone())
            .unwrap_or_default();
        let workspace_diagnostics = selection
            .as_ref()
            .map(crate::workspaces::navigation::WorkspaceSelection::diagnostics)
            .unwrap_or_default();
        // The selected composition decides which projects show, in its own
        // order and under its own display names.
        let projects = match &selection {
            None => projects,
            Some(sel) => sel
                .project_ids()
                .into_iter()
                .filter_map(|id| projects.iter().find(|p| p.project_id == id))
                .map(|p| ProjectRow {
                    name: sel
                        .project_name(&p.project_id)
                        .unwrap_or(&p.name)
                        .to_string(),
                    ..p.clone()
                })
                .collect(),
        };
        let mut st = self
            .store
            .connection()
            .prepare("SELECT worktree_id,project_id,path,branch FROM navigation_checkouts")?;
        for row in st.query_map([], |r| {
            Ok((
                r.get::<_, String>(0)?,
                r.get::<_, String>(1)?,
                r.get::<_, String>(2)?,
                r.get::<_, String>(3)?,
            ))
        })? {
            let (id, project_id, path, branch) = row?;
            if !worktrees.iter().any(|w| w.path == path) {
                worktrees.push(WorktreeRow {
                    worktree_id: id,
                    project_id,
                    path: path.clone(),
                    branch,
                    dirty: self.dirty_state(std::path::Path::new(&path)),
                    task_id: None,
                    source: "discovered (read-only)".into(),
                    pr_state: None,
                    ahead: None,
                    behind: None,
                });
            }
        }
        for p in &projects {
            if folders.iter().any(|f| {
                f.project_id.as_deref() == Some(&p.project_id)
                    && f.diagnostic.as_deref() == Some(crate::workspaces::navigation::DIAG_NOT_GIT)
            }) && !worktrees.iter().any(|w| w.project_id == p.project_id)
            {
                worktrees.push(WorktreeRow {
                    worktree_id: format!("directory:{}", p.project_id),
                    project_id: p.project_id.clone(),
                    path: p.repo_path.clone(),
                    branch: "(directory — not Git)".into(),
                    dirty: None,
                    task_id: None,
                    source: "directory".into(),
                    pr_state: None,
                    ahead: None,
                    behind: None,
                });
            }
        }
        worktrees.retain(|w| {
            selection
                .as_ref()
                .is_none_or(|sel| sel.contains_project(&w.project_id))
        });
        Ok(WorkbenchModel {
            agents: vec![],
            workspaces,
            workspace_id,
            workspace_diagnostics,
            projects,
            worktrees,
            tasks,
            terminals,
            attention,
            auto_pull: false,
            sync_note: None,
            machines: vec![],
        })
    }

    /// Real dirty state of a worktree: `git status --porcelain` non-empty.
    /// A probe failure is `None` (unknown), shown as unknown.
    fn dirty_state(&self, worktree_path: &std::path::Path) -> Option<bool> {
        let runner = crate::git::cli::CliRunner::default();
        let out = runner.git(worktree_path, &["status", "--porcelain"]).ok()?;
        Some(!out.stdout.trim().is_empty())
    }

    /// Open a terminal through an explicit harness combination (V09); the
    /// harness carries the absolute working location. The owner is typed:
    /// user tools (shells, editors, plain CLIs) can never carry an
    /// execution id.
    pub fn open_terminal(
        &self,
        worktree_id: Option<&crate::foundation::ids::WorktreeId>,
        purpose: &str,
        harness: &crate::harness::HarnessSpec,
    ) -> Result<crate::foundation::ids::TerminalId> {
        let mut spec =
            crate::terminal::TerminalSpec::new(harness.argv.clone(), harness.cwd.clone())?;
        spec.env = harness.env.clone();
        let (terminal_id, _handle) = self.terminals.spawn(
            spec,
            crate::foundation::records::TerminalOwner::UserShell,
            worktree_id.cloned(),
            purpose.to_string(),
            None,
            Some(self.store),
        )?;
        Ok(terminal_id)
    }

    /// Stop exactly one terminal through the owning registry (V05 stop
    /// discipline: graceful-first, neighbors untouched).
    pub fn stop_terminal(
        &self,
        terminal_id: &crate::foundation::ids::TerminalId,
    ) -> Result<crate::terminal::TerminalExit> {
        self.terminals.stop(
            terminal_id,
            crate::terminal::StopPolicy::default(),
            Some(self.store),
        )
    }

    /// Real, bounded diff of a worktree against HEAD (read-only).
    pub fn worktree_diff(
        &self,
        worktree_path: &std::path::Path,
        max_bytes: usize,
    ) -> Result<String> {
        let service =
            crate::git::worktrees::WorktreeService::new(self.store, self.protected.clone());
        service.worktree_diff(worktree_path, max_bytes)
    }

    /// Create a task (explicit user action).
    pub fn create_task(
        &self,
        goal: &str,
        assignee: Option<crate::foundation::ids::MemberId>,
        project_id: Option<crate::foundation::ids::ProjectId>,
    ) -> Result<crate::foundation::ids::TaskId> {
        use crate::tasks::TaskRegistry;
        let registry = TaskRegistry::new(self.store);
        let task = registry.create_task(goal, vec![], assignee, None, project_id)?;
        Ok(task.task_id)
    }

    /// Create an isolated worktree for a task from the latest remote
    /// default ref (explicit user action; the V08 service owns the
    /// policy — protected refs, one checkout per branch, fetch-only).
    #[allow(clippy::too_many_arguments)]
    pub fn create_task_worktree(
        &self,
        repo_root: &std::path::Path,
        base_dir: &std::path::Path,
        task_id: &crate::foundation::ids::TaskId,
        task_branch: &str,
    ) -> Result<crate::git::worktrees::TaskWorktreeRecord> {
        let mut service =
            crate::git::worktrees::WorktreeService::new(self.store, self.protected.clone());
        service.create_task_worktree(repo_root, base_dir, task_id, task_branch)
    }

    /// Adopt an existing checkout for a task (explicit user selection of a
    /// real directory — never automatic, never a private-format import).
    pub fn adopt_existing(
        &mut self,
        repo_root: &std::path::Path,
        worktree_path: &std::path::Path,
        task_id: &crate::foundation::ids::TaskId,
    ) -> Result<crate::git::worktrees::TaskWorktreeRecord> {
        let mut service =
            crate::git::worktrees::WorktreeService::new(self.store, self.protected.clone());
        service.adopt_existing(repo_root, worktree_path, task_id)
    }
}

fn owner_label(owner: &crate::foundation::records::TerminalOwner) -> String {
    use crate::foundation::records::TerminalOwner;
    match owner {
        TerminalOwner::MemberExecution(execution) => format!("member_execution:{execution}"),
        TerminalOwner::UserShell => "user_shell".into(),
        TerminalOwner::AgentCli => "agent_cli".into(),
        TerminalOwner::TestRun => "test_run".into(),
    }
}

// ---------------------------------------------------------------------------
// Server-side assembly (called by the resident host for WorkbenchView)
// ---------------------------------------------------------------------------

/// Assemble the workbench model from the real registries. This is what the
/// resident host serves for `WorkbenchView` requests; the client renders it
/// verbatim and owns no second source of these facts.
pub fn assemble_view(
    store: &crate::foundation::store::Store,
    terminals: &crate::terminal::TerminalRegistry,
) -> Result<WorkbenchModel> {
    let workbench = WorkbenchStore {
        store,
        terminals,
        protected: crate::git::worktrees::ProtectedRefs::new(vec![]),
    };
    workbench.refresh()
}

// ---------------------------------------------------------------------------
// The product entry: run the workbench as a CLIENT of the resident server
// ---------------------------------------------------------------------------

/// Run the interactive workbench as a client of the resident server
/// (ADR 0012, issue #43). All facts arrive as [`WorkbenchView`] projections
/// over the control channel; all actions go back as requests. `q` DETACHES:
/// the client exits, the server keeps every terminal running, and a later
/// attach restores the same view from server-held state.
pub fn run_client(mut client: crate::server::Client) -> Result<()> {
    // Honest gate: a workbench without a real terminal cannot work. Fail
    // loudly instead of half-working against a pipe.
    #[cfg(unix)]
    let stdin_is_tty = unsafe { libc::isatty(0) == 1 };
    #[cfg(not(unix))]
    let stdin_is_tty = true;
    if !stdin_is_tty {
        return Err(crate::foundation::Error::Validation(
            "viva workbench needs an interactive terminal (stdin is not a tty); \
             use `viva server` for the headless host and `viva terminal` for headless \
             terminals"
                .into(),
        ));
    }

    let guard = crate::tui::TerminalGuard::enter()?;
    let result = run_client_inner(&mut client);
    drop(guard); // raw mode off + main screen back on EVERY path
    result
}

/// Commit the tab this client is looking at, then pull the shared tree.
/// A refresh does not run before the first saved layout has been applied,
/// and it does not steal this client's scene or tab selection.
fn sync_shared_layout(
    client: &mut crate::server::Client,
    app: &mut WorkbenchApp,
    seen: &mut Option<String>,
    next_refresh: &mut std::time::Instant,
    allow_refresh: bool,
) {
    let saved_this_pass = if app.layout_dirty() {
        match client.call(crate::server::RequestKind::WorkbenchLayout {
            layout_json: Some(app.serialize_layout()),
        }) {
            Ok(_) => {
                app.clear_layout_dirty();
                *seen = None;
                true
            }
            Err(e) => {
                app.set_status(format!("layout save pending: {e}"));
                return;
            }
        }
    } else {
        false
    };
    if !allow_refresh {
        return;
    }
    let now = std::time::Instant::now();
    if !saved_this_pass && now < *next_refresh {
        return;
    }
    *next_refresh = now + std::time::Duration::from_millis(250);
    let Ok(value) = client.call(crate::server::RequestKind::WorkbenchLayout { layout_json: None })
    else {
        return;
    };
    let json = value
        .get("layout")
        .and_then(|v| v.as_str())
        .map(str::to_string);
    if json.as_ref() == seen.as_ref() {
        return;
    }
    if let Some(json) = json.as_ref() {
        app.adopt_shared_tree(json);
    }
    *seen = json;
}

fn run_client_inner(client: &mut crate::server::Client) -> Result<()> {
    use ratatui::crossterm::event::{self, Event};
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)
        .map_err(|e| crate::foundation::Error::Io(std::io::Error::other(e.to_string())))?;

    let mut app = WorkbenchApp::new();
    app.load_home_config();
    let polling = polling::Polling::start(client)?;
    let mut saved_layout = client
        .call(crate::server::RequestKind::WorkbenchLayout { layout_json: None })
        .ok()
        .and_then(|v| v.get("layout").and_then(|v| v.as_str()).map(str::to_string));
    let mut seen_layout = None;
    let mut holding_initial_layout = saved_layout.is_some();
    let mut next_refresh = std::time::Instant::now();
    loop {
        while let Ok(result) = polling.models.try_recv() {
            match result {
                Ok(model) => {
                    let known = model
                        .terminals
                        .iter()
                        .map(|t| t.terminal_id.clone())
                        .collect();
                    app.set_model(model);
                    if let Some(json) = saved_layout.take() {
                        app.restore_layout(&json, &known);
                        seen_layout = Some(json);
                        holding_initial_layout = false;
                    }
                }
                Err(e) => app.set_status(format!("view failed: {e}")),
            }
        }
        while let Ok((id, result)) = polling.screens.try_recv() {
            match result {
                Ok(snapshot) => app.set_snapshot(id, snapshot),
                Err(e) => {
                    app.snapshots.remove(&id);
                    app.set_status(format!("terminal unavailable: {e}"));
                }
            }
        }
        while let Ok(error) = polling.errors.try_recv() {
            app.set_status(error);
        }
        terminal
            .draw(|frame| app.draw(frame))
            .map_err(|e| crate::foundation::Error::Io(std::io::Error::other(e.to_string())))?;

        polling.request(
            app.visible_geometry()
                .iter()
                .filter(|(_, r)| r.width > 0 && r.height > 0)
                .map(|(id, r)| (id.clone(), r.width, r.height, app.scroll_offset(id)))
                .collect(),
        );
        if !event::poll(std::time::Duration::from_millis(32))
            .map_err(|e| crate::foundation::Error::Io(std::io::Error::other(e.to_string())))?
        {
            flush_tree_ops(client, &mut app);
            sync_shared_layout(
                client,
                &mut app,
                &mut seen_layout,
                &mut next_refresh,
                !holding_initial_layout,
            );
            continue;
        }
        let event = event::read()
            .map_err(|e| crate::foundation::Error::Io(std::io::Error::other(e.to_string())))?;
        match event {
            // Key handling is the existing flow; mouse events feed the
            // drag/click layer (herdr parity). Everything else is dropped.
            Event::Key(key) => match app.on_key(key) {
                // Detach, not shutdown: the resident server keeps running.
                KeyOutcome::QuitRequested => {
                    if app.layout_dirty() {
                        client.call(crate::server::RequestKind::WorkbenchLayout {
                            layout_json: Some(app.serialize_layout()),
                        })?;
                    }
                    break;
                }
                KeyOutcome::Action(action) => {
                    if let Err(err) = apply_client_action(client, &mut app, action) {
                        app.set_status(format!("action failed: {err}"));
                    }
                }
                KeyOutcome::Forward(bytes) => {
                    if let Some(id) = app.terminal_mode().map(str::to_string) {
                        if let Err(e) = polling.send(id, bytes) {
                            app.set_status(e);
                        }
                    }
                }
                KeyOutcome::Handled | KeyOutcome::Ignored => {}
            },
            Event::Paste(text) => {
                if let KeyOutcome::Forward(bytes) = app.on_paste(&text) {
                    if let Some(id) = app.terminal_mode().map(str::to_string) {
                        if let Err(e) =
                            deliver_terminal_bytes(client, &mut app, &polling, &id, bytes)
                        {
                            app.set_status(e);
                        }
                    }
                }
            }
            Event::Resize(_, _) => {}
            Event::Mouse(mouse) => {
                if let Some((id, bytes)) = app.terminal_mouse(mouse) {
                    if let Err(e) = deliver_terminal_bytes(client, &mut app, &polling, &id, bytes) {
                        app.set_status(e);
                    }
                } else {
                    app.on_mouse(mouse);
                    if let Some(action) = app.take_mouse_action() {
                        if let Err(err) = apply_client_action(client, &mut app, action) {
                            app.set_status(format!("navigation failed: {err}"));
                        }
                    }
                }
            }
            _ => {}
        }
        // Tree ops land before the layout snapshot so a later merge cannot
        // replay a split the server already applied.
        flush_tree_ops(client, &mut app);
        // Commit the viewed tab, then pick up other clients' tabs.
        sync_shared_layout(
            client,
            &mut app,
            &mut seen_layout,
            &mut next_refresh,
            !holding_initial_layout,
        );
    }
    Ok(())
}

fn flush_tree_ops(client: &mut crate::server::Client, app: &mut WorkbenchApp) {
    for op in app.take_tree_ops() {
        if let Err(err) = client.call(crate::server::RequestKind::WorkbenchTree { op }) {
            app.set_status(format!("tree update failed: {err}"));
        }
    }
}

/// One status line for a workspace reply: what happened, plus how many
/// folders are unavailable (the diagnostics themselves follow in the view).
fn workspace_feedback(reply: &serde_json::Value) -> String {
    let message = reply["message"].as_str().unwrap_or("workspace updated");
    match reply["diagnostics"].as_array().map_or(0, Vec::len) {
        0 => message.to_string(),
        n => format!("{message} · {n} folder(s) need attention"),
    }
}

/// Execute one workbench action against the resident server.
fn apply_client_action(
    client: &mut crate::server::Client,
    app: &mut WorkbenchApp,
    action: WorkbenchAction,
) -> Result<()> {
    match action {
        WorkbenchAction::Workspace {
            action,
            mut value,
            project_id,
        } => {
            if matches!(action.as_str(), "open" | "add" | "save") {
                if let Some(v) = &value {
                    value = Some(
                        crate::workspaces::navigation::absolutize(v, &std::env::current_dir()?)
                            .display()
                            .to_string(),
                    );
                }
            }
            let v = client.call(crate::server::RequestKind::Workspace {
                action,
                value,
                project_id,
            })?;
            app.set_status(workspace_feedback(&v));
            Ok(())
        }
        WorkbenchAction::SelectWorktree(id) => {
            app.select_worktree(id);
            Ok(())
        }
        WorkbenchAction::NewTab => {
            if !app.new_tab("Terminal".into()) {
                return Ok(());
            }
            let id = app.selected_scene().to_string();
            if id == FLOATING {
                spawn_and_split(client, app, SplitAxis::Horizontal)
            } else {
                apply_client_action(client, app, WorkbenchAction::OpenWorktreeShell(id))
            }
        }
        WorkbenchAction::EnterTerminal(id) => {
            app.attach_terminal(id.clone());
            app.enter_terminal_mode(id);
            Ok(())
        }
        WorkbenchAction::StopTerminal(id) => {
            client.call(crate::server::RequestKind::TerminalStop {
                terminal_id: id.clone(),
            })?;
            app.set_status(format!("terminal {id} stopped"));
            Ok(())
        }
        WorkbenchAction::ShowDiff(worktree_id) => {
            let value = client.call(crate::server::RequestKind::WorkbenchDiff {
                worktree_id: worktree_id.clone(),
            })?;
            let diff = value
                .get("diff")
                .and_then(|d| d.as_str())
                .unwrap_or("")
                .to_string();
            app.set_diff_view(Some(diff));
            Ok(())
        }
        WorkbenchAction::Refresh => {
            // The view refreshes every cycle; with a workspace selected,
            // `r` also re-reads its repos so worktrees created or removed
            // outside Viva show up.
            if app.model().workspace_id.is_some() {
                let v = client.call(crate::server::RequestKind::Workspace {
                    action: "refresh".into(),
                    value: None,
                    project_id: None,
                })?;
                app.set_status(workspace_feedback(&v));
            }
            Ok(())
        }
        WorkbenchAction::SplitRight => spawn_and_split(client, app, SplitAxis::Horizontal),
        WorkbenchAction::SplitBelow => spawn_and_split(client, app, SplitAxis::Vertical),
        WorkbenchAction::OpenWorktreeShell(worktree_id) => {
            if app.selected_scene() != worktree_id {
                app.select_worktree(worktree_id.clone());
            }
            let has_tab = app
                .scenes
                .scenes
                .get(app.selected_scene())
                .is_some_and(|s| !s.active.is_empty());
            if !has_tab && !app.new_tab("Terminal".into()) {
                return Ok(());
            }
            let value =
                client.call(crate::server::RequestKind::TerminalOpenInWorktree { worktree_id })?;
            let terminal_id = value
                .get("terminal_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    crate::foundation::Error::Validation("open returned no terminal id".into())
                })?
                .to_string();
            app.attach_terminal(terminal_id.clone());
            app.enter_terminal_mode(terminal_id);
            Ok(())
        }
        WorkbenchAction::BrowseFiles(worktree_id) => {
            let value = client.call(crate::server::RequestKind::WorktreeFiles {
                worktree_id: worktree_id.clone(),
            })?;
            let rows = value
                .get("files")
                .and_then(|v| v.as_array())
                .map(|files| {
                    files
                        .iter()
                        .filter_map(|f| {
                            Some((
                                f.get("path")?.as_str()?.to_string(),
                                f.get("status")?.as_str()?.to_string(),
                            ))
                        })
                        .collect::<Vec<_>>()
                })
                .unwrap_or_default();
            if rows.is_empty() {
                app.set_status("no files in this worktree");
                return Ok(());
            }
            let truncated = value
                .get("truncated")
                .and_then(|v| v.as_bool())
                .unwrap_or(false);
            let total = value
                .get("total")
                .and_then(|v| v.as_u64())
                .unwrap_or(rows.len() as u64) as usize;
            app.file_panel = Some(FilePanel {
                worktree_id: worktree_id.clone(),
                rows,
                truncated,
                total,
                selected: 0,
                viewing: None,
                full_path: None,
                scroll: 0,
            });
            app.file_view = None;
            if truncated {
                app.set_status(format!(
                    "file list truncated to {} of {total} — narrow the worktree first",
                    worktree_id
                ));
            }
            Ok(())
        }
        WorkbenchAction::ViewFile { worktree_id, path } => {
            let value = client.call(crate::server::RequestKind::WorktreeFileContent {
                worktree_id: worktree_id.clone(),
                path: path.clone(),
            })?;
            let kind = value
                .get("kind")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            let text = value
                .get("text")
                .and_then(|v| v.as_str())
                .unwrap_or("(binary file — no text preview)")
                .to_string();
            let full_path = value
                .get("full_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            if let Some(panel) = app.file_panel.as_mut() {
                panel.viewing = Some(path);
                panel.full_path = Some(full_path);
                panel.scroll = 0;
            }
            app.file_view = Some(format!("[{kind}]\n{text}"));
            Ok(())
        }
        WorkbenchAction::OpenInEditor { full_path } => {
            let opener = if cfg!(target_os = "macos") {
                "open"
            } else {
                "xdg-open"
            };
            let opened = Command::new(opener).arg(&full_path).spawn().is_ok();
            app.set_status(if opened {
                format!("opened {full_path} with {opener}")
            } else {
                format!("could not launch {opener} for {full_path}")
            });
            Ok(())
        }
        WorkbenchAction::CleanupWorktree(worktree_id) => {
            let value = client.call(crate::server::RequestKind::WorktreeCleanup {
                worktree_id: worktree_id.clone(),
            })?;
            let path = value
                .get("path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            app.set_status(format!("worktree cleaned: {path}"));
            Ok(())
        }
        WorkbenchAction::HandoffTo { worktree_id, agent } => {
            let value = client.call(crate::server::RequestKind::AgentHandoff {
                worktree_id: worktree_id.clone(),
                to_agent: agent.clone(),
            })?;
            let terminal_id = value
                .get("terminal_id")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            app.attach_terminal(terminal_id.clone());
            app.enter_terminal_mode(terminal_id);
            app.set_status(format!("handed to {agent} — brief delivered"));
            Ok(())
        }
        WorkbenchAction::ToggleAutoPull => {
            let on = !app.model.auto_pull;
            client.call(crate::server::RequestKind::SetAutoPull { on })?;
            app.model.auto_pull = on;
            app.set_status(format!(
                "auto_pull {} (main checkout only; worktrees stay fetch-only)",
                if on { "ON" } else { "OFF" }
            ));
            Ok(())
        }
        WorkbenchAction::CreateTaskWorktree(task_id) => {
            let value = client.call(crate::server::RequestKind::WorktreeCreateForTask {
                task_id: task_id.clone(),
                branch: None,
                base_dir: None,
            })?;
            let path = value
                .get("worktree_path")
                .and_then(|v| v.as_str())
                .unwrap_or("")
                .to_string();
            app.set_status(format!("worktree created for task {task_id}: {path}"));
            Ok(())
        }
        WorkbenchAction::UseMachine {
            machine_id,
            workspace_id,
            terminal_id,
        } => {
            let Some(machine) = app
                .model
                .machines
                .iter()
                .find(|machine| machine.id == machine_id)
                .cloned()
            else {
                app.set_status(format!("machine {machine_id} is not in the sidebar"));
                return Ok(());
            };
            if !machine.connected || !machine.enabled {
                let why = if machine.enabled {
                    "disconnected"
                } else {
                    "disabled"
                };
                app.set_status(format!("{} is {why}; input is refused", machine.label));
                return Ok(());
            }
            if machine_id == crate::machines::LOCAL_MACHINE_ID {
                if let Some(id) = workspace_id {
                    return apply_client_action(
                        client,
                        app,
                        WorkbenchAction::Workspace {
                            action: "select".into(),
                            value: Some(id),
                            project_id: None,
                        },
                    );
                }
                if let Some(id) = terminal_id {
                    app.enter_terminal_mode(id);
                }
                return Ok(());
            }
            if let Some(id) = workspace_id {
                client.call(crate::server::RequestKind::MachineForward {
                    id: machine_id.clone(),
                    kind: Box::new(crate::server::RequestKind::Workspace {
                        action: "select".into(),
                        value: Some(id),
                        project_id: None,
                    }),
                })?;
                app.set_status(format!("machine {}: workspace selected", machine.label));
                return Ok(());
            }
            if let Some(id) = terminal_id {
                app.enter_remote_terminal(machine_id, id);
                return Ok(());
            }
            Ok(())
        }
    }
}

fn deliver_terminal_bytes(
    client: &mut crate::server::Client,
    app: &mut WorkbenchApp,
    polling: &polling::Polling,
    id: &str,
    bytes: Vec<u8>,
) -> std::result::Result<(), String> {
    if let Some((machine, terminal)) = app.remote_terminal.clone() {
        if terminal == id {
            if !app.machine_accepts_input(&machine) {
                let label = app
                    .model
                    .machines
                    .iter()
                    .find(|view| view.id == machine)
                    .map(|view| view.label.clone())
                    .unwrap_or(machine);
                app.terminal_mode = None;
                app.remote_terminal = None;
                return Err(format!("{label} is disconnected; input is refused"));
            }
            return client
                .call(crate::server::RequestKind::MachineForward {
                    id: machine,
                    kind: Box::new(crate::server::RequestKind::TerminalInput {
                        terminal_id: terminal,
                        bytes_hex: crate::server::hex_encode(&bytes),
                    }),
                })
                .map(|_| ())
                .map_err(|err| err.to_string());
        }
    }
    polling.send(id.to_string(), bytes)
}

/// Spawn a shell next to the focused pane: same worktree when the focused
/// terminal has one, else the client's own directory. The pane focus moves
/// into the new leaf.
fn spawn_and_split(
    client: &mut crate::server::Client,
    app: &mut WorkbenchApp,
    axis: SplitAxis,
) -> Result<()> {
    // Capacity BEFORE spawning (QA F6): a terminal the grid cannot place
    // would run orphaned server-side — visible in the agents list, with no
    // pane and no way to close it from the grid.
    if !app.can_split() {
        app.set_status(format!(
            "pane limit reached ({}) — close one first",
            crate::tui::layout::MAX_PANES
        ));
        return Ok(());
    }
    if app
        .scenes
        .scenes
        .get(app.selected_scene())
        .is_none_or(|s| s.active.is_empty())
        && !app.new_tab("Terminal".into())
    {
        return Ok(());
    }
    let scene = app.focused_worktree_id();
    let worktree_id = scene.clone().filter(|id| !id.starts_with("directory:"));
    let cwd = scene
        .as_ref()
        .and_then(|wid| app.model().worktrees.iter().find(|w| &w.worktree_id == wid))
        .map(|w| w.path.clone())
        .unwrap_or_else(|| {
            std::env::current_dir()
                .unwrap_or_default()
                .display()
                .to_string()
        });
    let value = client.call(crate::server::RequestKind::TerminalCreate {
        argv: vec![std::env::var("SHELL").unwrap_or_else(|_| "/bin/sh".into())],
        cwd,
        env: vec![],
        cols: 80,
        rows: 24,
        purpose: "shell".into(),
        worktree_id,
        owner: "user_shell".into(),
    })?;
    let terminal_id = value
        .get("terminal_id")
        .and_then(|v| v.as_str())
        .ok_or_else(|| {
            crate::foundation::Error::Validation("spawn returned no terminal id".into())
        })?
        .to_string();
    if !app.split_pane(axis, terminal_id.clone()) {
        // The split was refused after the spawn (e.g. a pane would fall
        // below the minimum size): roll the terminal back so nothing runs
        // orphaned. The raw-mode client has no log sink — the status line
        // is the record — so a FAILED rollback says so instead of being
        // swallowed.
        match client.call(crate::server::RequestKind::TerminalStop {
            terminal_id: terminal_id.clone(),
        }) {
            Ok(_) => app.set_status(format!("split refused — terminal {terminal_id} stopped")),
            Err(err) => app.set_status(format!(
                "split refused — terminal {terminal_id} could NOT be stopped ({err}); \
                 it keeps running server-side with no pane"
            )),
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &mut WorkbenchApp, w: u16, h: u16) -> String {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| app.draw(f)).expect("draw");
        terminal.backend().to_string()
    }

    fn sample() -> WorkbenchModel {
        WorkbenchModel {
            projects: vec![ProjectRow {
                project_id: "p1".into(),
                name: "viva".into(),
                repo_path: "/code/viva".into(),
            }],
            worktrees: vec![WorktreeRow {
                worktree_id: "wt1".into(),
                project_id: "p1".into(),
                path: "/wt/dev4".into(),
                branch: "agent/feat-x".into(),
                dirty: Some(true),
                task_id: Some("t1".into()),
                source: "created".into(),
                pr_state: None,
                ahead: None,
                behind: None,
            }],
            tasks: vec![TaskRow {
                task_id: "t1".into(),
                goal: "ship the workbench".into(),
                status: "in_progress".into(),
            }],
            terminals: vec![TerminalRow {
                terminal_id: "term-1".into(),
                worktree_id: Some("wt1".into()),
                purpose: "user shell".into(),
                owner_label: "user_shell".into(),
                live: Some(true),
                agent_status: vec![],
            }],
            attention: vec![AttentionMarker {
                kind: "execution_failed".into(),
                detail: "exec-1 exit 2".into(),
            }],
            auto_pull: false,
            sync_note: None,
            agents: vec![],
            workspaces: vec![],
            workspace_id: None,
            workspace_diagnostics: vec![],
            machines: vec![],
        }
    }

    #[test]
    fn enter_on_worktree_selects_scene_without_opening_shell() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        assert_eq!(app.focus(), Focus::Worktrees);
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            KeyOutcome::Action(WorkbenchAction::SelectWorktree("wt1".into()))
        );
        app.select_worktree("wt1".into());
        assert_eq!(app.selected_scene(), "wt1");
        assert_eq!(app.terminal_leaves(), Vec::<String>::new());
    }

    #[test]
    fn disconnected_machine_stays_dim_while_the_other_still_accepts_input() {
        use crate::agents::AgentSessionRow;
        use crate::machines::{LOCAL_MACHINE_ID, MachineView};
        use crate::workspaces::navigation::WorkspaceRow;

        let workspace = |id: &str, name: &str| WorkspaceRow {
            workspace_id: id.into(),
            name: name.into(),
            source_path: None,
            opened_at: String::new(),
        };
        let agent = |id: &str, name: &str| AgentSessionRow {
            terminal_id: id.into(),
            tool: "pi".into(),
            name: name.into(),
            worktree_id: None,
            location: name.into(),
            live: Some(true),
            records: vec![],
            stale: false,
            in_workspace: true,
            report_capability: "report:unverified".into(),
        };
        let machine =
            |id: &str, label: &str, connected: bool, ws: &str, agent_name: &str| MachineView {
                id: id.into(),
                label: label.into(),
                ssh_target: if id == LOCAL_MACHINE_ID {
                    String::new()
                } else {
                    format!("{id}-host")
                },
                remote_session: "default".into(),
                enabled: true,
                connected,
                error: if connected {
                    None
                } else {
                    Some("connection closed".into())
                },
                workspaces: vec![workspace(&format!("{id}-ws"), ws)],
                agents: vec![agent(&format!("{id}-term"), agent_name)],
                workspace_id: Some(format!("{id}-ws")),
            };
        let mut model = sample();
        model.machines = vec![
            machine(LOCAL_MACHINE_ID, "local", true, "homews", "homeag"),
            machine("lab", "Lab", false, "labws", "labag"),
            machine("other", "Other", true, "otherws", "otherag"),
        ];
        let mut app = WorkbenchApp::new();
        app.set_model(model);
        let view = render(&mut app, 120, 48);
        assert!(view.contains("labws"), "{view}");
        assert!(view.contains("disconnected"), "{view}");
        assert!(view.contains("otherws"), "{view}");
        assert!(view.contains("homews"), "{view}");

        // The local worktree is still the first row.
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            KeyOutcome::Action(WorkbenchAction::SelectWorktree("wt1".into()))
        );
        // worktree, homews, labws.
        app.on_key(KeyEvent::from(KeyCode::Down));
        app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            KeyOutcome::Handled
        );
        assert!(
            app.status_line.contains("input is refused"),
            "{}",
            app.status_line
        );
        app.on_key(KeyEvent::from(KeyCode::Down));
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            KeyOutcome::Action(WorkbenchAction::UseMachine {
                machine_id: "other".into(),
                workspace_id: Some("other-ws".into()),
                terminal_id: None,
            })
        );

        let _ = render(&mut app, 120, 48);
        let click = |x: u16, y: u16| {
            use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
            MouseEvent {
                kind: MouseEventKind::Down(MouseButton::Left),
                column: x,
                row: y,
                modifiers: ratatui::crossterm::event::KeyModifiers::empty(),
            }
        };
        let lab_agent = app
            .sidebar_hits
            .iter()
            .find(|(focus, index, _)| *focus == Focus::Terminals && *index == 0)
            .map(|(_, _, rect)| *rect)
            .expect("lab agent row");
        app.on_mouse(click(lab_agent.x, lab_agent.y));
        assert!(app.take_mouse_action().is_none());
        assert!(
            app.status_line.contains("input is refused"),
            "{}",
            app.status_line
        );
        let other_agent = app
            .sidebar_hits
            .iter()
            .find(|(focus, index, _)| *focus == Focus::Terminals && *index == 1)
            .map(|(_, _, rect)| *rect)
            .expect("other agent row");
        app.on_mouse(click(other_agent.x, other_agent.y));
        assert_eq!(
            app.take_mouse_action(),
            Some(WorkbenchAction::UseMachine {
                machine_id: "other".into(),
                workspace_id: None,
                terminal_id: Some("other-term".into()),
            })
        );

        app.enter_remote_terminal("lab".into(), "lab-term".into());
        let mut disconnected = app.model().clone();
        for machine in &mut disconnected.machines {
            if machine.id == "lab" {
                machine.connected = false;
            }
        }
        app.set_model(disconnected);
        assert!(app.terminal_mode().is_none());
        assert!(
            app.status_line.contains("input is refused"),
            "{}",
            app.status_line
        );
    }

    #[test]
    fn prefix_shift_rename_tab_matches_uppercase_terminal_events() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        let prefix = KeyEvent::new(
            KeyCode::Char('b'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        );
        app.on_key(prefix);
        assert_eq!(
            app.on_key(KeyEvent::new(
                KeyCode::Char('T'),
                ratatui::crossterm::event::KeyModifiers::SHIFT,
            )),
            KeyOutcome::Handled,
        );
        assert!(app.tab_prompt.is_some());
    }

    #[test]
    fn prefix_opens_new_tab_and_split_actions() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        let prefix = KeyEvent::new(
            KeyCode::Char('b'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        );
        assert_eq!(app.on_key(prefix), KeyOutcome::Handled);
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('c'))),
            KeyOutcome::Action(WorkbenchAction::NewTab)
        );
        assert_eq!(app.on_key(prefix), KeyOutcome::Handled);
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('v'))),
            KeyOutcome::Action(WorkbenchAction::SplitRight)
        );
    }

    #[test]
    fn sidebar_renders_worktree_facts_and_prefix_toggles_it() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        assert_eq!(app.focus(), Focus::Worktrees);
        let view = render(&mut app, 100, 24);
        assert!(view.contains("agent/feat-x"), "{view}");
        assert!(view.contains("dirty"), "real dirty state is a fact: {view}");
        assert!(view.contains("└─"), "tree glyphs group worktrees: {view}");
        let prefix = KeyEvent::new(
            KeyCode::Char('b'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        );
        app.on_key(prefix);
        app.on_key(KeyEvent::from(KeyCode::Char('b')));
        let collapsed = render(&mut app, 100, 24);
        assert!(
            !collapsed.contains("agent/feat-x"),
            "collapsed sidebar hides list rows: {collapsed}"
        );
    }

    #[test]
    fn attention_markers_surface_without_second_state() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        // Wide enough that the attention suffix is not clipped.
        let view = render(&mut app, 200, 20);
        assert!(
            view.contains("needs attention") && view.contains("execution_failed"),
            "markers are visible facts: {view}"
        );
    }

    #[test]
    fn terminal_mode_forwards_bytes_and_esc_reaches_cli() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.enter_terminal_mode("term-1");

        // Typing forwards real bytes; Ctrl-C forwards 0x03 to the child
        // instead of killing the workbench.
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('x'))),
            KeyOutcome::Forward(b"x".to_vec())
        );
        let ctrl_c = KeyEvent::new(
            KeyCode::Char('c'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        );
        assert_eq!(app.on_key(ctrl_c), KeyOutcome::Forward(vec![0x03]));
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            KeyOutcome::Forward(b"\r".to_vec())
        );
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Esc)),
            KeyOutcome::Forward(vec![27])
        );
        assert_eq!(app.terminal_mode(), Some("term-1"));
        let prefix = KeyEvent::new(
            KeyCode::Char('b'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        );
        assert_eq!(app.on_key(prefix), KeyOutcome::Handled);
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('?'))),
            KeyOutcome::Handled,
            "prefix+? works while forwarding mode is active"
        );
        assert_eq!(app.terminal_mode(), Some("term-1"));
    }

    #[test]
    fn prefix_focus_moves_retarget_terminal_forwarding() {
        use ratatui::crossterm::event::KeyModifiers;
        let none = KeyModifiers::empty();
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.grid = PaneNode::split_new(
            PaneContent::Terminal("term-1".into()),
            SplitAxis::Horizontal,
            PaneContent::Terminal("term-2".into()),
        );
        app.pane_focus = PaneContent::Terminal("term-1".into());
        app.last_pane_area = Some(Rect::new(0, 0, 100, 40));
        app.enter_terminal_mode("term-1");

        assert_eq!(
            follow(&mut app, KeyCode::Char('l'), none),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("term-2".into()));
        assert_eq!(app.terminal_mode(), Some("term-2"));
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('a'))),
            KeyOutcome::Forward(b"a".to_vec())
        );
    }

    #[test]
    fn control_combos_map_to_real_control_bytes_without_panicking() {
        // D7 regression: every Ctrl combo must produce a defined byte —
        // no overflow panic (Ctrl+Space), no garbage (Ctrl+9).
        let ctrl = ratatui::crossterm::event::KeyModifiers::CONTROL;
        assert_eq!(encode_char(ctrl, 'c'), vec![0x03]);
        assert_eq!(encode_char(ctrl, ' '), vec![0x00]); // Ctrl+Space → NUL
        assert_eq!(encode_char(ctrl, '['), vec![0x1b]); // Ctrl+[ → Esc
        assert_eq!(encode_char(ctrl, 'z'), vec![0x1a]);
        // A Ctrl combo with no control-byte meaning forwards the raw char.
        assert_eq!(encode_char(ctrl, '9'), b"9".to_vec());
        assert_eq!(encode_char(ctrl, 'é'), "é".as_bytes().to_vec());
        // Plain characters are unaffected.
        assert_eq!(
            encode_char(ratatui::crossterm::event::KeyModifiers::empty(), 'x'),
            b"x".to_vec()
        );
    }

    #[test]
    fn editing_and_navigation_keys_are_forwarded_in_terminal_mode() {
        let mut app = WorkbenchApp::new();
        app.enter_terminal_mode("t");
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Delete)),
            KeyOutcome::Forward(b"\x1b[3~".to_vec())
        );
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::PageUp)),
            KeyOutcome::Forward(b"\x1b[5~".to_vec())
        );
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Home)),
            KeyOutcome::Forward(b"\x1b[H".to_vec())
        );
    }

    #[test]
    fn paste_forwards_bracketed_when_cli_reports_the_mode() {
        let mut app = WorkbenchApp::new();
        app.attach_terminal("t1".into());
        app.enter_terminal_mode("t1");
        app.set_snapshot(
            "t1",
            snapshot_with_modes(crate::terminal::screen::InputModes {
                bracketed_paste: true,
                ..Default::default()
            }),
        );
        assert_eq!(
            app.on_paste("line1\nline2"),
            KeyOutcome::Forward(b"\x1b[200~line1\nline2\x1b[201~".to_vec())
        );
    }

    #[test]
    fn kitty_disambiguate_forwards_ctrl_c_and_ctrl_bracket_still_releases() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let ctrl = ratatui::crossterm::event::KeyModifiers::CONTROL;
        let mut app = app_with_terminal_pane();
        app.pane_focus = PaneContent::Terminal("t1".into());
        app.enter_terminal_mode("t1");
        app.set_snapshot(
            "t1",
            snapshot_with_modes(crate::terminal::screen::InputModes {
                kitty_flags: 1,
                ..Default::default()
            }),
        );
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char('c'), ctrl)),
            KeyOutcome::Forward(b"\x1b[99;5u".to_vec())
        );
        assert_eq!(
            app.on_key(KeyEvent::new(KeyCode::Char(']'), ctrl)),
            KeyOutcome::Forward(b"\x1b[93;5u".to_vec()),
            "Ctrl+] forwards as Kitty CSI u; workbench prefix is ctrl+b on main"
        );
        assert_eq!(app.terminal_mode(), Some("t1"));
        app.on_mouse(MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 5,
            row: 5,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        });
        assert!(
            app.terminal_mode().is_none(),
            "switching pane focus releases terminal forwarding on main"
        );
    }

    fn app_with_terminal_pane() -> WorkbenchApp {
        let mut app = WorkbenchApp::new();
        app.attach_terminal("t1".into());
        app.grid = PaneNode::split_new(
            PaneContent::Browser,
            SplitAxis::Horizontal,
            PaneContent::Terminal("t1".into()),
        );
        app.last_pane_area = Some(Rect::new(0, 0, 100, 40));
        app
    }

    #[test]
    fn terminal_mouse_forwards_sgr_and_shift_keeps_host_scroll() {
        use ratatui::crossterm::event::{MouseButton, MouseEvent, MouseEventKind};
        let mut app = app_with_terminal_pane();
        app.set_snapshot(
            "t1",
            snapshot_with_modes(crate::terminal::screen::InputModes {
                mouse_mode: 3,
                mouse_encoding: 2,
                ..Default::default()
            }),
        );
        app.pane_geometry = vec![("t1".into(), Rect::new(50, 5, 40, 20))];
        app.enter_terminal_mode("t1");
        let event = MouseEvent {
            kind: MouseEventKind::Down(MouseButton::Left),
            column: 52,
            row: 7,
            modifiers: ratatui::crossterm::event::KeyModifiers::NONE,
        };
        let (id, bytes) = app.terminal_mouse(event).expect("cli mouse");
        assert_eq!(id, "t1");
        assert_eq!(bytes, b"\x1b[<0;3;3M");
        let shifted = MouseEvent {
            modifiers: ratatui::crossterm::event::KeyModifiers::SHIFT,
            ..event
        };
        assert!(
            app.terminal_mouse(shifted).is_none(),
            "Shift reserves host scroll/selection"
        );
    }

    #[test]
    fn draw_records_terminal_inner_rect_for_resize() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.attach_terminal("term-1".into());
        render(&mut app, 120, 30);
        let geom = app.visible_geometry();
        assert!(
            geom.iter()
                .any(|(id, rect)| id == "term-1" && rect.width > 0 && rect.height > 0),
            "pane inner rect must be published for PTY resize: {geom:?}"
        );
    }

    fn snapshot_with(lines: &[&str]) -> TerminalSnapshot {
        TerminalSnapshot {
            screen: None,
            cols: 80,
            rows: 12,
            visible: lines.iter().map(|s| s.to_string()).collect(),
            scrollback: vec![],
            scrollback_capped: false,
            total_output_bytes: 64,
            log_truncated: false,
        }
    }

    fn snapshot_with_modes(modes: crate::terminal::screen::InputModes) -> TerminalSnapshot {
        TerminalSnapshot {
            screen: Some(crate::terminal::screen::ScreenView {
                lines: vec![],
                cursor: (0, 0),
                cursor_visible: true,
                modes,
                scrollback_offset: 0,
                retained: 0,
            }),
            cols: 80,
            rows: 12,
            visible: vec![],
            scrollback: vec![],
            scrollback_capped: false,
            total_output_bytes: 0,
            log_truncated: false,
        }
    }

    #[test]
    fn pane_tree_renders_multiple_terminal_panes_side_by_side() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.attach_terminal("t1".into());
        app.split_pane(SplitAxis::Vertical, "t2".into());
        app.split_pane(SplitAxis::Horizontal, "t3".into());
        assert_eq!(app.terminal_leaves(), vec!["t1", "t2", "t3"]);
        app.set_snapshot("t1", snapshot_with(&["alpha-out"]));
        app.set_snapshot("t2", snapshot_with(&["beta-out"]));
        app.set_snapshot("t3", snapshot_with(&["gamma-out"]));

        let view = render(&mut app, 160, 44);
        assert!(view.contains("terminal t1"), "{view}");
        assert!(view.contains("terminal t2"));
        assert!(view.contains("terminal t3"));
        assert!(view.contains("alpha-out"));
        assert!(view.contains("beta-out"));
        assert!(view.contains("gamma-out"));
    }

    #[test]
    fn closing_a_pane_is_a_view_operation_and_zoom_isolates_one_pane() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.attach_terminal("t1".into());
        app.split_pane(SplitAxis::Vertical, "t2".into());
        // The focused pane is t2; closing it keeps t1 running in the server.
        app.close_pane();
        assert_eq!(app.terminal_leaves(), vec!["t1"]);
        // Focus t1 (close returned focus to the browser), then zoom in on it.
        app.attach_terminal("t1".into());
        app.toggle_zoom();
        app.set_snapshot("t1", snapshot_with(&["solo-out"]));
        let view = render(&mut app, 160, 44);
        assert!(view.contains("solo-out"), "{view}");
        assert!(view.contains("ZOOM"), "zoom shows as a mode chip: {view}");
        // Zoom shows only the focused pane: the overview card is not part
        // of the zoomed chrome.
        assert!(
            !view.contains("opens a worktree shell"),
            "zoom hides the overview card: {view}"
        );
        app.toggle_zoom();
        let view = render(&mut app, 160, 44);
        assert!(view.contains("solo-out"));
        // The sidebar survives zoom, like herdr's.
        assert!(view.contains("spaces") && view.contains("agents"));
    }

    #[test]
    fn pane_focus_moves_geometrically_between_leaves() {
        let mut app = WorkbenchApp::new();
        app.attach_terminal("t1".into());
        app.split_pane(SplitAxis::Vertical, "t2".into());
        // Focus is on t2 after the split; move up to t1, left to the browser.
        app.move_pane_focus(Direction::Up);
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("t1".into()));
        assert!(
            !app.move_pane_focus(Direction::Left),
            "single terminal column has no browser neighbor"
        );
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("t1".into()));
    }

    #[test]
    fn unknown_states_are_displayed_not_guessed() {
        let mut model = sample();
        model.worktrees[0].dirty = None;
        model.terminals[0].live = None;
        let mut app = WorkbenchApp::new();
        app.set_model(model);
        // Spaces section: unreadable tree shown as unknown.
        let view = render(&mut app, 100, 24);
        assert!(view.contains("unknown"), "{view}");
    }

    #[test]
    fn spaces_group_worktrees_under_their_project_with_tree_glyphs() {
        let mut model = sample();
        model.worktrees.push(WorktreeRow {
            worktree_id: "wt2".into(),
            project_id: "p1".into(),
            path: "/wt/dev5".into(),
            branch: "agent/feat-y".into(),
            dirty: Some(false),
            task_id: None,
            source: "created".into(),
            pr_state: Some("open".into()),
            ahead: Some(1),
            behind: Some(2),
        });
        let mut app = WorkbenchApp::new();
        app.set_model(model);
        let view = render(&mut app, 100, 24);
        assert!(
            view.contains("├─") && view.contains("└─"),
            "tree glyphs nest children: {view}"
        );
        assert!(view.contains("agent/feat-y"));
        assert!(view.contains("pr:open"), "PR state is a fact: {view}");
        assert!(view.contains("↑1↓2"), "git distance is a fact: {view}");
    }

    #[test]
    fn overview_card_shows_real_counts_only() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        let view = render(&mut app, 100, 24);
        assert!(
            view.contains("1 projects · 1 worktrees · 1 terminals (1 live) · 1 tasks"),
            "{view}"
        );
    }

    /// S4/§5.3 honesty, rendered: a screen-inferred status may appear as a
    /// dot, but never in the state color — only a controlled report may
    /// look authoritative.
    #[test]
    fn screen_inferred_status_dots_never_look_authoritative() {
        use crate::agents::{AgentStatus, AgentStatusRecord, StatusSource};
        let palette = Palette::catppuccin_mocha();
        let record = |source: StatusSource| AgentStatusRecord {
            terminal_id: "term-1".into(),
            agent: "pi".into(),
            status: AgentStatus::Working,
            source,
            detail: "test".into(),
            updated_at: "2026-10-05T00:00:00Z".into(),
        };

        let mut model = sample();
        model.terminals[0].agent_status = vec![record(StatusSource::ScreenInference)];
        model.agents = vec![crate::agents::AgentSessionRow {
            terminal_id: "term-1".into(),
            tool: "pi".into(),
            name: "Pi".into(),
            worktree_id: Some("wt1".into()),
            location: "viva / agent/feat-x".into(),
            live: Some(true),
            records: vec![record(StatusSource::ScreenInference)],
            stale: false,
            in_workspace: true,
            report_capability: "report:unverified".into(),
        }];
        let mut app = WorkbenchApp::new();
        app.set_model(model);
        assert_eq!(
            sidebar_dot(&mut app, 100, 24),
            palette.overlay0,
            "screen inference stays muted"
        );

        let mut model = sample();
        model.terminals[0].agent_status = vec![record(StatusSource::ControlledReport)];
        model.agents = vec![crate::agents::AgentSessionRow {
            terminal_id: "term-1".into(),
            tool: "pi".into(),
            name: "Pi".into(),
            worktree_id: Some("wt1".into()),
            location: "viva / agent/feat-x".into(),
            live: Some(true),
            records: vec![record(StatusSource::ControlledReport)],
            stale: false,
            in_workspace: true,
            report_capability: "report:live".into(),
        }];
        let mut app = WorkbenchApp::new();
        app.set_model(model);
        assert_eq!(
            sidebar_dot(&mut app, 100, 24),
            palette.yellow,
            "a controlled report may carry the state color"
        );
    }

    /// The first filled dot in the sidebar's leftmost column: the agents
    /// section's status dot for the first terminal row.
    fn sidebar_dot(app: &mut WorkbenchApp, w: u16, h: u16) -> ratatui::style::Color {
        let backend = TestBackend::new(w, h);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|f| app.draw(f)).expect("draw");
        let buffer = terminal.backend().buffer();
        for y in 0..h {
            if let Some(cell) = buffer.cell(ratatui::layout::Position::new(0, y)) {
                if cell.symbol() == "●" {
                    return cell.fg;
                }
            }
        }
        panic!("no status dot found in the sidebar");
    }

    // ------------------------------------------------------------------
    // QA-round regressions (branch agent/feat-herdr-style-workbench-ui)
    // ------------------------------------------------------------------

    /// F1: the snapshot facts footer (scrollback size, output bytes) must
    /// stay visible when the output is BUSY — that is exactly when the
    /// honest history reading matters.
    #[test]
    fn terminal_pane_footer_line_stays_visible_on_busy_output() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.attach_terminal("t1".into());
        let busy: Vec<String> = (0..45).map(|i| format!("out-{i}")).collect();
        let view = snapshot_with(&busy.iter().map(String::as_str).collect::<Vec<_>>());
        app.set_snapshot(
            "t1",
            TerminalSnapshot {
                screen: None,
                total_output_bytes: 123_456,
                ..view
            },
        );
        let frame = render(&mut app, 160, 44);
        assert!(
            frame.contains("output bytes: 123456"),
            "the facts footer must survive clipping on busy output: {frame}"
        );
        assert!(frame.contains("out-44"), "the newest output stays: {frame}");
    }

    /// F2: the sidebar viewport follows the keyboard selection — the
    /// agents/tasks sections stay reachable on tall workspaces.
    #[test]
    fn sidebar_viewport_follows_the_selection() {
        let mut model = sample();
        for i in 0..12 {
            model.worktrees.push(WorktreeRow {
                worktree_id: format!("wt-{i}"),
                project_id: "p1".into(),
                path: format!("/wt/dev-{i}"),
                branch: format!("agent/feat-{i}"),
                dirty: Some(false),
                task_id: None,
                source: "created".into(),
                pr_state: None,
                ahead: None,
                behind: None,
            });
        }
        let mut app = WorkbenchApp::new();
        app.set_model(model);
        // Walk to the last worktree row (index 12): it must be on screen.
        for _ in 0..12 {
            app.on_key(KeyEvent::from(KeyCode::Char('j')));
        }
        let frame = render(&mut app, 100, 24);
        assert!(
            frame.contains("agent/feat-11"),
            "the selected row must follow the viewport: {frame}"
        );
    }

    /// F4: the diff overlay closes with Esc (it used to be unclosable)
    /// and scrolls, so a bounded/truncated diff stays readable.
    #[test]
    fn diff_overlay_closes_with_esc_and_scrolls() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        let diff = (0..50)
            .map(|i| format!("line-{i} +added"))
            .collect::<Vec<_>>()
            .join("\n");
        app.set_diff_view(Some(diff));
        let frame = render(&mut app, 120, 24);
        assert!(frame.contains("line-1 +added"), "{frame}");
        assert!(
            frame.contains("Esc close"),
            "the close affordance is advertised: {frame}"
        );
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Esc)),
            KeyOutcome::Handled
        );
        assert!(
            !render(&mut app, 120, 24).contains("+added"),
            "the diff overlay closed"
        );

        // Scrolling moves the window and shows the range note.
        app.set_diff_view(Some(
            (0..50)
                .map(|i| format!("line-{i}"))
                .collect::<Vec<_>>()
                .join("\n"),
        ));
        app.on_key(KeyEvent::from(KeyCode::PageDown));
        let frame = render(&mut app, 120, 24);
        assert!(frame.contains("lines 21"), "scroll range shown: {frame}");
    }

    /// F5: needs-attention markers are facts — they stay visible on a
    /// narrow terminal; the key hints are what get truncated.
    #[test]
    fn attention_markers_survive_narrow_terminals() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        let frame = render(&mut app, 100, 24);
        assert!(
            frame.contains("needs attention") && frame.contains("execution_failed"),
            "{frame}"
        );
    }

    /// F7: attaching from the browser never silently consumes an existing
    /// pane; re-attaching a placed terminal just focuses it.
    #[test]
    fn attach_from_browser_splits_and_never_consumes_a_pane() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.attach_terminal("t1".into());
        app.move_pane_focus(Direction::Left); // back to the browser leaf
        app.attach_terminal("t2".into());
        let leaves = app.terminal_leaves();
        assert!(
            leaves.contains(&"t1".to_string()) && leaves.contains(&"t2".to_string()),
            "no pane may vanish: {leaves:?}"
        );
        // Re-attaching a placed terminal only moves focus.
        app.attach_terminal("t1".into());
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("t1".into()));
        assert_eq!(app.terminal_leaves().len(), 2);
    }

    /// F6: `can_split` gates before the pane cap, so the run loop can
    /// refuse a spawn before creating an orphaned terminal.
    #[test]
    fn can_split_gates_before_the_pane_cap() {
        let mut app = WorkbenchApp::new();
        assert!(app.can_split());
        for i in 0..8 {
            assert!(app.split_pane(SplitAxis::Horizontal, format!("t{i}")));
        }
        assert_eq!(app.terminal_leaves().len(), 8, "browser + 8 terminals");
        assert!(
            !app.can_split(),
            "at the cap no further split may be attempted"
        );
    }

    /// P3: split refuses — with a status note — instead of crushing a
    /// neighbor into an unusable sliver at small terminal sizes.
    #[test]
    fn split_refuses_panes_below_the_minimum_size() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        render(&mut app, 100, 24); // records the real pane area (74×23)
        let mut placed = 0;
        for i in 1..=8 {
            if app.split_pane(SplitAxis::Horizontal, format!("t{i}")) {
                placed += 1;
            } else {
                break;
            }
        }
        assert!(placed < 8, "some split must be refused at 74 columns");
        assert_eq!(
            app.terminal_leaves().len(),
            placed,
            "a refused split never enters the tree"
        );
        let frame = render(&mut app, 100, 24);
        assert!(
            frame.contains("too small"),
            "the refusal is explained: {frame}"
        );
    }

    /// The min-size check covers EVERY leaf: the ratio split can hand the
    /// kept (focused) pane the smaller half on odd widths.
    #[test]
    fn split_checks_the_kept_leaf_too() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        // 15 columns: a 50% split gives the KEPT pane 7 (< 8) and the new
        // leaf 8+1 — the asymmetric (new-leaf-only) check let this through.
        app.last_pane_area = Some(Rect::new(0, 0, 15, 23));
        assert!(
            !app.split_pane(SplitAxis::Horizontal, "t1".into()),
            "a split that starves the kept pane must be refused"
        );
        assert_eq!(
            app.terminal_leaves().len(),
            0,
            "the refused split never entered the tree"
        );
    }

    /// The bottom bar is the current prefix and the mode. Old footer hints
    /// are gone, and the version badge is not part of that bar.
    #[test]
    fn status_bar_shows_only_the_prefix_and_the_mode() {
        let mut app = WorkbenchApp::new();
        let frame = render(&mut app, 100, 24);
        assert!(frame.contains("ctrl+b"), "{frame}");
        assert!(frame.contains("WORKBENCH"), "{frame}");
        assert!(!frame.contains("Enter opens"), "{frame}");
        assert!(
            !frame.contains(concat!("v", env!("CARGO_PKG_VERSION"))),
            "{frame}"
        );
        app.enter_terminal_mode("term-1");
        let frame = render(&mut app, 100, 24);
        assert!(frame.contains("TERMINAL"), "{frame}");
        assert!(!frame.contains("WORKBENCH"), "{frame}");
    }

    fn prefix(app: &mut WorkbenchApp) {
        let out = app.on_key(KeyEvent::new(
            KeyCode::Char('b'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        ));
        assert_eq!(
            out,
            KeyOutcome::Handled,
            "ctrl+b is intercepted and is not written to the pane"
        );
        assert!(app.prefix_pending);
    }

    fn follow(
        app: &mut WorkbenchApp,
        code: KeyCode,
        mods: ratatui::crossterm::event::KeyModifiers,
    ) -> KeyOutcome {
        prefix(app);
        app.on_key(KeyEvent::new(code, mods))
    }

    fn shifted(c: char) -> (KeyCode, ratatui::crossterm::event::KeyModifiers) {
        (
            KeyCode::Char(c.to_ascii_uppercase()),
            ratatui::crossterm::event::KeyModifiers::SHIFT,
        )
    }

    fn terminal_app() -> WorkbenchApp {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.attach_terminal("term-1".into());
        app.grid = PaneNode::split_new(
            PaneContent::Browser,
            SplitAxis::Horizontal,
            PaneContent::Terminal("term-1".into()),
        );
        app.pane_focus = PaneContent::Terminal("term-1".into());
        app.last_pane_area = Some(Rect::new(0, 0, 100, 40));
        app.enter_terminal_mode("term-1");
        app
    }

    #[test]
    fn every_default_binding_triggers_and_ctrl_b_stays_out_of_the_pane() {
        use ratatui::crossterm::event::KeyModifiers;
        let none = KeyModifiers::empty();

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('?'), none),
            KeyOutcome::Handled
        );
        assert!(app.help_open());

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('s'), none),
            KeyOutcome::Handled
        );
        assert!(app.settings_open());

        let mut app = terminal_app();
        let (code, mods) = shifted('n');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        assert_eq!(app.workspace_prompt.as_ref().map(|p| p.action), Some("new"));

        let mut app = terminal_app();
        let (code, mods) = shifted('g');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        assert_eq!(
            app.workspace_prompt.as_ref().map(|p| p.action),
            Some("worktree")
        );

        let mut app = terminal_app();
        let (code, mods) = shifted('w');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        assert_eq!(
            app.workspace_prompt.as_ref().map(|p| p.action),
            Some("rename")
        );

        let mut app = terminal_app();
        let (code, mods) = shifted('d');
        assert_eq!(
            follow(&mut app, code, mods),
            KeyOutcome::Action(WorkbenchAction::Workspace {
                action: "close".into(),
                value: None,
                project_id: None,
            })
        );

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('w'), none),
            KeyOutcome::Handled
        );
        assert!(app.navigate_mode());
        app.model.workspaces = vec![
            crate::workspaces::navigation::WorkspaceRow {
                workspace_id: "ws-a".into(),
                name: "a".into(),
                source_path: None,
                opened_at: String::new(),
            },
            crate::workspaces::navigation::WorkspaceRow {
                workspace_id: "ws-b".into(),
                name: "b".into(),
                source_path: None,
                opened_at: String::new(),
            },
        ];
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Down)),
            KeyOutcome::Action(WorkbenchAction::Workspace {
                action: "select".into(),
                value: Some("ws-b".into()),
                project_id: None,
            })
        );
        assert_eq!(app.workspace_nav, 1);
        assert_eq!(app.take_mouse_action(), None);
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Up)),
            KeyOutcome::Action(WorkbenchAction::Workspace {
                action: "select".into(),
                value: Some("ws-a".into()),
                project_id: None,
            })
        );
        assert_eq!(app.workspace_nav, 0);
        assert_eq!(app.take_mouse_action(), None);
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('h'))),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Browser);
        app.pane_focus = PaneContent::Terminal("term-1".into());
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Right)),
            KeyOutcome::Ignored
        );
        app.pane_focus = PaneContent::Browser;
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('l'))),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("term-1".into()));
        app.grid = PaneNode::split_new(
            PaneContent::Browser,
            SplitAxis::Vertical,
            PaneContent::Terminal("term-1".into()),
        );
        app.pane_focus = PaneContent::Browser;
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('j'))),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("term-1".into()));
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('k'))),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Browser);
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Left)),
            KeyOutcome::Ignored
        );

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('q'), none),
            KeyOutcome::QuitRequested
        );

        let mut app = terminal_app();
        let (code, mods) = shifted('r');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        assert!(app.status_line.contains("defaults"));

        let mut app = terminal_app();
        app.model.attention.push(AttentionMarker {
            kind: "execution_failed".into(),
            detail: "exec-1".into(),
        });
        assert_eq!(
            follow(&mut app, KeyCode::Char('o'), none),
            KeyOutcome::Handled
        );
        assert_eq!(app.focus(), Focus::Terminals);

        let mut app = terminal_app();
        assert!(app.new_tab("one".into()));
        assert!(app.new_tab("two".into()));
        let first = app.scenes.scenes[&app.scenes.selected].active.clone();
        assert_eq!(
            follow(&mut app, KeyCode::Char('n'), none),
            KeyOutcome::Handled
        );
        assert_ne!(app.scenes.scenes[&app.scenes.selected].active, first);
        assert_eq!(
            follow(&mut app, KeyCode::Char('p'), none),
            KeyOutcome::Handled
        );
        assert_eq!(app.scenes.scenes[&app.scenes.selected].active, first);
        assert_eq!(
            follow(&mut app, KeyCode::Char('1'), none),
            KeyOutcome::Handled
        );

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('c'), none),
            KeyOutcome::Action(WorkbenchAction::NewTab)
        );

        let mut app = terminal_app();
        let (code, mods) = shifted('t');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        assert!(app.tab_prompt.is_some());

        let mut app = terminal_app();
        assert!(app.new_tab("only".into()));
        let (code, mods) = shifted('x');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        assert!(
            app.take_tree_ops()
                .iter()
                .any(|op| matches!(op, crate::server::WorkbenchTreeOp::CloseTab { .. }))
        );

        let mut app = terminal_app();
        let (code, mods) = shifted('p');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        assert!(app.pane_prompt.is_some());

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('e'), none),
            KeyOutcome::Handled
        );
        assert!(app.scrollback_edit_path().is_some());

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('['), none),
            KeyOutcome::Handled
        );
        assert!(app.copy_mode);

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('h'), none),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Browser);
        let mut app = terminal_app();
        app.pane_focus = PaneContent::Browser;
        assert_eq!(
            follow(&mut app, KeyCode::Char('l'), none),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("term-1".into()));
        let mut app = terminal_app();
        app.grid = PaneNode::split_new(
            PaneContent::Browser,
            SplitAxis::Vertical,
            PaneContent::Terminal("term-1".into()),
        );
        app.pane_focus = PaneContent::Browser;
        assert_eq!(
            follow(&mut app, KeyCode::Char('j'), none),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("term-1".into()));
        app.pane_focus = PaneContent::Terminal("term-1".into());
        assert_eq!(
            follow(&mut app, KeyCode::Char('k'), none),
            KeyOutcome::Handled
        );
        assert_eq!(app.pane_focus(), &PaneContent::Browser);

        let mut app = terminal_app();
        let (code, mods) = shifted('h');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        let mut app = terminal_app();
        app.pane_focus = PaneContent::Browser;
        let (code, mods) = shifted('l');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        let mut app = terminal_app();
        app.grid = PaneNode::split_new(
            PaneContent::Browser,
            SplitAxis::Vertical,
            PaneContent::Terminal("term-1".into()),
        );
        app.pane_focus = PaneContent::Browser;
        let (code, mods) = shifted('j');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);
        app.pane_focus = PaneContent::Terminal("term-1".into());
        let (code, mods) = shifted('k');
        assert_eq!(follow(&mut app, code, mods), KeyOutcome::Handled);

        let mut app = terminal_app();
        let before = app.pane_focus().clone();
        assert_eq!(follow(&mut app, KeyCode::Tab, none), KeyOutcome::Handled);
        assert_ne!(app.pane_focus(), &before);
        assert_eq!(
            follow(&mut app, KeyCode::BackTab, none),
            KeyOutcome::Handled
        );

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('v'), none),
            KeyOutcome::Action(WorkbenchAction::SplitRight)
        );
        assert_eq!(
            follow(&mut app, KeyCode::Char('-'), none),
            KeyOutcome::Action(WorkbenchAction::SplitBelow)
        );

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('x'), none),
            KeyOutcome::Handled
        );
        assert!(app.take_tree_ops().iter().any(|op| matches!(
            op,
            crate::server::WorkbenchTreeOp::ClosePane { pane_id, .. } if pane_id == "term-1"
        )));

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('z'), none),
            KeyOutcome::Handled
        );
        assert!(app.zoomed());

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('r'), none),
            KeyOutcome::Handled
        );
        assert!(app.resize_mode);

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('b'), none),
            KeyOutcome::Handled
        );
        assert!(app.sidebar_collapsed);

        let mut app = terminal_app();
        assert_eq!(
            follow(&mut app, KeyCode::Char('g'), none),
            KeyOutcome::Handled
        );
        assert!(app.goto_open());

        let help = WorkbenchApp::new().help_text();
        assert!(!help.to_ascii_lowercase().contains("open pi"), "{help}");
        assert!(!help.contains("打开 Pi"), "{help}");
        for row in WorkbenchApp::new().command_catalog() {
            assert!(!row.description.to_ascii_lowercase().contains("open pi"));
            assert!(!row.id.to_ascii_lowercase().contains("open pi"));
            assert!(!row.description.contains("打开 Pi"));
            assert!(!row.id.contains("打开"));
        }
    }

    #[test]
    fn retired_keys_reach_the_pane_in_terminal_mode() {
        use ratatui::crossterm::event::KeyModifiers;
        let mut app = terminal_app();
        for (label, key, bytes) in [
            ("p", KeyEvent::from(KeyCode::Char('p')), b"p".to_vec()),
            ("d", KeyEvent::from(KeyCode::Char('d')), b"d".to_vec()),
            ("f", KeyEvent::from(KeyCode::Char('f')), b"f".to_vec()),
            ("q", KeyEvent::from(KeyCode::Char('q')), b"q".to_vec()),
            (
                "ctrl+]",
                KeyEvent::new(KeyCode::Char(']'), KeyModifiers::CONTROL),
                vec![0x1d],
            ),
        ] {
            assert_eq!(app.on_key(key), KeyOutcome::Forward(bytes), "{label}");
        }
        let mut idle = WorkbenchApp::new();
        assert_ne!(
            idle.on_key(KeyEvent::from(KeyCode::Char('q'))),
            KeyOutcome::QuitRequested
        );
    }
}

#[cfg(test)]
mod v15_tui_fix_tests {
    use super::*;
    use ratatui::backend::TestBackend;

    fn sample() -> WorkbenchModel {
        WorkbenchModel {
            projects: vec![ProjectRow {
                project_id: "p1".into(),
                name: "viva".into(),
                repo_path: "/code/viva".into(),
            }],
            worktrees: vec![WorktreeRow {
                worktree_id: "wt1".into(),
                project_id: "p1".into(),
                path: "/wt/dev4".into(),
                branch: "agent/feat-x".into(),
                dirty: Some(true),
                task_id: Some("t1".into()),
                source: "created".into(),
                pr_state: None,
                ahead: None,
                behind: None,
            }],
            tasks: vec![],
            terminals: vec![],
            attention: vec![],
            auto_pull: false,
            sync_note: None,
            agents: vec![],
            workspaces: vec![],
            workspace_id: None,
            workspace_diagnostics: vec![],
            machines: vec![],
        }
    }

    #[test]
    fn prefix_goto_opens_a_searchable_list() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.on_key(KeyEvent::from(KeyCode::Char('j')));
        assert_eq!(app.selected, 0);
        let prefix = KeyEvent::new(
            KeyCode::Char('b'),
            ratatui::crossterm::event::KeyModifiers::CONTROL,
        );
        app.on_key(prefix);
        app.on_key(KeyEvent::from(KeyCode::Char('g')));
        assert!(app.goto_open());
    }

    #[test]
    fn chrome_advertises_herdr_prefix_keys() {
        let mut app = WorkbenchApp::new();
        let backend = TestBackend::new(200, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|f| app.draw(f)).expect("draw");
        let view = terminal.backend().to_string();
        assert!(view.contains("ctrl+b"), "{view}");
        assert!(view.contains("prefix"), "{view}");
    }

    // -- mouse: divider drags, pane clicks, sidebar resize (herdr parity) --

    fn mouse(
        kind: ratatui::crossterm::event::MouseEventKind,
        x: u16,
        y: u16,
    ) -> ratatui::crossterm::event::MouseEvent {
        ratatui::crossterm::event::MouseEvent {
            kind,
            column: x,
            row: y,
            modifiers: ratatui::crossterm::event::KeyModifiers::empty(),
        }
    }

    fn mouse_down(x: u16, y: u16) -> ratatui::crossterm::event::MouseEvent {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        mouse(MouseEventKind::Down(MouseButton::Left), x, y)
    }

    fn mouse_drag(x: u16, y: u16) -> ratatui::crossterm::event::MouseEvent {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        mouse(MouseEventKind::Drag(MouseButton::Left), x, y)
    }

    fn mouse_up(x: u16, y: u16) -> ratatui::crossterm::event::MouseEvent {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        mouse(MouseEventKind::Up(MouseButton::Left), x, y)
    }

    /// Browser | t1 at 50/50 over a 100x40 pane area: the root divider is
    /// the shared cell at column 49, band 48..=50.
    fn app_with_two_panes() -> WorkbenchApp {
        let mut app = WorkbenchApp::new();
        app.attach_terminal("t1".into());
        app.grid = PaneNode::split_new(
            PaneContent::Browser,
            SplitAxis::Horizontal,
            PaneContent::Terminal("t1".into()),
        );
        app.last_pane_area = Some(Rect::new(0, 0, 100, 40));
        app.last_sidebar_edge = None;
        app
    }

    #[test]
    fn divider_drag_resizes_the_split_and_persists_when_released() {
        let mut app = app_with_two_panes();
        app.clear_layout_dirty();
        app.on_mouse(mouse_down(49, 10));
        assert!(
            matches!(app.drag, DragState::PaneSplit { .. }),
            "a grab on the band starts a pane-split drag"
        );
        app.on_mouse(mouse_drag(69, 10));
        assert!(app.layout_dirty(), "a resize must be persisted");
        assert!(
            matches!(app.grid, PaneNode::Split { ratio: 70, .. }),
            "drag to column 69 = first side 70 cells = ratio 70"
        );
        app.on_mouse(mouse_up(69, 10));
        assert!(matches!(app.drag, DragState::None), "release ends the drag");
        assert!(matches!(app.grid, PaneNode::Split { ratio: 70, .. }));
    }

    #[test]
    fn grab_offset_keeps_the_divider_glued_to_the_pointer() {
        // Grab one cell left of the divider (inside the tolerance band):
        // the grabbed point must stay under the pointer, so dragging to 58
        // puts the divider at 59, i.e. ratio 60 — not 59.
        let mut app = app_with_two_panes();
        app.on_mouse(mouse_down(48, 10));
        app.on_mouse(mouse_drag(58, 10));
        assert!(matches!(app.grid, PaneNode::Split { ratio: 60, .. }));
    }

    #[test]
    fn drag_clamps_into_the_never_zero_band() {
        let mut app = app_with_two_panes();
        app.on_mouse(mouse_down(49, 10));
        app.on_mouse(mouse_drag(0, 10));
        assert!(matches!(app.grid, PaneNode::Split { ratio: 10, .. }));
        app.on_mouse(mouse_drag(99, 10));
        assert!(matches!(app.grid, PaneNode::Split { ratio: 90, .. }));
    }

    #[test]
    fn topology_change_under_a_drag_cancels_it() {
        let mut app = app_with_two_panes();
        app.on_mouse(mouse_down(49, 10));
        // The tree collapses under the drag (e.g. the server pruned the
        // terminal): the path now points elsewhere — cancel, never resize.
        app.grid = PaneNode::leaf(PaneContent::Browser);
        app.on_mouse(mouse_drag(69, 10));
        assert!(matches!(app.drag, DragState::None));
        assert!(matches!(app.grid, PaneNode::Leaf(_)));
    }

    #[test]
    fn clicking_a_pane_focuses_it_and_releases_terminal_mode() {
        let mut app = app_with_two_panes();
        // Focus starts on t1 after the attach; engage keyboard forwarding.
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("t1".into()));
        app.enter_terminal_mode("t1");
        // Clicking the same pane keeps the mode.
        app.on_mouse(mouse_down(75, 10));
        app.on_mouse(mouse_up(75, 10));
        assert_eq!(app.terminal_mode(), Some("t1"));
        // Clicking the browser focuses it and releases the mode.
        app.on_mouse(mouse_down(10, 10));
        app.on_mouse(mouse_up(10, 10));
        assert_eq!(app.pane_focus(), &PaneContent::Browser);
        assert_eq!(app.terminal_mode(), None, "keys must not keep flowing");
    }

    #[test]
    fn double_click_resets_split_and_sidebar() {
        let mut app = app_with_two_panes();
        assert!(app.grid.set_ratio_at_path(&[], 70));
        app.clear_layout_dirty();
        // First click happened a moment ago at the same cell: this is the
        // second click of a double-click.
        app.last_divider_click = Some((std::time::Instant::now(), 69, 10));
        app.on_mouse(mouse_down(69, 10));
        assert!(matches!(app.grid, PaneNode::Split { ratio: 50, .. }));
        assert!(app.layout_dirty(), "the reset must be persisted too");

        let mut app = WorkbenchApp::new();
        app.sidebar_width = 34;
        app.last_sidebar_edge = Some(26);
        app.clear_layout_dirty();
        app.last_divider_click = Some((std::time::Instant::now(), 26, 5));
        app.on_mouse(mouse_down(26, 5));
        assert_eq!(app.sidebar_width, SIDEBAR_WIDTH, "sidebar resets to 26");
        assert!(app.layout_dirty());
    }

    #[test]
    fn sidebar_edge_drag_resizes_within_bounds_and_persists() {
        let mut app = WorkbenchApp::new();
        app.last_sidebar_edge = Some(26);
        app.clear_layout_dirty();
        app.on_mouse(mouse_down(26, 5));
        assert!(matches!(app.drag, DragState::Sidebar));
        app.on_mouse(mouse_drag(10, 5));
        assert_eq!(app.sidebar_width, SIDEBAR_MIN_WIDTH, "clamped at 18");
        app.on_mouse(mouse_drag(99, 5));
        assert_eq!(app.sidebar_width, SIDEBAR_MAX_WIDTH, "clamped at 36");
        app.on_mouse(mouse_drag(22, 5));
        assert_eq!(app.sidebar_width, 22);
        assert!(app.layout_dirty(), "resizes persist");
        app.on_mouse(mouse_up(22, 5));
        assert!(matches!(app.drag, DragState::None));
    }

    #[test]
    fn overlays_swallow_mouse_clicks() {
        let mut app = app_with_two_panes();
        app.file_panel = Some(FilePanel {
            worktree_id: "wt1".into(),
            rows: vec![],
            truncated: false,
            total: 0,
            selected: 0,
            viewing: None,
            full_path: None,
            scroll: 0,
        });
        app.clear_layout_dirty();
        app.on_mouse(mouse_down(49, 10));
        app.on_mouse(mouse_drag(69, 10));
        assert!(matches!(app.drag, DragState::None));
        assert!(
            matches!(app.grid, PaneNode::Split { ratio: 50, .. }),
            "no resize through the overlay"
        );
        assert!(!app.layout_dirty());
    }

    #[test]
    fn the_dragged_divider_lights_up_in_the_drawn_chrome() {
        let mut app = app_with_two_panes();
        let backend = TestBackend::new(160, 44);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        // First draw fixes the real geometry: 160 cols → sidebar 26, pane
        // area 134 wide, root divider at 26 + 67 - 1 = 92.
        terminal.draw(|f| app.draw(f)).expect("draw");
        app.on_mouse(mouse_down(92, 10));
        terminal.draw(|f| app.draw(f)).expect("draw");
        let cell = terminal
            .backend()
            .buffer()
            .cell(ratatui::layout::Position::new(92, 10))
            .expect("cell");
        assert_eq!(cell.symbol(), "│", "the divider stays a line: {cell:?}");
        assert_eq!(
            cell.style().fg,
            Some(ratatui::style::Color::Rgb(203, 166, 247)),
            "dragged divider highlights in mauve: {cell:?}"
        );
        app.on_mouse(mouse_up(92, 10));
    }

    /// Haisu's follow-up QA: grab → nudge → release → grab again is how a
    /// human repositions a divider. The second grab must START a drag, not
    /// fire the double-click reset (which silently snapped the split back
    /// to 50/50 and looked exactly like "the divider won't move").
    #[test]
    fn regrabbing_after_a_real_drag_is_a_drag_not_a_reset() {
        let mut app = app_with_two_panes();
        assert!(app.grid.set_ratio_at_path(&[], 65));
        app.clear_layout_dirty();
        // Drag one: grab at the divider (pos 64 on the 100-wide pane
        // area), move, release.
        app.on_mouse(mouse_down(64, 10));
        app.on_mouse(mouse_drag(54, 10));
        app.on_mouse(mouse_up(54, 10));
        assert!(matches!(app.grid, PaneNode::Split { ratio: 55, .. }));
        // Immediate re-grab at the SAME cell, inside the double-click
        // window: must be a drag, never a reset.
        app.on_mouse(mouse_down(54, 10));
        assert!(
            matches!(app.drag, DragState::PaneSplit { .. }),
            "a re-grab after a moved drag starts a drag, not a reset"
        );
        app.on_mouse(mouse_drag(74, 10));
        assert!(matches!(app.grid, PaneNode::Split { ratio: 75, .. }));
        app.on_mouse(mouse_up(74, 10));

        // A TRUE double-click (two clicks, no drag movement in between)
        // still resets.
        let mut app = app_with_two_panes();
        assert!(app.grid.set_ratio_at_path(&[], 70));
        app.clear_layout_dirty();
        app.on_mouse(mouse_down(69, 10));
        app.on_mouse(mouse_up(69, 10));
        app.on_mouse(mouse_down(69, 10));
        assert!(matches!(app.grid, PaneNode::Split { ratio: 50, .. }));
    }

    #[test]
    fn adopt_shared_tree_keeps_this_clients_tab_and_takes_the_other_tabs_edit() {
        let mut app = WorkbenchApp::new();
        app.sidebar_width = 31;
        app.select_worktree("a".into());
        app.new_tab("local".into());
        let local_id = {
            let saved: serde_json::Value = serde_json::from_str(&app.serialize_layout()).unwrap();
            saved["scenes"]["a"]["active"].as_str().unwrap().to_string()
        };
        let mut server: SceneBook = serde_json::from_str(&app.serialize_layout()).unwrap();
        let extra = TabView::new("from-the-other-client".into());
        let extra_id = extra.id.clone();
        server.scenes.get_mut("a").unwrap().tabs.push(extra);
        server.sidebar = 18;
        let local = server
            .scenes
            .get_mut("a")
            .unwrap()
            .tabs
            .iter_mut()
            .find(|tab| tab.id == local_id)
            .unwrap();
        local.zoom = true;
        local.grid = PaneNode::split_new(
            PaneContent::Browser,
            SplitAxis::Horizontal,
            PaneContent::Terminal("t9".into()),
        );
        app.clear_layout_dirty();
        app.adopt_shared_tree(&serde_json::to_string(&server).unwrap());
        assert_eq!(app.selected_scene(), "a");
        assert_eq!(app.sidebar_width, 31, "sidebar width stays on this client");
        assert!(app.zoomed(), "the viewed tab picks up the server zoom");
        assert_eq!(
            app.terminal_leaves(),
            vec!["t9".to_string()],
            "the viewed tab picks up the server split"
        );
        let saved: serde_json::Value = serde_json::from_str(&app.serialize_layout()).unwrap();
        let names: Vec<_> = saved["scenes"]["a"]["tabs"]
            .as_array()
            .unwrap()
            .iter()
            .map(|tab| tab["name"].as_str().unwrap())
            .collect();
        assert!(names.contains(&"from-the-other-client"), "{names:?}");
        assert!(
            saved["scenes"]["a"]["tabs"]
                .as_array()
                .unwrap()
                .iter()
                .any(|tab| tab["id"] == extra_id)
        );
        assert!(!app.layout_dirty());
    }

    #[test]
    fn sidebar_width_round_trips_through_the_layout_payload() {
        let mut app = WorkbenchApp::new();
        app.sidebar_width = 31;
        let json = app.serialize_layout();
        let mut other = WorkbenchApp::new();
        other.restore_layout(&json, &std::collections::HashSet::new());
        assert_eq!(other.sidebar_width, 31);
        // A payload from before the sidebar field: the default survives.
        let legacy = serde_json::json!({ "grid": PaneNode::leaf(PaneContent::Browser), "focus": PaneContent::Browser }).to_string();
        let mut other = WorkbenchApp::new();
        other.restore_layout(&legacy, &std::collections::HashSet::new());
        assert_eq!(other.sidebar_width, SIDEBAR_WIDTH);
    }

    /// Haisu's 0.3.4 hands-on QA: with two panes stacked on the LEFT
    /// (root horizontal split whose first child is a vertical split),
    /// the stacked pair's divider would not drag. Reproduction at the
    /// real geometry: sidebar 26, terminal 160x44.
    #[test]
    fn nested_first_child_vertical_divider_drags() {
        let mut app = WorkbenchApp::new();
        app.grid = PaneNode::Split {
            axis: SplitAxis::Horizontal,
            first: Box::new(PaneNode::split_new(
                PaneContent::Browser,
                SplitAxis::Vertical,
                PaneContent::Terminal("t2".into()),
            )),
            second: Box::new(PaneNode::leaf(PaneContent::Terminal("t1".into()))),
            ratio: 50,
        };
        app.pane_focus = PaneContent::Browser;
        app.last_pane_area = Some(Rect::new(26, 0, 134, 43));
        app.last_sidebar_edge = Some(26);
        let pane_area = app.last_pane_area.unwrap();

        // The hit model must report the stacked divider on the row the
        // chrome actually draws.
        let hits = app.grid.splits(pane_area);
        let vertical = hits
            .iter()
            .find(|hit| hit.axis == SplitAxis::Vertical)
            .expect("a vertical split exists");
        let (cx, cy) = (60, vertical.pos);
        app.clear_layout_dirty();

        app.on_mouse(mouse_down(cx, cy));
        assert!(
            matches!(app.drag, DragState::PaneSplit { .. }),
            "grabbing the stacked divider at ({cx},{cy}) must start a drag, band {:?}",
            vertical.hit_rect
        );
        app.on_mouse(mouse_drag(cx, cy + 6));
        let ratio_after = match &app.grid {
            PaneNode::Split { first, .. } => match **first {
                PaneNode::Split { ratio, .. } => Some(ratio),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(
            ratio_after,
            Some(62),
            "dragging 6 rows down must grow the top pane (row 26 -> 27/43 = 62%)"
        );
        assert!(app.layout_dirty());
        app.on_mouse(mouse_up(cx, cy + 6));

        // And the same for a stacked pair on the RIGHT side (the second
        // child of the root): browser | (t1 / t2).
        let mut app = WorkbenchApp::new();
        app.grid = PaneNode::Split {
            axis: SplitAxis::Horizontal,
            first: Box::new(PaneNode::leaf(PaneContent::Browser)),
            second: Box::new(PaneNode::split_new(
                PaneContent::Terminal("t1".into()),
                SplitAxis::Vertical,
                PaneContent::Terminal("t2".into()),
            )),
            ratio: 50,
        };
        app.pane_focus = PaneContent::Browser;
        app.last_pane_area = Some(Rect::new(26, 0, 134, 43));
        app.last_sidebar_edge = Some(26);
        let pane_area = app.last_pane_area.unwrap();
        let vertical = app
            .grid
            .splits(pane_area)
            .into_iter()
            .find(|hit| hit.axis == SplitAxis::Vertical)
            .expect("nested vertical split");
        let (cx, cy) = (100, vertical.pos);
        app.clear_layout_dirty();
        app.on_mouse(mouse_down(cx, cy));
        assert!(matches!(app.drag, DragState::PaneSplit { .. }));
        app.on_mouse(mouse_drag(cx, cy - 5));
        let ratio_after = match &app.grid {
            PaneNode::Split { second, .. } => match **second {
                PaneNode::Split { ratio, .. } => Some(ratio),
                _ => None,
            },
            _ => None,
        };
        assert_eq!(
            ratio_after,
            Some(37),
            "dragging 5 rows up must shrink the top pane of the right column (row 15 -> 16/43 = 37%)"
        );
        assert!(app.layout_dirty());
    }

    fn mouse_right(x: u16, y: u16) -> ratatui::crossterm::event::MouseEvent {
        use ratatui::crossterm::event::{MouseButton, MouseEventKind};
        mouse(MouseEventKind::Down(MouseButton::Right), x, y)
    }

    #[test]
    fn mouse_click_drag_and_right_click_split_queue_server_tree_ops() {
        let mut app = app_with_two_panes();
        app.on_mouse(mouse_down(75, 10));
        assert!(app.take_tree_ops().iter().any(|op| matches!(
            op,
            crate::server::WorkbenchTreeOp::FocusPane { pane_id, .. } if pane_id == "t1"
        )));

        let mut app = app_with_two_panes();
        app.on_mouse(mouse_down(49, 10));
        app.on_mouse(mouse_drag(69, 10));
        assert!(
            app.take_tree_ops()
                .iter()
                .any(|op| matches!(op, crate::server::WorkbenchTreeOp::Resize { ratio: 70, .. }))
        );

        let mut app = app_with_two_panes();
        app.on_mouse(mouse_right(40, 8));
        let backend = TestBackend::new(120, 30);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        let (action, rect) = app.menu_hits[0];
        assert_eq!(action, MenuAction::SplitRight);
        app.on_mouse(mouse_right(rect.x + 1, rect.y));
        assert_eq!(app.take_mouse_action(), Some(WorkbenchAction::SplitRight));
        let source = "t1";
        assert!(app.split_pane(SplitAxis::Horizontal, "spawned".into()));
        assert!(app.take_tree_ops().iter().any(|op| matches!(
            op,
            crate::server::WorkbenchTreeOp::Split {
                direction,
                pane_id,
                terminal_id,
                ..
            } if direction == "right"
                && pane_id.as_deref() == Some(source)
                && terminal_id.as_deref() == Some("spawned")
        )));
    }
}
