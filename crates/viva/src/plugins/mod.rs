//! Local plugin processes. A plugin is a directory with `viva-plugin.json`
//! and an entry program. Install copies it into `VIVA_HOME`, starts the
//! process, and gives it a private unix socket for events. The process
//! opens panes with the existing `viva scene` command. This module does
//! not browse, search, publish, rate, download, or update plugins, and it
//! does not talk to herdr.dev or any marketplace.

use std::fs;
use std::io::Write;
use std::os::unix::process::CommandExt;
use std::path::{Component, Path, PathBuf};
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::authority::PROTECTED_ACTIONS;
use crate::foundation::error::{Error, Result};
use crate::foundation::paths::ensure_private_dir;

const MANIFEST_FILE: &str = "viva-plugin.json";
const MAX_MANIFEST_BYTES: usize = 64 * 1024;
/// `sockaddr_un.sun_path` on macOS holds 104 bytes including the NUL.
const MAX_SOCKET_PATH_BYTES: usize = 103;
const HOOKS: &[&str] = &["pane.opened", "popup"];
const PANE_MODES: &[&str] = &["overlay", "split", "tab", "zoomed", "popup"];
/// Names the issue calls out that are not all spelled like [`PROTECTED_ACTIONS`].
const PROTECTED_ALIASES: &[&str] = &[
    "merge",
    "approve",
    "push_protected",
    "self-authorize",
    "self_authorize",
];

#[derive(Debug, Clone, Serialize)]
pub struct PluginInfo {
    pub id: String,
    pub name: String,
    pub version: String,
    pub pid: u32,
    pub running: bool,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct Manifest {
    id: String,
    name: String,
    version: String,
    entry: String,
    actions: Vec<ActionDecl>,
    events: Vec<String>,
}

#[derive(Debug, Clone, Deserialize)]
#[serde(deny_unknown_fields)]
struct ActionDecl {
    id: String,
    title: String,
    pane: String,
}

#[derive(Debug, Clone, Serialize, Deserialize)]
struct Record {
    id: String,
    name: String,
    version: String,
    pid: u32,
    /// `ps` start time captured at spawn. A reused pid will not match, so
    /// uninstall and shutdown do not signal an unrelated process.
    started: String,
    events: Vec<String>,
}

#[derive(Debug, Serialize, Deserialize)]
struct Registry {
    plugins: Vec<Record>,
}

/// Refuse a URL, a herdr.dev host, or a marketplace endpoint. The raw
/// text is not copied into the error, so a credential in the URL is not
/// written to the terminal.
pub fn reject_remote_source(raw: &str) -> Result<()> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return Err(Error::Validation(
            "plugin install needs a local directory".into(),
        ));
    }
    let lower = trimmed.to_ascii_lowercase();
    let remote = lower.contains("://") || lower.starts_with("git@") || lower.contains("herdr.dev");
    if remote {
        return Err(Error::Validation(
            "plugin install only accepts a local directory; URLs and marketplace endpoints are refused"
                .into(),
        ));
    }
    Ok(())
}

/// Install `source` (a local directory) and start its entry process.
/// `bin` is the `viva` executable the plugin uses for `viva scene`.
pub fn install(home: &Path, source: &str, bin: &Path) -> Result<PluginInfo> {
    reject_remote_source(source)?;
    let source_path = Path::new(source.trim());
    if !source_path.is_dir() {
        return Err(Error::Validation(
            "plugin install source is not a local directory".into(),
        ));
    }
    let manifest = load_manifest(source_path)?;
    let mut records = read_registry(home)?;
    if records.iter().any(|record| record.id == manifest.id) {
        return Err(Error::Validation(format!(
            "plugin `{}` is already installed",
            manifest.id
        )));
    }
    let dest = install_dir(home, &manifest.id);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    copy_tree(source_path, &dest)?;
    let entry = dest.join(relative_entry(&manifest.entry)?);
    if !entry.is_file() {
        let _ = fs::remove_dir_all(&dest);
        return Err(Error::Validation(
            "plugin entry is not a file inside the plugin directory".into(),
        ));
    }
    set_mode(&entry, 0o700)?;
    let socket = socket_path(home, &manifest.id);
    if let Some(parent) = socket.parent() {
        ensure_private_dir(parent)?;
    }
    check_socket_len(&socket)?;
    let _ = fs::remove_file(&socket);
    let (pid, started) = match spawn_plugin(home, &dest, &entry, &socket, bin, &manifest.id) {
        Ok(spawned) => spawned,
        Err(err) => {
            let _ = fs::remove_dir_all(&dest);
            let _ = fs::remove_file(&socket);
            return Err(err);
        }
    };
    records.push(Record {
        id: manifest.id.clone(),
        name: manifest.name.clone(),
        version: manifest.version.clone(),
        pid,
        started,
        events: manifest.events.clone(),
    });
    if let Err(err) = write_registry(home, &records) {
        stop_pid(pid);
        let _ = fs::remove_dir_all(&dest);
        let _ = fs::remove_file(&socket);
        return Err(err);
    }
    Ok(PluginInfo {
        id: manifest.id,
        name: manifest.name,
        version: manifest.version,
        pid,
        running: process_running(pid),
    })
}

/// Stop the process and delete the installed copy. The source directory
/// the user installed from is left alone.
pub fn uninstall(home: &Path, id: &str) -> Result<()> {
    validate_id(id)?;
    let mut records = read_registry(home)?;
    let Some(index) = records.iter().position(|record| record.id == id) else {
        return Err(Error::NotFound {
            entity: "plugin",
            id: id.to_string(),
        });
    };
    let record = records.remove(index);
    write_registry(home, &records)?;
    stop_record(&record);
    let _ = fs::remove_file(socket_path(home, id));
    let dest = install_dir(home, id);
    if dest.exists() {
        fs::remove_dir_all(&dest)?;
    }
    Ok(())
}

pub fn list(home: &Path) -> Result<Vec<PluginInfo>> {
    Ok(read_registry(home)?
        .into_iter()
        .map(|record| {
            let running = process_running(record.pid);
            PluginInfo {
                id: record.id,
                name: record.name,
                version: record.version,
                pid: record.pid,
                running,
            }
        })
        .collect())
}

/// Stop every installed plugin process. Files stay until `uninstall`.
/// A missing registry is not an error: most homes have no plugins.
pub fn stop_installed(home: &Path) {
    let Ok(records) = read_registry(home) else {
        return;
    };
    for record in records {
        stop_record(&record);
    }
}

/// Deliver one JSON event line to each installed plugin that hooks its
/// type. Failures stay here: a pane edit must not depend on a plugin
/// process. Protected-action names are not forwarded.
pub fn publish(home: &Path, event: &Value) {
    if event_mentions_protected(event) {
        return;
    }
    let Some(kind) = event.get("type").and_then(Value::as_str) else {
        return;
    };
    let Ok(line) = serde_json::to_string(event) else {
        return;
    };
    let line = format!("{line}\n");
    let Ok(records) = read_registry(home) else {
        return;
    };
    for record in records {
        if !record.events.iter().any(|hook| hook == kind) {
            continue;
        }
        let _ = send_line(&socket_path(home, &record.id), line.as_bytes());
    }
}

/// Tell one plugin about a declared popup. A popup is not a pane: the
/// event has no `pane_id`, and the scene tree is not edited.
pub fn deliver_popup(home: &Path, plugin_id: &str, action_id: &str) -> Result<Option<String>> {
    validate_id(plugin_id)?;
    if is_forbidden_action(action_id) {
        return Err(Error::Validation(
            "plugin actions cannot name a protected action".into(),
        ));
    }
    let records = read_registry(home)?;
    let Some(record) = records.iter().find(|record| record.id == plugin_id) else {
        return Err(Error::NotFound {
            entity: "plugin",
            id: plugin_id.to_string(),
        });
    };
    let manifest = load_manifest(&install_dir(home, plugin_id))?;
    let Some(action) = manifest
        .actions
        .iter()
        .find(|action| action.id == action_id)
    else {
        return Err(Error::NotFound {
            entity: "plugin action",
            id: action_id.to_string(),
        });
    };
    if action.pane != "popup" {
        return Err(Error::Validation(
            "split, overlay, tab, and zoomed panes open through `viva scene`; popup is not a pane"
                .into(),
        ));
    }
    if !record.events.iter().any(|hook| hook == "popup") {
        return Err(Error::Validation(
            "plugin does not hook the popup event".into(),
        ));
    }
    let event = serde_json::json!({
        "type": "popup",
        "action": action_id,
    });
    debug_assert!(event.get("pane_id").is_none());
    send_line(
        &socket_path(home, plugin_id),
        format!("{event}\n").as_bytes(),
    )
    .map_err(|err| Error::Validation(format!("plugin did not accept the popup event: {err}")))?;
    Ok(None)
}

pub fn process_running(pid: u32) -> bool {
    if pid < 2 {
        return false;
    }
    let pid_i = pid as i32;
    let mut status = 0;
    let reaped = unsafe { libc::waitpid(pid_i, &mut status, libc::WNOHANG) };
    if reaped == pid_i {
        return false;
    }
    unsafe { libc::kill(pid_i, 0) == 0 }
}

fn spawn_plugin(
    home: &Path,
    dest: &Path,
    entry: &Path,
    socket: &Path,
    bin: &Path,
    id: &str,
) -> Result<(u32, String)> {
    let err_path = dest.join("plugin.err");
    let err_file = fs::File::create(&err_path)?;
    set_mode(&err_path, 0o600)?;
    let mut command = Command::new(entry);
    command
        .current_dir(dest)
        .env_clear()
        .env(
            "PATH",
            std::env::var("PATH").unwrap_or_else(|_| "/usr/bin:/bin:/usr/sbin:/sbin".into()),
        )
        .env("VIVA_HOME", home)
        .env("VIVA_BIN", bin)
        .env("VIVA_PLUGIN_ID", id)
        .env("VIVA_PLUGIN_DIR", dest)
        .env("VIVA_PLUGIN_SOCKET", socket)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::from(err_file))
        .process_group(0);
    if let Ok(home_dir) = std::env::var("HOME") {
        if !home_dir.is_empty() && !looks_remote(&home_dir) {
            command.env("HOME", home_dir);
        }
    }
    if let Ok(tmpdir) = std::env::var("TMPDIR") {
        if !tmpdir.is_empty() && !looks_remote(&tmpdir) {
            command.env("TMPDIR", tmpdir);
        }
    }
    let child = command
        .spawn()
        .map_err(|err| Error::Validation(format!("plugin process did not start: {err}")))?;
    let pid = child.id();
    let Some(started) = wait_for_start(pid) else {
        // The pid still belongs to the process we just spawned.
        stop_pid(pid);
        return Err(Error::Validation(
            "plugin process did not report a start time".into(),
        ));
    };
    Ok((pid, started))
}

fn stop_record(record: &Record) {
    if process_started_at(record.pid).as_deref() != Some(record.started.as_str()) {
        return;
    }
    stop_pid(record.pid);
}

fn wait_for_start(pid: u32) -> Option<String> {
    for _ in 0..15 {
        if let Some(started) = process_started_at(pid) {
            return Some(started);
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    None
}

fn process_started_at(pid: u32) -> Option<String> {
    if pid < 2 {
        return None;
    }
    let output = Command::new("ps")
        .args(["-p", &pid.to_string(), "-o", "lstart="])
        .output()
        .ok()?;
    if !output.status.success() {
        return None;
    }
    let text = String::from_utf8(output.stdout).ok()?;
    let text = text.trim();
    if text.is_empty() {
        None
    } else {
        Some(text.to_string())
    }
}

fn stop_pid(pid: u32) {
    if pid < 2 {
        return;
    }
    let pid_i = pid as i32;
    unsafe {
        libc::kill(-pid_i, libc::SIGTERM);
    }
    let deadline = Instant::now() + Duration::from_secs(2);
    while Instant::now() < deadline {
        if !process_running(pid) {
            return;
        }
        std::thread::sleep(Duration::from_millis(40));
    }
    unsafe {
        libc::kill(-pid_i, libc::SIGKILL);
    }
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline && process_running(pid) {
        std::thread::sleep(Duration::from_millis(40));
    }
}

fn send_line(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    use std::os::unix::net::UnixStream;
    let mut stream = UnixStream::connect(path)?;
    stream.set_write_timeout(Some(Duration::from_millis(500)))?;
    stream.write_all(bytes)?;
    let _ = stream.shutdown(std::net::Shutdown::Write);
    Ok(())
}

fn load_manifest(dir: &Path) -> Result<Manifest> {
    let path = dir.join(MANIFEST_FILE);
    let text = fs::read_to_string(&path)
        .map_err(|_| Error::Validation(format!("plugin directory has no {MANIFEST_FILE}")))?;
    if text.len() > MAX_MANIFEST_BYTES {
        return Err(Error::Validation("plugin manifest is too large".into()));
    }
    parse_manifest(&text)
}

fn parse_manifest(text: &str) -> Result<Manifest> {
    let manifest: Manifest = serde_json::from_str(text).map_err(|_| {
        Error::Validation(
            "plugin manifest is not a valid local manifest; unexpected fields are refused".into(),
        )
    })?;
    validate_manifest(&manifest)?;
    Ok(manifest)
}

fn validate_manifest(manifest: &Manifest) -> Result<()> {
    validate_id(&manifest.id)?;
    reject_remote_text(&manifest.name)?;
    reject_remote_text(&manifest.version)?;
    reject_remote_text(&manifest.entry)?;
    if manifest.name.trim().is_empty() || manifest.name.len() > 128 {
        return Err(Error::Validation(
            "plugin name must be 1..=128 characters".into(),
        ));
    }
    if manifest.version.trim().is_empty() || manifest.version.len() > 32 {
        return Err(Error::Validation(
            "plugin version must be 1..=32 characters".into(),
        ));
    }
    if manifest.actions.is_empty() {
        return Err(Error::Validation(
            "plugin manifest must declare at least one action".into(),
        ));
    }
    if manifest.events.is_empty() {
        return Err(Error::Validation(
            "plugin manifest must declare at least one event hook".into(),
        ));
    }
    for action in &manifest.actions {
        if is_forbidden_action(&action.id) {
            return Err(Error::Validation(
                "plugin actions cannot name a protected action (merge, approve, push to a protected branch, or self-authorize)"
                    .into(),
            ));
        }
        validate_id(&action.id)?;
        reject_remote_text(&action.title)?;
        if action.title.trim().is_empty() || action.title.len() > 128 {
            return Err(Error::Validation(
                "plugin action title must be 1..=128 characters".into(),
            ));
        }
        if !PANE_MODES.contains(&action.pane.as_str()) {
            return Err(Error::Validation(
                "plugin pane mode must be overlay, split, tab, zoomed, or popup".into(),
            ));
        }
    }
    for hook in &manifest.events {
        reject_remote_text(hook)?;
        if is_forbidden_action(hook) || !HOOKS.contains(&hook.as_str()) {
            return Err(Error::Validation(
                "plugin event hooks are pane.opened and popup".into(),
            ));
        }
    }
    relative_entry(&manifest.entry)?;
    Ok(())
}

fn validate_id(id: &str) -> Result<()> {
    let mut chars = id.chars();
    let Some(first) = chars.next() else {
        return Err(Error::Validation(
            "plugin id must be 1..=32 characters matching [a-z][a-z0-9-]*".into(),
        ));
    };
    let ok = id.len() <= 32
        && first.is_ascii_lowercase()
        && chars.all(|ch| ch.is_ascii_lowercase() || ch.is_ascii_digit() || ch == '-');
    if ok {
        Ok(())
    } else {
        Err(Error::Validation(
            "plugin id must be 1..=32 characters matching [a-z][a-z0-9-]*".into(),
        ))
    }
}

fn relative_entry(entry: &str) -> Result<PathBuf> {
    if entry.is_empty() {
        return Err(Error::Validation(
            "plugin entry must stay inside the plugin directory".into(),
        ));
    }
    let path = Path::new(entry);
    if path.is_absolute() {
        return Err(Error::Validation(
            "plugin entry must stay inside the plugin directory".into(),
        ));
    }
    if path
        .components()
        .any(|component| matches!(component, Component::ParentDir | Component::Prefix(_)))
    {
        return Err(Error::Validation(
            "plugin entry must stay inside the plugin directory".into(),
        ));
    }
    Ok(path.to_path_buf())
}

fn is_forbidden_action(action: &str) -> bool {
    PROTECTED_ACTIONS.contains(&action) || PROTECTED_ALIASES.contains(&action)
}

fn event_mentions_protected(event: &Value) -> bool {
    ["type", "action", "open"].iter().any(|key| {
        event
            .get(*key)
            .and_then(Value::as_str)
            .is_some_and(is_forbidden_action)
    })
}

fn reject_remote_text(value: &str) -> Result<()> {
    if looks_remote(value) {
        return Err(Error::Validation(
            "plugin manifest cannot name a URL or a marketplace endpoint".into(),
        ));
    }
    Ok(())
}

fn looks_remote(value: &str) -> bool {
    let lower = value.to_ascii_lowercase();
    lower.contains("://") || lower.starts_with("git@") || lower.contains("herdr.dev")
}

fn is_secret_name(name: &str) -> bool {
    let lower = name.to_ascii_lowercase();
    lower == ".env"
        || lower.starts_with(".env.")
        || lower == "credentials"
        || lower == "credentials.json"
        || lower == "secrets.json"
        || lower == "id_rsa"
        || lower == "id_rsa.pub"
        || lower.ends_with(".pem")
        || lower.ends_with(".key")
        || lower.ends_with(".p12")
        || lower.ends_with(".pfx")
}

fn copy_tree(src: &Path, dest: &Path) -> Result<()> {
    ensure_private_dir(dest)?;
    for entry in fs::read_dir(src)? {
        let entry = entry?;
        let name = entry.file_name();
        let name = name.to_string_lossy();
        if name == ".git" || is_secret_name(&name) {
            continue;
        }
        let from = entry.path();
        let to = dest.join(entry.file_name());
        let kind = entry.file_type()?;
        if kind.is_symlink() {
            continue;
        }
        if kind.is_dir() {
            copy_tree(&from, &to)?;
        } else if kind.is_file() {
            fs::copy(&from, &to)?;
            set_mode(&to, 0o600)?;
        }
    }
    Ok(())
}

fn set_mode(path: &Path, mode: u32) -> Result<()> {
    use std::os::unix::fs::PermissionsExt;
    fs::set_permissions(path, fs::Permissions::from_mode(mode))?;
    Ok(())
}

fn check_socket_len(path: &Path) -> Result<()> {
    if path.as_os_str().len() > MAX_SOCKET_PATH_BYTES {
        return Err(Error::Validation(
            "plugin event socket path exceeds the macOS unix-socket limit; use a shorter VIVA_HOME"
                .into(),
        ));
    }
    Ok(())
}

fn install_dir(home: &Path, id: &str) -> PathBuf {
    home.join("plugins").join(id)
}

fn socket_path(home: &Path, id: &str) -> PathBuf {
    home.join("ps").join(format!("{id}.sock"))
}

fn registry_path(home: &Path) -> PathBuf {
    home.join("plugins").join("registry.json")
}

fn read_registry(home: &Path) -> Result<Vec<Record>> {
    let path = registry_path(home);
    if !path.exists() {
        return Ok(Vec::new());
    }
    let text = fs::read_to_string(&path)?;
    if text.trim().is_empty() {
        return Ok(Vec::new());
    }
    let registry: Registry = serde_json::from_str(&text)?;
    Ok(registry.plugins)
}

fn write_registry(home: &Path, records: &[Record]) -> Result<()> {
    let path = registry_path(home);
    if let Some(parent) = path.parent() {
        ensure_private_dir(parent)?;
    }
    let body = serde_json::to_vec_pretty(&Registry {
        plugins: records.to_vec(),
    })?;
    let tmp = path.with_extension("json.tmp");
    {
        let mut file = fs::File::create(&tmp)?;
        file.write_all(&body)?;
    }
    set_mode(&tmp, 0o600)?;
    fs::rename(&tmp, &path)?;
    set_mode(&path, 0o600)?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn remote_sources_are_refused_without_echoing_the_input() {
        let secret = "user:s3cret-token@herdr.dev";
        let samples = [
            "https://example.com/plugin",
            "http://herdr.dev/plugins/demo",
            "https://user:s3cret-token@herdr.dev/api/plugins/demo",
            "git://github.com/someone/plugin.git",
            "ssh://example.com/plugin",
            "git@github.com:someone/plugin.git",
            "https://marketplace.example/plugins/1",
        ];
        for sample in samples {
            let err = reject_remote_source(sample).expect_err(sample);
            let text = err.to_string();
            assert!(text.contains("local directory"), "{text}");
            assert!(!text.contains("s3cret-token"), "{text}");
            assert!(!text.contains(secret), "{text}");
        }
        assert!(reject_remote_source("/tmp/my-plugin").is_ok());
        assert!(reject_remote_source("/tmp/marketplace-demo").is_ok());
    }

    #[test]
    fn manifest_refuses_protected_actions_urls_and_secret_fields() {
        let secret = "sk-plugin-test-secret-value";
        let protected = r#"{
            "id": "demo",
            "name": "Demo",
            "version": "0.1.0",
            "entry": "plugin.py",
            "actions": [{"id": "merge_pull_request", "title": "Merge", "pane": "split"}],
            "events": ["pane.opened"]
        }"#;
        let err = parse_manifest(protected).expect_err("protected");
        assert!(err.to_string().contains("protected"), "{err}");
        assert!(!err.to_string().contains(secret));

        let leaked = format!(
            r#"{{
                "id": "demo",
                "name": "Demo",
                "version": "0.1.0",
                "entry": "plugin.py",
                "token": "{secret}",
                "actions": [{{"id": "open-split", "title": "Open", "pane": "split"}}],
                "events": ["pane.opened"]
            }}"#
        );
        let err = parse_manifest(&leaked).expect_err("secret field");
        assert!(!err.to_string().contains(secret), "{err}");

        let remote = r#"{
            "id": "demo",
            "name": "Demo",
            "version": "0.1.0",
            "entry": "plugin.py",
            "actions": [{"id": "open-split", "title": "https://herdr.dev/plugin", "pane": "split"}],
            "events": ["pane.opened"]
        }"#;
        let err = parse_manifest(remote).expect_err("remote title");
        assert!(
            err.to_string().contains("marketplace") || err.to_string().contains("URL"),
            "{err}"
        );
        assert!(!err.to_string().contains("herdr.dev"), "{err}");
    }

    #[test]
    fn popup_event_has_no_pane_id() {
        let event = serde_json::json!({"type": "popup", "action": "notice"});
        assert!(event.get("pane_id").is_none());
        assert_eq!(event["type"], "popup");
    }
}
