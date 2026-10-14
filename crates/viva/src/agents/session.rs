//! Agent-only session projection for the workbench (#76).

use super::{
    AgentSessionRow, AgentStatusRecord, StatusSource, capabilities::controlled_report_label,
    report_stale,
};
use crate::tui::workbench::{ProjectRow, TerminalRow, WorktreeRow};

/// Context for turning terminal rows into agent session rows.
pub struct SessionContext<'a> {
    pub worktrees: &'a [WorktreeRow],
    pub projects: &'a [ProjectRow],
    pub workspace_id: Option<&'a str>,
}

/// Ordinary shells and test runs never appear in the agent list (#76).
pub fn qualifies_for_agent_list(owner_label: &str, records: &[AgentStatusRecord]) -> bool {
    if owner_label == "test_run" {
        return false;
    }
    !records.is_empty()
}

pub fn location_label(
    worktree_id: &Option<String>,
    worktrees: &[WorktreeRow],
    projects: &[ProjectRow],
) -> String {
    let worktree = worktree_id
        .as_ref()
        .and_then(|id| worktrees.iter().find(|w| &w.worktree_id == id));
    worktree
        .map(|w| {
            let project = projects
                .iter()
                .find(|p| p.project_id == w.project_id)
                .map(|p| p.name.as_str())
                .unwrap_or("project");
            format!("{project} / {}", w.branch)
        })
        .unwrap_or_else(|| {
            worktree_id
                .clone()
                .unwrap_or_else(|| "floating / directory".into())
        })
}

pub fn build_session_row(
    terminal: &TerminalRow,
    ctx: &SessionContext<'_>,
) -> Option<AgentSessionRow> {
    if !qualifies_for_agent_list(&terminal.owner_label, &terminal.agent_status) {
        return None;
    }
    let stale = terminal
        .agent_status
        .iter()
        .any(|r| r.source == StatusSource::ControlledReport && report_stale(r));
    let worktree = terminal
        .worktree_id
        .as_ref()
        .and_then(|id| ctx.worktrees.iter().find(|w| &w.worktree_id == id));
    let in_workspace = ctx.workspace_id.is_none() || worktree.is_some();
    let tool = terminal
        .agent_status
        .iter()
        .find(|r| r.source == StatusSource::ProcessTree)
        .or_else(|| terminal.agent_status.first())
        .map(|r| r.agent.clone())
        .unwrap_or_else(|| "unknown".into());
    let has_controlled = terminal
        .agent_status
        .iter()
        .any(|r| r.source == StatusSource::ControlledReport);
    let report_capability = controlled_report_label(&tool, has_controlled).to_string();
    Some(AgentSessionRow {
        terminal_id: terminal.terminal_id.clone(),
        tool,
        name: terminal.purpose.clone(),
        worktree_id: terminal.worktree_id.clone(),
        location: location_label(&terminal.worktree_id, ctx.worktrees, ctx.projects),
        live: terminal.live,
        records: terminal.agent_status.clone(),
        stale,
        in_workspace,
        report_capability,
    })
}

pub fn sort_sessions(sessions: &mut [AgentSessionRow]) {
    sessions.sort_by_key(|a| {
        (
            !a.needs_attention(),
            a.location.clone(),
            a.terminal_id.clone(),
        )
    });
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agents::{AgentStatus, AgentStatusRecord, StatusSource};

    fn record(source: StatusSource) -> AgentStatusRecord {
        AgentStatusRecord {
            terminal_id: "t1".into(),
            agent: "pi".into(),
            status: AgentStatus::Unknown,
            source,
            detail: String::new(),
            updated_at: "2026-10-14T00:00:00Z".into(),
        }
    }

    #[test]
    fn test_runs_never_qualify() {
        assert!(!qualifies_for_agent_list(
            "test_run",
            &[record(StatusSource::ProcessTree)]
        ));
        assert!(qualifies_for_agent_list(
            "user_shell",
            &[record(StatusSource::ProcessTree)]
        ));
        assert!(!qualifies_for_agent_list("user_shell", &[]));
    }

    #[test]
    fn build_row_carries_report_capability_label() {
        let terminal = TerminalRow {
            terminal_id: "t1".into(),
            worktree_id: None,
            purpose: "pi".into(),
            owner_label: "agent_cli".into(),
            live: Some(true),
            agent_status: vec![record(StatusSource::ProcessTree)],
        };
        let row = build_session_row(
            &terminal,
            &SessionContext {
                worktrees: &[],
                projects: &[],
                workspace_id: None,
            },
        )
        .expect("row");
        assert_eq!(row.report_capability, "report:unverified");
    }
}
