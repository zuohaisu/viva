//! Restore a workspace after the server process itself has exited.
//!
//! Live handoff (`viva update`, `server --resume`) does not use this module:
//! those paths keep the existing processes. This path runs only when a new
//! server is started because the previous process is gone. It restores the
//! scene / tab / pane tree, each pane's saved directory, and focus. It does
//! not resurrect the old shell, test process, or arbitrary command, and it
//! does not re-run a finished execution.
//!
//! A pane with one valid integration resume command starts that command so
//! the agent can reopen its own session. A missing, duplicated, or failed
//! command falls back to a new shell in the same directory. `resume_agents_on_restore`
//! in `config.toml` turns that off. A directory that is gone, or a shell that
//! fails to start, leaves the pane in the tree and records an error. The pane
//! is not moved to the home directory and it is not removed.
//!
//! Pane screen history is off unless `pane_screen_history` is set. When it is
//! on, the text is private, redacted with the existing redactor before it is
//! written, and replayed only when the saved layout matches exactly.

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};
use std::str::FromStr;
use std::time::{Duration, Instant};

use crate::foundation::error::{Error, Result};
use crate::foundation::ids::{TerminalId, utc_now};
use crate::foundation::records::TerminalOwner;
use crate::foundation::store::Store;
use crate::redaction::Redactor;
use crate::terminal::{TerminalSnapshot, TerminalSpec};
use crate::tui::layout::PaneContent;
use crate::tui::workbench::scenes::SceneBook;

use super::Shared;

const HISTORY_FILE: &str = "pane-history.json";
const SECRETS_FILE: &str = "redaction.secrets";
const HISTORY_FLUSH: Duration = Duration::from_secs(2);

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct RestoreSettings {
    pub resume_agents_on_restore: bool,
    pub pane_screen_history: bool,
}

impl Default for RestoreSettings {
    fn default() -> Self {
        Self {
            resume_agents_on_restore: true,
            pane_screen_history: false,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct ResumeRow {
    pub terminal_id: String,
    pub native_session: String,
    pub resume_command: String,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum ShellWhy {
    /// The pane had no resume command. A new shell is the normal case.
    NoResume,
    Disabled,
    Missing,
    Duplicate,
    Ambiguous,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum PaneStart {
    Shell { why: ShellWhy },
    Resume { argv: Vec<String> },
}

#[derive(Debug)]
pub(crate) enum HistoryRead {
    Absent,
    Replay(BTreeMap<String, Vec<String>>),
    Discarded,
    Corrupt { backup: PathBuf },
}

/// Start new processes for the saved panes. Safe to call only on a server
/// that did not adopt live terminals. Panes that already have a process are
/// left alone.
pub fn restore_workspace(shared: &Shared) -> Result<()> {
    let (settings, warnings) = read_settings_file(&shared.home);
    for warning in &warnings {
        eprintln!("server: {warning}");
    }
    let tree = shared.tree.lock().expect("scene tree");
    if tree.unmanaged.is_some() || !tree.stored {
        drop(tree);
        return Ok(());
    }
    let book = tree.book.clone();
    drop(tree);

    let layout = serde_json::to_string(&book)?;
    let history = match read_history(&shared.home, &layout, settings.pane_screen_history)? {
        HistoryRead::Replay(lines) => lines,
        HistoryRead::Corrupt { backup } => {
            let store = shared.store.lock().expect("store");
            let _ = super::record_recovery(
                &store,
                &shared.host_id,
                "snapshot_corrupt",
                &format!(
                    "pane screen history could not be read; a backup of the original bytes is at {}; the history file was not replaced and was not cleared",
                    backup.display()
                ),
            );
            BTreeMap::new()
        }
        HistoryRead::Absent | HistoryRead::Discarded => BTreeMap::new(),
    };

    let (cwds, rows) = {
        let store = shared.store.lock().expect("store");
        (
            load_pane_cwds(store.connection()),
            load_resume_rows(store.connection())?,
        )
    };
    let panes = terminal_panes(&book);
    let plan = plan_pane_starts(&panes, &rows, settings.resume_agents_on_restore);
    let mut shells = 0usize;
    let mut resumes = 0usize;
    let mut kept = 0usize;
    let mut errors = BTreeMap::new();

    for pane in &panes {
        if let Ok(id) = TerminalId::from_str(pane) {
            if shared.terminals.handle(&id)?.is_some() {
                continue;
            }
        }
        let Some(cwd) = cwds.get(pane).cloned() else {
            errors.insert(pane.clone(), "no saved directory; pane kept".into());
            kept += 1;
            continue;
        };
        if !Path::new(&cwd).is_absolute() || !Path::new(&cwd).is_dir() {
            errors.insert(
                pane.clone(),
                format!("directory does not exist: {cwd}; pane kept"),
            );
            let store = shared.store.lock().expect("store");
            let _ = super::record_recovery(
                &store,
                &shared.host_id,
                "pane_restore_kept",
                &format!(
                    "pane `{pane}` directory does not exist: {cwd}; pane kept, shell not started"
                ),
            );
            kept += 1;
            continue;
        }
        let lines = history.get(pane).cloned().unwrap_or_default();
        let start = plan.get(pane).cloned().unwrap_or(PaneStart::Shell {
            why: ShellWhy::NoResume,
        });
        match start {
            PaneStart::Resume { argv } => {
                let program = argv.first().cloned().unwrap_or_default();
                match spawn_pane(
                    shared,
                    pane,
                    &argv,
                    &cwd,
                    TerminalOwner::AgentCli,
                    "agent resume",
                    &lines,
                ) {
                    Ok(()) => {
                        resumes += 1;
                        let store = shared.store.lock().expect("store");
                        let _ = super::record_recovery(
                            &store,
                            &shared.host_id,
                            "pane_restore_resume",
                            &format!(
                                "pane `{pane}` resumed its agent session with `{program}` in {cwd}; this is not a task completion"
                            ),
                        );
                    }
                    Err(err) => {
                        let store = shared.store.lock().expect("store");
                        let _ = super::record_recovery(
                            &store,
                            &shared.host_id,
                            "pane_restore_fallback",
                            &format!(
                                "pane `{pane}` resume command failed ({err}); starting a shell in {cwd}"
                            ),
                        );
                        drop(store);
                        if let Err(shell_err) = spawn_shell(shared, pane, &cwd, &lines) {
                            errors.insert(
                                pane.clone(),
                                format!("shell failed to start in {cwd}: {shell_err}; pane kept"),
                            );
                            kept += 1;
                        } else {
                            shells += 1;
                        }
                    }
                }
            }
            PaneStart::Shell { why } => {
                if why != ShellWhy::NoResume {
                    let store = shared.store.lock().expect("store");
                    let _ = super::record_recovery(
                        &store,
                        &shared.host_id,
                        "pane_restore_fallback",
                        &format!(
                            "pane `{pane}` resume was not used ({}); starting a shell in {cwd}",
                            why.as_str()
                        ),
                    );
                }
                if let Err(err) = spawn_shell(shared, pane, &cwd, &lines) {
                    errors.insert(
                        pane.clone(),
                        format!("shell failed to start in {cwd}: {err}; pane kept"),
                    );
                    let store = shared.store.lock().expect("store");
                    let _ = super::record_recovery(
                        &store,
                        &shared.host_id,
                        "pane_restore_kept",
                        &format!("pane `{pane}` shell failed to start in {cwd}: {err}; pane kept"),
                    );
                    kept += 1;
                } else {
                    shells += 1;
                }
            }
        }
    }

    *shared.pane_errors.lock().expect("pane errors") = errors;
    let store = shared.store.lock().expect("store");
    let _ = super::record_recovery(
        &store,
        &shared.host_id,
        "workspace_restored",
        &format!(
            "restored the saved layout after the previous server exited: {shells} new shell(s), {resumes} agent resume(s), {kept} pane(s) kept without a process; previous commands were not restarted and finished executions were not re-run"
        ),
    );
    Ok(())
}

impl ShellWhy {
    fn as_str(self) -> &'static str {
        match self {
            ShellWhy::NoResume => "no resume command",
            ShellWhy::Disabled => "resume_agents_on_restore is off",
            ShellWhy::Missing => "command missing",
            ShellWhy::Duplicate => "command duplicated",
            ShellWhy::Ambiguous => "more than one resume command",
        }
    }
}

fn spawn_shell(shared: &Shared, pane: &str, cwd: &str, history: &[String]) -> Result<()> {
    let shell = user_shell();
    spawn_pane(
        shared,
        pane,
        &[shell],
        cwd,
        TerminalOwner::UserShell,
        "shell",
        history,
    )
}

fn spawn_pane(
    shared: &Shared,
    pane: &str,
    argv: &[String],
    cwd: &str,
    owner: TerminalOwner,
    purpose: &str,
    history: &[String],
) -> Result<()> {
    let id = TerminalId::from_str(pane).map_err(|_| {
        Error::Validation(format!(
            "pane `{pane}` is not a terminal id; it was kept and no process was started"
        ))
    })?;
    let spec = TerminalSpec::new(argv.to_vec(), PathBuf::from(cwd))?;
    let store = shared.store.lock().expect("store");
    shared.terminals.spawn_with_id(
        id.clone(),
        spec,
        owner,
        None,
        purpose,
        history.to_vec(),
        Some(&store),
    )?;
    remember_pane_cwd(store.connection(), pane, cwd)?;
    drop(store);
    if purpose == "agent resume" {
        super::seed_agent_identification(shared, &id, argv);
    }
    Ok(())
}

/// A pane that could not be started. The workbench draws this instead of a
/// live grid. `None` when the pane has a process or no recorded error.
pub(super) fn error_snapshot(shared: &Shared, pane_id: &str) -> Option<TerminalSnapshot> {
    let message = shared
        .pane_errors
        .lock()
        .expect("pane errors")
        .get(pane_id)?
        .clone();
    if let Ok(id) = TerminalId::from_str(pane_id) {
        if shared.terminals.handle(&id).ok().flatten().is_some() {
            return None;
        }
    }
    Some(TerminalSnapshot {
        screen: None,
        cols: 80,
        rows: 24,
        visible: vec![message],
        scrollback: Vec::new(),
        scrollback_capped: false,
        total_output_bytes: 0,
        log_truncated: false,
    })
}

pub(super) fn maybe_flush_screen_history(shared: &Shared) {
    let mut slot = shared.history_flushed_at.lock().expect("history flush");
    let due = slot.map(|at| at.elapsed() >= HISTORY_FLUSH).unwrap_or(true);
    if !due {
        return;
    }
    *slot = Some(Instant::now());
    drop(slot);
    if !read_settings_file(&shared.home).0.pane_screen_history {
        return;
    }
    let _ = flush_screen_history(shared);
}

pub(super) fn flush_screen_history(shared: &Shared) -> Result<()> {
    let settings = read_settings_file(&shared.home).0;
    if !settings.pane_screen_history {
        return Ok(());
    }
    let book = shared.tree.lock().expect("scene tree").book.clone();
    let layout = serde_json::to_string(&book)?;
    let secrets = load_redaction_secrets(&shared.home);
    let mut panes = BTreeMap::new();
    for pane in terminal_panes(&book) {
        let Ok(id) = TerminalId::from_str(&pane) else {
            continue;
        };
        let Some(handle) = shared.terminals.handle(&id)? else {
            continue;
        };
        let snap = handle.snapshot()?;
        let mut lines = snap.scrollback;
        lines.extend(snap.visible);
        if lines.len() > crate::terminal::SCROLLBACK_LINES {
            let skip = lines.len() - crate::terminal::SCROLLBACK_LINES;
            lines.drain(..skip);
        }
        panes.insert(pane, redact_lines(&lines, &secrets));
    }
    if panes.is_empty() {
        return Ok(());
    }
    let body = serde_json::to_vec(&HistoryFile { layout, panes })?;
    write_private(&shared.home.join(HISTORY_FILE), &body)
}

/// Copy `bytes` to a new backup file. Does not open, replace, or truncate
/// whatever currently holds those bytes.
pub(crate) fn backup_original_bytes(home: &Path, stem: &str, bytes: &[u8]) -> Result<PathBuf> {
    std::fs::create_dir_all(home)?;
    let stamp = utc_now().replace([':', '+'], "-");
    for n in 0..20 {
        let name = if n == 0 {
            format!("{stem}.corrupt-{stamp}")
        } else {
            format!("{stem}.corrupt-{stamp}-{n}")
        };
        let path = home.join(name);
        let mut options = std::fs::OpenOptions::new();
        options.create_new(true).write(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        match options.open(&path) {
            Ok(mut file) => {
                use std::io::Write;
                file.write_all(bytes)?;
                return Ok(path);
            }
            Err(err) if err.kind() == std::io::ErrorKind::AlreadyExists => continue,
            Err(err) => return Err(err.into()),
        }
    }
    Err(Error::Validation(format!(
        "could not allocate a backup name for {stem}"
    )))
}

pub(super) fn note_corrupt_snapshot(
    store: &Store,
    home: &Path,
    host_id: &str,
    label: &str,
    bytes: &[u8],
) {
    let detail = match backup_original_bytes(home, label, bytes) {
        Ok(path) => format!(
            "{label} could not be read; a backup of the original bytes is at {}; the stored bytes were not replaced and were not cleared",
            path.display()
        ),
        Err(err) => format!(
            "{label} could not be read; the backup could not be written ({err}); the stored bytes were not replaced and were not cleared"
        ),
    };
    let _ = super::record_recovery(store, host_id, "snapshot_corrupt", &detail);
}

pub(super) fn remember_pane_cwd(
    conn: &rusqlite::Connection,
    pane_id: &str,
    cwd: &str,
) -> Result<()> {
    if pane_id.is_empty() || cwd.is_empty() {
        return Ok(());
    }
    let mut map = load_pane_cwds(conn);
    if map.get(pane_id).map(String::as_str) == Some(cwd) {
        return Ok(());
    }
    map.insert(pane_id.to_string(), cwd.to_string());
    let json = serde_json::to_string(&map)?;
    if json.len() > super::LAYOUT_MAX_BYTES {
        return Err(Error::Validation(
            "pane directory map exceeds the 64 KiB bound".into(),
        ));
    }
    conn.execute(
        "INSERT INTO settings(key, value) VALUES ('pane_cwd', ?1)
         ON CONFLICT(key) DO UPDATE SET value = excluded.value",
        rusqlite::params![json],
    )?;
    Ok(())
}

pub(crate) fn parse_restore_settings(text: &str) -> (RestoreSettings, Vec<String>) {
    let mut settings = RestoreSettings::default();
    let mut warnings = Vec::new();
    let mut in_section = false;
    for raw in text.lines() {
        let line = strip_comment(raw).trim();
        if line.is_empty() {
            continue;
        }
        if line.starts_with('[') {
            in_section = true;
            continue;
        }
        if in_section {
            continue;
        }
        let Some((key, value)) = line.split_once('=') else {
            continue;
        };
        let key = key.trim();
        let value = value.trim();
        match key {
            "resume_agents_on_restore" => match parse_bool(value) {
                Some(flag) => settings.resume_agents_on_restore = flag,
                None => warnings.push(format!(
                    "resume_agents_on_restore value `{value}` is not true or false; ignored"
                )),
            },
            "pane_screen_history" => match parse_bool(value) {
                Some(flag) => settings.pane_screen_history = flag,
                None => warnings.push(format!(
                    "pane_screen_history value `{value}` is not true or false; ignored"
                )),
            },
            _ => {}
        }
    }
    (settings, warnings)
}

pub(crate) fn plan_pane_starts(
    panes: &[String],
    rows: &[ResumeRow],
    resume_enabled: bool,
) -> BTreeMap<String, PaneStart> {
    let chosen: BTreeMap<String, Chosen> = panes
        .iter()
        .map(|pane| (pane.clone(), choose_row(pane, rows)))
        .collect();
    let mut command_count: BTreeMap<String, usize> = BTreeMap::new();
    let mut session_count: BTreeMap<String, usize> = BTreeMap::new();
    for chosen in chosen.values() {
        if let Chosen::One(row) = chosen {
            *command_count.entry(row.command.clone()).or_default() += 1;
            if !row.session.is_empty() {
                *session_count.entry(row.session.clone()).or_default() += 1;
            }
        }
    }
    let mut plan = BTreeMap::new();
    for pane in panes {
        let start = match chosen.get(pane) {
            None | Some(Chosen::None) => PaneStart::Shell {
                why: ShellWhy::NoResume,
            },
            _ if !resume_enabled => PaneStart::Shell {
                why: ShellWhy::Disabled,
            },
            Some(Chosen::Ambiguous) => PaneStart::Shell {
                why: ShellWhy::Ambiguous,
            },
            Some(Chosen::One(row))
                if command_count.get(&row.command).copied().unwrap_or(0) > 1
                    || (!row.session.is_empty()
                        && session_count.get(&row.session).copied().unwrap_or(0) > 1) =>
            {
                PaneStart::Shell {
                    why: ShellWhy::Duplicate,
                }
            }
            Some(Chosen::One(row)) => match resume_argv(&row.command) {
                Some(argv) if program_available(&argv[0]) => PaneStart::Resume { argv },
                _ => PaneStart::Shell {
                    why: ShellWhy::Missing,
                },
            },
        };
        plan.insert(pane.clone(), start);
    }
    plan
}

pub(crate) fn resume_argv(command: &str) -> Option<Vec<String>> {
    let command = command.trim();
    if command.is_empty()
        || command.contains([
            '\n', '\r', '|', ';', '&', '<', '>', '$', '`', '\\', '"', '\'',
        ])
    {
        return None;
    }
    let argv: Vec<String> = command.split_whitespace().map(str::to_string).collect();
    if argv.is_empty() || argv[0].is_empty() {
        None
    } else {
        Some(argv)
    }
}

pub(crate) fn program_available(program: &str) -> bool {
    if program.is_empty() {
        return false;
    }
    let path = Path::new(program);
    if program.contains('/') {
        return path.is_file();
    }
    let Ok(paths) = std::env::var("PATH") else {
        return false;
    };
    std::env::split_paths(&paths).any(|dir| dir.join(program).is_file())
}

pub(crate) fn redact_lines(lines: &[String], secrets: &[String]) -> Vec<String> {
    let redactor = Redactor::new(secrets.iter().cloned());
    lines.iter().map(|line| redactor.redact_str(line)).collect()
}

pub(crate) fn read_history(home: &Path, layout: &str, enabled: bool) -> Result<HistoryRead> {
    if !enabled {
        return Ok(HistoryRead::Absent);
    }
    let path = home.join(HISTORY_FILE);
    if !path.exists() {
        return Ok(HistoryRead::Absent);
    }
    let bytes = std::fs::read(&path)?;
    let parsed: std::result::Result<HistoryFile, _> = serde_json::from_slice(&bytes);
    let Ok(parsed) = parsed else {
        let backup = backup_original_bytes(home, HISTORY_FILE, &bytes)?;
        return Ok(HistoryRead::Corrupt { backup });
    };
    if parsed.layout != layout {
        let _ = std::fs::remove_file(&path);
        return Ok(HistoryRead::Discarded);
    }
    Ok(HistoryRead::Replay(parsed.panes))
}

fn user_shell() -> String {
    if let Ok(shell) = std::env::var("SHELL") {
        if !shell.is_empty() && Path::new(&shell).is_file() {
            return shell;
        }
    }
    "/bin/sh".into()
}

fn terminal_panes(book: &SceneBook) -> Vec<String> {
    let mut ids = BTreeSet::new();
    for scene in book.scenes.values() {
        for tab in &scene.tabs {
            for leaf in tab.grid.leaves() {
                if let PaneContent::Terminal(id) = leaf {
                    ids.insert(id);
                }
            }
            ids.extend(tab.closed.iter().cloned());
        }
    }
    ids.into_iter().collect()
}

fn load_pane_cwds(conn: &rusqlite::Connection) -> BTreeMap<String, String> {
    let current: String = conn
        .query_row(
            "SELECT value FROM settings WHERE key = 'pane_cwd'",
            [],
            |row| row.get(0),
        )
        .unwrap_or_else(|_| "{}".into());
    serde_json::from_str(&current).unwrap_or_default()
}

fn load_resume_rows(conn: &rusqlite::Connection) -> Result<Vec<ResumeRow>> {
    let mut stmt = conn
        .prepare("SELECT terminal_id, native_session, resume_command FROM agent_native_sessions")?;
    let rows = stmt.query_map([], |row| {
        Ok(ResumeRow {
            terminal_id: row.get(0)?,
            native_session: row.get(1)?,
            resume_command: row.get(2)?,
        })
    })?;
    Ok(rows.collect::<std::result::Result<Vec<_>, _>>()?)
}

fn read_settings_file(home: &Path) -> (RestoreSettings, Vec<String>) {
    match std::fs::read_to_string(home.join("config.toml")) {
        Ok(text) => parse_restore_settings(&text),
        Err(_) => (RestoreSettings::default(), Vec::new()),
    }
}

fn load_redaction_secrets(home: &Path) -> Vec<String> {
    let Ok(text) = std::fs::read_to_string(home.join(SECRETS_FILE)) else {
        return Vec::new();
    };
    text.lines()
        .map(str::trim)
        .filter(|line| !line.is_empty() && !line.starts_with('#'))
        .map(str::to_string)
        .collect()
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct ChosenRow {
    command: String,
    session: String,
}

enum Chosen {
    None,
    Ambiguous,
    One(ChosenRow),
}

fn choose_row(pane: &str, rows: &[ResumeRow]) -> Chosen {
    let mut commands = BTreeSet::new();
    let mut session = String::new();
    let mut saw = false;
    for row in rows {
        if row.terminal_id != pane {
            continue;
        }
        let command = row.resume_command.trim();
        if command.is_empty() {
            continue;
        }
        saw = true;
        commands.insert(command.to_string());
        if session.is_empty() {
            session = row.native_session.trim().to_string();
        }
    }
    if !saw {
        Chosen::None
    } else if commands.len() != 1 {
        Chosen::Ambiguous
    } else {
        Chosen::One(ChosenRow {
            command: commands.into_iter().next().unwrap_or_default(),
            session,
        })
    }
}

#[derive(Debug, serde::Serialize, serde::Deserialize)]
struct HistoryFile {
    layout: String,
    panes: BTreeMap<String, Vec<String>>,
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let tmp_name = format!(
        ".{}.tmp",
        path.file_name()
            .and_then(|name| name.to_str())
            .unwrap_or("pane-history")
    );
    let tmp = path.with_file_name(tmp_name);
    {
        let mut options = std::fs::OpenOptions::new();
        options.create(true).write(true).truncate(true);
        #[cfg(unix)]
        {
            use std::os::unix::fs::OpenOptionsExt;
            options.mode(0o600);
        }
        let mut file = options.open(&tmp)?;
        use std::io::Write;
        file.write_all(bytes)?;
        file.sync_all()?;
    }
    std::fs::rename(&tmp, path)?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let _ = std::fs::set_permissions(path, std::fs::Permissions::from_mode(0o600));
    }
    Ok(())
}

fn parse_bool(value: &str) -> Option<bool> {
    match value.trim().trim_matches('"').trim_matches('\'') {
        "true" => Some(true),
        "false" => Some(false),
        _ => None,
    }
}

fn strip_comment(line: &str) -> &str {
    let mut quote = None;
    for (index, ch) in line.char_indices() {
        match ch {
            '"' | '\'' if quote.is_none() => quote = Some(ch),
            q if quote == Some(q) => quote = None,
            '#' if quote.is_none() => return &line[..index],
            _ => {}
        }
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    fn row(id: &str, session: &str, command: &str) -> ResumeRow {
        ResumeRow {
            terminal_id: id.into(),
            native_session: session.into(),
            resume_command: command.into(),
        }
    }

    #[test]
    fn defaults_resume_agents_and_keep_history_off() {
        let (settings, warnings) = parse_restore_settings("onboarding = false\n");
        assert!(settings.resume_agents_on_restore);
        assert!(!settings.pane_screen_history);
        assert!(warnings.is_empty());
    }

    #[test]
    fn config_flags_can_turn_resume_off_and_history_on() {
        let text = "\
resume_agents_on_restore = false
pane_screen_history = true
[keys]
prefix = \"ctrl+b\"
resume_agents_on_restore = true
";
        let (settings, warnings) = parse_restore_settings(text);
        assert!(!settings.resume_agents_on_restore);
        assert!(settings.pane_screen_history);
        assert!(warnings.is_empty());
    }

    #[test]
    fn illegal_flag_keeps_the_default() {
        let (settings, warnings) = parse_restore_settings("pane_screen_history = yes\n");
        assert!(!settings.pane_screen_history);
        assert_eq!(warnings.len(), 1);
    }

    #[test]
    fn a_unique_command_resumes_and_a_duplicate_or_missing_one_does_not() {
        let panes = vec!["a".into(), "b".into(), "c".into(), "d".into()];
        let rows = vec![
            row("a", "sess-a", "sh -c \"echo hi\""),
            row("b", "sess-b", "/no/such/viva-resume"),
            row("c", "same", "/bin/echo c"),
            row("d", "same", "/bin/echo d"),
        ];
        let plan = plan_pane_starts(&panes, &rows, true);
        assert!(matches!(
            plan["a"],
            PaneStart::Shell {
                why: ShellWhy::Missing
            }
        ));
        assert!(matches!(
            plan["b"],
            PaneStart::Shell {
                why: ShellWhy::Missing
            }
        ));
        assert!(matches!(
            plan["c"],
            PaneStart::Shell {
                why: ShellWhy::Duplicate
            }
        ));
        assert!(matches!(
            plan["d"],
            PaneStart::Shell {
                why: ShellWhy::Duplicate
            }
        ));

        let only = plan_pane_starts(&["a".into()], &[row("a", "sess-a", "/bin/echo hi")], true);
        match &only["a"] {
            PaneStart::Resume { argv } => {
                assert_eq!(argv, &vec!["/bin/echo".to_string(), "hi".into()])
            }
            other => panic!("expected resume, got {other:?}"),
        }

        let off = plan_pane_starts(&["a".into()], &[row("a", "sess-a", "/bin/echo hi")], false);
        assert!(matches!(
            off["a"],
            PaneStart::Shell {
                why: ShellWhy::Disabled
            }
        ));
    }

    #[test]
    fn two_commands_on_one_pane_are_ambiguous() {
        let panes = vec!["a".into()];
        let rows = vec![
            row("a", "s", "/bin/echo one"),
            row("a", "s", "/bin/echo two"),
        ];
        let plan = plan_pane_starts(&panes, &rows, true);
        assert!(matches!(
            plan["a"],
            PaneStart::Shell {
                why: ShellWhy::Ambiguous
            }
        ));
    }

    #[test]
    fn history_is_redacted_replayed_only_on_an_exact_layout_and_kept_when_corrupt() {
        let home = tempfile::tempdir().expect("home");
        let secrets = ["hunter2".into()];
        let lines = redact_lines(&["token is hunter2".into()], &secrets);
        assert!(!lines[0].contains("hunter2"), "{lines:?}");
        assert!(lines[0].contains("[REDACTED]"), "{lines:?}");

        let layout = "{\"version\":2}";
        let mut panes = BTreeMap::new();
        panes.insert("term-1".into(), lines);
        let body = serde_json::to_vec(&HistoryFile {
            layout: layout.into(),
            panes: panes.clone(),
        })
        .unwrap();
        write_private(&home.path().join(HISTORY_FILE), &body).unwrap();
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(home.path().join(HISTORY_FILE))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "history is private: {mode:o}");
        }

        match read_history(home.path(), layout, true).unwrap() {
            HistoryRead::Replay(got) => assert_eq!(got, panes),
            other => panic!("expected replay, got {other:?}"),
        }
        assert!(home.path().join(HISTORY_FILE).exists());

        match read_history(home.path(), "{\"version\":2,\"other\":true}", true).unwrap() {
            HistoryRead::Discarded => {}
            other => panic!("expected discard, got {other:?}"),
        }
        assert!(!home.path().join(HISTORY_FILE).exists());

        let corrupt = b"{not-json";
        std::fs::write(home.path().join(HISTORY_FILE), corrupt).unwrap();
        let backup = match read_history(home.path(), layout, true).unwrap() {
            HistoryRead::Corrupt { backup } => backup,
            other => panic!("expected corrupt, got {other:?}"),
        };
        assert_eq!(
            std::fs::read(home.path().join(HISTORY_FILE)).unwrap(),
            corrupt
        );
        assert_eq!(std::fs::read(&backup).unwrap(), corrupt);
        assert_ne!(backup, home.path().join(HISTORY_FILE));

        assert!(matches!(
            read_history(home.path(), layout, false).unwrap(),
            HistoryRead::Absent
        ));
        assert_eq!(
            std::fs::read(home.path().join(HISTORY_FILE)).unwrap(),
            corrupt
        );
    }

    #[test]
    fn a_corrupt_backup_does_not_replace_or_clear_the_current_bytes() {
        let home = tempfile::tempdir().expect("home");
        let current = home.path().join("workbench_layout");
        let original = b"{broken";
        std::fs::write(&current, original).unwrap();
        let older = home.path().join("workbench_layout.bak");
        std::fs::write(&older, b"{\"version\":2}").unwrap();

        let backup = backup_original_bytes(home.path(), "workbench_layout", original).unwrap();
        assert_eq!(std::fs::read(&current).unwrap(), original);
        assert_eq!(std::fs::read(&older).unwrap(), b"{\"version\":2}");
        assert_eq!(std::fs::read(&backup).unwrap(), original);
        assert_ne!(backup, current);
        assert_ne!(backup, older);
    }
}
