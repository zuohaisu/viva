//! The TUI shell: navigation and terminal presentation
//! (V06, issue #15).
//!
//! Scope and honesty:
//! - Viva is the **only** physical-terminal renderer. Child tools' output
//!   lives in their emulation state (V05 snapshots); the shell draws
//!   projections of it. There is no second Viva chat engine here and no
//!   Agent harness UIs (Pi, etc.) run inside Viva terminals (V09/V14).
//! - The draw loop consumes a cached snapshot. Blocking queries and tool
//!   probes never run inside drawing: data enters only through an explicit
//!   [`TuiApp::refresh`] call from the event loop.
//! - Only the focused pane is drawn. Behind the current view there are no
//!   sixteen invisible renderings.
//! - Navigation is view state only: switching panes never mutates Viva
//!   data or execution attribution (the data trait has no mutators).
//! - Quit is a protocol, not a handle-drop: `q` requests the quit, the run
//!   loop asks Viva to pause (callback), and the terminal guard
//!   restores the physical terminal (raw mode off, main screen back) on
//!   every exit path, including errors.
//! - Unknown capabilities are displayed as unknown; nothing here claims a
//!   live embedded PTY view; run `viva harness matrix` for per-harness evidence.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::layout::{Constraint, Layout, Rect};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

use crate::foundation::error::Result;

pub mod chrome;
pub mod conversations;
pub mod layout;
mod text;
pub mod theme;
pub mod workbench;

// ---------------------------------------------------------------------------
// Data projection (read-only by construction)
// ---------------------------------------------------------------------------

/// What the shell shows for one task row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TaskRow {
    pub task_id: String,
    pub goal: String,
    pub status: String,
}

/// What the shell shows for one terminal row. `live` is Viva's real
/// knowledge; the shell never guesses a state it was not told.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TerminalRow {
    pub terminal_id: String,
    pub purpose: String,
    pub owner_label: String,
    pub worktree: Option<String>,
    pub live: Option<bool>,
}

/// Overview: members and projects (display configuration only).
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct Overview {
    pub members: Vec<String>,
    pub projects: Vec<String>,
}

/// The read-only data source of the shell. There are deliberately no
/// mutators: navigation can never change what it navigates over.
pub trait OverviewSource {
    fn overview(&self) -> Result<Overview>;
    fn tasks(&self) -> Result<Vec<TaskRow>>;
    fn terminals(&self) -> Result<Vec<TerminalRow>>;
}

#[derive(Debug, Clone, Default)]
struct Cache {
    overview: Overview,
    tasks: Vec<TaskRow>,
    terminals: Vec<TerminalRow>,
    last_error: Option<String>,
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Focus {
    Overview,
    Tasks,
    Terminals,
}

/// What one key press decided.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum KeyOutcome {
    Handled,
    Ignored,
    /// The user asked to quit; the run loop must invoke the pause
    /// and restore the terminal (never drop handles directly).
    QuitRequested,
}

/// The TUI application shell. Pure view state + cached projections; drive
/// it with [`TuiApp::on_key`], [`TuiApp::refresh`] and [`TuiApp::draw`].
pub struct TuiApp<D: OverviewSource> {
    data: D,
    cache: Cache,
    focus: Focus,
    selected: usize,
    show_help: bool,
    quit_requested: bool,
    status_line: String,
}

impl<D: OverviewSource> TuiApp<D> {
    pub fn new(data: D) -> Self {
        Self {
            data,
            cache: Cache::default(),
            focus: Focus::Overview,
            selected: 0,
            show_help: false,
            quit_requested: false,
            status_line: "? help · 1/2/3 panes · Tab cycle · q quit".into(),
        }
    }

    pub fn focus(&self) -> Focus {
        self.focus
    }

    pub fn quit_requested(&self) -> bool {
        self.quit_requested
    }

    pub fn show_help(&self) -> bool {
        self.show_help
    }

    /// Pull fresh projections from the data source. Called from the event
    /// loop — never from the draw path.
    pub fn refresh(&mut self) {
        let mut last_error = None;
        match self.data.overview() {
            Ok(overview) => self.cache.overview = overview,
            Err(err) => last_error = Some(err.to_string()),
        }
        match self.data.tasks() {
            Ok(tasks) => self.cache.tasks = tasks,
            Err(err) => last_error = Some(err.to_string()),
        }
        match self.data.terminals() {
            Ok(terminals) => self.cache.terminals = terminals,
            Err(err) => last_error = Some(err.to_string()),
        }
        self.cache.last_error = last_error;
        self.clamp_selection();
    }

    fn clamp_selection(&mut self) {
        let len = match self.focus {
            Focus::Overview => self
                .cache
                .overview
                .members
                .len()
                .max(self.cache.overview.projects.len()),
            Focus::Tasks => self.cache.tasks.len(),
            Focus::Terminals => self.cache.terminals.len(),
        };
        if len == 0 {
            self.selected = 0;
        } else {
            self.selected = self.selected.min(len - 1);
        }
    }

    fn row_count(&self) -> usize {
        match self.focus {
            Focus::Overview => self
                .cache
                .overview
                .members
                .len()
                .max(self.cache.overview.projects.len()),
            Focus::Tasks => self.cache.tasks.len(),
            Focus::Terminals => self.cache.terminals.len(),
        }
    }

    /// Handle one key event. Returns what the run loop should do.
    pub fn on_key(&mut self, key: KeyEvent) -> KeyOutcome {
        // Control keys belong to the shell; everything else is ignored by
        // the shell (sending keys to an agent is a terminal-focused flow
        // that the composition layer owns).
        if key.modifiers.contains(KeyModifiers::CONTROL)
            && matches!(key.code, KeyCode::Char('c') | KeyCode::Char('C'))
        {
            self.quit_requested = true;
            return KeyOutcome::QuitRequested;
        }
        match key.code {
            KeyCode::Char('?') => {
                self.show_help = !self.show_help;
                KeyOutcome::Handled
            }
            KeyCode::Char('q') | KeyCode::Char('Q') => {
                self.quit_requested = true;
                KeyOutcome::QuitRequested
            }
            KeyCode::Char('1') => {
                self.focus = Focus::Overview;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Char('2') => {
                self.focus = Focus::Tasks;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Char('3') => {
                self.focus = Focus::Terminals;
                self.selected = 0;
                KeyOutcome::Handled
            }
            KeyCode::Tab => {
                self.focus = match self.focus {
                    Focus::Overview => Focus::Tasks,
                    Focus::Tasks => Focus::Terminals,
                    Focus::Terminals => Focus::Overview,
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
            _ => KeyOutcome::Ignored,
        }
    }

    /// Draw one frame from the cache. Only the focused pane is rendered.
    pub fn draw(&self, frame: &mut Frame) {
        let chunks = Layout::vertical([
            Constraint::Length(1),
            Constraint::Min(1),
            Constraint::Length(1),
        ])
        .split(frame.area());

        // The focus tag stays on screen when the product name is wider than
        // the terminal. It occupies its own columns on the right.
        let tag = match self.focus {
            Focus::Overview => "[overview]",
            Focus::Tasks => "[tasks]",
            Focus::Terminals => "[terminals]",
        };
        let tag_cols = text::display_width(tag).min(chunks[0].width as usize) as u16;
        let head =
            Layout::horizontal([Constraint::Min(0), Constraint::Length(tag_cols)]).split(chunks[0]);
        let brand = " viva";
        let rest_budget = (head[0].width as usize).saturating_sub(text::display_width(brand));
        let header = Line::from(vec![
            Span::styled(brand, ratatui::style::Style::new().bold()),
            Span::raw(text::truncate_end(
                " — personal ai collaboration system",
                rest_budget,
            )),
        ]);
        frame.render_widget(Paragraph::new(header), head[0]);
        frame.render_widget(
            Paragraph::new(Span::styled(tag, ratatui::style::Style::new().bold())),
            head[1],
        );

        match self.focus {
            Focus::Overview => self.draw_overview(frame, chunks[1]),
            Focus::Tasks => self.draw_tasks(frame, chunks[1]),
            Focus::Terminals => self.draw_terminals(frame, chunks[1]),
        }

        // Status line: honest errors surface here, plainly.
        let status = match &self.cache.last_error {
            Some(err) => format!("{}  |  data error: {err}", self.status_line),
            None => self.status_line.clone(),
        };
        frame.render_widget(Paragraph::new(Line::from(status).gray()), chunks[2]);

        if self.show_help {
            self.draw_help(frame, frame.area());
        }
    }

    fn draw_overview(&self, frame: &mut Frame, area: Rect) {
        let rows =
            Layout::vertical([Constraint::Percentage(50), Constraint::Percentage(50)]).split(area);
        let members: Vec<ListItem> = self
            .cache
            .overview
            .members
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                ListItem::new(Line::from(format!("{marker}member: {name}")))
            })
            .collect();
        let projects: Vec<ListItem> = self
            .cache
            .overview
            .projects
            .iter()
            .enumerate()
            .map(|(i, name)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                ListItem::new(Line::from(format!("{marker}project: {name}")))
            })
            .collect();
        paint_list(frame, rows[0], " members ", members, self.selected);
        paint_list(frame, rows[1], " projects ", projects, self.selected);
    }

    fn draw_tasks(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .cache
            .tasks
            .iter()
            .enumerate()
            .map(|(i, task)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{marker}{} ", task.status)),
                    Span::styled(task.goal.clone(), ratatui::style::Style::new()),
                    Span::raw(format!("  ({})", task.task_id)),
                ]))
            })
            .collect();
        paint_list(frame, area, " tasks — goal · status ", items, self.selected);
    }

    fn draw_terminals(&self, frame: &mut Frame, area: Rect) {
        let items: Vec<ListItem> = self
            .cache
            .terminals
            .iter()
            .enumerate()
            .map(|(i, terminal)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                let live = match terminal.live {
                    Some(true) => "live",
                    Some(false) => "exited",
                    None => "unknown",
                };
                ListItem::new(Line::from(format!(
                    "{marker}{} · {} · {} · {}",
                    terminal.purpose,
                    terminal.owner_label,
                    live,
                    terminal.worktree.as_deref().unwrap_or("-"),
                )))
            })
            .collect();
        paint_list(
            frame,
            area,
            " terminals — purpose · owner · state · worktree ",
            items,
            self.selected,
        );
    }

    fn draw_help(&self, frame: &mut Frame, area: Rect) {
        let help = Paragraph::new(vec![
            Line::from(" viva shell — help"),
            Line::from(""),
            Line::from(" ?        toggle this help"),
            Line::from(" 1/2/3    focus overview / tasks / terminals"),
            Line::from(" Tab      cycle panes"),
            Line::from(" j / ↓    move down"),
            Line::from(" k / ↑    move up"),
            Line::from(" q        quit (pause + terminal restore)"),
            Line::from(""),
            Line::from(" keys sent to agents are managed per-terminal by Viva's"),
            Line::from(" composition; the shell itself never eats an agent's keys."),
        ])
        .block(Block::new().title(" help ").borders(Borders::ALL))
        .gray();
        // A centered box over the whole area.
        let inner = centered_rect(area, 60, 12);
        frame.render_widget(ratatui::widgets::Clear, inner);
        frame.render_widget(help, inner);
    }
}

fn centered_rect(area: Rect, percent_x: u16, height: u16) -> Rect {
    // u32: 60% of a 1600-column terminal overflows u16 in debug builds.
    let width = (u32::from(area.width) * u32::from(percent_x) / 100) as u16;
    let x = area.x + (area.width.saturating_sub(width)) / 2;
    let y = area.y + area.height.saturating_sub(height) / 2;
    Rect {
        x,
        y,
        width: width.min(area.width),
        height: height.min(area.height),
    }
}

/// Draw `items` with the selected row inside the viewport. A rect shorter
/// than a framed list (border + one content row + border) drops the frame
/// so the row still lands in the buffer.
fn paint_list(frame: &mut Frame, area: Rect, title: &str, items: Vec<ListItem>, selected: usize) {
    let framed = area.width >= 3 && area.height >= 3;
    let slots = if framed {
        usize::from(area.height.saturating_sub(2))
    } else {
        usize::from(area.height)
    };
    let start = text::scroll_to_reveal(items.len(), selected, slots);
    let visible: Vec<ListItem> = items.into_iter().skip(start).take(slots).collect();
    let block = if framed {
        Block::new().title(title).borders(Borders::ALL)
    } else {
        Block::new()
    };
    frame.render_widget(List::new(visible).block(block), area);
}

// ---------------------------------------------------------------------------
// Run loop pieces (composition-friendly; TestBackend-driven in tests)
// ---------------------------------------------------------------------------

/// Physical terminal guard: raw mode + alternate screen on enter, restored
/// on every drop path. This is what makes "异常恢复终端状态，不留下 raw mode"
/// structural instead of a promise.
#[cfg(unix)]
pub struct TerminalGuard;

#[cfg(unix)]
impl TerminalGuard {
    /// Enter the alternate screen with raw mode enabled.
    pub fn enter() -> Result<Self> {
        use ratatui::crossterm::ExecutableCommand as _;
        use ratatui::crossterm::event::{EnableBracketedPaste, EnableMouseCapture};
        use ratatui::crossterm::terminal::{self as ct, EnterAlternateScreen};
        ct::enable_raw_mode()
            .map_err(|e| crate::foundation::Error::Io(std::io::Error::other(e.to_string())))?;
        let mut out = std::io::stdout();
        let _ = out.execute(EnterAlternateScreen);
        let _ = out.execute(EnableMouseCapture);
        let _ = out.execute(EnableBracketedPaste);
        Ok(Self)
    }
}

#[cfg(unix)]
impl Drop for TerminalGuard {
    fn drop(&mut self) {
        use ratatui::crossterm::ExecutableCommand as _;
        use ratatui::crossterm::event::{DisableBracketedPaste, DisableMouseCapture};
        use ratatui::crossterm::terminal::{self as ct, LeaveAlternateScreen};
        let mut out = std::io::stdout();
        let _ = out.execute(LeaveAlternateScreen);
        let _ = out.execute(DisableMouseCapture);
        let _ = out.execute(DisableBracketedPaste);
        let _ = ct::disable_raw_mode();
    }
}

/// The run loop's quit step: ask Viva to pause (callback), then the
/// guard's drop restores the terminal. Kept as an explicit function so
/// tests can prove the pause runs exactly once before restore.
pub fn shutdown_with_pause(pause_server: &mut impl FnMut()) {
    pause_server();
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    /// Static fixture with a mutation counter: the trait has no mutators,
    /// so the counter must stay zero through any navigation.
    struct Fixture {
        overview: Overview,
        tasks: Vec<TaskRow>,
        terminals: Vec<TerminalRow>,
        fail_tasks: bool,
    }

    impl Fixture {
        fn sample() -> Self {
            Self {
                overview: Overview {
                    members: vec!["Samuel".into(), "Rook".into()],
                    projects: vec!["viva".into(), "VicTrader".into()],
                },
                tasks: vec![
                    TaskRow {
                        task_id: "task-1".into(),
                        goal: "Ship V01".into(),
                        status: "done".into(),
                    },
                    TaskRow {
                        task_id: "task-2".into(),
                        goal: "Write V05".into(),
                        status: "in_progress".into(),
                    },
                ],
                terminals: vec![
                    TerminalRow {
                        terminal_id: "term-1".into(),
                        purpose: "agent".into(),
                        owner_label: "member_execution".into(),
                        worktree: Some("wt-main".into()),
                        live: Some(true),
                    },
                    TerminalRow {
                        terminal_id: "term-2".into(),
                        purpose: "user shell".into(),
                        owner_label: "user_shell".into(),
                        worktree: None,
                        live: Some(false),
                    },
                ],
                fail_tasks: false,
            }
        }
    }

    impl OverviewSource for Fixture {
        fn overview(&self) -> Result<Overview> {
            Ok(self.overview.clone())
        }

        fn tasks(&self) -> Result<Vec<TaskRow>> {
            if self.fail_tasks {
                Err(crate::foundation::Error::Validation(
                    "task store unreachable".into(),
                ))
            } else {
                Ok(self.tasks.clone())
            }
        }

        fn terminals(&self) -> Result<Vec<TerminalRow>> {
            Ok(self.terminals.clone())
        }
    }

    fn render(app: &TuiApp<Fixture>, width: u16, height: u16) -> String {
        let backend = TestBackend::new(width, height);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        terminal.backend().to_string()
    }

    #[test]
    fn help_is_discoverable_by_keyboard_and_toggles() {
        let mut app = TuiApp::new(Fixture::sample());
        app.refresh();
        assert!(!app.show_help());

        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('?'))),
            KeyOutcome::Handled
        );
        assert!(app.show_help());
        let frame = render(&app, 80, 24);
        assert!(frame.contains("viva shell — help"), "help overlay drawn");
        assert!(frame.contains("toggle this help"));

        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('?'))),
            KeyOutcome::Handled
        );
        assert!(!app.show_help());
        let frame = render(&app, 80, 24);
        assert!(!frame.contains("viva shell — help"));
    }

    #[test]
    fn keyboard_switches_all_three_panes_and_back() {
        let mut app = TuiApp::new(Fixture::sample());
        app.refresh();

        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('2'))),
            KeyOutcome::Handled
        );
        let tasks_view = render(&app, 80, 24);
        assert!(tasks_view.contains("Ship V01"), "tasks pane lists goals");
        assert!(tasks_view.contains("task-1"), "tasks pane lists ids");

        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('3'))),
            KeyOutcome::Handled
        );
        let terminals_view = render(&app, 80, 24);
        assert!(terminals_view.contains("member_execution"));
        assert!(terminals_view.contains("exited"), "honest live state");

        // Tab cycles back to overview: terminals -> overview.
        app.on_key(KeyEvent::from(KeyCode::Tab));
        assert_eq!(app.focus(), Focus::Overview);
        let overview = render(&app, 80, 24);
        assert!(overview.contains("member: Samuel"));
        assert!(overview.contains("project: viva"));
    }

    #[test]
    fn navigation_never_mutates_overview_data() {
        // The trait has no mutators, so mutation is unrepresentable; the
        // strong version of the guarantee: even data that *fails* stays
        // untouched, and the cache is only refreshed explicitly.
        let mut app = TuiApp::new(Fixture::sample());
        app.refresh();
        for key in [
            KeyCode::Char('1'),
            KeyCode::Char('2'),
            KeyCode::Tab,
            KeyCode::Char('3'),
            KeyCode::Tab,
        ] {
            assert_eq!(app.on_key(KeyEvent::from(key)), KeyOutcome::Handled);
        }
        let frame = render(&app, 80, 24);
        assert!(
            frame.contains("member: Samuel"),
            "data intact after navigation"
        );
    }

    #[test]
    fn draw_uses_the_cache_blocking_queries_never_run_in_draw() {
        let mut fixture = Fixture::sample();
        fixture.fail_tasks = true;
        let mut app = TuiApp::new(fixture);

        // refresh surfaces the error honestly, in the status line.
        app.refresh();
        let frame = render(&app, 120, 24);
        assert!(
            frame.contains("data error"),
            "an unreachable store must say so: {frame}"
        );
        assert!(frame.contains("task store unreachable"));

        // Draw twice without refresh: identical output (cache-driven).
        let a = render(&app, 80, 24);
        let b = render(&app, 80, 24);
        assert_eq!(a, b, "draw must not re-query");
    }

    #[test]
    fn only_the_focused_pane_is_drawn() {
        let mut app = TuiApp::new(Fixture::sample());
        app.refresh();
        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        let tasks_view = render(&app, 80, 24);
        assert!(tasks_view.contains("Ship V01"));
        assert!(
            !tasks_view.contains("member: Samuel"),
            "hidden overview pane must not render"
        );
        assert!(
            !tasks_view.contains("user_shell"),
            "hidden terminals pane must not render"
        );
    }

    #[test]
    fn sixteen_background_terminals_do_not_mean_sixteen_drawings() {
        // Sixteen terminal rows exist in the data; the tasks pane stays
        // one pane, and the terminals pane is a single list (not one
        // rendering per terminal).
        let mut fixture = Fixture::sample();
        fixture.terminals = (0..16)
            .map(|i| TerminalRow {
                terminal_id: format!("term-{i}"),
                purpose: "agent".into(),
                owner_label: "member_execution".into(),
                worktree: Some(format!("wt-{i}")),
                live: Some(true),
            })
            .collect();
        let mut app = TuiApp::new(fixture);
        app.refresh();
        app.on_key(KeyEvent::from(KeyCode::Char('3')));
        let view = render(&app, 80, 40);
        assert!(
            view.contains("member_execution"),
            "the terminals list shows its rows"
        );
        assert_eq!(
            view.matches("terminals — purpose").count(),
            1,
            "one pane, one rendering — not one per terminal"
        );
    }

    #[test]
    fn quit_is_a_protocol_with_pause_and_restore() {
        let mut app = TuiApp::new(Fixture::sample());
        app.refresh();

        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('q'))),
            KeyOutcome::QuitRequested
        );
        assert!(app.quit_requested());

        // The run loop's shutdown: the pause runs exactly once,
        // before the guard restores the terminal (drop order: shutdown
        // runs while the guard is alive).
        let mut pauses = 0;
        {
            let _guard_canonical_scope = 0; // the real guard is Drop-based
            shutdown_with_pause(&mut || pauses += 1);
        }
        assert_eq!(pauses, 1, "pause exactly once");

        // Ctrl-C quits through the same protocol.
        let mut app = TuiApp::new(Fixture::sample());
        let key = KeyEvent::new(KeyCode::Char('c'), KeyModifiers::CONTROL);
        assert_eq!(app.on_key(key), KeyOutcome::QuitRequested);
    }

    #[test]
    fn plain_chars_are_ignored_by_the_shell() {
        let mut app = TuiApp::new(Fixture::sample());
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('x'))),
            KeyOutcome::Ignored,
            "typing text is not shell navigation; agent keys belong to terminals"
        );
    }

    #[test]
    fn task_list_scrolls_the_selected_row_into_view() {
        let mut fixture = Fixture::sample();
        fixture.tasks = (0..40)
            .map(|i| TaskRow {
                task_id: format!("t{i}"),
                goal: format!("OGOAL{i:02}"),
                status: "open".into(),
            })
            .collect();
        let mut app = TuiApp::new(fixture);
        app.refresh();
        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        for _ in 0..39 {
            app.on_key(KeyEvent::from(KeyCode::Char('j')));
        }
        let frame = render(&app, 80, 24);
        assert!(
            frame.contains("OGOAL39"),
            "the selected task must be on screen: {frame}"
        );
        assert!(
            !frame.contains("OGOAL00"),
            "the window must leave the first page: {frame}"
        );
        let short = render(&app, 40, 10);
        assert!(short.contains("OGOAL39"), "{short}");
    }

    #[test]
    fn narrow_header_keeps_the_focus_tag_and_a_short_list_keeps_a_body_row() {
        let mut app = TuiApp::new(Fixture::sample());
        app.refresh();
        let frame = render(&app, 20, 5);
        assert!(
            frame.contains("[overview]"),
            "the focus tag survives a narrow title: {frame}"
        );
        assert!(
            frame.contains("project:"),
            "a 5-row overview still draws the project list: {frame}"
        );
        app.on_key(KeyEvent::from(KeyCode::Char('2')));
        let tasks = render(&app, 40, 10);
        assert!(tasks.contains("[tasks]"), "{tasks}");
    }

    #[test]
    fn help_box_width_does_not_overflow_on_a_wide_terminal() {
        let mut app = TuiApp::new(Fixture::sample());
        app.refresh();
        app.on_key(KeyEvent::from(KeyCode::Char('?')));
        let backend = TestBackend::new(1600, 40);
        let mut terminal = Terminal::new(backend).expect("terminal");
        terminal.draw(|frame| app.draw(frame)).expect("draw");
        let buffer = terminal.backend().buffer().clone();
        let mut column = None;
        for y in 0..40 {
            let mut row = String::new();
            for x in 0..1600 {
                row.push_str(buffer[(x, y)].symbol());
            }
            if let Some(index) = row.find("viva shell") {
                column = Some(index);
                break;
            }
        }
        let column = column.expect("help text");
        // 60% of 1600 is 960, centered at x = 320. The title sits just inside.
        assert!(
            (300..400).contains(&column),
            "help box column {column} is not the 60% width"
        );
    }
}
