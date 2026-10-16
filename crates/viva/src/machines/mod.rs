//! Saved SSH machines for one workbench.
//!
//! The catalog in `VIVA_HOME/machines.json` stores only an id, a label, an
//! OpenSSH target, a remote session name, and whether the machine is
//! enabled. Passwords, private keys, and agent tickets are not fields and
//! are rejected if they show up in those strings. Authentication stays with
//! OpenSSH (`VIVA_SSH` overrides the client program; the default is `ssh`).
//!
//! Each machine runs its own viva server. This process never copies plugins,
//! config, or keys to it, and never installs a missing `viva`. The remote
//! command is `viva machine bridge --session <name>`, which attaches stdio
//! to that session's socket. The local default socket stays `server.sock`.

use std::io::{Read, Write};
use std::os::unix::io::AsRawFd;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::foundation::error::{Error, Result};

pub const CATALOG_FILE: &str = "machines.json";
pub const LOCAL_MACHINE_ID: &str = "local";
const CATALOG_VERSION: u32 = 1;

/// One saved machine. These five fields are the whole record.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MachineRecord {
    pub id: String,
    pub label: String,
    pub ssh_target: String,
    pub remote_session: String,
    pub enabled: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Catalog {
    version: u32,
    machines: Vec<MachineRecord>,
}

impl Default for Catalog {
    fn default() -> Self {
        Self {
            version: CATALOG_VERSION,
            machines: Vec::new(),
        }
    }
}

/// Workspaces and agents copied from one machine's own server.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct MachineProjection {
    #[serde(default)]
    pub workspaces: Vec<crate::workspaces::navigation::WorkspaceRow>,
    #[serde(default)]
    pub workspace_id: Option<String>,
    #[serde(default)]
    pub agents: Vec<crate::agents::AgentSessionRow>,
}

/// Sidebar row for one machine, including the last good snapshot.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MachineView {
    pub id: String,
    pub label: String,
    pub ssh_target: String,
    pub remote_session: String,
    pub enabled: bool,
    pub connected: bool,
    #[serde(default)]
    pub error: Option<String>,
    #[serde(default)]
    pub workspaces: Vec<crate::workspaces::navigation::WorkspaceRow>,
    #[serde(default)]
    pub agents: Vec<crate::agents::AgentSessionRow>,
    #[serde(default)]
    pub workspace_id: Option<String>,
}

/// Last snapshots. The workbench reads this; a refresh publishes them.
///
/// Refresh runs off the request thread and again from the poller. Each
/// attempt takes an epoch, and only the newest attempt (or a disconnect
/// that happened after it started) may write. An older SSH round that
/// finishes late cannot put a dead machine back online.
type RefreshWait = Arc<(Mutex<Option<Vec<MachineView>>>, Condvar)>;

#[derive(Default)]
pub struct MachineCache {
    inner: Mutex<Vec<MachineView>>,
    epoch: Mutex<u64>,
    /// Coalesce poller and `MachineRefresh` onto one SSH round.
    refresh: Mutex<Option<RefreshWait>>,
}

impl MachineCache {
    pub fn snapshot(&self) -> Vec<MachineView> {
        self.inner.lock().expect("machine cache").clone()
    }

    fn begin(&self) -> u64 {
        let mut epoch = self.epoch.lock().expect("machine epoch");
        *epoch = epoch.saturating_add(1);
        *epoch
    }

    /// Publish when `epoch` is still the latest claim. Lock order is epoch, then views.
    fn publish(&self, epoch: u64, views: Vec<MachineView>) -> bool {
        let current = self.epoch.lock().expect("machine epoch");
        if *current != epoch {
            return false;
        }
        *self.inner.lock().expect("machine cache") = views;
        true
    }

    /// Keep the last workspaces and agents, and stop accepting input.
    /// Bumps the epoch so an in-flight refresh cannot undo the refusal.
    pub fn mark_disconnected(&self, id: &str, error: String) {
        let mut epoch = self.epoch.lock().expect("machine epoch");
        *epoch = epoch.saturating_add(1);
        let mut views = self.inner.lock().expect("machine cache");
        if let Some(slot) = views.iter_mut().find(|view| view.id == id) {
            slot.connected = false;
            slot.error = Some(error);
        }
    }
}

static INVOCATION: Mutex<Option<MachineRecord>> = Mutex::new(None);

/// `--machine` for this process. Dropping the guard restores the local socket.
pub struct InvocationGuard;

impl InvocationGuard {
    pub fn arm(record: MachineRecord) -> Self {
        *INVOCATION.lock().expect("machine target") = Some(record);
        Self
    }
}

impl Drop for InvocationGuard {
    fn drop(&mut self) {
        *INVOCATION.lock().expect("machine target") = None;
    }
}

pub fn invocation_target() -> Option<MachineRecord> {
    INVOCATION.lock().expect("machine target").clone()
}

pub fn catalog_path(home: &Path) -> PathBuf {
    home.join(CATALOG_FILE)
}

pub fn load(home: &Path) -> Result<Vec<MachineRecord>> {
    let path = catalog_path(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = std::fs::read_to_string(&path)?;
    let catalog: Catalog = serde_json::from_str(&text).map_err(|err| {
        Error::Validation(format!(
            "machine catalog {} only stores id, label, ssh target, session, and enabled ({err})",
            path.display()
        ))
    })?;
    if catalog.version != CATALOG_VERSION {
        return Err(Error::Validation(format!(
            "machine catalog version {} is not supported",
            catalog.version
        )));
    }
    for record in &catalog.machines {
        reject_secret_material(record)?;
    }
    Ok(catalog.machines)
}

fn save(home: &Path, machines: &[MachineRecord]) -> Result<()> {
    crate::foundation::paths::ensure_private_dir(home)?;
    for record in machines {
        reject_secret_material(record)?;
    }
    let catalog = Catalog {
        version: CATALOG_VERSION,
        machines: machines.to_vec(),
    };
    let text = serde_json::to_string_pretty(&catalog)?;
    let path = catalog_path(home);
    let tmp = home.join(".machines.json.tmp");
    write_private(&tmp, text.as_bytes())?;
    std::fs::rename(&tmp, &path)?;
    Ok(())
}

fn write_private(path: &Path, bytes: &[u8]) -> Result<()> {
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    options.open(path)?.write_all(bytes)?;
    Ok(())
}

pub fn require(home: &Path, id: &str) -> Result<MachineRecord> {
    load(home)?
        .into_iter()
        .find(|record| record.id == id)
        .ok_or_else(|| Error::NotFound {
            entity: "machine",
            id: id.to_string(),
        })
}

pub fn add(
    home: &Path,
    id: Option<&str>,
    label: Option<&str>,
    ssh_target: &str,
    remote_session: Option<&str>,
) -> Result<MachineRecord> {
    let mut machines = load(home)?;
    let ssh_target = validate_target(ssh_target)?;
    let label = validate_label(label.unwrap_or(&ssh_target))?;
    let remote_session = validate_session(remote_session.unwrap_or("default"))?;
    let id = match id {
        Some(id) => validate_id(id)?,
        None => unique_id(&machines, &slug(&label)),
    };
    if machines.iter().any(|record| record.id == id) {
        return Err(Error::Validation(format!("machine `{id}` already exists")));
    }
    let record = MachineRecord {
        id,
        label,
        ssh_target,
        remote_session,
        enabled: true,
    };
    reject_secret_material(&record)?;
    machines.push(record.clone());
    save(home, &machines)?;
    Ok(record)
}

pub fn rename(home: &Path, id: &str, label: &str) -> Result<MachineRecord> {
    let mut machines = load(home)?;
    let label = validate_label(label)?;
    let record = machines
        .iter_mut()
        .find(|record| record.id == id)
        .ok_or_else(|| Error::NotFound {
            entity: "machine",
            id: id.to_string(),
        })?;
    record.label = label;
    reject_secret_material(record)?;
    let updated = record.clone();
    save(home, &machines)?;
    Ok(updated)
}

pub fn set_enabled(home: &Path, id: &str, enabled: bool) -> Result<MachineRecord> {
    let mut machines = load(home)?;
    let record = machines
        .iter_mut()
        .find(|record| record.id == id)
        .ok_or_else(|| Error::NotFound {
            entity: "machine",
            id: id.to_string(),
        })?;
    record.enabled = enabled;
    let updated = record.clone();
    save(home, &machines)?;
    Ok(updated)
}

pub fn remove(home: &Path, id: &str) -> Result<()> {
    let mut machines = load(home)?;
    let before = machines.len();
    machines.retain(|record| record.id != id);
    if machines.len() == before {
        return Err(Error::NotFound {
            entity: "machine",
            id: id.to_string(),
        });
    }
    save(home, &machines)?;
    Ok(())
}

/// Local machine first, then each saved machine's last snapshot.
pub fn with_local(
    workspaces: &[crate::workspaces::navigation::WorkspaceRow],
    workspace_id: Option<String>,
    agents: &[crate::agents::AgentSessionRow],
    remote: &[MachineView],
) -> Vec<MachineView> {
    let mut views = vec![MachineView {
        id: LOCAL_MACHINE_ID.into(),
        label: "local".into(),
        ssh_target: String::new(),
        remote_session: String::new(),
        enabled: true,
        connected: true,
        error: None,
        workspaces: workspaces.to_vec(),
        agents: agents.to_vec(),
        workspace_id,
    }];
    views.extend(
        remote
            .iter()
            .filter(|view| view.id != LOCAL_MACHINE_ID)
            .cloned(),
    );
    views
}

/// Refresh every saved machine. A failure keeps that machine's last snapshot
/// and does not stop the others.
pub fn refresh(home: &Path, cache: &MachineCache) -> Result<Vec<MachineView>> {
    let (leader, done) = {
        let mut slot = cache.refresh.lock().expect("machine refresh");
        if let Some(done) = slot.as_ref() {
            (false, Arc::clone(done))
        } else {
            let done = Arc::new((Mutex::new(None), Condvar::new()));
            *slot = Some(Arc::clone(&done));
            (true, done)
        }
    };

    if !leader {
        let mut guard = done.0.lock().expect("refresh wait");
        while guard.is_none() {
            guard = done.1.wait(guard).expect("refresh wait");
        }
        return Ok(guard.clone().expect("refresh finished"));
    }

    let result = refresh_inner(home, cache);
    let shared = result
        .as_ref()
        .map(|views| views.clone())
        .unwrap_or_else(|_| cache.snapshot());
    {
        let mut guard = done.0.lock().expect("refresh result");
        *guard = Some(shared);
        done.1.notify_all();
    }
    *cache.refresh.lock().expect("machine refresh") = None;
    result
}

fn refresh_inner(home: &Path, cache: &MachineCache) -> Result<Vec<MachineView>> {
    let epoch = cache.begin();
    let records = load(home)?;
    let previous = cache.snapshot();
    let mut next = Vec::with_capacity(records.len());
    for record in records {
        let prev = previous.iter().find(|view| view.id == record.id);
        next.push(refresh_one(&record, prev));
    }
    let _published = cache.publish(epoch, next.clone());
    Ok(next)
}

fn refresh_one(record: &MachineRecord, prev: Option<&MachineView>) -> MachineView {
    if !record.enabled {
        return snapshot_from(record, prev, false, Some("disabled".into()), None);
    }
    match fetch_projection(record) {
        Ok(projection) => snapshot_from(record, prev, true, None, Some(projection)),
        Err(err) => snapshot_from(record, prev, false, Some(err.to_string()), None),
    }
}

fn snapshot_from(
    record: &MachineRecord,
    prev: Option<&MachineView>,
    connected: bool,
    error: Option<String>,
    projection: Option<MachineProjection>,
) -> MachineView {
    let workspaces = projection
        .as_ref()
        .map(|projection| projection.workspaces.clone())
        .or_else(|| prev.map(|view| view.workspaces.clone()))
        .unwrap_or_default();
    let agents = projection
        .as_ref()
        .map(|projection| projection.agents.clone())
        .or_else(|| prev.map(|view| view.agents.clone()))
        .unwrap_or_default();
    let workspace_id = projection
        .and_then(|projection| projection.workspace_id)
        .or_else(|| prev.and_then(|view| view.workspace_id.clone()));
    MachineView {
        id: record.id.clone(),
        label: record.label.clone(),
        ssh_target: record.ssh_target.clone(),
        remote_session: record.remote_session.clone(),
        enabled: record.enabled,
        connected,
        error,
        workspaces,
        agents,
        workspace_id,
    }
}

pub fn fetch_projection(record: &MachineRecord) -> Result<MachineProjection> {
    let mut client = connect_client(record)?;
    let value = client.call(crate::server::RequestKind::MachineProjection)?;
    Ok(serde_json::from_value(value)?)
}

/// OpenSSH arguments. No identity file, password, or forwarded agent.
pub fn ssh_args(record: &MachineRecord) -> Vec<String> {
    vec![
        "-o".into(),
        "BatchMode=yes".into(),
        "-o".into(),
        "ConnectTimeout=5".into(),
        "-o".into(),
        "ForwardAgent=no".into(),
        "-o".into(),
        "ClearAllForwardings=yes".into(),
        "-T".into(),
        record.ssh_target.clone(),
        "viva".into(),
        "machine".into(),
        "bridge".into(),
        "--session".into(),
        record.remote_session.clone(),
    ]
}

pub fn ssh_program() -> String {
    std::env::var("VIVA_SSH")
        .ok()
        .filter(|program| !program.is_empty())
        .unwrap_or_else(|| "ssh".into())
}

/// Attach to the machine's viva server through OpenSSH. Does not spawn a
/// local server and does not change `VIVA_HOME`.
pub fn connect_client(record: &MachineRecord) -> Result<crate::server::Client> {
    if !record.enabled {
        return Err(Error::Validation(format!(
            "machine {} is disabled; input is refused",
            record.label
        )));
    }
    let program = ssh_program();
    let mut cmd = Command::new(&program);
    cmd.args(ssh_args(record)).env_remove("VIVA_HOME");
    for key in ["VIVA_FAKE_SSH_MAP", "VIVA_BRIDGE_BIN"] {
        if let Ok(value) = std::env::var(key) {
            cmd.env(key, value);
        }
    }
    let mut child = cmd
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .map_err(|err| {
            Error::Validation(format!(
                "cannot start {} for machine {} ({}): {err}",
                ssh_program(),
                record.id,
                record.ssh_target
            ))
        })?;
    let ssh_stdin = child
        .stdin
        .take()
        .ok_or_else(|| Error::Validation("ssh stdin is unavailable".into()))?;
    let ssh_stdout = child
        .stdout
        .take()
        .ok_or_else(|| Error::Validation("ssh stdout is unavailable".into()))?;
    let mut ssh_stderr = child
        .stderr
        .take()
        .ok_or_else(|| Error::Validation("ssh stderr is unavailable".into()))?;
    let (local, remote) = std::os::unix::net::UnixStream::pair()?;
    std::thread::spawn(move || relay_ssh_stdio(remote, ssh_stdin, ssh_stdout));
    let stderr_text = Arc::new(Mutex::new(String::new()));
    let stderr_slot = Arc::clone(&stderr_text);
    std::thread::spawn(move || {
        let mut buf = String::new();
        let _ = ssh_stderr.read_to_string(&mut buf);
        *stderr_slot.lock().expect("ssh stderr") = buf;
    });
    let home = crate::foundation::paths::viva_home(None);
    match crate::server::Client::from_stream(local, &home) {
        Ok(client) => {
            std::thread::spawn(move || {
                let _ = child.wait();
            });
            Ok(client)
        }
        Err(err) => {
            let _ = child.kill();
            let status = child.wait().ok();
            std::thread::sleep(Duration::from_millis(30));
            let stderr = stderr_text.lock().expect("ssh stderr").clone();
            Err(Error::Validation(explain_failure(
                record,
                status.and_then(|status| status.code()),
                &stderr,
                &err,
            )))
        }
    }
}

pub fn send_via_bridge(
    record: &MachineRecord,
    request: crate::server::Request,
) -> Result<crate::server::Response> {
    let mut client = connect_client(record)?;
    client.round_trip_raw(&request)
}

/// Session name → that machine's `VIVA_HOME`. `default` is `~/.viva`.
/// An absolute path is used as given so a fixture can point at another home
/// on this computer. Ambient `VIVA_HOME` is not consulted.
pub fn session_home(session: &str) -> Result<PathBuf> {
    if session.is_empty() || session == "default" {
        return Ok(user_home()?.join(".viva"));
    }
    if session.starts_with('/') {
        let path = PathBuf::from(session);
        if path
            .components()
            .any(|component| matches!(component, Component::ParentDir))
        {
            return Err(Error::Validation(
                "remote session path cannot contain ..".into(),
            ));
        }
        return Ok(path);
    }
    if session.contains('/') || session.contains('\\') || session.contains("..") {
        return Err(Error::Validation(
            "remote session name must be a single directory name or an absolute path".into(),
        ));
    }
    Ok(user_home()?.join(session))
}

pub fn open_session_socket(session: &str) -> Result<std::os::unix::net::UnixStream> {
    let home = session_home(session)?;
    let sock = home.join(crate::server::SOCKET_NAME);
    std::os::unix::net::UnixStream::connect(&sock).map_err(|err| {
        Error::Validation(format!(
            "no viva server for session `{session}` at {} ({err}). Start it there with `viva server`; Viva does not install a remote copy",
            sock.display()
        ))
    })
}

/// Stdio proxy used by `ssh host viva machine bridge --session <name>`.
///
/// Stdout is block-buffered when ssh is the consumer (a pipe, not a tty).
/// A response has to be flushed per write or the client waits until the
/// buffer fills.
pub fn run_bridge(session: &str) -> Result<()> {
    let mut stream = open_session_socket(session)?;
    let mut inbound = stream.try_clone()?;
    let writer = std::thread::spawn(move || {
        let mut stdin = std::io::stdin();
        let mut buf = [0u8; 8192];
        loop {
            let n = match stdin.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => n,
            };
            if inbound.write_all(&buf[..n]).is_err() {
                break;
            }
        }
        let _ = inbound.shutdown(std::net::Shutdown::Write);
    });
    let mut stdout = std::io::stdout();
    let mut buf = [0u8; 8192];
    loop {
        let n = match stream.read(&mut buf) {
            Ok(0) | Err(_) => break,
            Ok(n) => n,
        };
        if stdout.write_all(&buf[..n]).is_err() || stdout.flush().is_err() {
            break;
        }
    }
    let _ = writer.join();
    Ok(())
}

/// Multiplex one socket leg and ssh stdio without two `copy` threads on the
/// same socket end (Linux would let the outbound reader steal responses).
fn relay_ssh_stdio(
    mut socket: std::os::unix::net::UnixStream,
    mut ssh_stdin: impl Write + Send + 'static,
    mut ssh_stdout: impl Read + AsRawFd + Send + 'static,
) {
    std::thread::spawn(move || {
        let socket_fd = socket.as_raw_fd();
        let ssh_fd = ssh_stdout.as_raw_fd();
        let mut buf = [0u8; 8192];
        loop {
            let mut fds = [
                libc::pollfd {
                    fd: socket_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
                libc::pollfd {
                    fd: ssh_fd,
                    events: libc::POLLIN,
                    revents: 0,
                },
            ];
            let ready = unsafe { libc::poll(fds.as_mut_ptr(), 2, -1) };
            if ready <= 0 {
                break;
            }
            if fds[0].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                match socket.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if ssh_stdin.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
            if fds[1].revents & (libc::POLLIN | libc::POLLHUP | libc::POLLERR) != 0 {
                match ssh_stdout.read(&mut buf) {
                    Ok(0) | Err(_) => break,
                    Ok(n) => {
                        if socket.write_all(&buf[..n]).is_err() {
                            break;
                        }
                    }
                }
            }
        }
        let _ = socket.shutdown(std::net::Shutdown::Write);
    });
}

fn explain_failure(record: &MachineRecord, code: Option<i32>, stderr: &str, err: &Error) -> String {
    let stderr = stderr.trim();
    let missing =
        code == Some(127) || stderr.contains("command not found") || stderr.contains("not found");
    if missing {
        format!(
            "remote machine {} ({}) is missing the viva command; Viva does not install it or copy local plugins, config, or keys",
            record.id, record.ssh_target
        )
    } else if stderr.is_empty() {
        format!(
            "cannot reach viva on {} ({}): {err}",
            record.label, record.ssh_target
        )
    } else {
        format!(
            "cannot reach viva on {} ({}): {stderr}",
            record.label, record.ssh_target
        )
    }
}

fn user_home() -> Result<PathBuf> {
    std::env::var_os("HOME")
        .filter(|home| !home.is_empty())
        .map(PathBuf::from)
        .ok_or_else(|| Error::Validation("HOME is not set; cannot resolve a remote session".into()))
}

fn validate_id(id: &str) -> Result<String> {
    let id = id.trim();
    if id == LOCAL_MACHINE_ID {
        return Err(Error::Validation(
            "machine id `local` is reserved for this computer".into(),
        ));
    }
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return Err(Error::Validation("machine id cannot be empty".into()));
    };
    if !first.is_ascii_alphanumeric()
        || id.len() > 64
        || !id
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || c == '-' || c == '_')
    {
        return Err(Error::Validation(format!(
            "machine id `{id}` must be letters, digits, '-' and '_'"
        )));
    }
    Ok(id.to_string())
}

fn validate_label(label: &str) -> Result<String> {
    let label = label.trim();
    if label.is_empty() || label.len() > 64 || label.chars().any(|c| c.is_control()) {
        return Err(Error::Validation(
            "machine label must be 1–64 characters without control characters".into(),
        ));
    }
    Ok(label.to_string())
}

fn validate_target(target: &str) -> Result<String> {
    let target = target.trim();
    if target.is_empty()
        || target.len() > 256
        || target.chars().any(|c| c.is_whitespace() || c.is_control())
        || target.contains("://")
    {
        return Err(Error::Validation(
            "ssh target must be a host or user@host with no spaces".into(),
        ));
    }
    Ok(target.to_string())
}

fn validate_session(session: &str) -> Result<String> {
    let session = session.trim();
    if session.is_empty() {
        return Ok("default".into());
    }
    session_home(session)?;
    Ok(session.to_string())
}

fn slug(source: &str) -> String {
    let mut out = String::new();
    for ch in source.chars() {
        if ch.is_ascii_alphanumeric() {
            out.push(ch.to_ascii_lowercase());
        } else if !out.is_empty() && !out.ends_with('-') {
            out.push('-');
        }
        if out.len() >= 32 {
            break;
        }
    }
    let out = out.trim_matches('-').to_string();
    if out.is_empty() || out == LOCAL_MACHINE_ID {
        "machine".into()
    } else {
        out
    }
}

fn unique_id(machines: &[MachineRecord], base: &str) -> String {
    if !machines.iter().any(|record| record.id == base) {
        return base.to_string();
    }
    for n in 2..1000 {
        let candidate = format!("{base}-{n}");
        if !machines.iter().any(|record| record.id == candidate) {
            return candidate;
        }
    }
    format!("{base}-{}", uuid::Uuid::new_v4().simple())
}

fn reject_secret_material(record: &MachineRecord) -> Result<()> {
    for (field, value) in [
        ("id", record.id.as_str()),
        ("label", record.label.as_str()),
        ("ssh_target", record.ssh_target.as_str()),
        ("remote_session", record.remote_session.as_str()),
    ] {
        if contains_secret(value) {
            return Err(Error::Validation(format!(
                "machine {field} cannot hold a password, private key, or agent ticket"
            )));
        }
    }
    Ok(())
}

fn contains_secret(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    [
        "private key",
        "begin ",
        "ssh-ed25519",
        "ssh-rsa",
        "password",
        "passphrase",
        "agent ticket",
        "ssh-agent",
    ]
    .iter()
    .any(|marker| lower.contains(marker))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::server::{Host, RequestKind, new_request, round_trip};
    use std::os::unix::fs::PermissionsExt;
    use std::time::Instant;

    #[test]
    fn catalog_roundtrip_keeps_only_the_five_fields() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let added = add(home, Some("lab"), Some("Lab"), "me@lab", Some("default")).unwrap();
        assert_eq!(added.remote_session, "default");
        assert!(added.enabled);
        rename(home, "lab", "Lab Mac").unwrap();
        set_enabled(home, "lab", false).unwrap();
        set_enabled(home, "lab", true).unwrap();
        let listed = load(home).unwrap();
        assert_eq!(listed.len(), 1);
        assert_eq!(listed[0].label, "Lab Mac");
        remove(home, "lab").unwrap();
        assert!(load(home).unwrap().is_empty());

        add(home, Some("other"), Some("Other"), "other-host", None).unwrap();
        let text = std::fs::read_to_string(catalog_path(home)).unwrap();
        for forbidden in [
            "password",
            "PRIVATE",
            "ticket",
            "ssh-ed25519",
            "ssh-rsa",
            "BEGIN ",
            "IdentityFile",
        ] {
            assert!(!text.contains(forbidden), "{text}");
        }
        let value: serde_json::Value = serde_json::from_str(&text).unwrap();
        let mut keys = Vec::new();
        collect_keys(&value, &mut keys);
        let allowed = [
            "version",
            "machines",
            "id",
            "label",
            "ssh_target",
            "remote_session",
            "enabled",
        ];
        for key in keys {
            assert!(
                allowed.contains(&key.as_str()),
                "unexpected catalog key {key}"
            );
        }
        let mode = std::fs::metadata(catalog_path(home))
            .unwrap()
            .permissions()
            .mode()
            & 0o777;
        assert_eq!(mode, 0o600, "catalog should be owner-read/write only");
    }

    #[test]
    fn secret_material_is_rejected_and_not_written() {
        let dir = tempfile::tempdir().unwrap();
        let home = dir.path();
        let err = add(
            home,
            Some("lab"),
            Some("BEGIN OPENSSH PRIVATE KEY"),
            "me@lab",
            None,
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("private key") || err.contains("password") || err.contains("ticket"),
            "{err}"
        );
        assert!(!catalog_path(home).exists());
        let err = add(
            home,
            Some("lab"),
            Some("Lab"),
            "me@lab",
            Some("password=hunter2"),
        )
        .unwrap_err()
        .to_string();
        assert!(
            err.contains("password") || err.contains("ticket") || err.contains("private"),
            "{err}"
        );
    }

    #[test]
    fn ssh_command_carries_no_key_material() {
        let record = MachineRecord {
            id: "lab".into(),
            label: "Lab".into(),
            ssh_target: "me@lab".into(),
            remote_session: "default".into(),
            enabled: true,
        };
        let args = ssh_args(&record);
        let joined = args.join(" ");
        assert!(joined.contains("viva machine bridge --session default"));
        assert!(joined.contains("ForwardAgent=no"));
        assert!(joined.contains("BatchMode=yes"));
        assert!(args.iter().any(|arg| arg == "me@lab"));
        for forbidden in ["IdentityFile", "password", "SendEnv", "PRIVATE", "-A", "-i"] {
            assert!(
                !args
                    .iter()
                    .any(|arg| arg == forbidden || arg.contains("Identity")),
                "{joined}"
            );
        }
    }

    #[test]
    fn session_socket_follows_the_named_home_not_ambient_viva_home() {
        let dir = tempfile::tempdir().unwrap();
        let home_a = dir.path().join("a");
        let home_b = dir.path().join("b");
        let host_a = Host::open(&home_a).unwrap();
        let host_b = Host::open(&home_b).unwrap();
        let id_a = host_a.shared().host_id.clone();
        let id_b = host_b.shared().host_id.clone();
        let _server_a = host_a.serve_background();
        let _server_b = host_b.serve_background();
        wait_socket(&home_b);
        let session = home_b.to_str().unwrap();
        let mut stream = open_session_socket(session).unwrap();
        let response = round_trip(&mut stream, &new_request(RequestKind::Ping)).unwrap();
        let host = response.result.unwrap()["host_id"]
            .as_str()
            .unwrap()
            .to_string();
        assert_eq!(host, id_b);
        assert_ne!(host, id_a);
        let _ = crate::server::Client::connect(&home_a)
            .unwrap()
            .call(RequestKind::Shutdown { close_policy: None });
        let _ = crate::server::Client::connect(&home_b)
            .unwrap()
            .call(RequestKind::Shutdown { close_policy: None });
    }

    fn wait_socket(home: &Path) {
        let socket = home.join(crate::server::SOCKET_NAME);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !socket.exists() && Instant::now() < deadline {
            std::thread::sleep(Duration::from_millis(20));
        }
        assert!(socket.exists(), "socket {}", socket.display());
    }

    fn collect_keys(value: &serde_json::Value, keys: &mut Vec<String>) {
        match value {
            serde_json::Value::Object(map) => {
                for (key, child) in map {
                    keys.push(key.clone());
                    collect_keys(child, keys);
                }
            }
            serde_json::Value::Array(items) => {
                for child in items {
                    collect_keys(child, keys);
                }
            }
            _ => {}
        }
    }
}
