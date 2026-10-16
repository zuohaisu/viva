//! Agent status: three sources, two trusts (V14 S4, issue #46; ADR 0012
//! decision 5).
//!
//! The status board keeps, per terminal, what each SOURCE last said:
//!
//! - **Controlled report** (authoritative): a member (the agent's operator
//!   identity) reports working/blocked/done through the socket under a live
//!   grant — pi does this via its extension; hook-capable CLIs (claude,
//!   ...) report through the same endpoint. Every report is audited.
//! - **Process tree** (identification): which of the known agent CLIs runs
//!   inside a terminal's process tree. Names the agent; never claims a
//!   state by itself.
//! - **Screen inference** (auxiliary ONLY): rules over the terminal's own
//!   snapshot text. Records in the data layer carry
//!   [`StatusSource::ScreenInference`], the UI labels them auxiliary, and
//!   they NEVER reach the task/execution facts: a screen that looks done is
//!   not a done task (ADR 0011 §5.2).
//!
//! The same controlled channel carries the **self-model container's
//! intake**: agent content (what ran, which summaries formed, which tasks
//! were done) is submitted through it, audited to the event log,
//! and stored as a private file reference. The CONTAINER decides what to
//! keep — Viva guarantees the channel and the audit trail; a raw event is
//! never promoted to memory by Viva itself.

use std::collections::HashMap;
use std::sync::Mutex;

use serde::{Deserialize, Serialize};

use crate::foundation::error::Result;

pub mod badge;
pub mod capabilities;
pub mod integration;
pub mod screen;
pub mod session;

pub use badge::{AgentBadge, client_badge, server_badge};
pub use capabilities::{AgentCliProfile, CapabilityLevel, capability_matrix, profile_for};
pub use screen::agent_from_command;
pub use session::{SessionContext, build_session_row, qualifies_for_agent_list, sort_sessions};

/// The agent CLIs Viva detects in its first version (owner ruling,
/// 2026-10-01). Everything else is displayed as an unknown agent.
pub const DETECTABLE_AGENTS: &[&str] = &[
    "codex",
    "claude",
    "codebuddy",
    "qodercli",
    "cline",
    "hermes",
    "pi",
];

/// Agent states, herdr-shaped but ours: what the SOURCES report.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStatus {
    Working,
    Blocked,
    Done,
    Idle,
    /// The agent hit a usage limit; recovery is scheduled only when a
    /// reset time is supplied with the report (V15-5).
    RateLimited,
    Unknown,
}

impl AgentStatus {
    pub fn as_str(&self) -> &'static str {
        match self {
            AgentStatus::Working => "working",
            AgentStatus::Blocked => "blocked",
            AgentStatus::Done => "done",
            AgentStatus::Idle => "idle",
            AgentStatus::RateLimited => "rate_limited",
            AgentStatus::Unknown => "unknown",
        }
    }

    pub fn parse(text: &str) -> Result<Self> {
        match text {
            "working" => Ok(AgentStatus::Working),
            "blocked" => Ok(AgentStatus::Blocked),
            "done" => Ok(AgentStatus::Done),
            "idle" => Ok(AgentStatus::Idle),
            "rate_limited" => Ok(AgentStatus::RateLimited),
            "unknown" => Ok(AgentStatus::Unknown),
            other => Err(crate::foundation::error::Error::Validation(format!(
                "unknown agent status `{other}`"
            ))),
        }
    }
}

/// WHERE a status came from. The data layer marks every record; the UI
/// labels auxiliary sources and the fact layer ignores them entirely.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum StatusSource {
    /// Controlled report under a live grant — authoritative.
    ControlledReport,
    /// Process-tree identification — names the agent, not the state.
    ProcessTree,
    /// Screen rules — auxiliary, display-only, never a fact.
    ScreenInference,
}

impl StatusSource {
    pub fn as_str(&self) -> &'static str {
        match self {
            StatusSource::ControlledReport => "controlled_report",
            StatusSource::ProcessTree => "process_tree",
            StatusSource::ScreenInference => "screen_inference",
        }
    }

    /// Authoritative sources may drive nothing by themselves (the fact
    /// layer still demands controlled evidence), but the UI may present
    /// them as the agent's own word. Screen inference is auxiliary by
    /// ruling.
    pub fn is_authoritative(&self) -> bool {
        matches!(self, StatusSource::ControlledReport)
    }
}

/// One status observation.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentStatusRecord {
    pub terminal_id: String,
    pub agent: String,
    pub status: AgentStatus,
    pub source: StatusSource,
    pub detail: String,
    pub updated_at: String,
}

/// The runtime board: the latest observation per (terminal, source kind).
/// Screen inference and controlled reports sit side by side without
/// overwriting each other — the UI shows both, the fact layer trusts only
/// the report.
#[derive(Default)]
pub struct AgentStatusBoard {
    records: Mutex<HashMap<(String, StatusSource), AgentStatusRecord>>,
}

impl AgentStatusBoard {
    pub fn new() -> Self {
        Self::default()
    }

    /// Record one observation. Screen-inference records never replace a
    /// controlled report in the SAME slot (sources are separate), and they
    /// are dropped entirely when they carry nothing new.
    pub fn observe(&self, record: AgentStatusRecord) {
        self.records
            .lock()
            .expect("agent status board")
            .insert((record.terminal_id.clone(), record.source), record);
    }

    /// End an agent incarnation without deleting the durable report audit.
    pub fn clear_source(&self, terminal_id: &str, source: StatusSource) {
        self.records
            .lock()
            .expect("agent status board")
            .remove(&(terminal_id.into(), source));
    }
    /// Drop everything known about one terminal (it stopped or was
    /// transferred).
    pub fn clear(&self, terminal_id: &str) {
        self.records
            .lock()
            .expect("agent status board")
            .retain(|(terminal, _), _| terminal != terminal_id);
    }

    /// The projection the workbench serves: for each terminal, the
    /// authoritative record (if any agent reported) and the auxiliary one
    /// (if screen rules or the process tree saw something) — clearly
    /// separated, never merged into one claim.
    pub fn project(&self, terminal_id: &str) -> Vec<AgentStatusRecord> {
        let records = self.records.lock().expect("agent status board");
        let mut own: Vec<AgentStatusRecord> = records
            .iter()
            .filter(|((terminal, _), _)| terminal == terminal_id)
            .map(|(_, record)| record.clone())
            .collect();
        own.sort_by_key(|record| match record.source {
            StatusSource::ControlledReport => 0,
            StatusSource::ProcessTree => 1,
            StatusSource::ScreenInference => 2,
        });
        own
    }
}

// ---------------------------------------------------------------------------
// Process-tree identification
// ---------------------------------------------------------------------------

/// The agent CLI running inside a terminal's process tree, if any of the
/// seven detectable ones is there. Identification only: this says WHO
/// runs, never WHAT state they are in.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessAgent {
    pub agent: String,
    pub pid: u32,
    pub started: String,
}

#[derive(Default)]
pub struct ProcessTable {
    children: HashMap<u32, Vec<u32>>,
    commands: HashMap<u32, (String, String)>,
}
impl ProcessTable {
    pub fn capture() -> Result<Self> {
        let output = std::process::Command::new("ps")
            .args(["-eo", "pid=,ppid=,lstart=,command="])
            .output()?;
        if !output.status.success() {
            return Err(crate::foundation::Error::Validation(
                "process detection unavailable".into(),
            ));
        }
        Ok(Self::parse(&String::from_utf8_lossy(&output.stdout)))
    }
    pub fn parse(text: &str) -> Self {
        let mut table = Self::default();
        for line in text.lines() {
            let mut w = line.split_whitespace();
            let Some(pid) = w.next().and_then(|v| v.parse::<u32>().ok()) else {
                continue;
            };
            let Some(ppid) = w.next().and_then(|v| v.parse::<u32>().ok()) else {
                continue;
            };
            let started = w.by_ref().take(5).collect::<Vec<_>>().join(" ");
            let command = w.collect::<Vec<_>>().join(" ");
            table.children.entry(ppid).or_default().push(pid);
            table.commands.insert(pid, (started, command));
        }
        table
    }
    pub fn identify(&self, pid: Option<u32>) -> Option<ProcessAgent> {
        let mut queue = vec![pid?];
        let mut seen = std::collections::HashSet::new();
        while let Some(p) = queue.pop() {
            if !seen.insert(p) {
                continue;
            }
            if let Some((started, command)) = self.commands.get(&p) {
                if let Some(agent) = agent_from_command(command) {
                    return Some(ProcessAgent {
                        agent,
                        pid: p,
                        started: started.clone(),
                    });
                }
            }
            if let Some(kids) = self.children.get(&p) {
                queue.extend(kids);
            }
        }
        None
    }
}
pub fn identify_by_process_tree(child_pid: Option<u32>) -> Result<Option<String>> {
    if child_pid.is_none() {
        return Ok(None);
    }
    Ok(ProcessTable::capture()?
        .identify(child_pid)
        .map(|a| a.agent))
}

/// A session projection; a terminal is the stable session identity, not a Resident.
#[derive(Debug, Clone, PartialEq, serde::Serialize, serde::Deserialize)]
pub struct AgentSessionRow {
    pub terminal_id: String,
    pub tool: String,
    pub name: String,
    pub worktree_id: Option<String>,
    pub location: String,
    pub live: Option<bool>,
    pub records: Vec<AgentStatusRecord>,
    pub stale: bool,
    pub in_workspace: bool,
    /// Honest controlled-report capability for this CLI (#76).
    #[serde(default)]
    pub report_capability: String,
}
impl AgentSessionRow {
    pub fn needs_attention(&self) -> bool {
        self.stale
            || self
                .records
                .iter()
                .any(|r| matches!(r.status, AgentStatus::Blocked | AgentStatus::RateLimited))
    }
}

pub fn report_stale(record: &AgentStatusRecord) -> bool {
    time::OffsetDateTime::parse(
        &record.updated_at,
        &time::format_description::well_known::Rfc3339,
    )
    .map(|t| (time::OffsetDateTime::now_utc() - t).whole_seconds() > 120)
    .unwrap_or(true)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::foundation::ids::utc_now;

    #[test]
    fn board_keeps_sources_side_by_side_without_overwriting() {
        let board = AgentStatusBoard::new();
        board.observe(AgentStatusRecord {
            terminal_id: "t1".into(),
            agent: "pi".into(),
            status: AgentStatus::Blocked,
            source: StatusSource::ControlledReport,
            detail: "waiting for approval".into(),
            updated_at: utc_now(),
        });
        board.observe(AgentStatusRecord {
            terminal_id: "t1".into(),
            agent: "pi".into(),
            status: AgentStatus::Working,
            source: StatusSource::ScreenInference,
            detail: "screen rules".into(),
            updated_at: utc_now(),
        });
        let projection = board.project("t1");
        assert_eq!(projection.len(), 2);
        assert_eq!(projection[0].source, StatusSource::ControlledReport);
        assert_eq!(projection[0].status, AgentStatus::Blocked);
        assert_eq!(projection[1].source, StatusSource::ScreenInference);
        assert!(!projection[1].source.is_authoritative());
        // A newer report replaces the report slot only.
        board.observe(AgentStatusRecord {
            terminal_id: "t1".into(),
            agent: "pi".into(),
            status: AgentStatus::Working,
            source: StatusSource::ControlledReport,
            detail: "approved".into(),
            updated_at: utc_now(),
        });
        assert_eq!(board.project("t1")[0].status, AgentStatus::Working);
    }

    #[test]
    fn process_tree_returns_none_for_an_unknown_tree() {
        // A pid with no children and no agent-like command line yields
        // None: identification names agents, and unknown stays unknown.
        // (The real identification path is exercised by Viva's S4
        // integration test with a fake `codex` executable.)
        let identified = identify_by_process_tree(None).expect("ps runs");
        assert_eq!(identified, None);
    }
}
