//! Local plugins: install from a directory, open a split through `viva scene`,
//! receive the event on the plugin socket, and leave nothing behind on
//! uninstall. URLs are refused. A member still needs a grant.

use std::fs;
use std::path::{Path, PathBuf};
use std::process::Command;
use std::thread;
use std::time::{Duration, Instant};

use viva::server::{Client, Host, RequestKind, SOCKET_NAME};

fn viva_bin() -> &'static str {
    env!("CARGO_BIN_EXE_viva")
}

struct Running {
    home: PathBuf,
    server: Option<std::thread::JoinHandle<()>>,
    _dir: tempfile::TempDir,
}

fn start() -> Running {
    let dir = tempfile::tempdir().expect("tempdir");
    let home = dir.path().join("h");
    fs::create_dir_all(&home).expect("home");
    let host = Host::open(&home).expect("host");
    let server = host.serve_background();
    let socket = home.join(SOCKET_NAME);
    let deadline = Instant::now() + Duration::from_secs(5);
    while !socket.exists() && Instant::now() < deadline {
        thread::sleep(Duration::from_millis(20));
    }
    assert!(socket.exists(), "server socket did not appear");
    Running {
        home,
        server: Some(server),
        _dir: dir,
    }
}

impl Running {
    fn shutdown(&mut self) {
        if let Ok(mut client) = Client::connect(&self.home) {
            let _ = client.call(RequestKind::Shutdown { close_policy: None });
        }
        if let Some(server) = self.server.take() {
            let _ = server.join();
        }
    }
}

impl Drop for Running {
    fn drop(&mut self) {
        self.shutdown();
    }
}

fn run_viva(home: &Path, args: &[&str]) -> std::process::Output {
    Command::new(viva_bin())
        .env("VIVA_HOME", home)
        .env_remove("VIVA_PLUGIN_SOCKET")
        .env_remove("VIVA_PLUGIN_DIR")
        .env_remove("VIVA_PLUGIN_ID")
        .args(args)
        .output()
        .expect("spawn viva")
}

fn wait_until(mut pred: impl FnMut() -> bool) -> bool {
    let deadline = Instant::now() + Duration::from_secs(15);
    while Instant::now() < deadline {
        if pred() {
            return true;
        }
        thread::sleep(Duration::from_millis(50));
    }
    pred()
}

fn example_dir() -> PathBuf {
    PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../../examples/plugins/split-echo")
}

#[test]
fn install_rejects_urls_without_echoing_credentials() {
    let running = start();
    let secret = "s3cret-token";
    let source = format!("https://user:{secret}@herdr.dev/api/plugins/demo");
    let output = run_viva(&running.home, &["plugin", "install", &source]);
    assert!(!output.status.success(), "URL install must fail");
    let err = String::from_utf8_lossy(&output.stderr);
    assert!(err.contains("local directory"), "{err}");
    assert!(!err.contains(secret), "{err}");
    assert!(
        !running.home.join("plugins").join("registry.json").exists(),
        "a refused install must not write a registry"
    );
}

#[test]
fn example_plugin_opens_a_split_receives_the_event_and_uninstall_clears_it() {
    let running = start();
    let home = &running.home;
    let source = example_dir();
    assert!(
        source.join("viva-plugin.json").is_file(),
        "example plugin missing"
    );

    let installed = run_viva(home, &["plugin", "install", source.to_str().unwrap()]);
    let stdout = String::from_utf8_lossy(&installed.stdout);
    let stderr = String::from_utf8_lossy(&installed.stderr);
    assert!(
        installed.status.success(),
        "install failed\nstdout: {stdout}\nstderr: {stderr}"
    );
    let info: serde_json::Value = serde_json::from_str(&stdout).expect("install json");
    let pid = info["pid"].as_u64().expect("pid") as u32;
    assert!(
        viva::plugins::process_running(pid),
        "plugin process is not running"
    );

    let received = home.join("plugins/split-echo/received.json");
    let err_log = home.join("plugins/split-echo/plugin.err");
    let arrived = wait_until(|| received.is_file());
    if !arrived {
        let err_text = fs::read_to_string(&err_log).unwrap_or_default();
        panic!("plugin did not record an event\nplugin.err:\n{err_text}");
    }
    let event: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&received).unwrap()).expect("event json");
    assert_eq!(event["type"], "pane.opened");
    assert_eq!(event["open"], "split");
    assert!(!event["pane_id"].as_str().unwrap().is_empty());

    let listed = run_viva(home, &["scene", "list"]);
    let tree = String::from_utf8_lossy(&listed.stdout);
    assert!(
        tree.contains("\"Split\""),
        "scene tree has no split: {tree}"
    );

    let list = run_viva(home, &["plugin", "list"]);
    assert!(list.status.success());
    assert!(String::from_utf8_lossy(&list.stdout).contains("split-echo"));

    let removed = run_viva(home, &["plugin", "uninstall", "split-echo"]);
    assert!(
        removed.status.success(),
        "uninstall failed: {}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!home.join("plugins/split-echo").exists());
    assert!(!home.join("ps/split-echo.sock").exists());
    assert!(!received.exists());
    let still = wait_until(|| !viva::plugins::process_running(pid));
    assert!(still, "plugin process {pid} survived uninstall");
    let after = run_viva(home, &["plugin", "list"]);
    let after_text = String::from_utf8_lossy(&after.stdout);
    assert!(!after_text.contains("split-echo"), "{after_text}");
}

#[test]
fn open_modes_are_ordinary_panes_and_popup_has_no_pane_id() {
    let running = start();
    let home = &running.home;
    let source = home_sibling_source(&running);
    write_recorder(&source);
    let installed = run_viva(home, &["plugin", "install", source.to_str().unwrap()]);
    assert!(
        installed.status.success(),
        "recorder install failed: {} {}",
        String::from_utf8_lossy(&installed.stdout),
        String::from_utf8_lossy(&installed.stderr)
    );
    let ready = home.join("plugins/mode-recorder/ready");
    assert!(
        wait_until(|| ready.is_file()),
        "recorder did not listen: {}",
        fs::read_to_string(home.join("plugins/mode-recorder/plugin.err")).unwrap_or_default()
    );

    for args in [
        vec!["scene", "create", "--id", "modes", "--name", "Modes"],
        vec![
            "scene", "tab", "create", "--scene", "modes", "--name", "main",
        ],
        vec![
            "scene",
            "pane",
            "create",
            "--scene",
            "modes",
            "--direction",
            "right",
        ],
        vec!["scene", "pane", "zoom", "--scene", "modes", "--on", "true"],
        vec![
            "scene",
            "pane",
            "split",
            "--scene",
            "modes",
            "--direction",
            "down",
        ],
    ] {
        let output = run_viva(home, &args);
        assert!(
            output.status.success(),
            "{args:?} failed: {}",
            String::from_utf8_lossy(&output.stderr)
        );
    }

    let log_path = home.join("plugins/mode-recorder/events.jsonl");
    let saw = wait_until(|| {
        let text = fs::read_to_string(&log_path).unwrap_or_default();
        [
            "\"open\":\"tab\"",
            "\"open\":\"overlay\"",
            "\"open\":\"zoomed\"",
            "\"open\":\"split\"",
        ]
        .iter()
        .all(|needle| text.contains(needle))
    });
    let text = fs::read_to_string(&log_path).unwrap_or_default();
    assert!(saw, "missing pane events:\n{text}");
    for line in text.lines() {
        let event: serde_json::Value = serde_json::from_str(line).expect(line);
        assert_eq!(event["type"], "pane.opened");
        assert!(event["pane_id"].as_str().is_some_and(|id| !id.is_empty()));
    }

    let popup_src = source.parent().unwrap().join("popup-note");
    write_popup(&popup_src);
    let popup_install = run_viva(home, &["plugin", "install", popup_src.to_str().unwrap()]);
    assert!(
        popup_install.status.success(),
        "popup install failed: {}",
        String::from_utf8_lossy(&popup_install.stderr)
    );
    let popup_ready = home.join("plugins/popup-note/ready");
    assert!(
        wait_until(|| popup_ready.is_file()),
        "popup plugin did not listen"
    );
    let opened = viva::plugins::deliver_popup(home, "popup-note", "notice").expect("popup");
    assert!(opened.is_none());
    let popup_received = home.join("plugins/popup-note/received.json");
    assert!(
        wait_until(|| popup_received.is_file()),
        "popup event was not delivered"
    );
    let popup: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(&popup_received).unwrap()).unwrap();
    assert_eq!(popup["type"], "popup");
    assert!(popup.get("pane_id").is_none());

    for id in ["mode-recorder", "popup-note"] {
        let output = run_viva(home, &["plugin", "uninstall", id]);
        assert!(
            output.status.success(),
            "{id} uninstall: {}",
            String::from_utf8_lossy(&output.stderr)
        );
        assert!(!home.join("plugins").join(id).exists());
    }
}

#[test]
fn secrets_are_not_copied_and_member_calls_still_need_a_grant() {
    let running = start();
    let home = &running.home;
    let source = home_sibling_source(&running).join("secret-plugin");
    fs::create_dir_all(&source).unwrap();
    let secret = "sk-plugin-test-secret-value";
    fs::write(source.join(".env"), format!("TOKEN={secret}\n")).unwrap();
    fs::write(
        source.join("viva-plugin.json"),
        r#"{
            "id": "secret-plugin",
            "name": "Secret plugin",
            "version": "0.1.0",
            "entry": "plugin.py",
            "actions": [{"id": "sleep", "title": "Sleep", "pane": "split"}],
            "events": ["pane.opened"]
        }"#,
    )
    .unwrap();
    fs::write(
        source.join("plugin.py"),
        "#!/usr/bin/env python3\nimport time\nwhile True:\n    time.sleep(3600)\n",
    )
    .unwrap();

    let installed = run_viva(home, &["plugin", "install", source.to_str().unwrap()]);
    assert!(
        installed.status.success(),
        "install failed: {}",
        String::from_utf8_lossy(&installed.stderr)
    );
    assert!(!String::from_utf8_lossy(&installed.stdout).contains(secret));
    assert!(!String::from_utf8_lossy(&installed.stderr).contains(secret));
    let copied = home.join("plugins/secret-plugin");
    assert!(!copied.join(".env").exists());
    for entry in walk(&copied) {
        let text = fs::read_to_string(&entry).unwrap_or_default();
        assert!(
            !text.contains(secret),
            "{} contains a secret",
            entry.display()
        );
    }
    let registry = fs::read_to_string(home.join("plugins/registry.json")).unwrap_or_default();
    assert!(!registry.contains(secret));

    let created = run_viva(
        home,
        &["scene", "create", "--id", "grant", "--name", "Grant"],
    );
    assert!(
        created.status.success(),
        "{}",
        String::from_utf8_lossy(&created.stderr)
    );
    let tab = run_viva(
        home,
        &[
            "scene", "tab", "create", "--scene", "grant", "--name", "main",
        ],
    );
    assert!(
        tab.status.success(),
        "{}",
        String::from_utf8_lossy(&tab.stderr)
    );
    let denied = run_viva(
        home,
        &[
            "scene",
            "pane",
            "split",
            "--scene",
            "grant",
            "--member",
            "member-1",
            "--direction",
            "right",
        ],
    );
    assert!(
        !denied.status.success(),
        "member split without a grant must fail"
    );
    let err = String::from_utf8_lossy(&denied.stderr);
    assert!(err.contains("grant"), "{err}");

    let removed = run_viva(home, &["plugin", "uninstall", "secret-plugin"]);
    assert!(
        removed.status.success(),
        "{}",
        String::from_utf8_lossy(&removed.stderr)
    );
    assert!(!copied.exists());
}

fn home_sibling_source(running: &Running) -> PathBuf {
    let path = running._dir.path().join("src");
    fs::create_dir_all(&path).unwrap();
    path
}

fn write_recorder(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("viva-plugin.json"),
        r#"{
            "id": "mode-recorder",
            "name": "Mode recorder",
            "version": "0.1.0",
            "entry": "plugin.py",
            "actions": [
                {"id": "overlay", "title": "Overlay", "pane": "overlay"},
                {"id": "split", "title": "Split", "pane": "split"},
                {"id": "tab", "title": "Tab", "pane": "tab"},
                {"id": "zoomed", "title": "Zoomed", "pane": "zoomed"}
            ],
            "events": ["pane.opened"]
        }"#,
    )
    .unwrap();
    fs::write(
        dir.join("plugin.py"),
        r#"#!/usr/bin/env python3
import os, socket
sock_path = os.environ["VIVA_PLUGIN_SOCKET"]
plugin_dir = os.environ["VIVA_PLUGIN_DIR"]
try:
    os.remove(sock_path)
except FileNotFoundError:
    pass
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(sock_path)
os.chmod(sock_path, 0o600)
server.listen(16)
open(os.path.join(plugin_dir, "ready"), "w").close()
log_path = os.path.join(plugin_dir, "events.jsonl")
while True:
    conn, _addr = server.accept()
    buf = b""
    while b"\n" not in buf:
        chunk = conn.recv(4096)
        if not chunk:
            break
        buf += chunk
    conn.close()
    with open(log_path, "ab") as handle:
        handle.write(buf if buf.endswith(b"\n") else buf + b"\n")
"#,
    )
    .unwrap();
}

fn write_popup(dir: &Path) {
    fs::create_dir_all(dir).unwrap();
    fs::write(
        dir.join("viva-plugin.json"),
        r#"{
            "id": "popup-note",
            "name": "Popup note",
            "version": "0.1.0",
            "entry": "plugin.py",
            "actions": [{"id": "notice", "title": "Notice", "pane": "popup"}],
            "events": ["popup"]
        }"#,
    )
    .unwrap();
    fs::write(
        dir.join("plugin.py"),
        r#"#!/usr/bin/env python3
import json, os, socket
sock_path = os.environ["VIVA_PLUGIN_SOCKET"]
plugin_dir = os.environ["VIVA_PLUGIN_DIR"]
try:
    os.remove(sock_path)
except FileNotFoundError:
    pass
server = socket.socket(socket.AF_UNIX, socket.SOCK_STREAM)
server.bind(sock_path)
os.chmod(sock_path, 0o600)
server.listen(1)
open(os.path.join(plugin_dir, "ready"), "w").close()
conn, _addr = server.accept()
buf = b""
while b"\n" not in buf:
    chunk = conn.recv(4096)
    if not chunk:
        break
    buf += chunk
conn.close()
event = json.loads(buf.decode())
assert "pane_id" not in event
with open(os.path.join(plugin_dir, "received.json"), "w") as handle:
    json.dump(event, handle)
import time
while True:
    time.sleep(3600)
"#,
    )
    .unwrap();
}

fn walk(dir: &Path) -> Vec<PathBuf> {
    let mut out = Vec::new();
    let entries = match fs::read_dir(dir) {
        Ok(entries) => entries,
        Err(_) => return out,
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            out.extend(walk(&path));
        } else {
            out.push(path);
        }
    }
    out
}
