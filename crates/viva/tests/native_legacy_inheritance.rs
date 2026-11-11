//! Real cross-backend inheritance (issue #191): a live legacy `viva server`
//! hands its PTY masters to the native host. The marker is read back through
//! the native HTTP API, the old host is gone and the shell PID is unchanged.
//! The failure case corrupts the stored scene book so takeover aborts AFTER
//! the fds arrived: the old host must reattach its readers and keep serving.
//! Never counted as coverage from the default zero-test feature-disabled run.
#![cfg(all(feature = "herdr-runtime", unix))]
use serde_json::{Value, json};
use std::{
    io::{BufRead, BufReader, Write},
    os::unix::net::{UnixListener, UnixStream},
    path::Path,
    process::{Child, Command, Stdio},
    thread,
    time::{Duration, Instant},
};
use viva::{
    foundation::{paths, store::Store},
    server::{Client, RequestKind, WorkbenchTreeOp},
};

struct Fixture(Child);
impl Drop for Fixture {
    fn drop(&mut self) {
        // Exact task-owned PID. Forced fixture cleanup is not lifecycle PASS.
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

fn log_tail(home: &Path, name: &str) -> String {
    std::fs::read_to_string(home.join(name)).unwrap_or_default()
}

fn spawn_legacy(home: &Path) -> Fixture {
    Fixture(
        Command::new(env!("CARGO_BIN_EXE_viva"))
            .arg("server")
            .current_dir(home)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("VIVA_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(home.join("legacy.log")).unwrap())
            .spawn()
            .unwrap(),
    )
}

fn spawn_native(home: &Path) -> Fixture {
    Fixture(
        Command::new(env!("CARGO_BIN_EXE_viva"))
            .args(["runtime", "server"])
            .current_dir(home)
            .env_clear()
            .env("PATH", "/usr/bin:/bin")
            .env("HOME", home)
            .env("SHELL", "/bin/sh")
            .env("TERM", "xterm-256color")
            .env("VIVA_HOME", home)
            .stdin(Stdio::null())
            .stdout(Stdio::null())
            .stderr(std::fs::File::create(home.join("native.log")).unwrap())
            .spawn()
            .unwrap(),
    )
}

fn connect_legacy(home: &Path, legacy: &mut Fixture) -> Client {
    let deadline = Instant::now() + Duration::from_secs(20);
    loop {
        if let Ok(client) = Client::connect(home) {
            return client;
        }
        if let Some(status) = legacy.0.try_wait().unwrap() {
            panic!(
                "legacy host exited before it served: {status}; log: {}",
                log_tail(home, "legacy.log")
            );
        }
        assert!(
            Instant::now() < deadline,
            "legacy host did not answer: {}",
            log_tail(home, "legacy.log")
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn screen_text(snapshot: &Value) -> String {
    snapshot["visible"]
        .as_array()
        .map(|rows| {
            rows.iter()
                .filter_map(|row| row.as_str())
                .collect::<Vec<_>>()
                .join("\n")
        })
        .unwrap_or_default()
}

fn submit_and_wait(client: &mut Client, terminal_id: &str, command: &str, needle: &str) {
    client
        .call(RequestKind::TerminalInput {
            terminal_id: terminal_id.to_string(),
            bytes_hex: viva::server::hex_encode(command.as_bytes()),
        })
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let snapshot = client
            .call(RequestKind::TerminalSnapshot {
                terminal_id: terminal_id.to_string(),
            })
            .unwrap();
        let seen = screen_text(&snapshot).replace(['\n', '\r'], "");
        if seen.contains(needle) {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "`{needle}` never reached the legacy screen: {seen}"
        );
        thread::sleep(Duration::from_millis(30));
    }
}

/// A shell whose marker output proves the child actually executed a command,
/// placed in a saved scene/tab so the import plan has a real layout to map.
fn marked_shell(client: &mut Client, home: &Path) -> (String, u32) {
    const SCENE: &str = "legacy-fixture";
    let created = client
        .call(RequestKind::TerminalCreate {
            argv: vec!["/bin/sh".into()],
            cwd: home.display().to_string(),
            env: Vec::new(),
            cols: 80,
            rows: 24,
            purpose: "legacy inheritance fixture".into(),
            worktree_id: None,
            owner: "user_shell".into(),
        })
        .unwrap();
    let terminal_id = created["terminal_id"].as_str().unwrap().to_string();
    let pid = created["pid"].as_u64().unwrap() as u32;
    client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreateScene {
                id: Some(SCENE.into()),
                name: "legacy scene".into(),
            },
        })
        .unwrap();
    let tree = client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreateTab {
                scene_id: Some(SCENE.into()),
                name: "main".into(),
            },
        })
        .unwrap();
    let tab = tree["tree"]["scenes"][SCENE]["tabs"]
        .as_array()
        .unwrap_or_else(|| panic!("no tabs after CreateTab: {tree}"))
        .iter()
        .find(|tab| tab["name"] == "main")
        .and_then(|tab| tab["id"].as_str())
        .unwrap()
        .to_string();
    client
        .call(RequestKind::WorkbenchTree {
            op: WorkbenchTreeOp::CreatePane {
                scene_id: Some(SCENE.into()),
                tab_id: Some(tab),
                terminal_id: Some(terminal_id.clone()),
                direction: None,
            },
        })
        .unwrap();
    submit_and_wait(
        client,
        &terminal_id,
        "printf 'ALIVE-%s\\n' \"$((6*7))\"\n",
        "ALIVE-42",
    );
    (terminal_id, pid)
}

fn wait_native_ready(home: &Path, native: &mut Fixture) {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = native.0.try_wait().unwrap() {
            panic!(
                "native host exited before serving: {status}; log: {}",
                log_tail(home, "native.log")
            );
        }
        if home.join("runtime/workbench/herdr.sock").exists() {
            return;
        }
        assert!(
            Instant::now() < deadline,
            "native host never bound its socket; log: {}",
            log_tail(home, "native.log")
        );
        thread::sleep(Duration::from_millis(25));
    }
}

fn wait_exit(child: &mut Fixture, what: &str) -> std::process::ExitStatus {
    let deadline = Instant::now() + Duration::from_secs(30);
    loop {
        if let Some(status) = child.0.try_wait().unwrap() {
            return status;
        }
        assert!(Instant::now() < deadline, "{what} did not exit");
        thread::sleep(Duration::from_millis(25));
    }
}

fn call(home: &Path, method: &str, params: Value) -> Value {
    let mut socket = UnixStream::connect(home.join("runtime/workbench/herdr.sock")).unwrap();
    socket
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    writeln!(
        socket,
        "{}",
        json!({"id":"inheritance-fixture", "method":method, "params":params,
            "transport_peer":{"pid":1,"uid":0}, "peer_pid":1, "peer_uid":0, "claimed_role":"user"})
    )
    .unwrap();
    let mut line = String::new();
    BufReader::new(socket).read_line(&mut line).unwrap();
    serde_json::from_str(&line).unwrap()
}

#[test]
fn live_legacy_session_is_inherited_with_its_process_and_screen() {
    let temp = tempfile::Builder::new()
        .prefix("legacy-inherit-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let mut legacy = spawn_legacy(home);
    let mut client = connect_legacy(home, &mut legacy);
    let old_pid = legacy.0.id();
    let (_terminal_id, shell_pid) = marked_shell(&mut client, home);
    let layout: Value = client
        .call(RequestKind::WorkbenchLayout { layout_json: None })
        .unwrap();
    assert!(
        layout["layout"].as_str().is_some_and(|raw| !raw.is_empty()),
        "the legacy host must have persisted a scene book to inherit: {layout}"
    );
    drop(client);

    let mut native = spawn_native(home);
    wait_native_ready(home, &mut native);
    let status = wait_exit(&mut legacy, "legacy host after the prepared takeover");
    assert!(
        status.success(),
        "the acknowledged legacy host should retire cleanly: {status}; log: {}",
        log_tail(home, "legacy.log")
    );

    let snapshot = call(home, "viva.snapshot", json!({}));
    let snapshot = &snapshot["result"]["snapshot"];
    assert_eq!(snapshot["backend"], "herdr-source-library", "{snapshot}");
    assert_eq!(
        snapshot["integration_state"], "native-default-entry-legacy-inheritance",
        "{snapshot}"
    );
    let handoff = &snapshot["legacy_handoff"];
    assert_eq!(handoff["inherited"], true, "{snapshot}");
    assert_eq!(handoff["old_host_pid"].as_u64(), Some(u64::from(old_pid)));
    assert_eq!(handoff["adopted_panes"], 1, "{snapshot}");
    assert_eq!(handoff["fidelity"], "degraded_import");
    let not_preserved = handoff["not_preserved"].as_array().unwrap();
    assert!(
        handoff["preserved"]
            .as_array()
            .unwrap()
            .iter()
            .any(|item| item == "live_child_process")
    );
    assert!(not_preserved.iter().any(|item| item == "pane_cwd"));
    assert!(
        not_preserved
            .iter()
            .any(|item| item == "split_ratios_focus_zoom")
    );

    let panes = call(home, "pane.list", json!({}));
    let panes = panes["result"]["panes"]
        .as_array()
        .unwrap_or_else(|| panic!("pane.list did not answer with panes: {panes}"));
    assert_eq!(
        panes.len(),
        1,
        "exactly the one live legacy terminal: {panes:?}"
    );
    let pane = &panes[0];
    assert_eq!(pane["workspace_id"], "ws-legacy-1");
    let pane_id = pane["pane_id"].as_str().unwrap();

    let process = call(home, "pane.process_info", json!({"pane_id": pane_id}));
    assert_eq!(
        process["result"]["process_info"]["shell_pid"].as_u64(),
        Some(u64::from(shell_pid)),
        "the adopted pane must still own the original child: {process}"
    );
    let read = call(
        home,
        "pane.read",
        json!({"pane_id": pane_id, "source": "recent", "format": "text"}),
    );
    let text = read["result"]["read"]["text"].as_str().unwrap_or_default();
    assert!(
        text.contains("ALIVE-42"),
        "the inherited screen lost the marker: {text}"
    );
    assert!(!home.join("server.sock").exists());
}

#[test]
fn failed_adoption_leaves_the_old_host_serving_and_its_process_alive() {
    let temp = tempfile::Builder::new()
        .prefix("legacy-refuse-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    let mut legacy = spawn_legacy(home);
    let mut client = connect_legacy(home, &mut legacy);
    let (terminal_id, shell_pid) = marked_shell(&mut client, home);
    drop(client);

    // Corruption lands AFTER the transfer begins: the receiver accepts the
    // fds, then refuses to build a plan from this book. It must close the
    // fds without acknowledging.
    let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
    store
        .connection()
        .execute(
            "INSERT OR REPLACE INTO settings(key,value) VALUES('workbench_layout','not-json')",
            [],
        )
        .unwrap();
    drop(store);

    let mut native = spawn_native(home);
    let status = wait_exit(&mut native, "native host after a refused takeover");
    assert!(!status.success(), "a refused takeover must not serve");
    let native_log = log_tail(home, "native.log");
    assert!(
        native_log.contains("legacy scene book is invalid"),
        "expected the corrupt-book refusal, got: {native_log}"
    );
    assert!(!home.join("runtime/workbench/herdr.sock").exists());
    assert!(
        legacy.0.try_wait().unwrap().is_none(),
        "the old host must still be running; log: {}",
        log_tail(home, "legacy.log")
    );

    let mut client = connect_legacy(home, &mut legacy);
    assert_eq!(
        client.call(RequestKind::TerminalList).unwrap()["terminals"]
            .as_array()
            .map(Vec::len),
        Some(1)
    );
    submit_and_wait(
        &mut client,
        &terminal_id,
        "printf 'STILL-%s\\n' \"$((6*7))\"\n",
        "STILL-42",
    );
    let list = client.call(RequestKind::TerminalList).unwrap();
    // SAFETY: kill(pid, 0) is an existence probe with no signal delivery.
    assert_eq!(
        unsafe { libc::kill(shell_pid as i32, 0) },
        0,
        "the shell process must survive the refused takeover: {list}"
    );
    drop(client);

    let recovery: Vec<String> = {
        let store = Store::open(&paths::database_path(home), viva::server::migrations()).unwrap();
        let mut stmt = store
            .connection()
            .prepare("SELECT kind FROM recovery_events ORDER BY seq")
            .unwrap();
        let rows = stmt.query_map([], |row| row.get::<_, String>(0)).unwrap();
        rows.collect::<Result<Vec<_>, _>>().unwrap()
    };
    assert!(
        recovery.iter().any(|kind| kind == "live_handoff_failed"),
        "the old host must record the failed transfer: {recovery:?}"
    );
}

#[test]
fn a_stale_legacy_socket_does_not_block_a_fresh_native_session() {
    let temp = tempfile::Builder::new()
        .prefix("legacy-stale-")
        .tempdir_in("/tmp")
        .unwrap();
    let home = temp.path();
    // A crashed host leaves the bound path on disk; connecting refuses.
    let stale = home.join(viva::server::SOCKET_NAME);
    drop(UnixListener::bind(&stale).unwrap());

    let mut native = spawn_native(home);
    wait_native_ready(home, &mut native);
    let snapshot = call(home, "viva.snapshot", json!({}));
    let snapshot = &snapshot["result"]["snapshot"];
    assert_eq!(snapshot["integration_state"], "native-default-entry");
    assert_eq!(snapshot["legacy_handoff"]["inherited"], false, "{snapshot}");
    assert!(
        log_tail(home, "native.log").contains("starting a fresh native session"),
        "the stale socket must be reported, not silently claimed: {}",
        log_tail(home, "native.log")
    );
    assert!(
        stale.exists(),
        "the stale file stays for the legacy entry to claim later"
    );
}
