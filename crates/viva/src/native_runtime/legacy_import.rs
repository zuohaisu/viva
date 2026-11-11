//! Pure conversion from a legacy Viva live-handoff manifest + scene book into
//! the portable plan the native host will hand to Herdr's adoption path.
//!
//! No process is touched here: building a plan reads the old host's manifest
//! and persisted layout only. The receiver still owns the PTY fds and must not
//! acknowledge the old host until every pane has been adopted.
use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{ExecutionId, GrantId, MemberId, TaskId};
use crate::foundation::records::RequestOrigin;
use crate::foundation::store::Store;
use crate::server::{Client, HandoffManifest, RequestKind};
use crate::tui::layout::PaneContent;
use crate::tui::workbench::scenes::SceneBook;
use rusqlite::OptionalExtension as _;
use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::os::fd::OwnedFd;
use std::os::unix::net::UnixStream;
use std::path::{Path, PathBuf};
use std::str::FromStr;

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedPane {
    pub terminal_id: String,
    pub pid: u32,
    pub cols: u16,
    pub rows: u16,
    /// Scrollback + the legacy formatted screen, both already ANSI.
    pub history_ansi: String,
    pub purpose: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedTab {
    pub name: String,
    pub panes: Vec<ImportedPane>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ImportedScene {
    pub name: String,
    pub tabs: Vec<ImportedTab>,
    pub active_tab: usize,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct LegacyImportPlan {
    pub scenes: Vec<ImportedScene>,
    /// Index into `scenes` of the scene the old workbench had selected.
    pub selected: usize,
}

fn hex_decode(text: &str) -> Option<Vec<u8>> {
    if text.len() % 2 != 0 {
        return None;
    }
    let mut bytes = Vec::with_capacity(text.len() / 2);
    let raw = text.as_bytes();
    for pair in raw.chunks_exact(2) {
        let high = (pair[0] as char).to_digit(16)?;
        let low = (pair[1] as char).to_digit(16)?;
        bytes.push(((high << 4) | low) as u8);
    }
    Some(bytes)
}

fn history_ansi(entry: &crate::server::HandoffEntry) -> String {
    let mut text = String::new();
    if !entry.history.is_empty() {
        text.push_str(&entry.history.join("\r\n"));
        text.push_str("\r\n");
    }
    if let Some(bytes) = hex_decode(&entry.screen_hex) {
        text.push_str(&String::from_utf8_lossy(&bytes));
    }
    // The old reader consumed these trailing bytes but its parser had not
    // applied them yet. Replaying them after the screen keeps a split
    // escape/UTF-8 sequence assembled across the switchover.
    let pending = hex_decode(&entry.pending_hex).unwrap_or_default();
    text.push_str(&String::from_utf8_lossy(&pending));
    text
}

fn pane(entry: &crate::server::HandoffEntry) -> ImportedPane {
    ImportedPane {
        terminal_id: entry.terminal_id.clone(),
        pid: entry.pid,
        cols: entry.cols,
        rows: entry.rows,
        history_ansi: history_ansi(entry),
        purpose: entry.purpose.clone(),
    }
}

/// Convert a manifest plus the persisted legacy scene book. Panes the book
/// does not place are preserved in a trailing "restored" tab, so a stale or
/// partial layout can never drop a live terminal.
pub(crate) fn build_plan(
    manifest: &HandoffManifest,
    layout_json: Option<&str>,
) -> Result<LegacyImportPlan> {
    if manifest.entries.is_empty() {
        return Ok(LegacyImportPlan {
            scenes: Vec::new(),
            selected: 0,
        });
    }
    let by_terminal: BTreeMap<&str, &crate::server::HandoffEntry> = manifest
        .entries
        .iter()
        .map(|entry| (entry.terminal_id.as_str(), entry))
        .collect();
    let mut used: BTreeSet<String> = BTreeSet::new();
    let mut scenes = Vec::new();
    let mut selected = 0usize;

    if let Some(raw) = layout_json {
        let book: SceneBook = serde_json::from_str(raw)
            .map_err(|err| Error::Validation(format!("legacy scene book is invalid: {err}")))?;
        for (scene_id, scene) in &book.scenes {
            let mut tabs = Vec::new();
            let mut active_tab = 0usize;
            for tab in &scene.tabs {
                if tab.hidden {
                    continue;
                }
                let mut panes = Vec::new();
                for leaf in tab.grid.leaves() {
                    let PaneContent::Terminal(terminal_id) = leaf else {
                        continue;
                    };
                    let Some(entry) = by_terminal.get(terminal_id.as_str()) else {
                        continue;
                    };
                    // One live PTY can be placed twice in a scene book (the
                    // legacy UI allowed it). It is still ONE session: the
                    // first placement wins and the duplicate is skipped.
                    if !used.insert(terminal_id.clone()) {
                        continue;
                    }
                    panes.push(pane(entry));
                }
                if !panes.is_empty() {
                    if tab.id == scene.active {
                        active_tab = tabs.len();
                    }
                    tabs.push(ImportedTab {
                        name: if tab.name.trim().is_empty() {
                            format!("tab {}", tabs.len() + 1)
                        } else {
                            tab.name.clone()
                        },
                        panes,
                    });
                }
            }
            if tabs.is_empty() {
                continue;
            }
            if scene_id == &book.selected {
                selected = scenes.len();
            }
            scenes.push(ImportedScene {
                name: if scene.name.trim().is_empty() {
                    scene_id.clone()
                } else {
                    scene.name.clone()
                },
                tabs,
                active_tab,
            });
        }
    }

    let orphaned: Vec<_> = manifest
        .entries
        .iter()
        .filter(|entry| !used.contains(&entry.terminal_id))
        .map(pane)
        .collect();
    if !orphaned.is_empty() {
        let tab = ImportedTab {
            name: "restored".into(),
            panes: orphaned,
        };
        match scenes.last_mut() {
            Some(scene) => {
                // Keep the user's original active tab; the restored tab is
                // reachable but does not steal focus during a takeover.
                scene.tabs.push(tab);
            }
            None => scenes.push(ImportedScene {
                name: "restored".into(),
                tabs: vec![tab],
                active_tab: 0,
            }),
        }
    }
    Ok(LegacyImportPlan { scenes, selected })
}

/// The legacy PTY carried no cell pixel size and Herdr's TUI refreshes the
/// real metrics when a client attaches. This neutral fallback is an
/// assumption, not measured geometry.
pub(crate) const DEFAULT_CELL_WIDTH_PX: u32 = 8;
pub(crate) const DEFAULT_CELL_HEIGHT_PX: u32 = 16;

/// One adopted pane, still paired with its manifest entry index so the
/// caller can take the matching received fd exactly once.
pub(crate) struct PlannedPane {
    pub pane_id: u32,
    pub workspace_id: String,
    pub public_pane_number: usize,
    pub public_pane_id: String,
    pub terminal_id: String,
    pub entry_index: usize,
    pub child_pid: u32,
    pub rows: u16,
    pub cols: u16,
    pub history_ansi: Option<String>,
}

pub(crate) struct PlannedSnapshot {
    pub snapshot: herdr::embedding::SessionSnapshot,
    pub panes: Vec<PlannedPane>,
}

fn legacy_layout(pane_ids: &[u32]) -> herdr::embedding::LayoutSnapshot {
    use herdr::embedding::{DirectionSnapshot, LayoutSnapshot};
    let mut ids = pane_ids.iter().rev();
    let first = *ids.next().expect("a tab always has at least one pane");
    let mut node = LayoutSnapshot::Pane(first);
    for id in ids {
        // Legacy split ratios were not carried in the manifest/layout plan;
        // an even split preserves order and pane count, not geometry.
        node = LayoutSnapshot::Split {
            direction: DirectionSnapshot::Horizontal,
            ratio: 0.5,
            first: Box::new(LayoutSnapshot::Pane(*id)),
            second: Box::new(node),
        };
    }
    node
}

/// Convert the plan into the snapshot Herdr restores. Fidelity is partial
/// and deliberately explicit: pane identity, process PID, size, history and
/// screen are carried; the cwd is the new host's directory (the legacy
/// manifest did not carry it), split ratios/focus/zoom and agent/worktree
/// metadata are not reconstructed.
pub(crate) fn build_snapshot(
    plan: &LegacyImportPlan,
    manifest: &HandoffManifest,
) -> Result<PlannedSnapshot> {
    use herdr::embedding::{
        PaneSnapshot, SNAPSHOT_VERSION, SessionSnapshot, TabSnapshot, WorkspaceSnapshot,
    };
    let entry_index: BTreeMap<&str, usize> = manifest
        .entries
        .iter()
        .enumerate()
        .map(|(index, entry)| (entry.terminal_id.as_str(), index))
        .collect();
    let cwd = std::env::current_dir().unwrap_or_else(|_| PathBuf::from("/"));
    let mut next_pane_id: u32 = 1;
    let mut panes = Vec::new();
    let mut workspaces = Vec::new();
    for scene in &plan.scenes {
        let workspace_id = format!("ws-legacy-{}", workspaces.len() + 1);
        let mut next_public_pane_number = 1usize;
        let mut tabs = Vec::new();
        for tab in &scene.tabs {
            let mut pane_ids = Vec::new();
            let mut tab_panes = HashMap::new();
            for pane in &tab.panes {
                let index = *entry_index.get(pane.terminal_id.as_str()).ok_or_else(|| {
                    Error::Validation(format!(
                        "import plan names terminal `{}` that the manifest does not carry",
                        pane.terminal_id
                    ))
                })?;
                let pane_id = next_pane_id;
                next_pane_id += 1;
                pane_ids.push(pane_id);
                tab_panes.insert(
                    pane_id,
                    PaneSnapshot {
                        cwd: cwd.clone(),
                        label: None,
                        agent_name: None,
                        managed_agent_kind: None,
                        agent_session: None,
                        agent_resume: None,
                        launch_argv: None,
                    },
                );
                let public_pane_number = next_public_pane_number;
                next_public_pane_number += 1;
                panes.push(PlannedPane {
                    pane_id,
                    workspace_id: workspace_id.clone(),
                    public_pane_number,
                    public_pane_id: herdr::embedding::public_pane_id(
                        &workspace_id,
                        public_pane_number,
                    ),
                    terminal_id: pane.terminal_id.clone(),
                    entry_index: index,
                    child_pid: pane.pid,
                    rows: pane.rows,
                    cols: pane.cols,
                    history_ansi: (!pane.history_ansi.is_empty())
                        .then(|| pane.history_ansi.clone()),
                });
            }
            if pane_ids.is_empty() {
                continue;
            }
            tabs.push(TabSnapshot {
                custom_name: (!tab.name.trim().is_empty()).then(|| tab.name.clone()),
                layout: legacy_layout(&pane_ids),
                panes: tab_panes,
                zoomed: false,
                focused: pane_ids.first().copied(),
                root_pane: None,
            });
        }
        if tabs.is_empty() {
            continue;
        }
        let public_pane_numbers: HashMap<u32, usize> = panes
            .iter()
            .filter(|pane| pane.workspace_id == workspace_id)
            .map(|pane| (pane.pane_id, pane.public_pane_number))
            .collect();
        let active_tab = scene.active_tab.min(tabs.len() - 1);
        workspaces.push(WorkspaceSnapshot {
            id: Some(workspace_id),
            custom_name: (!scene.name.trim().is_empty()).then(|| scene.name.clone()),
            identity_cwd: cwd.clone(),
            worktree_space: None,
            public_pane_numbers,
            next_public_pane_number,
            public_tab_numbers: Vec::new(),
            next_public_tab_number: 0,
            tabs,
            active_tab,
        });
    }
    let selected = if workspaces.is_empty() {
        0
    } else {
        plan.selected.min(workspaces.len() - 1)
    };
    Ok(PlannedSnapshot {
        snapshot: SessionSnapshot {
            version: SNAPSHOT_VERSION,
            workspaces,
            active: (!plan.scenes.is_empty()).then_some(selected),
            selected,
            sidebar_width: None,
            sidebar_section_split: None,
            collapsed_space_keys: Default::default(),
        },
        panes,
    })
}

/// One live legacy host after a `ServerRestart`: its manifest and one owned
/// duplicate of every live PTY master. The old host is still serving; it
/// detached its readers only for the transfer window.
pub(crate) struct LegacyHandoff {
    pub manifest: HandoffManifest,
    pub fds: Vec<OwnedFd>,
    pub old_host_pid: u32,
    home: PathBuf,
    stream: UnixStream,
}

/// The only handle that can acknowledge the transfer. Consuming it makes an
/// accidental early ACK a type error; dropping it closes the socket and the
/// old host reattaches its readers.
pub(crate) struct LegacyConfirm {
    stream: UnixStream,
    home: PathBuf,
    old_host_id: String,
    old_host_pid: u32,
    adopted_panes: usize,
}

impl LegacyConfirm {
    /// Retire the old host. Call this ONLY after every pane is adopted and
    /// the new host's endpoints serve; an early ACK kills the only session.
    pub(crate) fn confirm(self) -> Result<()> {
        let ack = crate::server::acknowledge_prepared(
            &self.stream,
            &HandoffManifest {
                protocol: crate::server::HANDOFF_PROTOCOL,
                host_id: self.old_host_id.clone(),
                entries: Vec::new(),
            },
        );
        match ack {
            Ok(()) => {
                record_legacy_event(
                    &self.home,
                    "native_legacy_handoff_adopted",
                    serde_json::json!({
                        "old_host_id": self.old_host_id,
                        "old_host_pid": self.old_host_pid,
                        "adopted_panes": self.adopted_panes,
                        "fidelity": "degraded_import_process_and_screen_only",
                    }),
                );
                Ok(())
            }
            Err(error) => {
                // A failed ACK write means the sender side is gone or its
                // socket died. If the old host still answers, it will take
                // its readers back and the caller must abort; otherwise the
                // adopted fds are the only copies and the sessions live here.
                let old_host_alive = Client::connect(&self.home).is_ok();
                record_legacy_event(
                    &self.home,
                    "native_legacy_handoff_ack_unconfirmed",
                    serde_json::json!({
                        "old_host_id": self.old_host_id,
                        "old_host_pid": self.old_host_pid,
                        "old_host_alive": old_host_alive,
                        "reason": error.to_string(),
                    }),
                );
                if old_host_alive {
                    return Err(Error::Validation(format!(
                        "legacy host did not acknowledge the prepared takeover ({error}); \
                         it still serves, so the adopted sessions are dropped"
                    )));
                }
                Ok(())
            }
        }
    }
}

fn record_legacy_event(home: &Path, kind: &str, payload: serde_json::Value) {
    // Evidence only: never fail the takeover because a record could not be
    // written after the old host was already retired.
    let Ok(store) = Store::attach_for_handoff(
        &crate::foundation::paths::database_path(home),
        crate::server::migrations(),
    ) else {
        return;
    };
    let _ = crate::foundation::events::append(
        &store,
        crate::foundation::events::NewEvent {
            domain: crate::server::DOMAIN_HOST,
            kind: kind.into(),
            subject_type: "runtime_process".into(),
            subject_id: format!("native-{}", uuid::Uuid::new_v4().simple()),
            origin: "native_host".into(),
            payload,
        },
    );
}

impl LegacyHandoff {
    pub(crate) fn into_parts(self) -> (HandoffManifest, Vec<OwnedFd>, LegacyConfirm) {
        let LegacyHandoff {
            manifest,
            fds,
            old_host_pid,
            home,
            stream,
        } = self;
        let old_host_id = manifest.host_id.clone();
        let adopted_panes = manifest.entries.len();
        (
            manifest,
            fds,
            LegacyConfirm {
                stream,
                home,
                old_host_id,
                old_host_pid,
                adopted_panes,
            },
        )
    }
}

/// Any control socket a legacy host binds. Their presence without an answer
/// must block a fresh native start: two hosts on one home would allocate
/// terminals and executions behind each other.
pub(crate) fn legacy_control_socket_present(home: &Path) -> bool {
    [
        crate::server::SOCKET_NAME,
        crate::server::RESUME_SOCKET_NAME,
        crate::server::RESUME_NEXT_SOCKET_NAME,
    ]
    .iter()
    .any(|name| home.join(name).exists())
}

/// True only when every legacy control socket that exists refuses
/// connections: a crashed host's leftover file, not a live or wedged one.
/// Anything that answers, times out or denies access is not stale.
pub(crate) fn only_stale_control_sockets(home: &Path) -> bool {
    let mut present = false;
    for name in [
        crate::server::SOCKET_NAME,
        crate::server::RESUME_SOCKET_NAME,
        crate::server::RESUME_NEXT_SOCKET_NAME,
    ] {
        let path = home.join(name);
        if !path.exists() {
            continue;
        }
        present = true;
        match UnixStream::connect(&path) {
            Err(error)
                if matches!(
                    error.kind(),
                    std::io::ErrorKind::ConnectionRefused | std::io::ErrorKind::NotFound
                ) => {}
            _ => return false,
        }
    }
    present
}

/// The persisted legacy scene book, read WITHOUT migrating or renaming
/// anything underneath the active sender.
pub(crate) fn read_layout(home: &Path) -> Result<Option<String>> {
    let path = crate::foundation::paths::database_path(home);
    if !path.exists() {
        return Ok(None);
    }
    let store = Store::attach_for_handoff(&path, crate::server::migrations())?;
    let saved: Option<String> = store
        .connection()
        .query_row(
            "SELECT value FROM settings WHERE key = 'workbench_layout'",
            [],
            |row| row.get(0),
        )
        .optional()?;
    Ok(saved.filter(|value| !value.is_empty()))
}

#[derive(Debug, Clone)]
pub(crate) struct ActiveLegacyExecution {
    pub execution_id: ExecutionId,
    pub task_id: TaskId,
    pub member_id: MemberId,
    pub grant_id: GrantId,
    pub request_key: String,
    pub terminal_id: String,
    pub argv: Vec<String>,
    pub cwd: PathBuf,
    pub worktree_id: Option<String>,
    pub pid: i64,
    pub pid_start_marker: Option<String>,
}

struct LaunchIntentRow {
    request_key: String,
    member_id: String,
    state: String,
    pid: Option<i64>,
    pid_start_marker: Option<String>,
}

struct LaunchSpecRow {
    terminal_id: Option<String>,
    argv: String,
    cwd: String,
    worktree_id: Option<String>,
    origin: String,
}

fn cannot_adopt(execution: &str, reason: &str) -> Error {
    Error::Validation(format!(
        "legacy execution `{execution}` cannot be inherited: {reason}. The old host is still \
         serving. Stop that execution in the legacy workbench, or keep using `viva workbench`."
    ))
}

/// Canonical executions that are still requested/running in the legacy host.
/// Every one must be adoptable before this process may take the live session:
/// otherwise the native owner gate would have to treat a running member as
/// unowned work. No ACK has happened when this check fails.
pub(crate) fn active_executions(home: &Path) -> Result<Vec<ActiveLegacyExecution>> {
    let path = crate::foundation::paths::database_path(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let store = Store::attach_for_handoff(&path, crate::server::migrations())?;
    let rows: Vec<(String, String, String)> = {
        let mut stmt = store.connection().prepare(
            "SELECT execution_id, task_id, status FROM executions \
             WHERE status IN ('requested','running') ORDER BY created_at, execution_id",
        )?;
        stmt.query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
            .collect::<std::result::Result<_, _>>()?
    };
    let mut active = Vec::with_capacity(rows.len());
    for (execution_id, task_id, status) in rows {
        if status == "requested" {
            return Err(cannot_adopt(
                &execution_id,
                "it is still requested and has no confirmed live process",
            ));
        }
        let intent: Option<LaunchIntentRow> = store
            .connection()
            .query_row(
                "SELECT request_key, member_id, state, pid, pid_start_marker \
                 FROM launch_intents WHERE execution_id=?1 \
                 ORDER BY intended_at DESC LIMIT 1",
                [&execution_id],
                |row| {
                    Ok(LaunchIntentRow {
                        request_key: row.get(0)?,
                        member_id: row.get(1)?,
                        state: row.get(2)?,
                        pid: row.get(3)?,
                        pid_start_marker: row.get(4)?,
                    })
                },
            )
            .optional()?;
        let Some(LaunchIntentRow {
            request_key,
            member_id,
            state: intent_state,
            pid,
            pid_start_marker,
        }) = intent
        else {
            return Err(cannot_adopt(
                &execution_id,
                "it has no persisted launch intent",
            ));
        };
        if intent_state != "confirmed" {
            return Err(cannot_adopt(
                &execution_id,
                &format!("its launch intent is `{intent_state}`, not confirmed"),
            ));
        }
        let Some(pid) = pid.filter(|pid| *pid > 0) else {
            return Err(cannot_adopt(
                &execution_id,
                "its confirmed launch intent carries no process id",
            ));
        };
        let spec: Option<LaunchSpecRow> = store
            .connection()
            .query_row(
                "SELECT terminal_id, argv, cwd, worktree_id, request_origin \
                 FROM launch_specs WHERE execution_id=?1 ORDER BY created_at DESC LIMIT 1",
                [&execution_id],
                |row| {
                    Ok(LaunchSpecRow {
                        terminal_id: row.get(0)?,
                        argv: row.get(1)?,
                        cwd: row.get(2)?,
                        worktree_id: row.get(3)?,
                        origin: row.get(4)?,
                    })
                },
            )
            .optional()?;
        let Some(LaunchSpecRow {
            terminal_id,
            argv,
            cwd,
            worktree_id,
            origin,
        }) = spec
        else {
            return Err(cannot_adopt(
                &execution_id,
                "it has no persisted launch specification",
            ));
        };
        let Some(terminal_id) = terminal_id.filter(|id| !id.is_empty()) else {
            return Err(cannot_adopt(
                &execution_id,
                "its launch specification names no terminal",
            ));
        };
        let argv: Vec<String> = serde_json::from_str(&argv)
            .map_err(|err| cannot_adopt(&execution_id, &format!("invalid argv: {err}")))?;
        if argv.is_empty() || cwd.is_empty() {
            return Err(cannot_adopt(
                &execution_id,
                "its launch specification has no argv or cwd",
            ));
        }
        let grant_id = match serde_json::from_str::<RequestOrigin>(&origin)
            .map_err(|err| cannot_adopt(&execution_id, &format!("invalid request origin: {err}")))?
        {
            RequestOrigin::MemberDispatch { grant_id } | RequestOrigin::Automation { grant_id } => {
                grant_id
            }
            RequestOrigin::UserDirect => {
                return Err(cannot_adopt(
                    &execution_id,
                    "its launch origin carries no member/automation grant",
                ));
            }
        };
        active.push(ActiveLegacyExecution {
            execution_id: ExecutionId::from_str(&execution_id).map_err(|err| {
                cannot_adopt(&execution_id, &format!("invalid execution id: {err}"))
            })?,
            task_id: TaskId::from_str(&task_id)
                .map_err(|err| cannot_adopt(&execution_id, &format!("invalid task id: {err}")))?,
            member_id: MemberId::from_str(&member_id)
                .map_err(|err| cannot_adopt(&execution_id, &format!("invalid member id: {err}")))?,
            grant_id,
            request_key,
            terminal_id,
            argv,
            cwd: PathBuf::from(cwd),
            worktree_id,
            pid,
            pid_start_marker,
        });
    }
    Ok(active)
}

/// Ask the live legacy host for its terminals and take the fd copies. NO
/// acknowledgement happens here: the caller still has to adopt every pane.
pub(crate) fn receive_from_live_host(home: &Path) -> Result<LegacyHandoff> {
    let mut client = Client::connect(home)?;
    let before = client.call(RequestKind::Ping)?;
    let old_host_pid = before
        .get("pid")
        .and_then(|pid| pid.as_u64())
        .ok_or_else(|| Error::Validation("legacy host handshake carried no pid".into()))?
        as u32;
    let response = client.call(RequestKind::ServerRestart)?;
    let handoff_socket = response
        .get("handoff_socket")
        .and_then(|path| path.as_str())
        .ok_or_else(|| Error::Validation("restart response carried no handoff socket".into()))?;
    let stream = UnixStream::connect(handoff_socket).map_err(|error| {
        Error::Validation(format!(
            "could not connect to the legacy handoff socket: {error}"
        ))
    })?;
    // The old host closes the connection when this fails; its readers come
    // back and every process stays exactly as it was.
    let (manifest, fds) = crate::server::receive_entries(&stream).map_err(|error| {
        Error::Validation(format!(
            "legacy handoff transfer failed; the old host keeps serving: {error}"
        ))
    })?;
    if manifest.entries.len() != fds.len() {
        return Err(Error::Validation(format!(
            "legacy handoff manifest has {} entries but {} fds; refusing to adopt",
            manifest.entries.len(),
            fds.len()
        )));
    }
    Ok(LegacyHandoff {
        manifest,
        fds,
        old_host_pid,
        home: home.to_path_buf(),
        stream,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{HandoffEntry, HandoffManifest};

    fn entry(id: &str, pid: u32, history: &[&str], screen: &str) -> HandoffEntry {
        HandoffEntry {
            terminal_id: id.into(),
            owner: "user_shell".into(),
            worktree_id: None,
            purpose: format!("shell {id}"),
            pid,
            pid_start_marker: "marker".into(),
            cols: 80,
            rows: 24,
            history: history.iter().map(|line| line.to_string()).collect(),
            screen_hex: screen.into(),
            pending_hex: String::new(),
        }
    }

    #[test]
    fn scene_book_places_live_terminals_and_orphans_are_preserved() {
        let manifest = HandoffManifest {
            protocol: 2,
            host_id: "old-host".into(),
            entries: vec![
                entry("term-a", 10, &["old line"], "4142"),
                entry("term-b", 11, &[], "43"),
                entry("term-c", 12, &[], ""),
            ],
        };
        let layout = serde_json::json!({
            "version": 2,
            "selected": "scene-1",
            "sidebar": 26,
            "scenes": {
                "scene-1": {
                    "name": "repo",
                    "active": "tab-2",
                    "tabs": [
                        {
                            "id": "tab-1",
                            "name": "one",
                            "grid": {"Leaf": {"Terminal": "term-a"}},
                            "focus": {"Terminal": "term-a"},
                            "zoom": false
                        },
                        {
                            "id": "tab-2",
                            "name": "two",
                            "grid": {
                                "Split": {
                                    "axis": "Horizontal",
                                    "first": {"Leaf": {"Terminal": "term-b"}},
                                    "second": {"Leaf": "Browser"},
                                    "ratio": 50
                                }
                            },
                            "focus": {"Terminal": "term-b"},
                            "zoom": false
                        }
                    ]
                }
            }
        })
        .to_string();

        let plan = build_plan(&manifest, Some(&layout)).unwrap();
        assert_eq!(plan.scenes.len(), 1);
        assert_eq!(plan.scenes[0].name, "repo");
        assert_eq!(plan.scenes[0].tabs.len(), 3, "two book tabs + restored");
        assert_eq!(plan.scenes[0].tabs[0].panes[0].terminal_id, "term-a");
        assert!(
            plan.scenes[0].tabs[0].panes[0]
                .history_ansi
                .contains("old line")
        );
        assert!(plan.scenes[0].tabs[0].panes[0].history_ansi.contains("AB"));
        assert_eq!(plan.scenes[0].tabs[1].panes[0].terminal_id, "term-b");
        assert_eq!(plan.scenes[0].tabs[2].name, "restored");
        assert_eq!(plan.scenes[0].tabs[2].panes[0].terminal_id, "term-c");
        assert_eq!(plan.scenes[0].active_tab, 1);
    }

    #[test]
    fn missing_or_invalid_layout_keeps_every_terminal() {
        for layout in [None, Some("{not-json")] {
            let manifest = HandoffManifest {
                protocol: 2,
                host_id: "old-host".into(),
                entries: vec![entry("term-a", 1, &[], ""), entry("term-b", 2, &[], "")],
            };
            // Invalid JSON is an honest conversion error, not a silent drop.
            if layout == Some("{not-json") {
                assert!(build_plan(&manifest, layout).is_err());
                continue;
            }
            let plan = build_plan(&manifest, layout).unwrap();
            assert_eq!(plan.scenes.len(), 1);
            assert_eq!(plan.scenes[0].tabs.len(), 1);
            assert_eq!(plan.scenes[0].tabs[0].panes.len(), 2);
        }
    }

    #[test]
    fn duplicate_terminal_placement_yields_one_session() {
        let manifest = HandoffManifest {
            protocol: 2,
            host_id: "old-host".into(),
            entries: vec![entry("term-a", 7, &[], ""), entry("term-b", 8, &[], "")],
        };
        let layout = serde_json::json!({
            "version": 2,
            "selected": "scene-1",
            "sidebar": 26,
            "scenes": {
                "scene-1": {
                    "name": "repo",
                    "active": "tab-1",
                    "tabs": [{
                        "id": "tab-1",
                        "name": "one",
                        "grid": {"Split": {
                            "axis": "Horizontal",
                            "first": {"Leaf": {"Terminal": "term-a"}},
                            "second": {"Leaf": {"Terminal": "term-a"}},
                            "ratio": 50
                        }},
                        "focus": {"Terminal": "term-a"},
                        "zoom": false
                    }]
                }
            }
        })
        .to_string();
        let plan = build_plan(&manifest, Some(&layout)).unwrap();
        assert_eq!(plan.scenes.len(), 1);
        assert_eq!(plan.scenes[0].tabs.len(), 2, "book tab + orphan tab");
        assert_eq!(plan.scenes[0].tabs[0].panes.len(), 1);
        assert_eq!(plan.scenes[0].tabs[1].panes[0].terminal_id, "term-b");
    }

    #[test]
    fn pending_tail_is_replayed_after_the_screen() {
        let mut entry = entry("term-a", 9, &[], "4142");
        entry.pending_hex = "1b5b33".into();
        let manifest = HandoffManifest {
            protocol: 2,
            host_id: "old-host".into(),
            entries: vec![entry],
        };
        let plan = build_plan(&manifest, None).unwrap();
        let history = &plan.scenes[0].tabs[0].panes[0].history_ansi;
        assert_eq!(history, "AB\u{1b}[3");
    }

    /// Fake sender: the receiver must return the same manifest/fds WITHOUT
    /// acknowledging, and the sender must see exactly one prepared ACK.
    #[test]
    fn wire_round_trip_acknowledges_only_after_the_receiver_prepares() {
        use std::os::fd::AsRawFd as _;
        let (mut sender, receiver) = UnixStream::pair().unwrap();
        let manifest = HandoffManifest {
            protocol: crate::server::HANDOFF_PROTOCOL,
            host_id: "old-host".into(),
            entries: vec![entry("term-a", 1, &[], ""), entry("term-b", 2, &[], "")],
        };
        let fd_a = std::fs::File::open("/dev/null").unwrap();
        let fd_b = std::fs::File::open("/dev/null").unwrap();
        let raw = [fd_a.as_raw_fd(), fd_b.as_raw_fd()];
        let sending = manifest.clone();
        let sender_thread =
            std::thread::spawn(move || crate::server::send_entries(&mut sender, &sending, &raw));
        let (received, fds) = crate::server::receive_entries(&receiver).unwrap();
        assert_eq!(received, manifest);
        assert_eq!(fds.len(), 2);
        // Simulated adoption completed: only now the prepared ACK goes out.
        crate::server::acknowledge_prepared(&receiver, &received).unwrap();
        sender_thread.join().unwrap().unwrap();
    }

    #[test]
    fn plan_maps_scenes_tabs_and_panes_into_a_restorable_snapshot() {
        use herdr::embedding::LayoutSnapshot;
        let manifest = HandoffManifest {
            protocol: 2,
            host_id: "old-host".into(),
            entries: vec![
                entry("term-a", 10, &["history line"], "4142"),
                entry("term-b", 11, &[], "43"),
                entry("term-c", 12, &[], ""),
            ],
        };
        let layout = serde_json::json!({
            "version": 2,
            "selected": "scene-2",
            "sidebar": 26,
            "scenes": {
                "scene-1": {
                    "name": "first",
                    "active": "tab-1",
                    "tabs": [{
                        "id": "tab-1",
                        "name": "solo",
                        "grid": {"Leaf": {"Terminal": "term-a"}},
                        "focus": {"Terminal": "term-a"},
                        "zoom": false
                    }]
                },
                "scene-2": {
                    "name": "second",
                    "active": "tab-2",
                    "tabs": [{
                        "id": "tab-2",
                        "name": "pair",
                        "grid": {"Split": {
                            "axis": "Horizontal",
                            "first": {"Leaf": {"Terminal": "term-b"}},
                            "second": {"Leaf": {"Terminal": "term-c"}},
                            "ratio": 70
                        }},
                        "focus": {"Terminal": "term-c"},
                        "zoom": false
                    }]
                }
            }
        })
        .to_string();
        let plan = build_plan(&manifest, Some(&layout)).unwrap();
        assert_eq!(plan.selected, 1);
        let planned = build_snapshot(&plan, &manifest).unwrap();
        assert_eq!(planned.snapshot.version, herdr::embedding::SNAPSHOT_VERSION);
        assert_eq!(planned.snapshot.selected, 1);
        assert_eq!(planned.snapshot.active, Some(1));
        let workspaces = &planned.snapshot.workspaces;
        assert_eq!(workspaces.len(), 2);
        assert_eq!(workspaces[0].id.as_deref(), Some("ws-legacy-1"));
        assert_eq!(workspaces[1].custom_name.as_deref(), Some("second"));
        assert_eq!(workspaces[1].active_tab, 0);
        assert_eq!(planned.panes.len(), 3, "one adopted pane per live PTY");
        // Global monotonic pane ids; the second tab's order is preserved.
        assert_eq!(planned.panes[0].pane_id, 1);
        assert_eq!(planned.panes[1].pane_id, 2);
        assert_eq!(planned.panes[2].pane_id, 3);
        let tab = &workspaces[1].tabs[0];
        match &tab.layout {
            LayoutSnapshot::Split { first, second, .. } => {
                assert!(matches!(**first, LayoutSnapshot::Pane(2)));
                assert!(matches!(**second, LayoutSnapshot::Pane(3)));
            }
            LayoutSnapshot::Pane(_) => panic!("expected a split layout"),
        }
        assert_eq!(
            planned.panes[1].entry_index, 1,
            "term-b is manifest entry 1"
        );
        assert_eq!(planned.panes[1].child_pid, 11);
        assert!(
            planned.panes[0]
                .history_ansi
                .as_deref()
                .unwrap_or_default()
                .contains("history line")
        );
    }
}
