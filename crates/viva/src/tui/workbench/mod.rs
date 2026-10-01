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

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use crate::foundation::error::OfficeResult;
use crate::terminal::TerminalSnapshot;
use crate::tui::layout::{Direction, PaneContent, PaneNode, SplitAxis};

/// Reference area for geometric pane-focus moves: adjacency decisions are
/// proportional, so one fixed virtual size is stable for any real size.
const PANE_REF_AREA: ratatui::layout::Rect = ratatui::layout::Rect {
    x: 0,
    y: 0,
    width: 100,
    height: 40,
};

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
    pub projects: Vec<ProjectRow>,
    pub worktrees: Vec<WorktreeRow>,
    pub tasks: Vec<TaskRow>,
    pub terminals: Vec<TerminalRow>,
    pub attention: Vec<AttentionMarker>,
}

// ---------------------------------------------------------------------------
// View state machine (pure; the run loop executes actions)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Projects,
    Worktrees,
    Tasks,
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
}

impl WorkbenchApp {
    pub fn new() -> Self {
        Self {
            model: WorkbenchModel::default(),
            focus: Focus::Projects,
            selected: 0,
            grid: PaneNode::leaf(PaneContent::Browser),
            pane_focus: PaneContent::Browser,
            zoomed: false,
            snapshots: std::collections::HashMap::new(),
            terminal_mode: None,
            diff_view: None,
            status_line: "Ctrl+arrows panes · | - split · x close · z zoom · o open · w worktree · 1-4 lists · Enter terminal · q detach".into(),
            quit_requested: false,
            layout_dirty: false,
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
        self.model = model;
        self.clamp_selection();
    }

    pub fn model(&self) -> &WorkbenchModel {
        &self.model
    }

    pub fn set_diff_view(&mut self, diff: Option<String>) {
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

    /// The persisted layout payload: the pane tree plus the focused pane.
    pub fn serialize_layout(&self) -> String {
        serde_json::json!({ "grid": self.grid, "focus": self.pane_focus }).to_string()
    }

    /// Rebuild the pane tree from a saved layout (QA F5): terminal leaves
    /// whose session is gone are pruned, and the focus falls back to the
    /// browser unless it survived. Never fails — a broken layout degrades
    /// to the default view.
    pub fn restore_layout(&mut self, json: &str, live: &std::collections::HashSet<String>) {
        let Ok(value) = serde_json::from_str::<serde_json::Value>(json) else {
            return;
        };
        let Ok(mut grid) =
            serde_json::from_value::<PaneNode>(value.get("grid").cloned().unwrap_or_default())
        else {
            return;
        };
        grid.prune_dead_terminals(live);
        self.grid = grid;
        let leaves = self.grid.leaves();
        self.pane_focus = value
            .get("focus")
            .and_then(|focus| serde_json::from_value::<PaneContent>(focus.clone()).ok())
            .filter(|focus| leaves.contains(focus))
            .unwrap_or(PaneContent::Browser);
        self.layout_dirty = false;
    }

    /// Move pane focus geometrically (Ctrl+Arrows in the keymap).
    pub fn move_pane_focus(&mut self, direction: Direction) -> bool {
        match self
            .grid
            .neighbor(PANE_REF_AREA, &self.pane_focus, direction)
        {
            Some(next) => {
                self.pane_focus = next;
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
    }

    /// Point the focused pane at a terminal. From the browser: reuse the
    /// first terminal pane, or split one off when none exists. From a
    /// terminal pane: retarget it.
    pub fn attach_terminal(&mut self, terminal_id: String) {
        self.layout_dirty = true;
        let content = PaneContent::Terminal(terminal_id);
        match self.pane_focus.clone() {
            PaneContent::Browser => {
                let existing = self
                    .grid
                    .leaves()
                    .into_iter()
                    .find(|c| matches!(c, PaneContent::Terminal(_)));
                match existing {
                    Some(first) => {
                        self.grid.replace(&first, content.clone());
                    }
                    None => {
                        self.grid.split(
                            &PaneContent::Browser,
                            SplitAxis::Horizontal,
                            content.clone(),
                        );
                    }
                }
                self.pane_focus = content;
            }
            PaneContent::Terminal(old) => {
                self.grid
                    .replace(&PaneContent::Terminal(old), content.clone());
                self.pane_focus = content;
            }
        }
    }

    /// Split the focused pane and put a new terminal in the new leaf.
    /// Returns false (with a status message) at the pane cap.
    pub fn split_pane(&mut self, axis: SplitAxis, terminal_id: String) -> bool {
        self.layout_dirty = true;
        let content = PaneContent::Terminal(terminal_id);
        if self.grid.split(&self.pane_focus, axis, content.clone()) {
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
            self.layout_dirty = true;
            if let Some(removed) = self.grid.close(&PaneContent::Terminal(id)) {
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
        None
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
            Focus::Projects => self.model.projects.len(),
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
            KeyCode::Char('1') => {
                self.focus = Focus::Projects;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Char('2') => {
                self.focus = Focus::Worktrees;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Char('3') => {
                self.focus = Focus::Tasks;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Char('4') => {
                self.focus = Focus::Terminals;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Tab => {
                if self.pane_focus == PaneContent::Browser {
                    self.focus = match self.focus {
                        Focus::Projects => Focus::Worktrees,
                        Focus::Worktrees => Focus::Tasks,
                        Focus::Tasks => Focus::Terminals,
                        Focus::Terminals => Focus::Projects,
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
            KeyCode::Char('s') if self.focus == Focus::Terminals => {
                match self.selected_terminal() {
                    Some(id) => KeyOutcome::Action(WorkbenchAction::StopTerminal(id)),
                    None => KeyOutcome::Ignored,
                }
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
            // `o` on a worktree row: open a shell at that worktree.
            KeyCode::Char('o')
                if self.focus == Focus::Worktrees && self.pane_focus == PaneContent::Browser =>
            {
                match self.selected_worktree() {
                    Some(id) => KeyOutcome::Action(WorkbenchAction::OpenWorktreeShell(id)),
                    None => KeyOutcome::Ignored,
                }
            }
            // `w` on a task row: create the task's worktree (server-side
            // V08 policy; removal stays a human-authorized action).
            KeyCode::Char('w')
                if self.focus == Focus::Tasks && self.pane_focus == PaneContent::Browser =>
            {
                match self.model.tasks.get(self.selected) {
                    Some(task) => KeyOutcome::Action(WorkbenchAction::CreateTaskWorktree(
                        task.task_id.clone(),
                    )),
                    None => KeyOutcome::Ignored,
                }
            }
            KeyCode::Char('r') => KeyOutcome::Action(WorkbenchAction::Refresh),
            _ => KeyOutcome::Ignored,
        }
    }

    /// Draw one frame from the caches. In terminal mode the focused
    /// terminal's real snapshot is the main area; otherwise the focused
    /// pane's list is.
    pub fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

        let header = Line::from(vec![
            Span::styled(" viva workbench", ratatui::style::Style::new().bold()),
            Span::raw("  "),
            Span::styled(
                match self.focus {
                    Focus::Projects => "[projects]",
                    Focus::Worktrees => "[worktrees]",
                    Focus::Tasks => "[tasks]",
                    Focus::Terminals => "[terminals]",
                },
                ratatui::style::Style::new().bold(),
            ),
            Span::raw(if self.zoomed { "  [zoom]" } else { "" }),
            Span::raw(if self.terminal_mode.is_some() {
                "  [terminal focused]"
            } else {
                ""
            }),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        // The pane tree lays out the main area: the browser leaf renders
        // the focused list, terminal leaves render server snapshots. Zoom
        // draws only the focused pane, full-screen.
        let layout = if self.zoomed {
            vec![(self.pane_focus.clone(), chunks[1])]
        } else {
            self.grid.render_layout(chunks[1])
        };
        for (content, rect) in &layout {
            match content {
                PaneContent::Browser => self.draw_browser(frame, *rect),
                PaneContent::Terminal(id) => self.draw_terminal_pane(frame, *rect, id),
            }
        }

        let attention = if self.model.attention.is_empty() {
            String::new()
        } else {
            format!(
                "  |  needs attention: {}",
                self.model
                    .attention
                    .iter()
                    .map(|m| format!("{}: {}", m.kind, m.detail))
                    .collect::<Vec<_>>()
                    .join("; ")
            )
        };
        frame.render_widget(
            Paragraph::new(Line::from(format!("{}{}", self.status_line, attention)).gray()),
            chunks[2],
        );

        if let Some(diff) = &self.diff_view {
            self.draw_diff(frame, frame.area(), diff);
        }
    }

    fn draw_browser(&self, frame: &mut Frame, area: Rect) {
        match self.focus {
            Focus::Projects => self.draw_projects(frame, area),
            Focus::Worktrees => self.draw_worktrees(frame, area),
            Focus::Tasks => self.draw_tasks(frame, area),
            Focus::Terminals => self.draw_terminals(frame, area),
        }
    }

    /// One terminal leaf: the server snapshot, bottom-anchored so a
    /// reattached client sees the server-kept history.
    fn draw_terminal_pane(&self, frame: &mut Frame, area: Rect, id: &str) {
        let statuses = self
            .model
            .terminals
            .iter()
            .find(|t| t.terminal_id == id)
            .map(|t| Self::format_agent_status(&t.agent_status))
            .unwrap_or_default();
        let status_suffix = if statuses.is_empty() {
            String::new()
        } else {
            format!(" · {statuses}")
        };
        let title = if self.pane_focus == PaneContent::Terminal(id.to_string()) {
            format!(" terminal {id} · pane focus{status_suffix} ")
        } else {
            format!(" terminal {id}{status_suffix} ")
        };
        let lines: Vec<Line> = match self.snapshots.get(id) {
            Some(view) => {
                let mut lines: Vec<Line> = view
                    .scrollback
                    .iter()
                    .chain(view.visible.iter())
                    .map(|row| Line::from(row.clone()))
                    .collect();
                let max_lines = area.height.saturating_sub(2) as usize;
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
            None => vec![Line::from(" waiting for the first server snapshot… ")],
        };
        frame.render_widget(
            Paragraph::new(lines).block(Block::new().title(title).borders(Borders::ALL)),
            area,
        );
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

    fn draw_projects(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .model
            .projects
            .iter()
            .enumerate()
            .map(|(i, p)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                ListItem::new(Line::from(format!("{marker}{}  {}", p.name, p.repo_path)))
            })
            .collect();
        frame.render_widget(
            List::new(items).block(Block::new().title(" projects ").borders(Borders::ALL)),
            area,
        );
    }

    fn draw_worktrees(&self, frame: &mut Frame, area: Rect) {
        // Grouped by project (S2): a styled header per project, real rows
        // selectable beneath. Headers are render-only; the selection index
        // counts real rows only.
        let mut items: Vec<ListItem> = Vec::new();
        let mut current_project: Option<String> = None;
        for (real_index, w) in self.model.worktrees.iter().enumerate() {
            if current_project.as_ref() != Some(&w.project_id) {
                current_project = Some(w.project_id.clone());
                let name = self
                    .model
                    .projects
                    .iter()
                    .find(|p| p.project_id == w.project_id)
                    .map(|p| p.name.clone())
                    .unwrap_or_else(|| "no project".into());
                items.push(ListItem::new(Line::from(Span::styled(
                    format!("── {name}"),
                    ratatui::style::Style::new().bold().gray(),
                ))));
            }
            let focused = self.pane_focus == PaneContent::Browser && self.focus == Focus::Worktrees;
            let marker = if focused && self.selected == real_index {
                "▶ "
            } else {
                "  "
            };
            let dirty = match w.dirty {
                Some(true) => "dirty",
                Some(false) => "clean",
                None => "unknown",
            };
            items.push(ListItem::new(Line::from(format!(
                "{marker}{} · {} · {} · task:{} · {}",
                w.branch,
                dirty,
                w.path,
                w.task_id.as_deref().unwrap_or("-"),
                w.source
            ))));
        }
        frame.render_widget(
            List::new(items).block(
                Block::new()
                    .title(" worktrees — project · branch · state · path · task · source ")
                    .borders(Borders::ALL),
            ),
            area,
        );
    }

    fn draw_tasks(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .model
            .tasks
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                ListItem::new(Line::from(format!(
                    "{marker}{} {} ({})",
                    t.status, t.goal, t.task_id
                )))
            })
            .collect();
        frame.render_widget(
            List::new(items).block(
                Block::new()
                    .title(" tasks — status · goal ")
                    .borders(Borders::ALL),
            ),
            area,
        );
    }

    fn draw_terminals(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .model
            .terminals
            .iter()
            .enumerate()
            .map(|(i, t)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                let live = match t.live {
                    Some(true) => "live",
                    Some(false) => "exited",
                    None => "unknown",
                };
                let agents = Self::format_agent_status(&t.agent_status);
                let agents = if agents.is_empty() {
                    String::new()
                } else {
                    format!(" · {agents}")
                };
                ListItem::new(Line::from(format!(
                    "{marker}{} · {} · {} · wt:{}{}",
                    t.purpose,
                    t.owner_label,
                    live,
                    t.worktree_id.as_deref().unwrap_or("-"),
                    agents
                )))
            })
            .collect();
        frame.render_widget(
            List::new(items).block(
                Block::new()
                    .title(" terminals — purpose · owner · state · worktree ")
                    .borders(Borders::ALL),
            ),
            area,
        );
    }

    fn draw_diff(&self, frame: &mut Frame, area: Rect, diff: &str) {
        let inner = centered(area, 80, area.height.saturating_sub(4).max(5));
        let lines: Vec<Line> = diff.lines().map(Line::from).collect();
        frame.render_widget(Clear, inner);
        frame.render_widget(
            Paragraph::new(lines).block(
                Block::new()
                    .title(" worktree diff (HEAD) ")
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
    let width = area.width * percent_x / 100;
    let x = area.x + area.width.saturating_sub(width) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
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

        Ok(WorkbenchModel {
            projects,
            worktrees,
            tasks,
            terminals,
            attention,
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
        let Event::Key(key) = event::read().map_err(|e| {
            crate::foundation::OfficeError::Io(std::io::Error::other(e.to_string()))
        })?
        else {
            continue;
        };
        if key.kind != KeyEventKind::Press {
            continue;
        }
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
                    if let Err(err) = client.call(crate::office::OfficeRequestKind::TerminalInput {
                        terminal_id: id,
                        bytes_hex: crate::office::hex_encode(&bytes),
                    }) {
                        app.set_status(format!("input failed: {err}"));
                    }
                }
            }
            KeyOutcome::Handled | KeyOutcome::Ignored => {}
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
    let worktree_id = app.focused_worktree_id();
    let cwd = worktree_id
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
    app.split_pane(axis, terminal_id);
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &WorkbenchApp, w: u16, h: u16) -> String {
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
        }
    }

    #[test]
    fn keyboard_cycles_all_four_panes_and_renders_facts() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());

        for (key, expected_focus) in [
            (KeyCode::Char('2'), Focus::Worktrees),
            (KeyCode::Char('3'), Focus::Tasks),
            (KeyCode::Char('4'), Focus::Terminals),
            (KeyCode::Tab, Focus::Projects),
        ] {
            assert_eq!(app.on_key(KeyEvent::from(key)), KeyOutcome::Handled);
            assert_eq!(app.focus(), expected_focus);
        }
        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        let view = render(&app, 100, 20);
        assert!(view.contains("agent/feat-x"));
        assert!(view.contains("dirty"), "real dirty state is a fact: {view}");
        app.on_key(KeyEvent::from(KeyCode::Char('3')));
        assert!(render(&app, 100, 20).contains("ship the workbench"));
        app.on_key(KeyEvent::from(KeyCode::Char('4')));
        let terminals = render(&app, 100, 20);
        assert!(terminals.contains("live"));
        assert!(terminals.contains("user_shell"));
    }

    #[test]
    fn attention_markers_surface_without_second_state() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        // Wide enough that the attention suffix is not clipped.
        let view = render(&app, 200, 20);
        assert!(
            view.contains("needs attention") && view.contains("execution_failed"),
            "markers are visible facts: {view}"
        );
    }

    #[test]
    fn terminal_mode_forwards_bytes_and_esc_releases() {
        let mut app = WorkbenchApp::new();
        app.set_model(sample());
        app.on_key(KeyEvent::from(KeyCode::Char('4')));
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

        let view = render(&app, 160, 44);
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
        let view = render(&app, 160, 44);
        assert!(view.contains("solo-out"), "{view}");
        assert!(view.contains("[zoom]"));
        // The browser's list CONTENT is hidden (the header keeps the list
        // label; the sample project row path must be gone).
        assert!(
            !view.contains("/code/viva"),
            "zoom hides the browser content: {view}"
        );
        app.toggle_zoom();
        assert!(render(&app, 160, 44).contains("/code/viva"));
    }

    #[test]
    fn pane_focus_moves_geometrically_between_leaves() {
        let mut app = WorkbenchApp::new();
        app.attach_terminal("t1".into());
        app.split_pane(SplitAxis::Vertical, "t2".into());
        // Focus is on t2 after the split; move up to t1, left to the browser.
        app.move_pane_focus(Direction::Up);
        assert_eq!(app.pane_focus(), &PaneContent::Terminal("t1".into()));
        app.move_pane_focus(Direction::Left);
        assert_eq!(app.pane_focus(), &PaneContent::Browser);
    }

    #[test]
    fn unknown_states_are_displayed_not_guessed() {
        let mut model = sample();
        model.worktrees[0].dirty = None;
        model.terminals[0].live = None;
        let mut app = WorkbenchApp::new();
        app.set_model(model);
        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        assert!(render(&app, 100, 20).contains("unknown"));
        app.on_key(KeyEvent::from(KeyCode::Char('4')));
        assert!(render(&app, 100, 20).contains("unknown"));
    }
}
