//! The parallel-development workbench (V14, issue #27): one office spanning
//! many projects, tasks and worktrees, with real terminals per worktree.
//!
//! Since the resident-server split (S1, issue #43; ADR 0012) this module is
//! the CLIENT: facts arrive as `WorkbenchView` projections over the control
//! channel, actions go back as requests, and `q` detaches while the server
//! keeps every terminal running. The server side of the projections lives
//! in [`assemble_view`], called by the office host.
//!
//! Composition rules (this module only combines; the domains stay theirs):
//! - Facts come from the existing registries (projects/tasks V02–V03,
//!   worktrees V08, terminals V05). Lists, dirty flags and needs-attention
//!   markers are projections — there is no second workflow state here, and
//!   unknown states are shown as unknown, never guessed.
//! - Terminals are launched through explicit harness combinations (V09):
//!   Pi by default, any other installed CLI by explicit argv — and shells
//!   and editors run as plain user tools that never fake a member or an
//!   execution.
//! - Diffs and status are real git facts, read-only; editing is delegated
//!   to the user's own editor — the workbench builds no code editor.
//! - Quit is a detach: worktree contents and records are left exactly as
//!   they are, nothing is re-run when the client reattaches.

pub mod scenes;

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};
use scenes::{FLOATING, MAX_SCENES, MAX_TABS, Scene, SceneBook, TabView};

use std::process::Command;

use crate::foundation::error::OfficeResult;
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
    /// created (office allocated) or adopted (explicitly selected).
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
    /// Office's real knowledge; `None` is shown as unknown.
    pub live: Option<bool>,
    /// Agent status observations, PER SOURCE (S4): controlled reports are
    /// authoritative, screen inference is auxiliary. They sit side by
    /// side, never merged into one claim.
    #[serde(default)]
    pub agent_status: Vec<crate::agents::AgentStatusRecord>,
}

/// A needs-attention marker: a fact recorded by the office that a human
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
    /// Create the selected task's worktree (the `w` action).
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
    NewTab,
    Workspace {
        action: String,
        value: Option<String>,
        project_id: Option<String>,
    },
}

/// An in-progress mouse drag (herdr-parity interaction). A pane-split drag
/// mutates the tree's ratio directly — the tree is client-side, so no
/// server round-trip is needed; the existing `layout_dirty` → persistence
/// flow saves the result.
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
    /// `(path, status)` rows as served by the office (bounded: may be a
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
    /// The last divider click for double-click detection: (when, column,
    /// row). Cleared by any click that is not on a divider.
    last_divider_click: Option<(std::time::Instant, u16, u16)>,
    workspace_prompt: Option<(String, String)>,
    scenes: SceneBook,
    tab_prompt: Option<String>,
    tab_hits: Vec<(String, Rect)>,
    terminal_picker: Option<usize>,
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
            diff_view: None,
            status_line: "1 spaces · 2 agents · 3 tasks · Enter/o open · d diff · f files · D cleanup · h handoff · g auto-pull · | - split · x close · z zoom · Ctrl+arrows panes · q detach".into(),
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
            last_divider_click: None,
            workspace_prompt: None,
            scenes:SceneBook::default(),
            tab_prompt:None,
            tab_hits:vec![],
            terminal_picker:None,
        }
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
        if !model.workspace_diagnostics.is_empty() {
            self.set_status(model.workspace_diagnostics.join(" · "));
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
        self.snapshots.insert(terminal_id.into(), snapshot);
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
            "existing scene · n new tab · o new shell · t all terminals · no process started",
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
        let mut book = SceneBook::default();
        book.legacy = Some(value.clone());
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
        match self
            .grid
            .neighbor(PANE_REF_AREA, &self.pane_focus, direction)
        {
            Some(next) => {
                self.pane_focus = next;
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
        self.layout_dirty = true;
    }

    /// Point the focused pane at a terminal. From the browser: split a new
    /// pane whenever capacity allows — an existing pane is never silently
    /// consumed (QA F7); only at the pane cap does the first terminal pane
    /// get reused, with an explicit status note and its snapshot dropped.
    /// From a terminal pane: retarget it (the old terminal keeps running
    /// server-side; its snapshot cache entry goes with the pane).
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
            self.pane_focus = content;
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
        self.terminal_mode = Some(terminal_id.into());
        self.status_line = "terminal focused — Esc to release".into();
    }

    pub fn leave_terminal_mode(&mut self) {
        self.terminal_mode = None;
        self.status_line = "terminal released".into();
    }

    fn row_count(&self) -> usize {
        match self.focus {
            Focus::Worktrees => self.model.worktrees.len(),
            Focus::Tasks => self.model.tasks.len(),
            Focus::Terminals => self.model.terminals.len(),
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

    fn selected_terminal(&self) -> Option<String> {
        self.model
            .terminals
            .get(self.selected)
            .map(|t| t.terminal_id.clone())
    }

    fn selected_worktree(&self) -> Option<String> {
        self.model
            .worktrees
            .get(self.selected)
            .map(|w| w.worktree_id.clone())
    }

    /// Handle one key event.
    pub fn on_key(&mut self, key: KeyEvent) -> KeyOutcome {
        use ratatui::crossterm::event::KeyModifiers;
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
        if let Some((action, text)) = &mut self.workspace_prompt {
            match key.code {
                KeyCode::Esc => self.workspace_prompt = None,
                KeyCode::Backspace => {
                    text.pop();
                }
                KeyCode::Char(c) => text.push(c),
                KeyCode::Enter => {
                    let action = action.clone();
                    let value = Some(text.clone());
                    self.workspace_prompt = None;
                    return KeyOutcome::Action(WorkbenchAction::Workspace {
                        action,
                        value,
                        project_id: None,
                    });
                }
                _ => {}
            }
            return KeyOutcome::Handled;
        }
        // QA F8: stale cleanup arming must never survive an unrelated
        // keypress — only the confirming second `D` keeps it.
        if self.cleanup_armed.is_some()
            && !(key.code == KeyCode::Char('D') && self.focus == Focus::Worktrees)
        {
            self.cleanup_armed = None;
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
                    let (worktree_id, agent) = {
                        let picker = self.handoff_picker.as_ref().unwrap();
                        (
                            picker.worktree_id.clone(),
                            picker.agents[picker.selected].clone(),
                        )
                    };
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
                KeyCode::Char('q') | KeyCode::Char('Q') => {
                    self.quit_requested = true;
                    KeyOutcome::QuitRequested
                }
                _ => KeyOutcome::Ignored,
            };
        }
        // Terminal mode owns the keyboard first: in a focused terminal,
        // even Ctrl-C belongs to the child (0x03), exactly as a real
        // terminal would deliver it. Esc releases the focus.
        if self.terminal_mode.is_some() {
            return match key.code {
                KeyCode::Esc => {
                    self.leave_terminal_mode();
                    KeyOutcome::Handled
                }
                KeyCode::Char(c) => KeyOutcome::Forward(encode_char(key.modifiers, c)),
                KeyCode::Enter => KeyOutcome::Forward(b"\r".to_vec()),
                KeyCode::Backspace => KeyOutcome::Forward(b"\x7f".to_vec()),
                KeyCode::Tab => KeyOutcome::Forward(b"\t".to_vec()),
                KeyCode::Up => KeyOutcome::Forward(b"\x1b[A".to_vec()),
                KeyCode::Down => KeyOutcome::Forward(b"\x1b[B".to_vec()),
                KeyCode::Right => KeyOutcome::Forward(b"\x1b[C".to_vec()),
                KeyCode::Left => KeyOutcome::Forward(b"\x1b[D".to_vec()),
                KeyCode::Home => KeyOutcome::Forward(b"\x1b[H".to_vec()),
                KeyCode::End => KeyOutcome::Forward(b"\x1b[F".to_vec()),
                KeyCode::PageUp => KeyOutcome::Forward(b"\x1b[5~".to_vec()),
                KeyCode::PageDown => KeyOutcome::Forward(b"\x1b[6~".to_vec()),
                KeyCode::Delete => KeyOutcome::Forward(b"\x1b[3~".to_vec()),
                KeyCode::Insert => KeyOutcome::Forward(b"\x1b[2~".to_vec()),
                _ => KeyOutcome::Ignored,
            };
        }
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            self.quit_requested = true;
            return KeyOutcome::QuitRequested;
        }
        // Pane-grid movement (S2): Ctrl+Arrows. The browser pane
        // participates like any other leaf.
        if key.modifiers.contains(KeyModifiers::CONTROL) {
            let direction = match key.code {
                KeyCode::Left => Some(Direction::Left),
                KeyCode::Right => Some(Direction::Right),
                KeyCode::Up => Some(Direction::Up),
                KeyCode::Down => Some(Direction::Down),
                _ => None,
            };
            if let Some(direction) = direction {
                return if self.move_pane_focus(direction) {
                    KeyOutcome::Handled
                } else {
                    KeyOutcome::Ignored
                };
            }
        }
        match key.code {
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.quit_requested = true;
                KeyOutcome::QuitRequested
            }
            // Sidebar sections (herdr order: spaces, agents, tasks).
            KeyCode::Char('1') => {
                self.focus = Focus::Worktrees;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Char('2') => {
                self.focus = Focus::Terminals;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Char('3') => {
                self.focus = Focus::Tasks;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Tab => {
                if self.pane_focus == PaneContent::Browser {
                    self.focus = match self.focus {
                        Focus::Worktrees => Focus::Terminals,
                        Focus::Terminals => Focus::Tasks,
                        Focus::Tasks => Focus::Worktrees,
                    };
                    self.selected = 0;
                } else {
                    self.cycle_pane();
                }
                KeyOutcome::Handled
            }
            KeyCode::Down | KeyCode::Char('j') if self.row_count() > 0 => {
                self.selected = (self.selected + 1).min(self.row_count() - 1);
                KeyOutcome::Handled
            }
            KeyCode::Up | KeyCode::Char('k') => {
                self.selected = self.selected.saturating_sub(1);
                KeyOutcome::Handled
            }
            KeyCode::Enter if self.focus == Focus::Terminals => match self.selected_terminal() {
                Some(id) => KeyOutcome::Action(WorkbenchAction::EnterTerminal(id)),
                None => KeyOutcome::Ignored,
            },
            // Enter on a space row opens a shell at that worktree.
            KeyCode::Enter if self.focus == Focus::Worktrees => match self.selected_worktree() {
                Some(id) => KeyOutcome::Action(WorkbenchAction::SelectWorktree(id)),
                None => KeyOutcome::Ignored,
            },
            KeyCode::Char('s') if self.focus == Focus::Terminals => {
                match self.selected_terminal() {
                    Some(id) => KeyOutcome::Action(WorkbenchAction::StopTerminal(id)),
                    None => KeyOutcome::Ignored,
                }
            }
            KeyCode::Char('f') if self.focus == Focus::Worktrees => {
                match self.selected_worktree() {
                    Some(id) => KeyOutcome::Action(WorkbenchAction::BrowseFiles(id)),
                    None => KeyOutcome::Ignored,
                }
            }
            KeyCode::Char('D') if self.focus == Focus::Worktrees => {
                match self.selected_worktree() {
                    Some(id) => {
                        if self.cleanup_armed.as_deref() == Some(id.as_str()) {
                            self.cleanup_armed = None;
                            KeyOutcome::Action(WorkbenchAction::CleanupWorktree(id.clone()))
                        } else {
                            self.cleanup_armed = Some(id.clone());
                            let path = self
                                .model
                                .worktrees
                                .iter()
                                .find(|w| w.worktree_id == id)
                                .map(|w| w.path.clone())
                                .unwrap_or_default();
                            self.set_status(format!("confirm: remove {path}? press D again"));
                            KeyOutcome::Handled
                        }
                    }
                    None => KeyOutcome::Ignored,
                }
            }
            KeyCode::Char('h') if self.focus == Focus::Worktrees => {
                match self.selected_worktree() {
                    Some(id) => {
                        self.handoff_picker = Some(HandoffPicker {
                            worktree_id: id,
                            agents: crate::agents::DETECTABLE_AGENTS
                                .iter()
                                .map(|agent| agent.to_string())
                                .collect(),
                            selected: 0,
                        });
                        KeyOutcome::Handled
                    }
                    None => KeyOutcome::Ignored,
                }
            }
            // `g` flips a workspace-wide switch: keep it deliberate by
            // binding it to the worktrees list (QA F8).
            KeyCode::Char('g') if self.focus == Focus::Worktrees => {
                KeyOutcome::Action(WorkbenchAction::ToggleAutoPull)
            }
            KeyCode::Char('d') if self.focus == Focus::Worktrees => {
                match self.selected_worktree() {
                    Some(id) => KeyOutcome::Action(WorkbenchAction::ShowDiff(id)),
                    None => KeyOutcome::Ignored,
                }
            }
            KeyCode::Char('|') => KeyOutcome::Action(WorkbenchAction::SplitRight),
            KeyCode::Char('-') => KeyOutcome::Action(WorkbenchAction::SplitBelow),
            KeyCode::Char('x') if matches!(self.pane_focus, PaneContent::Terminal(_)) => {
                self.close_pane();
                KeyOutcome::Handled
            }
            KeyCode::Char('z') => {
                self.toggle_zoom();
                KeyOutcome::Handled
            }
            // Enter on a focused terminal pane: keyboard forwarding.
            KeyCode::Enter if matches!(self.pane_focus, PaneContent::Terminal(_)) => {
                let id = match &self.pane_focus {
                    PaneContent::Terminal(id) => id.clone(),
                    _ => unreachable!("guarded above"),
                };
                self.enter_terminal_mode(id);
                KeyOutcome::Handled
            }
            // `o` on a space row: open a shell at that worktree.
            KeyCode::Char('o') if self.focus == Focus::Worktrees => {
                match self.selected_worktree() {
                    Some(id) => KeyOutcome::Action(WorkbenchAction::OpenWorktreeShell(id)),
                    None => KeyOutcome::Ignored,
                }
            }
            // `w` on a task row: create the task's worktree (server-side
            // V08 policy; removal stays a human-authorized action).
            KeyCode::Char('w') if self.focus == Focus::Tasks => {
                match self.model.tasks.get(self.selected) {
                    Some(task) => KeyOutcome::Action(WorkbenchAction::CreateTaskWorktree(
                        task.task_id.clone(),
                    )),
                    None => KeyOutcome::Ignored,
                }
            }
            KeyCode::Char('t') => {
                self.terminal_picker = Some(0);
                KeyOutcome::Handled
            }
            KeyCode::Char('n') => KeyOutcome::Action(WorkbenchAction::NewTab),
            KeyCode::Char('T') => {
                self.tab_prompt = Some(String::new());
                KeyOutcome::Handled
            }
            KeyCode::Char('X') => {
                self.close_tab();
                KeyOutcome::Handled
            }
            KeyCode::Char(',' | '.') => {
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
                if !tabs.is_empty() {
                    let current = self.scenes.scenes[&self.scenes.selected].active.clone();
                    let i = tabs.iter().position(|t| t == &current).unwrap_or(0);
                    let j = if key.code == KeyCode::Char('.') {
                        (i + 1) % tabs.len()
                    } else {
                        (i + tabs.len() - 1) % tabs.len()
                    };
                    self.switch_tab(&tabs[j]);
                }
                KeyOutcome::Handled
            }
            KeyCode::Char('W' | 'N' | 'A' | 'S') => {
                let action = match key.code {
                    KeyCode::Char('W') => "open",
                    KeyCode::Char('N') => "new",
                    KeyCode::Char('A') => "add",
                    _ => "save",
                };
                self.workspace_prompt = Some((action.into(), String::new()));
                KeyOutcome::Handled
            }
            KeyCode::Char('[' | ']') => {
                let ws = &self.model.workspaces;
                if ws.is_empty() {
                    return KeyOutcome::Ignored;
                }
                let i = ws
                    .iter()
                    .position(|w| Some(&w.workspace_id) == self.model.workspace_id.as_ref())
                    .unwrap_or(0);
                let next = if key.code == KeyCode::Char(']') {
                    (i + 1) % ws.len()
                } else {
                    (i + ws.len() - 1) % ws.len()
                };
                KeyOutcome::Action(WorkbenchAction::Workspace {
                    action: "select".into(),
                    value: Some(ws[next].workspace_id.clone()),
                    project_id: None,
                })
            }
            KeyCode::Char('R') if self.focus == Focus::Worktrees => {
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
            KeyCode::Char('r') => KeyOutcome::Action(WorkbenchAction::Refresh),
            _ => KeyOutcome::Ignored,
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
            MouseEventKind::Up(MouseButton::Left) => self.drag = DragState::None,
            _ => {}
        }
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
            .tab_hits
            .iter()
            .find(|(_, r)| contains_point(*r, column, row))
        {
            let id = id.clone();
            self.switch_tab(&id);
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
            if self.pane_focus != content {
                self.pane_focus = content;
                self.layout_dirty = true;
                if self.terminal_mode.is_some() {
                    self.leave_terminal_mode();
                }
            }
        }
    }

    fn on_left_drag(&mut self, column: u16, row: u16) {
        match self.drag.clone() {
            DragState::None => {}
            DragState::Sidebar => {
                let width = column.clamp(SIDEBAR_MIN_WIDTH, SIDEBAR_MAX_WIDTH);
                if self.sidebar_width != width {
                    self.sidebar_width = width;
                    self.layout_dirty = true;
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
        let name = self
            .model
            .workspaces
            .iter()
            .find(|w| Some(&w.workspace_id) == self.model.workspace_id.as_ref())
            .map(|w| w.name.as_str())
            .unwrap_or("All projects");
        frame.render_widget(Paragraph::new(format!(" Workspace: {name} · [ ] switch/recent · W open · N new · A add · R remove · S save as")),rows[0]);
        let main = rows[1];

        // herdr hides the sidebar on narrow terminals; so do we. The
        // user-chosen width applies, clamped so the pane grid keeps a
        // usable minimum.
        let sidebar_width = if main.width >= 60 {
            self.sidebar_width.min(main.width.saturating_sub(20))
        } else {
            0
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
            frame.render_widget(
                Paragraph::new(" n New tab · o New shell · t All terminals"),
                right[0],
            );
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
        for (index, (content, rect)) in layout.iter().enumerate() {
            let content_area = if layout.len() >= 2 {
                crate::tui::chrome::inner_rect(*rect)
            } else {
                rects[index]
            };
            match content {
                PaneContent::Browser => self.draw_overview(frame, content_area),
                PaneContent::Terminal(id) => self.draw_terminal_pane(frame, content_area, id),
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
        if let Some((action, text)) = &self.workspace_prompt {
            let area = centered(frame.area(), 80, 3);
            frame.render_widget(Clear, area);
            frame.render_widget(
                Paragraph::new(text.as_str()).block(
                    Block::bordered()
                        .title(format!(" Workspace {action} · Enter confirm / Esc cancel ")),
                ),
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
                "o/Enter opens a worktree shell · 1 spaces · 2 agents · 3 tasks",
                ratatui::style::Style::new().fg(palette.overlay0),
            )),
            Line::from(Span::styled(
                "| split · - split below · z zoom · q detach",
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
    fn draw_status_bar(&self, frame: &mut Frame, area: Rect, palette: &Palette) {
        // The version badge reserves its columns at the right edge first
        // (dim, unobtrusive provenance); the hints budget shrinks around
        // it, so the two never overlap.
        let version = format!(" v{}", env!("CARGO_PKG_VERSION"));
        let version_width = version.chars().count() as u16;
        let (bar_area, version_area) = if area.width > version_width {
            (
                Rect {
                    width: area.width - version_width,
                    ..area
                },
                Rect {
                    x: area.x + area.width - version_width,
                    width: version_width,
                    ..area
                },
            )
        } else {
            (area, Rect::new(area.x, area.y, 0, area.height))
        };
        let mut spans: Vec<Span> = Vec::new();
        if self.terminal_mode.is_some() {
            spans.push(Self::chip(" TERMINAL ", palette.accent, palette));
        }
        if self.zoomed {
            spans.push(Self::chip(" ZOOM ", palette.accent, palette));
        }
        if self.model.auto_pull {
            spans.push(Self::chip(" AUTO-PULL ", palette.mauve, palette));
        }
        if self.cleanup_armed.is_some() {
            spans.push(Self::chip(" CLEANUP ARMED ", palette.red, palette));
        }
        for marker in &self.model.attention {
            spans.push(Span::styled(
                format!("  ● needs attention: {}: {}", marker.kind, marker.detail),
                ratatui::style::Style::new().fg(palette.red),
            ));
        }
        if let Some(note) = &self.model.sync_note {
            spans.push(Span::styled(
                format!("  |  {note}"),
                ratatui::style::Style::new().fg(palette.overlay0),
            ));
        }
        // The hints are decoration: they get whatever columns remain, with
        // an ellipsis when they do not fit.
        let used: usize = spans.iter().map(|s| s.content.chars().count()).sum();
        let budget = (bar_area.width as usize).saturating_sub(used);
        if budget > 1 {
            let body_budget = budget - 1; // one leading space
            let truncated = self.status_line.chars().count() > body_budget;
            let mut body = fit(&self.status_line, body_budget);
            if truncated {
                body = fit(&body, body_budget.saturating_sub(1));
            }
            spans.push(Span::styled(
                format!(" {body}{}", if truncated { "…" } else { "" }),
                ratatui::style::Style::new().fg(palette.overlay0),
            ));
        }
        frame.render_widget(Paragraph::new(Line::from(spans)), bar_area);
        if version_area.width > 0 {
            frame.render_widget(
                Paragraph::new(Line::from(Span::styled(
                    version,
                    ratatui::style::Style::new().fg(palette.overlay0),
                ))),
                version_area,
            );
        }
    }

    fn chip(label: &str, bg: ratatui::style::Color, palette: &Palette) -> Span<'static> {
        Span::styled(
            label.to_string(),
            ratatui::style::Style::new()
                .fg(palette.chip_fg())
                .bg(bg)
                .bold(),
        )
    }

    /// The herdr-style sidebar: `spaces` (projects → worktrees as a tree),
    /// `agents` (terminals with status dots), `tasks`. The active
    /// section's header is accented; the selected row carries the
    /// selection background across its whole entity block. The viewport
    /// follows the keyboard selection (QA F2) — with no mouse, a selected
    /// row that scrolled off-screen would be unreachable. Facts only —
    /// unknown stays unknown.
    fn draw_sidebar(&self, frame: &mut Frame, area: Rect, palette: &Palette) {
        let width = area.width as usize;
        // Each line carries the entity it belongs to (section + model
        // index); headers and dividers carry None. The viewport window is
        // computed over the SELECTED entity's whole block.
        let mut lines: Vec<(Line<'static>, Option<(Focus, usize)>)> = Vec::new();

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

        // -- agents --
        lines.push((Self::section_divider(width, palette), None));
        lines.push((
            Self::section_header(" agents", self.focus == Focus::Terminals, palette),
            None,
        ));
        for (index, terminal) in self.model.terminals.iter().enumerate() {
            let selected = self.focus == Focus::Terminals && self.selected == index;
            // The dot shows the best observation's state. Honesty (S4,
            // ruling §5.3): only a controlled report may look authoritative
            // — screen/process-tree dots stay muted.
            let (dot, dot_color) = match terminal.agent_status.first() {
                Some(record) => {
                    let color = if record.source.is_authoritative() {
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
                            format!(" {}", fit(&terminal.purpose, width.saturating_sub(3))),
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
            let mut detail = format!("{} · {live_word}", terminal.owner_label);
            let records = Self::format_agent_status(&terminal.agent_status);
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

        // Viewport window: keep the selected entity's whole block visible.
        let viewport = area.height as usize;
        let selected_key = Some((self.focus, self.selected));
        let first = lines.iter().position(|(_, key)| *key == selected_key);
        let mut offset = 0usize;
        if let Some(first) = first {
            let span = lines
                .iter()
                .filter(|(_, key)| *key == selected_key)
                .count()
                .max(1);
            let end = first + span;
            if end > offset + viewport {
                offset = end - viewport;
            }
            if first < offset {
                offset = first;
            }
        }
        let visible: Vec<Line<'static>> = lines
            .into_iter()
            .skip(offset)
            .take(viewport)
            .map(|(line, _)| line)
            .collect();
        frame.render_widget(Paragraph::new(visible), area);
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

/// The workbench's data + action layer over the real office store. It owns
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
    pub fn refresh(&self) -> OfficeResult<WorkbenchModel> {
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
        let mut worktrees = worktree_service
            .all_records()?
            .into_iter()
            .filter(|r| r.released_at.is_none())
            .map(|r| {
                let dirty = self.dirty_state(&r.worktree_path);
                WorktreeRow {
                    worktree_id: r.worktree_id.to_string(),
                    project_id: project_of_task
                        .get(&r.task_id.to_string())
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
            "SELECT COUNT(*) FROM office_executions WHERE status = 'failed'",
            [],
            |row| row.get(0),
        )?;
        if failed > 0 {
            attention.push(AttentionMarker {
                kind: "execution_failed".into(),
                detail: format!("{failed} execution(s) failed — see the office records"),
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
        let workspace_id = nav.selected()?;
        let folders = workspace_id
            .as_deref()
            .map(|id| nav.folders(id))
            .transpose()?
            .unwrap_or_default();
        let workspace_diagnostics = folders
            .iter()
            .filter_map(|f| f.diagnostic.as_ref().map(|d| format!("{}: {d}", f.name)))
            .collect();
        let projects = projects
            .into_iter()
            .filter(|p| {
                workspace_id.is_none()
                    || folders
                        .iter()
                        .any(|f| f.project_id.as_deref() == Some(&p.project_id))
            })
            .map(|mut p| {
                if let Some(f) = folders
                    .iter()
                    .find(|f| f.project_id.as_deref() == Some(&p.project_id))
                {
                    p.name = f.name.clone();
                }
                p
            })
            .collect::<Vec<_>>();
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
                    && f.diagnostic.as_deref() == Some("non-Git directory: shell only")
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
            workspace_id.is_none() || projects.iter().any(|p| p.project_id == w.project_id)
        });
        Ok(WorkbenchModel {
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
    ) -> OfficeResult<crate::foundation::ids::TerminalId> {
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
    ) -> OfficeResult<crate::terminal::TerminalExit> {
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
    ) -> OfficeResult<String> {
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
    ) -> OfficeResult<crate::foundation::ids::TaskId> {
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
    ) -> OfficeResult<crate::git::worktrees::TaskWorktreeRecord> {
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
    ) -> OfficeResult<crate::git::worktrees::TaskWorktreeRecord> {
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
) -> OfficeResult<WorkbenchModel> {
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

/// Run the interactive workbench as a client of the resident office server
/// (ADR 0012, issue #43). All facts arrive as [`WorkbenchView`] projections
/// over the control channel; all actions go back as requests. `q` DETACHES:
/// the client exits, the server keeps every terminal running, and a later
/// attach restores the same view from server-held state.
pub fn run_client(mut client: crate::office::OfficeClient) -> OfficeResult<()> {
    // Honest gate: a workbench without a real terminal cannot work. Fail
    // loudly instead of half-working against a pipe.
    #[cfg(unix)]
    let stdin_is_tty = unsafe { libc::isatty(0) == 1 };
    #[cfg(not(unix))]
    let stdin_is_tty = true;
    if !stdin_is_tty {
        return Err(crate::foundation::OfficeError::Validation(
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

fn run_client_inner(client: &mut crate::office::OfficeClient) -> OfficeResult<()> {
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)
        .map_err(|e| crate::foundation::OfficeError::Io(std::io::Error::other(e.to_string())))?;

    let mut app = WorkbenchApp::new();
    // First facts, then the saved layout (QA F5): attach rebuilds the same
    // pane view, pruned to the terminals that still exist.
    match client.call(crate::office::OfficeRequestKind::WorkbenchView) {
        Ok(value) => {
            if let Ok(model) = serde_json::from_value::<WorkbenchModel>(value) {
                let live: std::collections::HashSet<String> = model
                    .terminals
                    .iter()
                    .map(|terminal| terminal.terminal_id.clone())
                    .collect();
                app.set_model(model);
                if let Ok(value) = client
                    .call(crate::office::OfficeRequestKind::WorkbenchLayout { layout_json: None })
                {
                    if let Some(json) = value.get("layout").and_then(|layout| layout.as_str()) {
                        app.restore_layout(json, &live);
                    }
                }
            }
        }
        Err(err) => app.set_status(format!("server error: {err}")),
    }
    loop {
        // Refresh from the server projection (never inside draw).
        match client.call(crate::office::OfficeRequestKind::WorkbenchView) {
            Ok(value) => match serde_json::from_value::<WorkbenchModel>(value) {
                Ok(model) => app.set_model(model),
                Err(err) => app.set_status(format!("view decode error: {err}")),
            },
            Err(err) => app.set_status(format!("server error: {err}")),
        }
        // Fetch one snapshot per terminal leaf (bounded by the pane cap):
        // every rendered pane stays live, focused or not.
        for id in app.terminal_leaves() {
            match client.call(crate::office::OfficeRequestKind::TerminalSnapshot {
                terminal_id: id.clone(),
            }) {
                Ok(value) => match serde_json::from_value::<TerminalSnapshot>(value) {
                    Ok(view) => app.set_snapshot(id, view),
                    Err(err) => app.set_status(format!("snapshot decode error: {err}")),
                },
                Err(err) => app.set_status(format!("snapshot error: {err}")),
            }
        }
        terminal.draw(|frame| app.draw(frame)).map_err(|e| {
            crate::foundation::OfficeError::Io(std::io::Error::other(e.to_string()))
        })?;

        if !event::poll(std::time::Duration::from_millis(200))
            .map_err(|e| crate::foundation::OfficeError::Io(std::io::Error::other(e.to_string())))?
        {
            continue;
        }
        let event = event::read().map_err(|e| {
            crate::foundation::OfficeError::Io(std::io::Error::other(e.to_string()))
        })?;
        match event {
            // Key handling is the existing flow; mouse events feed the
            // drag/click layer (herdr parity). Everything else is dropped.
            Event::Key(key) if key.kind == KeyEventKind::Press => {
                match app.on_key(key) {
                    // Detach, not shutdown: the resident server keeps running.
                    KeyOutcome::QuitRequested => break,
                    KeyOutcome::Action(action) => {
                        if let Err(err) = apply_client_action(client, &mut app, action) {
                            app.set_status(format!("action failed: {err}"));
                        }
                    }
                    KeyOutcome::Forward(bytes) => {
                        if let Some(id) = app.terminal_mode().map(str::to_string) {
                            if let Err(err) =
                                client.call(crate::office::OfficeRequestKind::TerminalInput {
                                    terminal_id: id,
                                    bytes_hex: crate::office::hex_encode(&bytes),
                                })
                            {
                                app.set_status(format!("input failed: {err}"));
                            }
                        }
                    }
                    KeyOutcome::Handled | KeyOutcome::Ignored => {}
                }
            }
            Event::Mouse(mouse) => app.on_mouse(mouse),
            _ => {}
        }
        // Persist pane-tree changes so the next attach restores them.
        if app.layout_dirty() {
            let payload = app.serialize_layout();
            if client
                .call(crate::office::OfficeRequestKind::WorkbenchLayout {
                    layout_json: Some(payload),
                })
                .is_ok()
            {
                app.clear_layout_dirty();
            }
        }
    }
    Ok(())
}

/// Execute one workbench action against the resident server.
fn apply_client_action(
    client: &mut crate::office::OfficeClient,
    app: &mut WorkbenchApp,
    action: WorkbenchAction,
) -> OfficeResult<()> {
    match action {
        WorkbenchAction::Workspace {
            action,
            value,
            project_id,
        } => {
            let v = client.call(crate::office::OfficeRequestKind::Workspace {
                action,
                value,
                project_id,
            })?;
            app.set_status(format!("workspace selected: {}", v["workspace_id"]));
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
            client.call(crate::office::OfficeRequestKind::TerminalStop {
                terminal_id: id.clone(),
            })?;
            app.set_status(format!("terminal {id} stopped"));
            Ok(())
        }
        WorkbenchAction::ShowDiff(worktree_id) => {
            let value = client.call(crate::office::OfficeRequestKind::WorkbenchDiff {
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
        WorkbenchAction::Refresh => Ok(()), // the loop refreshes every cycle
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
            let value = client
                .call(crate::office::OfficeRequestKind::TerminalOpenInWorktree { worktree_id })?;
            let terminal_id = value
                .get("terminal_id")
                .and_then(|v| v.as_str())
                .ok_or_else(|| {
                    crate::foundation::OfficeError::Validation(
                        "open returned no terminal id".into(),
                    )
                })?
                .to_string();
            app.attach_terminal(terminal_id.clone());
            app.enter_terminal_mode(terminal_id);
            Ok(())
        }
        WorkbenchAction::BrowseFiles(worktree_id) => {
            let value = client.call(crate::office::OfficeRequestKind::WorktreeFiles {
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
            let value = client.call(crate::office::OfficeRequestKind::WorktreeFileContent {
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
            let value = client.call(crate::office::OfficeRequestKind::WorktreeCleanup {
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
            let value = client.call(crate::office::OfficeRequestKind::AgentHandoff {
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
            client.call(crate::office::OfficeRequestKind::SetAutoPull { on })?;
            app.model.auto_pull = on;
            app.set_status(format!(
                "auto_pull {} (main checkout only; worktrees stay fetch-only)",
                if on { "ON" } else { "OFF" }
            ));
            Ok(())
        }
        WorkbenchAction::CreateTaskWorktree(task_id) => {
            let value = client.call(crate::office::OfficeRequestKind::WorktreeCreateForTask {
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
    }
}

/// Spawn a shell next to the focused pane: same worktree when the focused
/// terminal has one, else the client's own directory. The pane focus moves
/// into the new leaf.
fn spawn_and_split(
    client: &mut crate::office::OfficeClient,
    app: &mut WorkbenchApp,
    axis: SplitAxis,
) -> OfficeResult<()> {
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
    if !app
        .scenes
        .scenes
        .get(app.selected_scene())
        .is_some_and(|s| !s.active.is_empty())
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
    let value = client.call(crate::office::OfficeRequestKind::TerminalCreate {
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
            crate::foundation::OfficeError::Validation("spawn returned no terminal id".into())
        })?
        .to_string();
    if !app.split_pane(axis, terminal_id.clone()) {
        // The split was refused after the spawn (e.g. a pane would fall
        // below the minimum size): roll the terminal back so nothing runs
        // orphaned. The raw-mode client has no log sink — the status line
        // is the record — so a FAILED rollback says so instead of being
        // swallowed.
        match client.call(crate::office::OfficeRequestKind::TerminalStop {
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
            workspaces: vec![],
            workspace_id: None,
            workspace_diagnostics: vec![],
        }
    }

    #[test]
    fn sidebar_sections_switch_and_render_facts() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());

        // Spaces is the first section; the worktree is a tree child row
        // with its real state beside it.
        assert_eq!(app.focus(), Focus::Worktrees);
        let view = render(&mut app, 100, 24);
        assert!(view.contains("agent/feat-x"), "{view}");
        assert!(view.contains("dirty"), "real dirty state is a fact: {view}");
        assert!(view.contains("└─"), "tree glyphs group worktrees: {view}");

        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('2'))),
            KeyOutcome::Handled
        );
        assert_eq!(app.focus(), Focus::Terminals);
        let agents = render(&mut app, 100, 24);
        assert!(agents.contains("user_shell"));
        assert!(agents.contains("live"));

        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('3'))),
            KeyOutcome::Handled
        );
        assert!(render(&mut app, 100, 24).contains("ship the workbench"));

        // Tab cycles the sections and wraps back to spaces.
        app.on_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Worktrees);
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
    fn terminal_mode_forwards_bytes_and_esc_releases() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Enter)),
            KeyOutcome::Action(WorkbenchAction::EnterTerminal("term-1".into()))
        );
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
            KeyOutcome::Handled
        );
        assert!(app.terminal_mode().is_none());
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

    fn snapshot_with(lines: &[&str]) -> TerminalSnapshot {
        TerminalSnapshot {
            cols: 80,
            rows: 12,
            visible: lines.iter().map(|s| s.to_string()).collect(),
            scrollback: vec![],
            scrollback_capped: false,
            total_output_bytes: 64,
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
        assert!(render(&mut app, 100, 24).contains("unknown"));
        // Agents section: unreadable wait state shown as unknown.
        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        assert!(render(&mut app, 100, 24).contains("unknown"));
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
        let mut app = WorkbenchApp::new();
        app.set_model(model);
        assert_eq!(
            sidebar_dot(&mut app, 100, 24),
            palette.overlay0,
            "screen inference stays muted"
        );

        let mut model = sample();
        model.terminals[0].agent_status = vec![record(StatusSource::ControlledReport)];
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

    /// The client version is shown dim at the status bar's right edge,
    /// and the hints budget shrinks around it (the two never overlap).
    #[test]
    fn version_is_shown_dim_at_the_status_bar_edge() {
        let mut app = WorkbenchApp::new();
        let expected = concat!("v", env!("CARGO_PKG_VERSION"));
        let frame = render(&mut app, 100, 24);
        assert!(frame.contains(expected), "{frame}");
        // Narrow terminal: the hints yield, the version badge stays.
        let frame = render(&mut app, 40, 24);
        assert!(frame.contains(expected), "{frame}");
        assert!(
            frame.contains("…"),
            "hints truncate before the badge: {frame}"
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
            workspaces: vec![],
            workspace_id: None,
            workspace_diagnostics: vec![],
        }
    }

    /// QA F8: stale cleanup arming disarms on ANY key that is not the
    /// confirming D (a later D on another row must never delete).
    #[test]
    fn stale_cleanup_arming_disarms_on_any_other_key() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.on_key(KeyEvent::from(KeyCode::Char('1'))); // spaces (worktrees)
        // Arm on the selected worktree row.
        app.on_key(KeyEvent::from(KeyCode::Char('D')));
        assert!(app.cleanup_armed.is_some(), "first D arms");
        // Any unrelated key disarms.
        app.on_key(KeyEvent::from(KeyCode::Char('r')));
        assert!(app.cleanup_armed.is_none(), "stale arming disarmed");
    }

    /// QA F8: `g` is bound to the worktrees list — a global ambient
    /// toggle for a workspace-wide switch was the review's concern.
    #[test]
    fn auto_pull_toggle_requires_the_worktrees_focus() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.on_key(KeyEvent::from(KeyCode::Char('2'))); // agents focus
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('g'))),
            KeyOutcome::Ignored,
            "g outside worktrees focus is ignored"
        );
        app.on_key(KeyEvent::from(KeyCode::Char('1'))); // spaces (worktrees) focus
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('g'))),
            KeyOutcome::Action(WorkbenchAction::ToggleAutoPull)
        );
    }

    /// QA F8: the status line advertises the V15 keys (f/D/h/g) so the
    /// features are discoverable (QA round-2 F12).
    #[test]
    fn status_line_advertises_v15_keys() {
        let mut app = WorkbenchApp::new();
        let backend = TestBackend::new(200, 20);
        let mut terminal = ratatui::Terminal::new(backend).expect("terminal");
        terminal.draw(|f| app.draw(f)).expect("draw");
        let view = terminal.backend().to_string();
        assert!(view.contains("f files"), "{view}");
        assert!(view.contains("D cleanup"), "{view}");
        assert!(view.contains("h handoff"), "{view}");
        assert!(view.contains("g auto-pull"), "{view}");
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
}
