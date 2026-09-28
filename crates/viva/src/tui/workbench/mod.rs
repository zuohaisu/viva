//! The parallel-development workbench (V14, issue #27): one office spanning
//! many projects, tasks and worktrees, with real terminals per worktree.
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
//! - Quit is the office protocol: owned terminals are stopped, worktree
//!   contents and records are left exactly as they are, and nothing is
//!   re-run when the office reopens.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, Clear, List, ListItem, Paragraph};

use std::str::FromStr as _;

use crate::foundation::error::OfficeResult;
use crate::terminal::TerminalSnapshot;

// ---------------------------------------------------------------------------
// Projections (read-only facts assembled by the store-backed source)
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProjectRow {
    pub project_id: String,
    pub name: String,
    pub repo_path: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub task_id: String,
    pub goal: String,
    pub status: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRow {
    pub terminal_id: String,
    pub worktree_id: Option<String>,
    pub purpose: String,
    pub owner_label: String,
    /// Office's real knowledge; `None` is shown as unknown.
    pub live: Option<bool>,
}

/// A needs-attention marker: a fact recorded by the office that a human
/// should look at (a failed execution, an unresolved launch). Projections
/// only — the workbench never decides what to do about them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttentionMarker {
    pub kind: String,
    pub detail: String,
}

/// The workbench snapshot the view renders.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
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
    /// The user asked to quit; the run loop must stop owned terminals,
    /// persist state, and restore the physical terminal.
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
}

/// The workbench application state. Pure view state + explicit caches;
/// drive it with [`WorkbenchApp::on_key`], the setters, and [`Self::draw`].
pub struct WorkbenchApp {
    model: WorkbenchModel,
    focus: Focus,
    selected: usize,
    /// When set: keyboard bytes forward to this terminal; Esc releases.
    terminal_mode: Option<String>,
    terminal_view: Option<TerminalSnapshot>,
    diff_view: Option<String>,
    status_line: String,
    quit_requested: bool,
}

impl WorkbenchApp {
    pub fn new() -> Self {
        Self {
            model: WorkbenchModel::default(),
            focus: Focus::Projects,
            selected: 0,
            terminal_mode: None,
            terminal_view: None,
            diff_view: None,
            status_line: "1-4 panes · Tab cycle · Enter terminal · d diff · s stop · r refresh · Esc releases · q quit".into(),
            quit_requested: false,
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

    pub fn set_terminal_view(&mut self, view: Option<TerminalSnapshot>) {
        self.terminal_view = view;
    }

    pub fn set_diff_view(&mut self, diff: Option<String>) {
        self.diff_view = diff;
    }

    pub fn set_status(&mut self, line: impl Into<String>) {
        self.status_line = line.into();
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
        self.terminal_view = None;
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
                self.focus = match self.focus {
                    Focus::Projects => Focus::Worktrees,
                    Focus::Worktrees => Focus::Tasks,
                    Focus::Tasks => Focus::Terminals,
                    Focus::Terminals => Focus::Projects,
                };
                self.selected = 0;
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
            Span::raw(if self.terminal_mode.is_some() {
                "  [terminal focused]"
            } else {
                ""
            }),
        ]);
        frame.render_widget(Paragraph::new(header), chunks[0]);

        if let (Some(view), Some(_id)) = (&self.terminal_view, &self.terminal_mode) {
            let mut lines: Vec<Line> = view
                .visible
                .iter()
                .map(|row| Line::from(row.clone()))
                .collect();
            lines.push(Line::from(format!(
                "— {}×{} · scrollback capped: {} · output bytes: {} —",
                view.cols, view.rows, view.scrollback_capped, view.total_output_bytes
            )));
            frame.render_widget(
                Paragraph::new(lines).block(
                    Block::new()
                        .title(format!(
                            " terminal {} ",
                            self.terminal_mode.clone().unwrap_or_default()
                        ))
                        .borders(Borders::ALL),
                ),
                chunks[1],
            );
        } else {
            match self.focus {
                Focus::Projects => self.draw_projects(frame, chunks[1]),
                Focus::Worktrees => self.draw_worktrees(frame, chunks[1]),
                Focus::Tasks => self.draw_tasks(frame, chunks[1]),
                Focus::Terminals => self.draw_terminals(frame, chunks[1]),
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
        let items: Vec<ListItem> = self
            .model
            .worktrees
            .iter()
            .enumerate()
            .map(|(i, w)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                let dirty = match w.dirty {
                    Some(true) => "dirty",
                    Some(false) => "clean",
                    None => "unknown",
                };
                ListItem::new(Line::from(format!(
                    "{marker}{} · {} · {} · task:{} · {}",
                    w.branch,
                    dirty,
                    w.path,
                    w.task_id.as_deref().unwrap_or("-"),
                    w.source
                )))
            })
            .collect();
        frame.render_widget(
            List::new(items).block(
                Block::new()
                    .title(" worktrees — branch · state · path · task · source ")
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
                ListItem::new(Line::from(format!(
                    "{marker}{} · {} · {} · wt:{}",
                    t.purpose,
                    t.owner_label,
                    live,
                    t.worktree_id.as_deref().unwrap_or("-")
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

        let tasks = tasks_registry
            .list_tasks(0, 200)?
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
        let worktrees = worktree_service
            .all_records()?
            .into_iter()
            .filter(|r| r.released_at.is_none())
            .map(|r| {
                let dirty = self.dirty_state(&r.worktree_path);
                WorktreeRow {
                    worktree_id: r.worktree_id.to_string(),
                    project_id: String::new(),
                    path: r.worktree_path.to_string_lossy().into_owned(),
                    branch: r.branch,
                    dirty,
                    task_id: Some(r.task_id.to_string()),
                    source: r.source.as_str().to_string(),
                }
            })
            .collect::<Vec<_>>();

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
// The product entry: run the workbench as THE active office host
// ---------------------------------------------------------------------------

/// Run the interactive workbench on this terminal. The caller has already
/// opened the office host (`OfficeHost::open`) and runs its serve loop on
/// a background thread; this loop owns the UI and, on quit, performs the
/// real office shutdown (stop owned terminals, join watchers, persist the
/// handoff, release the channel) before restoring the physical terminal.
pub fn run(shared: std::sync::Arc<crate::office::OfficeShared>) -> OfficeResult<()> {
    // Honest gate: a workbench without a real terminal cannot work. Fail
    // loudly instead of half-working against a pipe.
    #[cfg(unix)]
    let stdin_is_tty = unsafe { libc::isatty(0) == 1 };
    #[cfg(not(unix))]
    let stdin_is_tty = true;
    if !stdin_is_tty {
        return Err(crate::foundation::OfficeError::Validation(
            "viva workbench needs an interactive terminal (stdin is not a tty); \
             use `viva office start` for the headless host"
                .into(),
        ));
    }

    let guard = crate::tui::TerminalGuard::enter()?;
    let result = run_inner(shared);
    drop(guard); // raw mode off + main screen back on EVERY path
    result
}

fn run_inner(shared: std::sync::Arc<crate::office::OfficeShared>) -> OfficeResult<()> {
    use ratatui::crossterm::event::{self, Event, KeyEventKind};
    let backend = ratatui::backend::CrosstermBackend::new(std::io::stdout());
    let mut terminal = ratatui::Terminal::new(backend)
        .map_err(|e| crate::foundation::OfficeError::Io(std::io::Error::other(e.to_string())))?;

    let mut app = WorkbenchApp::new();
    let quit = loop {
        // Refresh from the real registries (never inside draw).
        match refresh_app(&shared, &mut app) {
            Ok(()) => {}
            Err(err) => app.set_status(format!("data error: {err}")),
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
            KeyOutcome::QuitRequested => break true,
            KeyOutcome::Action(action) => {
                if let Err(err) = apply_action(&shared, &mut app, action) {
                    app.set_status(format!("action failed: {err}"));
                }
            }
            KeyOutcome::Forward(bytes) => {
                if let Some(id) = app.terminal_mode().map(str::to_string) {
                    if let Ok(terminal_id) = crate::foundation::ids::TerminalId::from_str(&id) {
                        if let Ok(Some(handle)) = shared.terminals.handle(&terminal_id) {
                            handle.input(&bytes)?;
                        }
                    }
                }
            }
            KeyOutcome::Handled | KeyOutcome::Ignored => {}
        }
    };
    if quit {
        // The quit protocol, really: stop owned terminals, join exit
        // watchers, persist the handoff, release the channel.
        crate::office::OfficeHost::shutdown_shared(&shared)?;
    }
    // Ask the serve loop to finish (its own shutdown is idempotent).
    shared
        .stopping
        .store(true, std::sync::atomic::Ordering::SeqCst);
    Ok(())
}

/// Pull a fresh model from the real registries into the app.
fn refresh_app(shared: &crate::office::OfficeShared, app: &mut WorkbenchApp) -> OfficeResult<()> {
    let store = shared.store.lock().expect("office store");
    let workbench = WorkbenchStore {
        store: &store,
        terminals: &shared.terminals,
        protected: crate::git::worktrees::ProtectedRefs::new(vec![]),
    };
    app.set_model(workbench.refresh()?);
    Ok(())
}

/// Execute one workbench action against the owning registries.
fn apply_action(
    shared: &crate::office::OfficeShared,
    app: &mut WorkbenchApp,
    action: WorkbenchAction,
) -> OfficeResult<()> {
    match action {
        WorkbenchAction::EnterTerminal(id) => {
            app.enter_terminal_mode(id);
            Ok(())
        }
        WorkbenchAction::StopTerminal(id) => {
            let terminal_id = crate::foundation::ids::TerminalId::from_str(&id)?;
            let store = shared.store.lock().expect("office store");
            let workbench = WorkbenchStore {
                store: &store,
                terminals: &shared.terminals,
                protected: crate::git::worktrees::ProtectedRefs::new(vec![]),
            };
            workbench.stop_terminal(&terminal_id)?;
            app.set_status(format!("terminal {id} stopped"));
            Ok(())
        }
        WorkbenchAction::ShowDiff(worktree_id) => {
            let worktree_id_parsed = crate::foundation::ids::WorktreeId::from_str(&worktree_id)?;
            let store = shared.store.lock().expect("office store");
            let workbench = WorkbenchStore {
                store: &store,
                terminals: &shared.terminals,
                protected: crate::git::worktrees::ProtectedRefs::new(vec![]),
            };
            let record = crate::git::worktrees::WorktreeService::new(
                &store,
                crate::git::worktrees::ProtectedRefs::new(vec![]),
            )
            .record(&worktree_id_parsed)?
            .ok_or_else(|| crate::foundation::OfficeError::NotFound {
                entity: "worktree",
                id: worktree_id.clone(),
            })?;
            let diff = workbench.worktree_diff(&record.worktree_path, 64 * 1024)?;
            app.set_diff_view(Some(diff));
            Ok(())
        }
        WorkbenchAction::Refresh => {
            refresh_app(shared, app)?;
            Ok(())
        }
    }
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
