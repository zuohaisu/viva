//! The pane badge.
//!
//! A controlled report is the badge whenever one exists, even if a later
//! screen scan looks more recent. Screen rules fill the badge only when
//! no controlled report is on the board. Process identification names the
//! agent and leaves the state unknown.
//!
//! "Seen done" is this client's display. The server record stays `done`.
//! Another client that has not seen that observation still shows done.
//! The CLI reads the server record and does not apply this map.

use crate::agents::{AgentStatus, AgentStatusRecord, StatusSource};

/// The status a pane badge shows, plus where it came from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AgentBadge {
    pub terminal_id: String,
    pub agent: String,
    pub status: AgentStatus,
    pub source: StatusSource,
    pub updated_at: String,
}

/// Server badge. Screen never replaces a controlled report.
pub fn server_badge(records: &[AgentStatusRecord]) -> Option<AgentBadge> {
    let record = records
        .iter()
        .find(|record| record.source == StatusSource::ControlledReport)
        .or_else(|| {
            records
                .iter()
                .find(|record| record.source == StatusSource::ScreenInference)
        })
        .or_else(|| {
            records
                .iter()
                .find(|record| record.source == StatusSource::ProcessTree)
        })?;
    Some(from_record(record))
}

/// This client's badge. A done observation this client has already shown
/// displays as idle. `seen_done_at` is that observation's `updated_at`.
pub fn client_badge(
    records: &[AgentStatusRecord],
    seen_done_at: Option<&str>,
) -> Option<AgentBadge> {
    let mut badge = server_badge(records)?;
    if badge.status == AgentStatus::Done && seen_done_at == Some(badge.updated_at.as_str()) {
        badge.status = AgentStatus::Idle;
    }
    Some(badge)
}

fn from_record(record: &AgentStatusRecord) -> AgentBadge {
    AgentBadge {
        terminal_id: record.terminal_id.clone(),
        agent: record.agent.clone(),
        status: record.status,
        source: record.source,
        updated_at: record.updated_at.clone(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn record(status: AgentStatus, source: StatusSource, updated_at: &str) -> AgentStatusRecord {
        AgentStatusRecord {
            terminal_id: "pane-1".into(),
            agent: "codex".into(),
            status,
            source,
            detail: String::new(),
            updated_at: updated_at.into(),
        }
    }

    #[test]
    fn controlled_report_is_the_badge_even_when_the_screen_is_newer() {
        let records = vec![
            record(
                AgentStatus::Working,
                StatusSource::ControlledReport,
                "2026-10-15T00:00:00Z",
            ),
            record(
                AgentStatus::Done,
                StatusSource::ScreenInference,
                "2026-10-15T00:05:00Z",
            ),
        ];
        let badge = server_badge(&records).expect("badge");
        assert_eq!(badge.status, AgentStatus::Working);
        assert_eq!(badge.source, StatusSource::ControlledReport);
        assert_eq!(
            client_badge(&records, None).expect("client").status,
            AgentStatus::Working
        );
    }

    #[test]
    fn screen_fills_the_badge_only_without_a_controlled_report() {
        let records = vec![
            record(
                AgentStatus::Unknown,
                StatusSource::ProcessTree,
                "2026-10-15T00:00:00Z",
            ),
            record(
                AgentStatus::Blocked,
                StatusSource::ScreenInference,
                "2026-10-15T00:00:01Z",
            ),
        ];
        let badge = server_badge(&records).expect("badge");
        assert_eq!(badge.status, AgentStatus::Blocked);
        assert_eq!(badge.source, StatusSource::ScreenInference);
    }

    #[test]
    fn this_client_shows_a_seen_done_as_idle_and_the_server_badge_stays_done() {
        let records = vec![record(
            AgentStatus::Done,
            StatusSource::ControlledReport,
            "2026-10-15T01:00:00Z",
        )];
        assert_eq!(
            server_badge(&records).expect("server").status,
            AgentStatus::Done
        );
        assert_eq!(
            client_badge(&records, None).expect("unseen").status,
            AgentStatus::Done
        );
        assert_eq!(
            client_badge(&records, Some("2026-10-15T01:00:00Z"))
                .expect("seen")
                .status,
            AgentStatus::Idle
        );
        assert_eq!(
            client_badge(&records, Some("2026-10-15T00:00:00Z"))
                .expect("older seen")
                .status,
            AgentStatus::Done,
            "a new done observation shows until this client sees it"
        );
    }
}
