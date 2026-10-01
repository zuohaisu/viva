//! The terminal registry: spawn/input/resize/snapshot/stop/wait over real
//! PTY sessions, per-session process groups, bounded memory and redacted
//! disk logs (V05, issue #14).
//!
//! Ownership is typed end to end: a member-execution terminal carries its
//! execution id, user shells and other auxiliaries cannot fake one (the
//! foundation CHECK backstops the events this registry records). One
//! worktree may host many purpose-terminals; switching or stopping one
//! never touches its neighbors.

mod pty;
mod session;

pub use session::{
    DiskLog, ExitVia, SCROLLBACK_LINES, StopPolicy, TerminalExit, TerminalSnapshot, TerminalSpec,
};

use std::sync::{Arc, Mutex};

use crate::foundation::error::{OfficeError, OfficeResult};
use crate::foundation::ids::{ExecutionId, TerminalId};
use crate::foundation::records::{TerminalEventKind, TerminalOwner};
use crate::foundation::store::Store;
use session::TerminalHandle;

/// One registered terminal: identity + ownership + the live handle.
#[derive(Debug, Clone)]
pub struct TerminalEntry {
    pub terminal_id: TerminalId,
    pub owner: TerminalOwner,
    /// The worktree this terminal works in, when the office allocated it
    /// against one (aux shells may float).
    pub worktree_id: Option<crate::foundation::ids::WorktreeId>,
    pub purpose: String,
}

pub struct TerminalRegistry {
    sessions: Mutex<Vec<(TerminalEntry, Arc<TerminalHandle>)>>,
}

impl Default for TerminalRegistry {
    fn default() -> Self {
        Self::new()
    }
}

impl TerminalRegistry {
    pub fn new() -> Self {
        Self {
            sessions: Mutex::new(Vec::new()),
        }
    }

    /// Spawn a real PTY session and register it. Records the `spawned`
    /// terminal event (with the foundation CHECK keeping owner/execution
    /// consistent) when a store is provided.
    pub fn spawn(
        &self,
        spec: TerminalSpec,
        owner: TerminalOwner,
        worktree_id: Option<crate::foundation::ids::WorktreeId>,
        purpose: impl Into<String>,
        disk_log: Option<Arc<DiskLog>>,
        store: Option<&Store>,
    ) -> OfficeResult<(TerminalId, Arc<TerminalHandle>)> {
        let handle = TerminalHandle::spawn(&spec, &owner, disk_log)?;
        let terminal_id = TerminalId::new();
        let entry = TerminalEntry {
            terminal_id: terminal_id.clone(),
            owner: owner.clone(),
            worktree_id,
            purpose: purpose.into(),
        };
        if let Some(store) = store {
            handle.record_event(store, &terminal_id, &owner, TerminalEventKind::Spawned)?;
        }
        self.sessions
            .lock()
            .expect("terminal registry")
            .push((entry, Arc::new(handle)));
        let handle = self.handle(&terminal_id)?.expect("just inserted");
        Ok((terminal_id, handle))
    }

    /// The live handle of a registered terminal.
    pub fn handle(&self, terminal_id: &TerminalId) -> OfficeResult<Option<Arc<TerminalHandle>>> {
        Ok(self
            .sessions
            .lock()
            .expect("terminal registry")
            .iter()
            .find(|(entry, _)| &entry.terminal_id == terminal_id)
            .map(|(_, handle)| handle.clone()))
    }

    pub fn entry(&self, terminal_id: &TerminalId) -> OfficeResult<Option<TerminalEntry>> {
        Ok(self
            .sessions
            .lock()
            .expect("terminal registry")
            .iter()
            .find(|(entry, _)| &entry.terminal_id == terminal_id)
            .map(|(entry, _)| entry.clone()))
    }

    /// All terminals attached to one worktree (the workbench projection).
    pub fn terminals_for_worktree(
        &self,
        worktree_id: &crate::foundation::ids::WorktreeId,
    ) -> Vec<TerminalEntry> {
        self.sessions
            .lock()
            .expect("terminal registry")
            .iter()
            .map(|(entry, _)| entry.clone())
            .filter(|entry| entry.worktree_id.as_ref() == Some(worktree_id))
            .collect()
    }

    /// Stop one terminal. Only that session's process group is signalled —
    /// neighbors and unowned processes are untouched. Idempotent: the first
    /// recorded conclusion is permanent. Records the `stopped` event.
    pub fn stop(
        &self,
        terminal_id: &TerminalId,
        policy: StopPolicy,
        store: Option<&Store>,
    ) -> OfficeResult<TerminalExit> {
        let (entry, handle) = self
            .sessions
            .lock()
            .expect("terminal registry")
            .iter()
            .find(|(entry, _)| &entry.terminal_id == terminal_id)
            .map(|(entry, handle)| (entry.clone(), handle.clone()))
            .ok_or_else(|| OfficeError::NotFound {
                entity: "terminal",
                id: terminal_id.to_string(),
            })?;
        let exit = handle.stop(policy)?;
        if let Some(store) = store {
            handle.record_event(
                store,
                &entry.terminal_id,
                &entry.owner,
                TerminalEventKind::Stopped,
            )?;
        }
        Ok(exit)
    }

    /// Every registered terminal (workbench list view).
    pub fn list(&self) -> Vec<TerminalEntry> {
        self.sessions
            .lock()
            .expect("terminal registry")
            .iter()
            .map(|(entry, _)| entry.clone())
            .collect()
    }

    /// Register an ADOPTED session received by live handoff (issue #45):
    /// the terminal keeps its identity (same id across the restart) and
    /// the handle runs on the transferred master fd.
    #[allow(clippy::too_many_arguments)]
    pub fn adopt(
        &self,
        terminal_id: TerminalId,
        owner: TerminalOwner,
        worktree_id: Option<crate::foundation::ids::WorktreeId>,
        purpose: String,
        master_fd: std::os::fd::RawFd,
        pid: u32,
        pid_start_marker: String,
        cols: u16,
        rows: u16,
        history: Vec<String>,
        disk_log: Option<Arc<DiskLog>>,
    ) -> OfficeResult<()> {
        let handle = Arc::new(TerminalHandle::adopt(
            master_fd,
            pid,
            pid_start_marker,
            cols,
            rows,
            history,
            disk_log,
        )?);
        self.sessions.lock().expect("terminal registry").push((
            TerminalEntry {
                terminal_id,
                owner,
                worktree_id,
                purpose,
            },
            handle,
        ));
        Ok(())
    }

    /// Live count: sessions that have not recorded an exit.
    pub fn live_count(&self) -> usize {
        self.sessions
            .lock()
            .expect("terminal registry")
            .iter()
            .filter(|(_, handle)| handle.try_wait().ok().flatten().is_none())
            .count()
    }
}

/// Convenience: an execution-bound owner for a member terminal.
pub fn member_execution_owner(execution: ExecutionId) -> TerminalOwner {
    TerminalOwner::MemberExecution(execution)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn spec_validation_rejects_bad_argv_cwd_and_size() {
        assert!(
            TerminalSpec::new(vec![], std::path::PathBuf::from("/tmp"))
                .err()
                .is_some()
        );
        assert!(
            TerminalSpec::new(vec!["sh".into()], std::path::PathBuf::from("relative"))
                .err()
                .is_some()
        );
        let mut spec = TerminalSpec::new(vec!["sh".into()], std::env::temp_dir()).expect("spec");
        spec.cols = 0;
        assert!(spec.validate().is_err());
    }
}
