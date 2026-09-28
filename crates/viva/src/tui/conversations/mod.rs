//! The conversation tree panel (V10, issue #19): a read-only projection of
//! the office conversation tree with its display names, harness references
//! and task associations. Like the V06 shell, this view never mutates what
//! it shows — rename/attach/archive are actions the run loop executes via
//! the conversations domain, fed back through [`ConversationTreeApp::set_rows`].
//!
//! Ownership is visible by design: display names and task links are office
//! facts; native session/node pointers are labeled as the harness's.

use ratatui::Frame;
use ratatui::crossterm::event::{KeyCode, KeyEvent};
use ratatui::layout::{Constraint, Layout};
use ratatui::style::Stylize;
use ratatui::text::{Line, Span};
use ratatui::widgets::{Block, Borders, List, ListItem, Paragraph};

/// One rendered row of the tree (depth comes from the composition layer's
/// tree walk; this view does not re-derive structure).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConversationRow {
    pub node_id: String,
    pub display_name: String,
    pub depth: usize,
    pub harness: String,
    /// Harness-native pointer, labeled as foreign.
    pub native_ref: Option<String>,
    pub task_id: Option<String>,
    pub archived: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TreeKeyOutcome {
    Handled,
    Ignored,
    /// Rename the selected branch (the run loop prompts and calls the
    /// conversations domain, then refreshes rows).
    RenameRequested,
}

/// The conversation tree view state.
pub struct ConversationTreeApp {
    rows: Vec<ConversationRow>,
    selected: usize,
    status_line: String,
}

impl ConversationTreeApp {
    pub fn new() -> Self {
        Self {
            rows: Vec::new(),
            selected: 0,
            status_line: "j/k select · r rename · display names and task links are office-owned; native refs belong to the harness".into(),
        }
    }

    pub fn set_rows(&mut self, rows: Vec<ConversationRow>) {
        self.rows = rows;
        if self.selected >= self.rows.len().max(1) {
            self.selected = self.rows.len().saturating_sub(1);
        }
    }

    pub fn rows(&self) -> &[ConversationRow] {
        &self.rows
    }

    pub fn selected_node(&self) -> Option<&ConversationRow> {
        self.rows.get(self.selected)
    }

    pub fn on_key(&mut self, key: KeyEvent) -> TreeKeyOutcome {
        match key.code {
            KeyCode::Char('j') | KeyCode::Down if !self.rows.is_empty() => {
                self.selected = (self.selected + 1).min(self.rows.len() - 1);
                TreeKeyOutcome::Handled
            }
            KeyCode::Char('k') | KeyCode::Up => {
                self.selected = self.selected.saturating_sub(1);
                TreeKeyOutcome::Handled
            }
            KeyCode::Char('r') => TreeKeyOutcome::RenameRequested,
            _ => TreeKeyOutcome::Ignored,
        }
    }

    pub fn draw(&self, frame: &mut Frame) {
        let chunks =
            Layout::vertical([Constraint::Min(1), Constraint::Length(1)]).split(frame.area());
        let items: Vec<ListItem> = self
            .rows
            .iter()
            .enumerate()
            .map(|(i, row)| {
                let marker = if self.selected == i { "▶ " } else { "  " };
                let indent = "  ".repeat(row.depth);
                let branch = if row.depth == 0 { "" } else { "└ " };
                let native = row
                    .native_ref
                    .as_deref()
                    .map(|n| format!(" @{n}"))
                    .unwrap_or_default();
                let task = row
                    .task_id
                    .as_deref()
                    .map(|t| format!(" [task {t}]"))
                    .unwrap_or_default();
                let archived = if row.archived { " (archived)" } else { "" };
                ListItem::new(Line::from(vec![
                    Span::raw(format!("{marker}{indent}{branch}")),
                    Span::styled(
                        row.display_name.clone(),
                        if row.archived {
                            ratatui::style::Style::new().gray()
                        } else {
                            ratatui::style::Style::new().bold()
                        },
                    ),
                    Span::raw(format!("  ({}){native}{task}{archived}", row.harness)),
                ]))
            })
            .collect();
        frame.render_widget(
            List::new(items).block(
                Block::new()
                    .title(" conversations — display name · harness · native ref · task ")
                    .borders(Borders::ALL),
            ),
            chunks[0],
        );
        frame.render_widget(
            Paragraph::new(Line::from(self.status_line.clone()).gray()),
            chunks[1],
        );
    }
}

impl Default for ConversationTreeApp {
    fn default() -> Self {
        Self::new()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use ratatui::Terminal;
    use ratatui::backend::TestBackend;

    fn render(app: &ConversationTreeApp) -> String {
        let mut terminal = Terminal::new(TestBackend::new(120, 16)).expect("terminal");
        terminal.draw(|f| app.draw(f)).expect("draw");
        terminal.backend().to_string()
    }

    fn sample() -> Vec<ConversationRow> {
        vec![
            ConversationRow {
                node_id: "n1".into(),
                display_name: "Main line".into(),
                depth: 0,
                harness: "pi".into(),
                native_ref: Some("pi-native-1".into()),
                task_id: None,
                archived: false,
            },
            ConversationRow {
                node_id: "n2".into(),
                display_name: "Theme A".into(),
                depth: 1,
                harness: "pi".into(),
                native_ref: Some("pi-native-a".into()),
                task_id: Some("task-1".into()),
                archived: false,
            },
            ConversationRow {
                node_id: "n3".into(),
                display_name: "Theme B".into(),
                depth: 1,
                harness: "pi".into(),
                native_ref: None,
                task_id: None,
                archived: true,
            },
        ]
    }

    #[test]
    fn tree_renders_hierarchy_ownership_and_archive_honestly() {
        let mut app = ConversationTreeApp::new();
        app.set_rows(sample());
        let view = render(&app);
        assert!(view.contains("Main line"));
        assert!(view.contains("Theme A"));
        assert!(view.contains("pi-native-a"), "native refs are labeled");
        assert!(view.contains("[task task-1]"), "task links are visible");
        assert!(view.contains("(archived)"), "archived branches say so");
        assert!(!view.contains("codex"), "only real harnesses render");
    }

    #[test]
    fn rename_is_an_explicit_request_on_the_selected_node() {
        let mut app = ConversationTreeApp::new();
        app.set_rows(sample());
        app.on_key(KeyEvent::from(KeyCode::Char('j'))); // select Theme A
        assert_eq!(
            app.on_key(KeyEvent::from(KeyCode::Char('r'))),
            TreeKeyOutcome::RenameRequested
        );
        assert_eq!(
            app.selected_node().map(|n| n.display_name.as_str()),
            Some("Theme A")
        );
    }

    #[test]
    fn navigation_stays_within_the_tree() {
        let mut app = ConversationTreeApp::new();
        app.set_rows(sample());
        for _ in 0..10 {
            app.on_key(KeyEvent::from(KeyCode::Char('j')));
        }
        assert_eq!(app.selected_node().map(|n| n.node_id.as_str()), Some("n3"));
        app.on_key(KeyEvent::from(KeyCode::Char('k')));
        assert_eq!(app.selected_node().map(|n| n.node_id.as_str()), Some("n2"));
    }
}
